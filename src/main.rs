use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueHint};
use gmx_rs::artifact::{GeneratedFile, merge_generated_files, write_generated_files};
use gmx_rs::assets::Assets;
use gmx_rs::cache::{BuildCache, build_dependencies, cache_root_for_target};
use gmx_rs::compile::{BuildOptions, write_vm_wad_with_artifacts_in};
use gmx_rs::config::Config;
use gmx_rs::gml::{analyze_assets, compile_vm};
use gmx_rs::project::ProjectManifest;

#[derive(Debug, Parser)]
#[command(
    name = "gmx",
    version,
    about = "Compile GameMaker: Studio 1.4 GMX projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Compile a project and its runtime files.
    Build(BuildArgs),
    /// Compile and run a project.
    Run(BuildArgs),
    /// Check that a project's GML can be compiled to VM bytecode.
    Check(ProjectArgs),
    /// Remove a project's build directory and caches.
    Clean(CleanArgs),
}

#[derive(Debug, Args)]
struct ManifestArgs {
    /// Path to the project's *.project.gmx manifest.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    manifest_path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ProjectArgs {
    #[command(flatten)]
    manifest: ManifestArgs,

    /// GameMaker project configuration to use.
    #[arg(long, value_name = "NAME", default_value = "Default")]
    config: String,
}

#[derive(Debug, Args)]
struct BuildArgs {
    #[command(flatten)]
    project: ProjectArgs,

    /// Directory for data.win, the Runner, external files, and caches.
    #[arg(long, value_name = "DIRECTORY", value_hint = ValueHint::DirPath)]
    target_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CleanArgs {
    #[command(flatten)]
    manifest: ManifestArgs,

    /// Build directory to remove.
    #[arg(long, value_name = "DIRECTORY", value_hint = ValueHint::DirPath)]
    target_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = error.exit_code();
            let _ = error.print();
            return ExitCode::from(u8::try_from(exit_code).unwrap_or(1));
        }
    };
    let Some(command) = cli.command else {
        let _ = Cli::command().print_help();
        println!();
        return ExitCode::SUCCESS;
    };

    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(AppError::Runtime(error)) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
        Err(AppError::Message(message)) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: CliCommand) -> Result<(), AppError> {
    match command {
        CliCommand::Build(args) => {
            let input = resolve_manifest(args.project.manifest.manifest_path)?;
            let target = resolve_target_dir(&input, args.target_dir)?;
            build(&input, &target.join("data.win"), &args.project.config)
        }
        CliCommand::Run(args) => {
            let input = resolve_manifest(args.project.manifest.manifest_path)?;
            let target = resolve_target_dir(&input, args.target_dir)?;
            run_project(&input, &target, &args.project.config)
        }
        CliCommand::Check(args) => {
            let input = resolve_manifest(args.manifest.manifest_path)?;
            check_project(&input, &args.config)
        }
        CliCommand::Clean(args) => {
            let input = resolve_manifest(args.manifest.manifest_path)?;
            let project_directory = input.parent().unwrap_or_else(|| Path::new("."));
            let target = resolve_target_dir(&input, args.target_dir)?;
            clean_project_build(project_directory, &target)
        }
    }
}

