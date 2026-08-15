use std::ffi::OsStr;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use zip::ZipArchive;

use crate::artifact::{GeneratedFile, validate_relative_path};
use crate::assets::Assets;
use crate::path::gmx_path;
use crate::project::{DataFile, ProjectManifest};
use crate::wad::WriteError;

pub(crate) fn collect_project_files(
    project: &ProjectManifest,
    assets: &Assets,
) -> Result<Vec<GeneratedFile>, WriteError> {
    let mut files = Vec::new();
    collect_included_files(project, assets, &mut files)?;
    collect_extension_files(assets, &mut files)?;
    Ok(files)
}

fn collect_included_files(
    project: &ProjectManifest,
    assets: &Assets,
    output: &mut Vec<GeneratedFile>,
) -> Result<(), WriteError> {
    let config = &assets.settings.options.config;
    let target_mask = assets.settings.target_mask;
    for file in &project.data_files {
        if !file.enabled_for(config, target_mask) {
            continue;
        }
        let Some(data) = read_project_file(assets, &file.source, false)? else {
            continue;
        };
        output.push(GeneratedFile::new(included_path(file)?, data));
    }
    Ok(())
}

fn collect_extension_files(
    assets: &Assets,
    output: &mut Vec<GeneratedFile>,
) -> Result<(), WriteError> {
    let config = &assets.settings.options.config;
    let target_mask = assets.settings.target_mask;
    for extension in &assets.extensions {
        if !extension.used_for(config, target_mask) {
            continue;
        }
        for file in &extension.files {
            if !file.enabled_for(config, target_mask) {
                continue;
            }
            for filename in file.filenames_for_target(target_mask) {
                let source = extension.folder.join(&filename);
                let extension_name = filename
                    .extension()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default();
                if extension_name.eq_ignore_ascii_case("gml") {
                    continue;
                }
                let Some(data) = read_project_file(assets, &source, true)? else {
                    if extension_name.eq_ignore_ascii_case("ext") {
                        continue;
                    }
                    return Err(invalid_package(format!(
                        "extension {} file {} does not exist",
                        extension.name,
                        source.display()
                    )));
                };
                if file.uncompress {
                    extract_extension_archive(&extension.name, &source, &data, output)?;
                } else {
                    output.push(GeneratedFile::new(
                        validate_relative_path(&filename)
                            .map_err(|error| invalid_package(error.to_string()))?,
                        data,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn extract_extension_archive(
    extension_name: &str,
    source: &Path,
    data: &[u8],
    output: &mut Vec<GeneratedFile>,
) -> Result<(), WriteError> {
    let mut archive = ZipArchive::new(Cursor::new(data)).map_err(|error| {
        invalid_package(format!(
            "extension {} archive {} is invalid: {error}",
            extension_name,
            source.display()
        ))
    })?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            invalid_package(format!(
                "cannot read entry {index} from extension {} archive {}: {error}",
                extension_name,
                source.display()
            ))
        })?;
        if entry.is_dir() {
            continue;
        }
        let path = portable_relative_path(entry.name()).map_err(|message| {
            invalid_package(format!(
                "extension {} archive {} has {message}",
                extension_name,
                source.display()
            ))
        })?;
        let size = usize::try_from(entry.size()).map_err(|_| WriteError::SizeOverflow {
            field: "extension archive entry",
            size: entry.size(),
        })?;
        let mut contents = Vec::with_capacity(size);
        entry.read_to_end(&mut contents).map_err(|error| {
            invalid_package(format!(
                "cannot decompress {} from extension {}: {error}",
                path.display(),
                extension_name
            ))
        })?;
        output.push(GeneratedFile::new(path, contents));
    }
    Ok(())
}

fn read_project_file(
    assets: &Assets,
    source: &Path,
    report_io_error: bool,
) -> Result<Option<Arc<[u8]>>, WriteError> {
    if let Some(file) = assets.binary_file(source) {
        return Ok(Some(Arc::clone(&file.data)));
    }
    match fs::read(source) {
        Ok(data) => Ok(Some(Arc::from(data))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !report_io_error => Ok(None),
        Err(error) => Err(invalid_package(format!("{}: {error}", source.display()))),
    }
}

fn included_path(file: &DataFile) -> Result<PathBuf, WriteError> {
    let mut components = file.relative_path.components();
    let first = components.next();
    let mut path = PathBuf::new();
    if !matches!(first, Some(Component::Normal(value)) if value == OsStr::new("datafiles"))
        && let Some(component) = first
    {
        path.push(component.as_os_str());
    }
    for component in components {
        path.push(component.as_os_str());
    }
    validate_relative_path(&path).map_err(|error| invalid_package(error.to_string()))
}

fn portable_relative_path(value: &str) -> Result<PathBuf, String> {
    if value.starts_with(['/', '\\']) {
        return Err(format!("unsafe absolute entry path {value:?}"));
    }
    let path = gmx_path(value);
    validate_relative_path(&path).map_err(|_| format!("unsafe entry path {value:?}"))
}

fn invalid_package(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: format!("external files: {}", message.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::{extract_extension_archive, included_path, portable_relative_path};
    use crate::project::DataFile;

    #[test]
    fn strips_only_the_top_level_datafiles_directory() {
        let file = data_file(PathBuf::from("datafiles/Music/theme.ogg"));
        assert_eq!(included_path(&file).unwrap(), Path::new("Music/theme.ogg"));

        let file = data_file(PathBuf::from("Files/datafiles/theme.ogg"));
        assert_eq!(
            included_path(&file).unwrap(),
            Path::new("Files/datafiles/theme.ogg")
        );
    }

    #[test]
    fn validates_portable_archive_paths() {
        assert_eq!(
            portable_relative_path("bin\\helper.dll").unwrap(),
            Path::new("bin/helper.dll")
        );
        assert!(portable_relative_path("../outside.dll").is_err());
        assert!(portable_relative_path("/absolute.dll").is_err());
    }

    #[test]
    fn extracts_deflated_extension_archives_in_memory() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        archive.start_file("bin/helper.dll", options).unwrap();
        archive.write_all(b"extension payload").unwrap();
        let bytes = archive.finish().unwrap().into_inner();

        let mut files = Vec::new();
        extract_extension_archive("Example", Path::new("bundle.ext"), &bytes, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, Path::new("bin/helper.dll"));
        assert_eq!(files[0].data.as_ref(), b"extension payload");
    }

    fn data_file(relative_path: PathBuf) -> DataFile {
        DataFile {
            name: "theme.ogg".to_owned(),
            listed_filename: None,
            source: PathBuf::new(),
            relative_path,
            exists: true,
            size: 0,
            export_action: 0,
            export_dir: String::new(),
            overwrite: false,
            free_data: false,
            remove_end: false,
            store: false,
            configs: Vec::new(),
        }
    }
}
