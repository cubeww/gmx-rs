use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
use std::process::ExitCode;
use std::time::Instant;

use gmx_rs::artifact::{GeneratedFile, merge_generated_files, write_generated_files};
use gmx_rs::assets::Assets;
use gmx_rs::cache::{BuildCache, build_dependencies};
use gmx_rs::compile::{BuildOptions, write_vm_wad_with_artifacts};
use gmx_rs::config::Config;
use gmx_rs::gml::{analyze_assets, compile_vm};
use gmx_rs::project::{ProjectManifest, ResourceKind};
use gmx_rs::semantic::diff_wads_semantic;
use gmx_rs::wad::{WadDiff, WadFile, diff_wads, extract_chunks};

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(AppError::Usage(message)) => {
            eprintln!("error: {message}\n");
            print_usage();
            ExitCode::from(2)
        }
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

fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), AppError> {
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };

    match command.to_str() {
        Some("help" | "-h" | "--help") => {
            ensure_no_more_args(args)?;
            print_usage();
            Ok(())
        }
        Some("inspect") => {
            let input = required_path(&mut args, "data.win path")?;
            ensure_no_more_args(args)?;
            inspect(&input)
        }
        Some("extract") => {
            let input = required_path(&mut args, "data.win path")?;
            let output = required_path(&mut args, "output directory")?;
            ensure_no_more_args(args)?;
            extract(&input, &output)
        }
        Some("inspect-project") => {
            let input = required_path(&mut args, "project.gmx path")?;
            let config = args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Default".to_owned());
            ensure_no_more_args(args)?;
            inspect_project(&input, &config)
        }
        Some("build") => {
            let Some(input) = args.next().map(PathBuf::from) else {
                let directory = env::current_dir()?;
                let input = find_project_in(&directory)?;
                return build(&input, &directory.join("build").join("data.win"), "Default");
            };
            let output = required_path(&mut args, "output data.win path")?;
            let config = args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Default".to_owned());
            ensure_no_more_args(args)?;
            build(&input, &output, &config)
        }
        Some("run") => {
            ensure_no_more_args(args)?;
            run_current_project(&env::current_dir()?)
        }
        Some("clean") => {
            ensure_no_more_args(args)?;
            let directory = env::current_dir()?;
            find_project_in(&directory)?;
            clean_project_build(&directory)
        }
        Some("check-gml") => {
            let input = required_path(&mut args, "project.gmx path")?;
            let config = args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Default".to_owned());
            ensure_no_more_args(args)?;
            check_gml(&input, &config)
        }
        Some("check-vm") => {
            let input = required_path(&mut args, "project.gmx path")?;
            let config = args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Default".to_owned());
            ensure_no_more_args(args)?;
            check_vm(&input, &config)
        }
        Some("diff") => {
            let expected = required_path(&mut args, "expected data.win path")?;
            let actual = required_path(&mut args, "actual data.win path")?;
            ensure_no_more_args(args)?;
            diff(&expected, &actual)
        }
        Some("diff-semantic") => {
            let expected = required_path(&mut args, "expected data.win path")?;
            let actual = required_path(&mut args, "actual data.win path")?;
            ensure_no_more_args(args)?;
            diff_semantic(&expected, &actual)
        }
        Some(command) => Err(AppError::Usage(format!("unknown command {command:?}"))),
        None => Err(AppError::Usage("command must be valid Unicode".to_owned())),
    }
}

fn inspect(path: &Path) -> Result<(), AppError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let wad = WadFile::read(&mut reader)?;

    println!("file:       {}", path.display());
    println!("file size:  {} bytes", wad.file_size);
    println!("FORM size:  {} bytes", wad.form_size);
    println!("chunks:     {}", wad.chunks.len());
    if wad.trailing_size() != 0 {
        println!("trailing:   {} bytes", wad.trailing_size());
    }

    println!();
    println!("  #  name  header offset    data offset      size       end");
    for (index, chunk) in wad.chunks.iter().enumerate() {
        println!(
            "{index:>3}  {:<4}  {:#014x}  {:#014x}  {:>10}  {:#014x}",
            chunk.name,
            chunk.header_offset,
            chunk.data_offset,
            chunk.size,
            chunk.end_offset(),
        );
    }

    Ok(())
}