fn build(input: &Path, output: &Path, config_name: &str) -> Result<(), AppError> {
    let started = Instant::now();
    if input == output {
        return Err(AppError::Message(
            "project input and WAD output paths must be different".to_owned(),
        ));
    }
    let project = ProjectManifest::load(input)?;
    let runner_path = find_external_runner()?;
    let runner = external_runner_file(&runner_path, &project.name)?;
    let output_dir = output.parent().unwrap_or_else(|| Path::new("."));
    let cache_root = cache_root_for_target(output_dir);
    let cache = BuildCache::from_env_at(input, config_name, &cache_root);
    let known_build = if let Some(cache) = &cache {
        match cache.known_build_if_unchanged() {
            Ok(Some(known)) => {
                let contains_runner = match known.contains_dependency(&runner_path) {
                    Ok(contains_runner) => contains_runner,
                    Err(error) => {
                        eprintln!("warning: cannot validate cached Runner dependency: {error}");
                        false
                    }
                };
                if contains_runner && restore_cached_build(cache, &known.key, output, started) {
                    return Ok(());
                }
                Some(known)
            }
            Ok(None) => None,
            Err(error) => {
                eprintln!("warning: cannot read build cache: {error}");
                None
            }
        }
    } else {
        None
    };
    let config = Config::load_from_project(&project, config_name)?;
    let assets = Assets::load(&project, &config)?;
    let mut dependencies = build_dependencies(&project, &config, &assets);
    dependencies.push(runner_path);
    let cache_key = cache.as_ref().and_then(|cache| match &known_build {
        Some(known) if known.matches_dependencies(&dependencies).unwrap_or(false) => {
            Some(known.key.clone())
        }
        _ => match cache.key_with_assets(&dependencies, &assets) {
            Ok(key) => Some(key),
            Err(error) => {
                eprintln!("warning: cannot fingerprint build dependencies: {error}");
                None
            }
        },
    });
    if let (Some(cache), Some(key)) = (&cache, &cache_key)
        && restore_cached_build(cache, key, output, started)
    {
        if let Err(error) = cache.save_dependencies(&dependencies) {
            eprintln!("warning: cannot update build-cache dependencies: {error}");
        }
        return Ok(());
    }
    let analysis = analyze_assets(&assets)
        .map_err(|errors| AppError::Message(format_diagnostics("GML analysis failed", &errors)))?;
    let compiled = compile_vm(&assets, &analysis).map_err(|errors| {
        AppError::Message(format_diagnostics("VM compilation failed", &errors))
    })?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut writer = BufWriter::new(file);
    let mut built = write_vm_wad_with_artifacts_in(
        &project,
        &assets,
        &BuildOptions::default(),
        &compiled,
        &cache_root,
        &mut writer,
    )?;
    writer.flush()?;
    built.external_files.push(runner);
    built.external_files = merge_generated_files(std::mem::take(&mut built.external_files))?;
    let output_name = output.file_name().unwrap_or_default().to_string_lossy();
    for artifact in &built.external_files {
        if artifact.path.components().count() == 1
            && artifact
                .path
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&output_name)
        {
            return Err(AppError::Message(format!(
                "generated file would overwrite WAD output: {}",
                artifact.path.display()
            )));
        }
    }
    write_generated_files(output_dir, &built.external_files)?;
    if let (Some(cache), Some(key)) = (&cache, &cache_key) {
        if let Err(error) = cache.store(key, output, &built.external_files) {
            eprintln!("warning: cannot store build cache: {error}");
        } else {
            if let Err(error) = cache.save_dependencies(&dependencies) {
                eprintln!("warning: cannot store build-cache dependencies: {error}");
            }
            if let Err(error) = cache.record_output(key, output, &built.external_files) {
                eprintln!("warning: cannot record build-cache output: {error}");
            }
        }
    }
    println!(
        "built {} bytes with {} chunks, {} VM code units, and {} external files in {:.3}s: {}",
        built.wad.file_size,
        built.wad.chunks.len(),
        compiled.codes.len(),
        built.external_files.len(),
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

fn restore_cached_build(cache: &BuildCache, key: &str, output: &Path, started: Instant) -> bool {
    match cache.restore(key, output) {
        Ok(Some(hit)) => {
            println!(
                "restored {} bytes and {} external files from build cache in {:.3}s: {}",
                hit.wad_bytes,
                hit.external_files,
                started.elapsed().as_secs_f64(),
                output.display()
            );
            true
        }
        Ok(None) => false,
        Err(error) => {
            eprintln!("warning: cannot restore build cache: {error}");
            false
        }
    }
}

#[cfg(windows)]
fn run_project(input: &Path, build_directory: &Path, config_name: &str) -> Result<(), AppError> {
    let project = ProjectManifest::load(input)?;
    build(input, &build_directory.join("data.win"), config_name)?;

    let executable = runner_executable_path(build_directory, &project.name);
    if !executable.is_file() {
        return Err(AppError::Message(format!(
            "built Runner executable is missing: {}",
            executable.display()
        )));
    }

    println!("running {}", executable.display());
    let status = Command::new(&executable)
        .current_dir(build_directory)
        .status()
        .map_err(|error| {
            AppError::Message(format!(
                "cannot start Runner executable {}: {error}",
                executable.display()
            ))
        })?;
    if !status.success() {
        return Err(AppError::Message(format!(
            "Runner executable {} exited with {status}",
            executable.display()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn run_project(_input: &Path, _build_directory: &Path, _config_name: &str) -> Result<(), AppError> {
    Err(AppError::Message(
        "gmx run requires Windows because the GMS 1.4 Runner is a Windows executable".to_owned(),
    ))
}

#[cfg(any(windows, test))]
fn runner_executable_path(build_directory: &Path, project_name: &str) -> PathBuf {
    build_directory.join(format!("{project_name}.exe"))
}

fn find_external_runner() -> Result<PathBuf, AppError> {
    let executable = env::current_exe()?;
    let runner = runner_path_next_to(&executable)?;
    if !runner.is_file() {
        return Err(AppError::Message(format!(
            "Runner.exe was not found next to gmx: {}",
            runner.display()
        )));
    }
    Ok(fs::canonicalize(runner)?)
}

fn runner_path_next_to(executable: &Path) -> Result<PathBuf, AppError> {
    let directory = executable.parent().ok_or_else(|| {
        AppError::Message(format!(
            "cannot determine the directory containing gmx: {}",
            executable.display()
        ))
    })?;
    Ok(directory.join("Runner.exe"))
}

fn external_runner_file(source: &Path, project_name: &str) -> Result<GeneratedFile, AppError> {
    let data = fs::read(source).map_err(|error| {
        AppError::Message(format!(
            "cannot read Runner executable {}: {error}",
            source.display()
        ))
    })?;
    if !data.starts_with(b"MZ") {
        return Err(AppError::Message(format!(
            "Runner executable is not a Windows PE file: {}",
            source.display()
        )));
    }
    Ok(GeneratedFile::new(
        PathBuf::from(format!("{project_name}.exe")),
        data,
    ))
}

fn format_diagnostics<T: std::fmt::Display>(heading: &str, errors: &[T]) -> String {
    const LIMIT: usize = 50;
    let mut message = format!("{heading} with {} diagnostic(s)", errors.len());
    for error in errors.iter().take(LIMIT) {
        message.push('\n');
        message.push_str(&error.to_string());
    }
    if errors.len() > LIMIT {
        message.push_str(&format!("\n... {} more", errors.len() - LIMIT));
    }
    message
}

fn check_project(input: &Path, config_name: &str) -> Result<(), AppError> {
    let project = ProjectManifest::load(input)?;
    let config = Config::load_from_project(&project, config_name)?;
    let assets = Assets::load(&project, &config)?;
    let analysis = analyze_assets(&assets).map_err(|errors| {
        for error in &errors {
            eprintln!("{error}");
        }
        AppError::Message(format!(
            "GML analysis failed with {} diagnostics",
            errors.len()
        ))
    })?;
    match compile_vm(&assets, &analysis) {
        Ok(compiled) => {
            let summary = compiled.summary;
            println!(
                "compiled {} code units into {} VM bytes",
                summary.code_units, summary.bytecode_bytes
            );
            println!(
                "recorded {} variable, {} function, and {} string references",
                summary.variable_references, summary.function_references, summary.string_references
            );
            Ok(())
        }
        Err(errors) => {
            for error in &errors {
                eprintln!("{error}");
            }
            Err(AppError::Message(format!(
                "VM compilation failed with {} diagnostics",
                errors.len()
            )))
        }
    }
}

fn resolve_manifest(manifest_path: Option<PathBuf>) -> Result<PathBuf, AppError> {
    let current_directory = env::current_dir()?;
    let manifest = strip_current_directory_components(match manifest_path {
        Some(path) if path.is_absolute() => path,
        Some(path) => current_directory.join(path),
        None => find_project_in(&current_directory)?,
    });
    if !manifest.is_file() {
        return Err(AppError::Message(format!(
            "project manifest does not exist or is not a file: {}",
            manifest.display()
        )));
    }
    Ok(manifest)
}

fn resolve_target_dir(
    manifest: &Path,
    target_directory: Option<PathBuf>,
) -> Result<PathBuf, AppError> {
    let current_directory = env::current_dir()?;
    let target = target_directory.unwrap_or_else(|| {
        manifest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join("build")
    });
    Ok(strip_current_directory_components(
        if target.is_absolute() {
            target
        } else {
            current_directory.join(target)
        },
    ))
}

fn strip_current_directory_components(path: PathBuf) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
}

fn find_project_in(directory: &Path) -> Result<PathBuf, AppError> {
    let mut projects = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name();
        if name
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(".project.gmx")
        {
            projects.push(entry.path());
        }
    }
    projects.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .cmp(right.file_name().unwrap_or_default())
    });
    match projects.as_slice() {
        [project] => Ok(project.clone()),
        [] => Err(AppError::Message(format!(
            "no *.project.gmx file found in current directory {}",
            directory.display()
        ))),
        _ => Err(AppError::Message(format!(
            "multiple *.project.gmx files found in current directory {}: {}",
            directory.display(),
            projects
                .iter()
                .map(|project| { project.file_name().unwrap_or_default().to_string_lossy() })
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn clean_project_build(project_directory: &Path, build_directory: &Path) -> Result<(), AppError> {
    let metadata = match fs::symlink_metadata(build_directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "no build artifacts to remove: {}",
                build_directory.display()
            );
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse_point(&metadata) {
        return Err(AppError::Message(format!(
            "refusing to clean linked build directory {}",
            build_directory.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(AppError::Message(format!(
            "refusing to clean non-directory build path {}",
            build_directory.display()
        )));
    }

    let project_directory = fs::canonicalize(project_directory)?;
    let resolved = fs::canonicalize(build_directory)?;
    if resolved.parent().is_none()
        || resolved == project_directory
        || project_directory.starts_with(&resolved)
    {
        return Err(AppError::Message(format!(
            "refusing to clean unsafe build directory: {}",
            resolved.display()
        )));
    }
    fs::remove_dir_all(&resolved).map_err(|error| {
        AppError::Message(format!(
            "cannot remove build artifacts {}: {error}",
            resolved.display()
        ))
    })?;
    println!("removed build artifacts: {}", build_directory.display());
    Ok(())
}

#[cfg(unix)]
fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[derive(Debug)]
enum AppError {
    Runtime(Box<dyn Error>),
    Message(String),
}

impl<E> From<E> for AppError
where
    E: Error + 'static,
{
    fn from(error: E) -> Self {
        Self::Runtime(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use clap::Parser;

    use super::{
        AppError, Cli, CliCommand, external_runner_file, find_project_in, runner_executable_path,
        runner_path_next_to,
    };

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn detects_the_only_project_in_a_directory() {
        let directory = TempDirectory::new("single");
        fs::write(directory.path.join("notes.txt"), b"ignored").unwrap();
        fs::create_dir(directory.path.join("folder.project.gmx")).unwrap();
        let project = directory.path.join("Game.PROJECT.GMX");
        fs::write(&project, b"project").unwrap();

        assert_eq!(find_project_in(&directory.path).unwrap(), project);
    }

    #[test]
    fn reports_missing_and_ambiguous_projects() {
        let directory = TempDirectory::new("errors");
        let error = find_project_in(&directory.path).unwrap_err();
        assert!(
            matches!(error, AppError::Message(message) if message.contains("no *.project.gmx"))
        );

        fs::write(directory.path.join("B.project.gmx"), b"project").unwrap();
        fs::write(directory.path.join("A.project.gmx"), b"project").unwrap();
        let error = find_project_in(&directory.path).unwrap_err();
        assert!(
            matches!(error, AppError::Message(message) if message.ends_with("A.project.gmx, B.project.gmx"))
        );
    }

    #[test]
    fn parses_cargo_style_build_options() {
        let cli = Cli::try_parse_from([
            "gmx",
            "build",
            "--manifest-path",
            "Game.project.gmx",
            "--target-dir",
            "artifacts",
            "--config",
            "Release",
        ])
        .unwrap();

        let Some(CliCommand::Build(args)) = cli.command else {
            panic!("expected build command");
        };
        assert_eq!(
            args.project.manifest.manifest_path,
            Some(PathBuf::from("Game.project.gmx"))
        );
        assert_eq!(args.target_dir, Some(PathBuf::from("artifacts")));
        assert_eq!(args.project.config, "Release");
    }

    #[test]
    fn rejects_removed_diagnostic_commands_and_aliases() {
        for command in [
            "check-vm",
            "check-gml",
            "inspect",
            "extract",
            "inspect-project",
            "diff",
            "diff-semantic",
        ] {
            assert!(
                Cli::try_parse_from(["gmx", command]).is_err(),
                "{command} should not be accepted"
            );
        }
    }

    #[test]
    fn rejects_legacy_build_positionals() {
        assert!(
            Cli::try_parse_from(["gmx", "build", "Game.project.gmx", "build/data.win"]).is_err()
        );
    }

    #[test]
    fn runner_executable_preserves_the_project_name() {
        assert_eq!(
            runner_executable_path(PathBuf::from("build").as_path(), "My Game"),
            PathBuf::from("build").join("My Game.exe")
        );
    }

    #[test]
    fn runner_source_is_only_next_to_gmx() {
        assert_eq!(
            runner_path_next_to(PathBuf::from("tools/gmx.exe").as_path()).unwrap(),
            PathBuf::from("tools/Runner.exe")
        );
    }

    #[test]
    fn external_runner_is_renamed_and_keeps_its_bytes() {
        let directory = TempDirectory::new("external-runner");
        let source = directory.path.join("Runner.exe");
        fs::write(&source, b"MZcustom runner").unwrap();

        let runner = external_runner_file(&source, "My Game").unwrap();

        assert_eq!(runner.path, PathBuf::from("My Game.exe"));
        assert_eq!(runner.data.as_ref(), b"MZcustom runner");
    }

    #[test]
    fn external_runner_rejects_non_pe_files() {
        let directory = TempDirectory::new("invalid-runner");
        let source = directory.path.join("Runner.exe");
        fs::write(&source, b"not an executable").unwrap();

        let error = external_runner_file(&source, "My Game").unwrap_err();

        assert!(
            matches!(error, AppError::Message(message) if message.contains("not a Windows PE"))
        );
    }

    #[test]
    fn clean_removes_only_the_project_build_directory() {
        let directory = TempDirectory::new("clean");
        let project = directory.path.join("Game.project.gmx");
        fs::write(&project, b"project").unwrap();
        let build = directory.path.join("build");
        fs::create_dir_all(build.join(".gmx-cache/build")).unwrap();
        fs::write(build.join("data.win"), b"artifact").unwrap();
        fs::write(build.join(".gmx-cache/build/complete"), b"cache").unwrap();

        super::clean_project_build(&directory.path, &build).unwrap();

        assert!(!build.exists());
        assert!(project.is_file());
        super::clean_project_build(&directory.path, &build).unwrap();
    }

    #[test]
    fn clean_rejects_a_non_directory_build_path() {
        let directory = TempDirectory::new("clean-file");
        let build = directory.path.join("build");
        fs::write(&build, b"not a directory").unwrap();

        let error = super::clean_project_build(&directory.path, &build).unwrap_err();

        assert!(matches!(error, AppError::Message(message) if message.contains("non-directory")));
        assert!(build.is_file());
    }

    struct TempDirectory {
        path: PathBuf,
    }

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let nonce = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gmx-rs-main-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
