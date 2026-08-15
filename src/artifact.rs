use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub data: Arc<[u8]>,
}

impl GeneratedFile {
    pub fn new(path: PathBuf, data: impl Into<Arc<[u8]>>) -> Self {
        Self {
            path,
            data: data.into(),
        }
    }
}

pub fn merge_generated_files(
    files: impl IntoIterator<Item = GeneratedFile>,
) -> Result<Vec<GeneratedFile>, ArtifactError> {
    let mut merged = Vec::<GeneratedFile>::new();
    let mut keys = HashMap::<String, usize>::new();
    for mut file in files {
        file.path = validate_relative_path(&file.path)?;
        let key = path_key(&file.path);
        if let Some(&index) = keys.get(&key) {
            if merged[index].data != file.data {
                return Err(ArtifactError::ConflictingFiles {
                    first: merged[index].path.clone(),
                    second: file.path,
                });
            }
            continue;
        }
        if let Some(existing) = merged.iter().find(|existing| {
            let existing_key = path_key(&existing.path);
            existing_key.starts_with(&(key.clone() + "/")) || key.starts_with(&(existing_key + "/"))
        }) {
            return Err(ArtifactError::ConflictingFiles {
                first: existing.path.clone(),
                second: file.path,
            });
        }
        keys.insert(key, merged.len());
        merged.push(file);
    }
    Ok(merged)
}

pub fn write_generated_files(
    output_dir: &Path,
    files: &[GeneratedFile],
) -> Result<Vec<PathBuf>, ArtifactError> {
    files
        .par_iter()
        .map(|file| {
            let relative = validate_relative_path(&file.path)?;
            let destination = output_dir.join(relative);
            if file_matches_data(&destination, &file.data).unwrap_or(false) {
                return Ok(destination);
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|source| ArtifactError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&destination, &file.data).map_err(|source| ArtifactError::Io {
                path: destination.clone(),
                source,
            })?;
            Ok(destination)
        })
        .collect()
}

pub(crate) fn file_matches_data(path: &Path, data: &[u8]) -> io::Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if metadata.len() != data.len() as u64 {
        return Ok(false);
    }
    let mut reader = BufReader::with_capacity(128 * 1024, File::open(path)?);
    let mut buffer = [0_u8; 128 * 1024];
    let mut offset = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(offset == data.len());
        }
        if buffer[..read] != data[offset..offset + read] {
            return Ok(false);
        }
        offset += read;
    }
}

pub fn validate_relative_path(path: &Path) -> Result<PathBuf, ArtifactError> {
    let mut validated = PathBuf::new();
    let raw = path.as_os_str().to_string_lossy();
    if raw.starts_with(['/', '\\']) {
        return Err(ArtifactError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    for component in raw.split(['/', '\\']).filter(|value| !value.is_empty()) {
        match component {
            "." => {}
            ".." => {
                return Err(ArtifactError::UnsafePath {
                    path: path.to_path_buf(),
                });
            }
            value if value.contains(['\0', ':']) => {
                return Err(ArtifactError::UnsafePath {
                    path: path.to_path_buf(),
                });
            }
            value => validated.push(value),
        }
    }
    if validated.as_os_str().is_empty() {
        return Err(ArtifactError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    Ok(validated)
}

fn path_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug)]
pub enum ArtifactError {
    UnsafePath { path: PathBuf },
    ConflictingFiles { first: PathBuf, second: PathBuf },
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePath { path } => {
                write!(formatter, "unsafe generated file path {}", path.display())
            }
            Self::ConflictingFiles { first, second } => write!(
                formatter,
                "generated files {} and {} conflict",
                first.display(),
                second.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        GeneratedFile, file_matches_data, merge_generated_files, validate_relative_path,
        write_generated_files,
    };

    #[test]
    fn rejects_paths_that_can_escape_the_output_directory() {
        assert!(validate_relative_path(Path::new("../outside.bin")).is_err());
        assert!(validate_relative_path(Path::new("..\\outside.bin")).is_err());
        assert!(validate_relative_path(Path::new("/absolute.bin")).is_err());
        assert!(validate_relative_path(Path::new("C:/absolute.bin")).is_err());
        assert_eq!(
            validate_relative_path(Path::new("Music/theme.ogg")).unwrap(),
            Path::new("Music/theme.ogg")
        );
    }

    #[test]
    fn deduplicates_equal_files_and_rejects_windows_path_collisions() {
        let files = merge_generated_files([
            GeneratedFile::new(PathBuf::from("Music/Theme.ogg"), Vec::from(&b"same"[..])),
            GeneratedFile::new(PathBuf::from("music/theme.ogg"), Vec::from(&b"same"[..])),
        ])
        .unwrap();
        assert_eq!(files.len(), 1);

        assert!(
            merge_generated_files([
                GeneratedFile::new(PathBuf::from("file.bin"), Vec::from(&b"first"[..])),
                GeneratedFile::new(PathBuf::from("FILE.BIN"), Vec::from(&b"second"[..])),
            ])
            .is_err()
        );
    }

    #[test]
    fn skips_generated_files_that_already_have_identical_contents() {
        let root = temp_dir("unchanged-output");
        fs::create_dir_all(&root).unwrap();
        let file = GeneratedFile::new(PathBuf::from("music/theme.ogg"), &b"same audio"[..]);
        let written = write_generated_files(&root, std::slice::from_ref(&file)).unwrap();
        assert_eq!(written, vec![root.join("music/theme.ogg")]);
        assert!(file_matches_data(&written[0], &file.data).unwrap());

        let permissions = fs::metadata(&written[0]).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&written[0], readonly).unwrap();
        write_generated_files(&root, &[file]).unwrap();
        fs::set_permissions(&written[0], permissions).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gmx-rs-{label}-{}-{nonce}", std::process::id()))
    }
}