fn extract(input: &Path, output: &Path) -> Result<(), AppError> {
    let file = File::open(input)?;
    let mut reader = BufReader::new(file);
    let wad = WadFile::read(&mut reader)?;
    let paths = extract_chunks(&mut reader, &wad, output)?;

    println!("extracted {} chunks to {}", paths.len(), output.display());
    for path in paths {
        println!("{}", path.display());
    }
    Ok(())
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
    let cache = BuildCache::from_env(input, config_name);
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
    let mut built = write_vm_wad_with_artifacts(
        &project,
        &assets,
        &BuildOptions::default(),
        &compiled,
        &mut writer,
    )?;
    writer.flush()?;
    built.external_files.push(runner);
    built.external_files = merge_generated_files(std::mem::take(&mut built.external_files))?;
    let output_dir = output.parent().unwrap_or_else(|| Path::new("."));
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
fn run_current_project(project_directory: &Path) -> Result<(), AppError> {
    let input = find_project_in(project_directory)?;
    let project = ProjectManifest::load(&input)?;
    let build_directory = project_directory.join("build");
    build(&input, &build_directory.join("data.win"), "Default")?;

    let executable = runner_executable_path(&build_directory, &project.name);
    if !executable.is_file() {
        return Err(AppError::Message(format!(
            "built Runner executable is missing: {}",
            executable.display()
        )));
    }

    println!("running {}", executable.display());
    let status = Command::new(&executable)
        .current_dir(&build_directory)
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
fn run_current_project(_project_directory: &Path) -> Result<(), AppError> {
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

fn diff(expected: &Path, actual: &Path) -> Result<(), AppError> {
    let mut expected_reader = BufReader::new(File::open(expected)?);
    let mut actual_reader = BufReader::new(File::open(actual)?);
    let difference = diff_wads(&mut expected_reader, &mut actual_reader)?;
    println!("expected: {}", expected.display());
    println!("actual:   {}", actual.display());
    if difference.is_identical() {
        println!("identical");
        return Ok(());
    }
    print_diff(&difference);
    Err(AppError::Message("WAD files differ".to_owned()))
}

fn diff_semantic(expected: &Path, actual: &Path) -> Result<(), AppError> {
    let mut expected_reader = BufReader::new(File::open(expected)?);
    let mut actual_reader = BufReader::new(File::open(actual)?);
    let difference = diff_wads_semantic(&mut expected_reader, &mut actual_reader)?;
    println!("expected: {}", expected.display());
    println!("actual:   {}", actual.display());
    if difference.is_equivalent() {
        println!("semantically equivalent");
        return Ok(());
    }
    for message in &difference.differences {
        println!("{message}");
    }
    Err(AppError::Message(
        "WAD files differ semantically".to_owned(),
    ))
}

fn check_gml(input: &Path, config_name: &str) -> Result<(), AppError> {
    let project = ProjectManifest::load(input)?;
    let config = Config::load_from_project(&project, config_name)?;
    let assets = Assets::load(&project, &config)?;
    match analyze_assets(&assets) {
        Ok(analysis) => {
            let summary = analysis.summary;
            println!(
                "checked {} code units, {} tokens, {} statements",
                summary.syntax.units, summary.syntax.tokens, summary.syntax.statements
            );
            println!(
                "resolved {} name uses against {} symbols ({} locals, {} globalvar declarations)",
                summary.names, summary.symbols, summary.locals, summary.global_variables
            );
            Ok(())
        }
        Err(errors) => {
            for error in &errors {
                eprintln!("{error}");
            }
            Err(AppError::Message(format!(
                "GML analysis failed with {} diagnostics",
                errors.len()
            )))
        }
    }
}

fn check_vm(input: &Path, config_name: &str) -> Result<(), AppError> {
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

fn print_diff(diff: &WadDiff) {
    if let Some(value) = &diff.form_size {
        println!(
            "FORM.size: expected {}, actual {}",
            value.expected, value.actual
        );
    }
    if let Some(value) = &diff.file_size {
        println!(
            "file.size: expected {}, actual {}",
            value.expected, value.actual
        );
    }
    if let Some(value) = &diff.trailing_size {
        println!(
            "FORM.trailing: expected {}, actual {}",
            value.expected, value.actual
        );
    }
    if let Some(value) = &diff.chunk_order {
        let names = |chunks: &[gmx_rs::wad::FourCc]| {
            chunks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!("chunks.order expected: {}", names(&value.expected));
        println!("chunks.order actual:   {}", names(&value.actual));
    }
    for chunk in &diff.chunks {
        let suffix = if chunk.occurrence == 0 {
            String::new()
        } else {
            format!("[{}]", chunk.occurrence)
        };
        match (chunk.expected_index, chunk.actual_index) {
            (Some(expected), None) => {
                println!(
                    "chunk {}{suffix}: missing (expected index {expected})",
                    chunk.name
                );
                continue;
            }
            (None, Some(actual)) => {
                println!(
                    "chunk {}{suffix}: unexpected (actual index {actual})",
                    chunk.name
                );
                continue;
            }
            (Some(expected), Some(actual)) if expected != actual => println!(
                "chunk {}{suffix}.index: expected {expected}, actual {actual}",
                chunk.name
            ),
            _ => {}
        }
        if let Some(value) = &chunk.header_offset {
            println!(
                "chunk {}{suffix}.header_offset: expected {:#x}, actual {:#x}",
                chunk.name, value.expected, value.actual
            );
        }
        if let Some(value) = &chunk.data_offset {
            println!(
                "chunk {}{suffix}.data_offset: expected {:#x}, actual {:#x}",
                chunk.name, value.expected, value.actual
            );
        }
        if let Some(value) = &chunk.size {
            println!(
                "chunk {}{suffix}.size: expected {}, actual {}",
                chunk.name, value.expected, value.actual
            );
        }
        if let Some(payload) = chunk.payload {
            println!(
                "chunk {}{suffix}.payload: first difference +{:#x}, {} differing bytes",
                chunk.name, payload.first_offset, payload.differing_bytes
            );
        }
    }
}

fn inspect_project(path: &Path, config_name: &str) -> Result<(), AppError> {
    let project = ProjectManifest::load(path)?;
    let config = Config::load_from_project(&project, config_name)?;
    let assets = Assets::load(&project, &config)?;

    println!("project:     {}", project.name);
    println!("file:        {}", project.project_file.display());
    println!("root:        {}", project.root_dir.display());
    println!("resources:   {}", project.resources.len());
    for kind in ResourceKind::ALL {
        let count = project.resource_count(kind);
        if count != 0 {
            println!("  {kind:<10} {count:>6}");
        }
    }
    println!("data files:  {}", project.data_files.len());
    println!("constants:   {}", project.constants.len());
    println!("audio groups:{}", project.audio_groups.len());
    println!("config:      {}", config.name);
    println!("options:     {}", config.options().len());
    println!("config const:{}", config.constants().len());
    println!("merged const:{}", assets.settings.constants.len());
    println!("audio model: {} groups", assets.settings.audio_groups.len());
    println!(
        "binary data: {} files, {} bytes",
        assets.binary_files.len(),
        assets
            .binary_files
            .iter()
            .map(|file| file.data.len() as u64)
            .sum::<u64>()
    );
    let extension_file_count = assets
        .extensions
        .iter()
        .map(|extension| extension.files.len())
        .sum::<usize>();
    let extension_function_count = assets
        .extensions
        .iter()
        .flat_map(|extension| &extension.files)
        .map(|file| file.functions.len())
        .sum::<usize>();
    let extension_constant_count = assets
        .extensions
        .iter()
        .flat_map(|extension| &extension.files)
        .map(|file| file.constants.len())
        .sum::<usize>();
    println!(
        "extensions:  {} loaded, {extension_file_count} files, {extension_function_count} functions, {extension_constant_count} constants",
        assets.extensions.len()
    );
    println!("ext func ID: next {}", assets.next_extension_function_id);
    println!("sounds:      {} loaded", assets.sounds.len());
    println!(
        "sprites:     {} loaded, {} frames",
        assets.sprites.len(),
        assets
            .sprites
            .iter()
            .map(|sprite| sprite.frames.len())
            .sum::<usize>()
    );
    println!("backgrounds: {} loaded", assets.backgrounds.len());
    println!("paths:       {} loaded", assets.paths.len());
    println!("scripts:     {} loaded", assets.scripts.len());
    println!("shaders:     {} loaded", assets.shaders.len());
    println!("fonts:       {} loaded", assets.fonts.len());
    println!("timelines:   {} loaded", assets.timelines.len());
    let object_event_count = assets
        .objects
        .iter()
        .flat_map(|object| &object.events)
        .map(Vec::len)
        .sum::<usize>();
    let object_action_count = assets
        .objects
        .iter()
        .flat_map(|object| &object.events)
        .flatten()
        .map(|event| event.actions.len())
        .sum::<usize>();
    println!(
        "objects:     {} loaded, {object_event_count} events, {object_action_count} actions",
        assets.objects.len()
    );
    let room_instance_count = assets
        .rooms
        .iter()
        .map(|room| room.instances.len())
        .sum::<usize>();
    let room_tile_count = assets
        .rooms
        .iter()
        .map(|room| room.tiles.len())
        .sum::<usize>();
    println!(
        "rooms:       {} loaded, {room_instance_count} instances, {room_tile_count} tiles",
        assets.rooms.len()
    );
    println!(
        "room IDs:    next instance {}, next tile {}",
        assets.next_room_instance_id, assets.next_room_tile_id
    );
    if let Some(game_id) = config.option("option_gameid") {
        println!("game ID:     {game_id}");
    }
    if let Some(game_guid) = config.option("option_gameguid") {
        println!("game GUID:   {game_guid}");
    }

    let missing_resources: Vec<_> = project
        .resources
        .iter()
        .filter(|resource| !resource.source.is_file())
        .collect();
    let missing_data_files: Vec<_> = project
        .data_files
        .iter()
        .filter(|data_file| data_file.exists && !data_file.source.is_file())
        .collect();
    let mut missing_asset_sources = Vec::new();
    for extension in &assets.extensions {
        for file in &extension.files {
            if !file.source.is_file() {
                missing_asset_sources.push((
                    format!("extension file {}/{}", extension.name, file.filename),
                    &file.source,
                ));
            }
        }
    }
    for sound in &assets.sounds {
        if !sound.audio_source.is_file() {
            missing_asset_sources.push((format!("sound data {}", sound.name), &sound.audio_source));
        }
    }
    for sprite in &assets.sprites {
        for frame in &sprite.frames {
            if !frame.source.is_file() {
                missing_asset_sources.push((
                    format!("sprite frame {}[{}]", sprite.name, frame.index),
                    &frame.source,
                ));
            }
        }
        if let Some(source) = sprite
            .swf_source
            .as_ref()
            .filter(|source| !source.is_file())
        {
            missing_asset_sources.push((format!("sprite SWF {}", sprite.name), source));
        }
        if let Some(source) = sprite
            .spine_source
            .as_ref()
            .filter(|source| !source.is_file())
        {
            missing_asset_sources.push((format!("sprite Spine {}", sprite.name), source));
        }
    }
    for background in &assets.backgrounds {
        if !background.image_source.is_file() {
            missing_asset_sources.push((
                format!("background data {}", background.name),
                &background.image_source,
            ));
        }
    }
    for font in &assets.fonts {
        if !font.image_source.is_file() {
            missing_asset_sources.push((format!("font bitmap {}", font.name), &font.image_source));
        }
    }
    let missing_count =
        missing_resources.len() + missing_data_files.len() + missing_asset_sources.len();
    println!("missing:     {missing_count}");

    for resource in missing_resources {
        println!(
            "  {} {}: {}",
            resource.kind,
            resource.name,
            resource.source.display()
        );
    }
    for data_file in missing_data_files {
        println!(
            "  datafile {}: {}",
            data_file.name,
            data_file.source.display()
        );
    }
    for (label, path) in missing_asset_sources {
        println!("  {label}: {}", path.display());
    }

    if missing_count != 0 {
        return Err(AppError::Message(format!(
            "project references {missing_count} missing files"
        )));
    }
    Ok(())
}

fn required_path(
    args: &mut impl Iterator<Item = OsString>,
    description: &str,
) -> Result<PathBuf, AppError> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Usage(format!("missing {description}")))
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

fn clean_project_build(project_directory: &Path) -> Result<(), AppError> {
    let build = project_directory.join("build");
    let metadata = match fs::symlink_metadata(&build) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("no build artifacts to remove: {}", build.display());
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse_point(&metadata) {
        return Err(AppError::Message(format!(
            "refusing to clean linked build directory {}",
            build.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(AppError::Message(format!(
            "refusing to clean non-directory build path {}",
            build.display()
        )));
    }

    let project_directory = fs::canonicalize(project_directory)?;
    let resolved = fs::canonicalize(&build)?;
    if resolved.parent() != Some(project_directory.as_path()) {
        return Err(AppError::Message(format!(
            "refusing to clean build directory outside project root: {}",
            resolved.display()
        )));
    }
    fs::remove_dir_all(&build).map_err(|error| {
        AppError::Message(format!(
            "cannot remove build artifacts {}: {error}",
            build.display()
        ))
    })?;
    println!("removed build artifacts: {}", build.display());
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

fn ensure_no_more_args(mut args: impl Iterator<Item = OsString>) -> Result<(), AppError> {
    match args.next() {
        Some(argument) => Err(AppError::Usage(format!(
            "unexpected argument {:?}",
            argument.to_string_lossy()
        ))),
        None => Ok(()),
    }
}

fn print_usage() {
    println!(
        "gmx - GameMaker Studio 1.4 project compiler\n\n\
         Usage:\n  \
            gmx build\n  \
            gmx run\n  \
            gmx clean\n  \
            gmx inspect <data.win>\n  \
            gmx extract <data.win> <output-directory>\n  \
            gmx inspect-project <project.gmx> [config]\n  \
            gmx check-gml <project.gmx> [config]\n  \
            gmx check-vm <project.gmx> [config]\n  \
            gmx build <project.gmx> <data.win> [config]\n  \
            gmx diff <expected-data.win> <actual-data.win>\n  \
            gmx diff-semantic <expected-data.win> <actual-data.win>\n  \
            gmx help\n\n\
         Commands:\n  \
           inspect   validate and list the chunks in a data.win file\n  \
           extract   copy each raw chunk payload to a separate file\n  \
           inspect-project  list and validate resources in a GMX project\n  \
           check-gml  parse every GML code unit in a GMX project\n  \
           check-vm   compile every GML code unit to GMS 1.4 VM bytecode\n  \
           build     auto-detect ./*.project.gmx and write ./build/data.win,\n  \
                     or compile explicit input/output paths; Runner.exe must\n+                     be next to the gmx executable\n  \
           run       build the current project and run ./build/<project>.exe\n  \
           clean     remove the current project's ./build directory, including cache\n  \
           diff      compare exact WAD structure and chunk payloads\n  \
           diff-semantic  compare VM links, texture pixels, and audio semantics"
    );
}

#[derive(Debug)]
enum AppError {
    Usage(String),
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

    use super::{
        AppError, external_runner_file, find_project_in, runner_executable_path,
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

        super::clean_project_build(&directory.path).unwrap();

        assert!(!build.exists());
        assert!(project.is_file());
        super::clean_project_build(&directory.path).unwrap();
    }

    #[test]
    fn clean_rejects_a_non_directory_build_path() {
        let directory = TempDirectory::new("clean-file");
        let build = directory.path.join("build");
        fs::write(&build, b"not a directory").unwrap();

        let error = super::clean_project_build(&directory.path).unwrap_err();

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
