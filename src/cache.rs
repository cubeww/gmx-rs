use std::collections::{BTreeMap, HashMap};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::artifact::{GeneratedFile, validate_relative_path};
use crate::assets::{Assets, BinaryFile};
use crate::config::Config;
use crate::project::{ProjectManifest, ResourceKind};
use crate::tool::find_tool;

const BUILD_CACHE_SCHEMA: &[u8] = b"gmx-rs-build-cache-v4\0";
const DEPENDENCY_MAGIC: &[u8] = b"GMXRSDEPS2";
const OUTPUT_MAGIC: &[u8] = b"GMXRSOUT1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct BuildCache {
    root: PathBuf,
    dependency_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheHit {
    pub key: String,
    pub wad_bytes: u64,
    pub external_files: usize,
}

#[derive(Debug, Clone)]
pub struct KnownBuild {
    pub key: String,
    dependency_keys: Vec<String>,
}

impl KnownBuild {
    pub fn matches_dependencies(&self, dependencies: &[PathBuf]) -> Result<bool, CacheError> {
        Ok(self.dependency_keys == dependency_keys(dependencies)?)
    }

    pub fn contains_dependency(&self, dependency: &Path) -> Result<bool, CacheError> {
        let keys = dependency_keys(&[dependency.to_path_buf()])?;
        Ok(keys
            .first()
            .is_some_and(|key| self.dependency_keys.contains(key)))
    }
}

impl BuildCache {
    pub fn from_env(project: &Path, config: &str) -> Option<Self> {
        build_cache_enabled().then(|| Self::at(cache_root(project).join("build"), project, config))
    }

    pub fn known_key(&self) -> Result<Option<String>, CacheError> {
        Ok(self.known_build()?.map(|build| build.key))
    }

    pub fn known_build(&self) -> Result<Option<KnownBuild>, CacheError> {
        let Some(saved) = read_saved_dependencies(&self.dependency_file)? else {
            return Ok(None);
        };
        let dependencies = saved
            .into_iter()
            .map(|dependency| dependency.path)
            .collect::<Vec<_>>();
        let dependency_keys = dependency_keys(&dependencies)?;
        let key = self.key(&dependencies)?;
        Ok(Some(KnownBuild {
            key,
            dependency_keys,
        }))
    }

    pub fn known_build_if_unchanged(&self) -> Result<Option<KnownBuild>, CacheError> {
        let Some(saved) = read_saved_dependencies(&self.dependency_file)? else {
            return Ok(None);
        };
        let unchanged = saved
            .par_iter()
            .map(|dependency| {
                dependency_stamp(&dependency.path)
                    .map(|current| current.reliable() && current == dependency.stamp)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|same| same);
        if !unchanged {
            return Ok(None);
        }
        let dependency_keys = saved
            .iter()
            .map(|dependency| path_key(&dependency.path))
            .collect();
        let key = fingerprint_saved(&saved)?;
        Ok(Some(KnownBuild {
            key,
            dependency_keys,
        }))
    }

    pub fn key(&self, dependencies: &[PathBuf]) -> Result<String, CacheError> {
        fingerprint(dependencies, &HashMap::new())
    }

    pub fn key_with_assets(
        &self,
        dependencies: &[PathBuf],
        assets: &Assets,
    ) -> Result<String, CacheError> {
        fingerprint(dependencies, &loaded_digests(&assets.binary_files))
    }

    pub fn save_dependencies(&self, dependencies: &[PathBuf]) -> Result<(), CacheError> {
        let dependencies = normalized_dependencies(dependencies)?;
        let stamps = dependencies
            .par_iter()
            .map(|(_, path)| dependency_stamp(path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(DEPENDENCY_MAGIC);
        encoded.extend_from_slice(
            &u32::try_from(dependencies.len())
                .map_err(|_| CacheError::TooManyDependencies(dependencies.len()))?
                .to_le_bytes(),
        );
        for ((_, path), stamp) in dependencies.into_iter().zip(stamps) {
            let value = path.to_string_lossy();
            let bytes = value.as_bytes();
            encoded.extend_from_slice(
                &u32::try_from(bytes.len())
                    .map_err(|_| CacheError::PathTooLong(path.clone()))?
                    .to_le_bytes(),
            );
            encoded.extend_from_slice(bytes);
            encoded.push(stamp.kind);
            encoded.extend_from_slice(&stamp.size.to_le_bytes());
            encoded.extend_from_slice(&stamp.modified_seconds.to_le_bytes());
            encoded.extend_from_slice(&stamp.modified_nanos.to_le_bytes());
        }
        write_atomic(&self.dependency_file, &encoded).map_err(|source| CacheError::Io {
            path: self.dependency_file.clone(),
            source,
        })
    }

    pub fn restore(&self, key: &str, output: &Path) -> Result<Option<CacheHit>, CacheError> {
        validate_key(key)?;
        let entry = self.root.join("artifacts").join(key);
        if !entry.join("complete").is_file() || !entry.join("data.win").is_file() {
            return Ok(None);
        }
        let output_dir = output.parent().unwrap_or_else(|| Path::new("."));
        let output_name = output.file_name().unwrap_or_default().to_string_lossy();
        let files_root = entry.join("files");
        let mut files = collect_files(&files_root)?;
        files.sort_by_key(|left| path_key(left));
        let relative_files = files
            .iter()
            .map(|source| {
                let relative = source
                    .strip_prefix(&files_root)
                    .map_err(|_| CacheError::InvalidEntry(source.clone()))?;
                let relative = validate_relative_path(relative)
                    .map_err(|_| CacheError::InvalidEntry(source.clone()))?;
                if relative.components().count() == 1
                    && relative
                        .as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&output_name)
                {
                    return Err(CacheError::InvalidEntry(source.clone()));
                }
                Ok(relative)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if output_marker_matches(&self.root, key, output, &relative_files) {
            let wad_bytes = fs::metadata(output)
                .map_err(|source| CacheError::Io {
                    path: output.to_path_buf(),
                    source,
                })?
                .len();
            return Ok(Some(CacheHit {
                key: key.to_owned(),
                wad_bytes,
                external_files: files.len(),
            }));
        }
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| CacheError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::copy(entry.join("data.win"), output).map_err(|source| CacheError::Io {
            path: output.to_path_buf(),
            source,
        })?;

        for (source, relative) in files.iter().zip(&relative_files) {
            let destination = output_dir.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|source| CacheError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::copy(source, &destination).map_err(|source| CacheError::Io {
                path: destination,
                source,
            })?;
        }
        let _ = write_output_marker(&self.root, key, output, &relative_files);
        let wad_bytes = fs::metadata(output)
            .map_err(|source| CacheError::Io {
                path: output.to_path_buf(),
                source,
            })?
            .len();
        Ok(Some(CacheHit {
            key: key.to_owned(),
            wad_bytes,
            external_files: files.len(),
        }))
    }

    pub fn record_output(
        &self,
        key: &str,
        output: &Path,
        files: &[GeneratedFile],
    ) -> Result<(), CacheError> {
        validate_key(key)?;
        let relative = files
            .iter()
            .map(|file| {
                validate_relative_path(&file.path)
                    .map_err(|_| CacheError::InvalidEntry(file.path.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        write_output_marker(&self.root, key, output, &relative)
    }

    pub fn store(&self, key: &str, wad: &Path, files: &[GeneratedFile]) -> Result<(), CacheError> {
        validate_key(key)?;
        let artifacts = self.root.join("artifacts");
        let destination = artifacts.join(key);
        if destination.join("complete").is_file() {
            return Ok(());
        }
        fs::create_dir_all(&artifacts).map_err(|source| CacheError::Io {
            path: artifacts.clone(),
            source,
        })?;
        let temporary = artifacts.join(format!(
            ".{key}.tmp-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary).map_err(|source| CacheError::Io {
            path: temporary.clone(),
            source,
        })?;
        let result = (|| {
            let copy_wad = || {
                fs::copy(wad, temporary.join("data.win"))
                    .map(|_| ())
                    .map_err(|source| CacheError::Io {
                        path: wad.to_path_buf(),
                        source,
                    })
            };
            let store_files = || {
                files.par_iter().try_for_each(|file| {
                    let relative = validate_relative_path(&file.path)
                        .map_err(|_| CacheError::InvalidEntry(file.path.clone()))?;
                    let path = temporary.join("files").join(relative);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent).map_err(|source| CacheError::Io {
                            path: parent.to_path_buf(),
                            source,
                        })?;
                    }
                    let blob = store_blob(&self.root, &file.data)?;
                    match fs::hard_link(&blob, &path) {
                        Ok(()) => Ok(()),
                        Err(_) => fs::copy(&blob, &path)
                            .map(|_| ())
                            .map_err(|source| CacheError::Io { path, source }),
                    }
                })
            };
            let (wad_result, files_result) = rayon::join(copy_wad, store_files);
            wad_result?;
            files_result?;
            fs::write(temporary.join("complete"), key).map_err(|source| CacheError::Io {
                path: temporary.join("complete"),
                source,
            })?;
            if destination.exists() {
                if destination.join("complete").is_file() {
                    return Ok(());
                }
                fs::remove_dir_all(&destination).map_err(|source| CacheError::Io {
                    path: destination.clone(),
                    source,
                })?;
            }
            match fs::rename(&temporary, &destination) {
                Ok(()) => Ok(()),
                Err(_) if destination.join("complete").is_file() => Ok(()),
                Err(source) => Err(CacheError::Io {
                    path: destination.clone(),
                    source,
                }),
            }
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }

    fn at(root: PathBuf, project: &Path, config: &str) -> Self {
        let project = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        let mut hasher = Md5::new();
        hasher.update(BUILD_CACHE_SCHEMA);
        hasher.update(path_key(&project).as_bytes());
        hasher.update([0]);
        hasher.update(config.as_bytes());
        let id = hex(&hasher.finalize());
        Self {
            dependency_file: root.join("projects").join(format!("{id}.deps")),
            root,
        }
    }
}

pub fn build_dependencies(
    project: &ProjectManifest,
    config: &Config,
    assets: &Assets,
) -> Vec<PathBuf> {
    let mut dependencies = vec![project.project_file.clone(), config.source.clone()];
    dependencies.extend(
        project
            .resources
            .iter()
            .filter(|resource| {
                resource.kind != ResourceKind::Config || resource.source == config.source
            })
            .map(|resource| resource.source.clone()),
    );
    dependencies.extend(assets.binary_files.iter().map(|file| file.source.clone()));
    dependencies.extend(
        assets
            .backgrounds
            .iter()
            .map(|background| &background.image_source)
            .filter(|path| !path.as_os_str().is_empty())
            .cloned(),
    );
    dependencies
}

pub fn cache_enabled() -> bool {
    !env_flag("GMX_RS_DISABLE_CACHE")
}

pub fn build_cache_enabled() -> bool {
    cache_enabled() && !env_flag("GMX_RS_DISABLE_BUILD_CACHE")
}

pub fn cache_root(project: &Path) -> PathBuf {
    env::var_os("GMX_RS_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cache_root(project))
}

fn default_cache_root(project: &Path) -> PathBuf {
    let project = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    project
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("build")
        .join(".gmx-cache")
}

pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(data)?;
        file.flush()?;
        drop(file);
        if path.exists()
            && let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(_) if path.is_file() => Ok(()),
            Err(error) => Err(error),
        }
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn fingerprint(
    dependencies: &[PathBuf],
    loaded: &HashMap<String, (u64, [u8; 16])>,
) -> Result<String, CacheError> {
    let dependencies = normalized_dependencies(dependencies)?;
    let digests = dependencies
        .par_iter()
        .map(|(key, path)| {
            loaded
                .get(key)
                .map(|(size, digest)| Ok((key.clone(), true, *size, *digest)))
                .unwrap_or_else(|| hash_file(key, path))
        })
        .collect::<Result<Vec<_>, _>>()?;
    finish_fingerprint(digests)
}

fn fingerprint_saved(dependencies: &[SavedDependency]) -> Result<String, CacheError> {
    let digests = dependencies
        .par_iter()
        .map(|dependency| {
            let key = path_key(&dependency.path);
            if dependency.stamp.kind == 0 {
                Ok((key, false, 0, [0; 16]))
            } else {
                hash_file_contents(&key, &dependency.path)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    finish_fingerprint(digests)
}

fn finish_fingerprint(digests: Vec<(String, bool, u64, [u8; 16])>) -> Result<String, CacheError> {
    let mut hasher = Md5::new();
    hasher.update(BUILD_CACHE_SCHEMA);
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(env::consts::OS.as_bytes());
    hasher.update(env::consts::ARCH.as_bytes());
    add_executable_fingerprint(&mut hasher);
    add_tool_fingerprints(&mut hasher);
    for (key, present, size, digest) in digests {
        hasher.update((key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
        hasher.update([u8::from(present)]);
        hasher.update(size.to_le_bytes());
        hasher.update(digest);
    }
    Ok(hex(&hasher.finalize()))
}

fn dependency_keys(dependencies: &[PathBuf]) -> Result<Vec<String>, CacheError> {
    Ok(normalized_dependencies(dependencies)?
        .into_iter()
        .map(|(key, _)| key)
        .collect())
}

fn loaded_digests(files: &[BinaryFile]) -> HashMap<String, (u64, [u8; 16])> {
    let entries = files
        .par_iter()
        .map(|file| {
            let value = (file.data.len() as u64, file.digest());
            let source = path_key(&file.source);
            let canonical = fs::canonicalize(&file.source)
                .ok()
                .map(|path| path_key(&path));
            (source, canonical, value)
        })
        .collect::<Vec<_>>();
    let mut loaded = HashMap::with_capacity(files.len() * 2);
    for (source, canonical, value) in entries {
        loaded.insert(source, value);
        if let Some(canonical) = canonical {
            loaded.insert(canonical, value);
        }
    }
    loaded
}

fn store_blob(root: &Path, data: &[u8]) -> Result<PathBuf, CacheError> {
    let digest = hex(&Md5::digest(data));
    let path = root
        .join("objects")
        .join(&digest[..2])
        .join(format!("{digest}-{}", data.len()));
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == data.len() as u64)
    {
        return Ok(path);
    }
    write_atomic(&path, data).map_err(|source| CacheError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

fn output_marker_path(root: &Path, output: &Path) -> PathBuf {
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(output)
    };
    let mut hasher = Md5::new();
    hasher.update(BUILD_CACHE_SCHEMA);
    hasher.update(path_key(&output).as_bytes());
    root.join("outputs")
        .join(format!("{}.state", hex(&hasher.finalize())))
}

fn write_output_marker(
    root: &Path,
    key: &str,
    output: &Path,
    relative_files: &[PathBuf],
) -> Result<(), CacheError> {
    let wad_stamp = dependency_stamp(output)?;
    if !wad_stamp.reliable() || wad_stamp.kind != 2 {
        return Ok(());
    }
    let output_dir = output.parent().unwrap_or_else(|| Path::new("."));
    let mut files = relative_files
        .iter()
        .map(|relative| {
            let relative = validate_relative_path(relative)
                .map_err(|_| CacheError::InvalidEntry(relative.clone()))?;
            let stamp = dependency_stamp(&output_dir.join(&relative))?;
            if !stamp.reliable() || stamp.kind != 2 {
                return Err(CacheError::InvalidEntry(relative));
            }
            Ok((path_key(&relative), relative, stamp))
        })
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut encoded = Vec::new();
    encoded.extend_from_slice(OUTPUT_MAGIC);
    encoded.extend_from_slice(key.as_bytes());
    write_stamp(&mut encoded, wad_stamp);
    encoded.extend_from_slice(
        &u32::try_from(files.len())
            .map_err(|_| CacheError::TooManyDependencies(files.len()))?
            .to_le_bytes(),
    );
    for (_, relative, stamp) in files {
        let value = relative.to_string_lossy();
        let bytes = value.as_bytes();
        encoded.extend_from_slice(
            &u32::try_from(bytes.len())
                .map_err(|_| CacheError::PathTooLong(relative.clone()))?
                .to_le_bytes(),
        );
        encoded.extend_from_slice(bytes);
        write_stamp(&mut encoded, stamp);
    }
    let marker = output_marker_path(root, output);
    write_atomic(&marker, &encoded).map_err(|source| CacheError::Io {
        path: marker,
        source,
    })
}

fn output_marker_matches(
    root: &Path,
    key: &str,
    output: &Path,
    relative_files: &[PathBuf],
) -> bool {
    let data = match fs::read(output_marker_path(root, output)) {
        Ok(data) => data,
        Err(_) => return false,
    };
    if data.len() < OUTPUT_MAGIC.len() + 32 || !data.starts_with(OUTPUT_MAGIC) {
        return false;
    }
    let mut offset = OUTPUT_MAGIC.len();
    if data.get(offset..offset + 32) != Some(key.as_bytes()) {
        return false;
    }
    offset += 32;
    let Some(wad_stamp) = read_stamp_option(&data, &mut offset) else {
        return false;
    };
    if !stamp_matches(output, wad_stamp) {
        return false;
    }
    let Some(count) = read_u32_option(&data, &mut offset).map(|value| value as usize) else {
        return false;
    };
    if count != relative_files.len() {
        return false;
    }
    let output_dir = output.parent().unwrap_or_else(|| Path::new("."));
    let mut expected = relative_files
        .iter()
        .map(|relative| (path_key(relative), relative))
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, expected_path) in expected {
        let Some(length) = read_u32_option(&data, &mut offset).map(|value| value as usize) else {
            return false;
        };
        let Some(bytes) = data.get(offset..offset.saturating_add(length)) else {
            return false;
        };
        offset += length;
        if bytes != expected_path.to_string_lossy().as_bytes() {
            return false;
        }
        let Some(stamp) = read_stamp_option(&data, &mut offset) else {
            return false;
        };
        if !stamp_matches(&output_dir.join(expected_path), stamp) {
            return false;
        }
    }
    offset == data.len()
}

fn stamp_matches(path: &Path, saved: DependencyStamp) -> bool {
    saved.reliable()
        && saved.kind == 2
        && dependency_stamp(path).is_ok_and(|current| current == saved)
}

fn write_stamp(output: &mut Vec<u8>, stamp: DependencyStamp) {
    output.push(stamp.kind);
    output.extend_from_slice(&stamp.size.to_le_bytes());
    output.extend_from_slice(&stamp.modified_seconds.to_le_bytes());
    output.extend_from_slice(&stamp.modified_nanos.to_le_bytes());
}

fn read_stamp_option(data: &[u8], offset: &mut usize) -> Option<DependencyStamp> {
    let kind = *data.get(*offset)?;
    *offset += 1;
    let size = read_u64_option(data, offset)?;
    let modified_seconds = read_u64_option(data, offset)?;
    let modified_nanos = read_u32_option(data, offset)?;
    Some(DependencyStamp {
        kind,
        size,
        modified_seconds,
        modified_nanos,
    })
}

fn read_u32_option(data: &[u8], offset: &mut usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let value = u32::from_le_bytes(data.get(*offset..end)?.try_into().ok()?);
    *offset = end;
    Some(value)
}

fn read_u64_option(data: &[u8], offset: &mut usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let value = u64::from_le_bytes(data.get(*offset..end)?.try_into().ok()?);
    *offset = end;
    Some(value)
}

fn normalized_dependencies(dependencies: &[PathBuf]) -> Result<Vec<(String, PathBuf)>, CacheError> {
    let mut normalized = BTreeMap::new();
    for dependency in dependencies {
        let path = match fs::canonicalize(dependency) {
            Ok(path) => path,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                if dependency.is_absolute() {
                    dependency.clone()
                } else {
                    env::current_dir()
                        .map_err(|source| CacheError::Io {
                            path: dependency.clone(),
                            source,
                        })?
                        .join(dependency)
                }
            }
            Err(source) => {
                return Err(CacheError::Io {
                    path: dependency.clone(),
                    source,
                });
            }
        };
        normalized.entry(path_key(&path)).or_insert(path);
    }
    Ok(normalized.into_iter().collect())
}

fn hash_file(key: &str, path: &Path) -> Result<(String, bool, u64, [u8; 16]), CacheError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok((key.to_owned(), false, 0, [0; 16])),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok((key.to_owned(), false, 0, [0; 16]));
        }
        Err(source) => {
            return Err(CacheError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    hash_file_contents(key, path)
}

fn hash_file_contents(key: &str, path: &Path) -> Result<(String, bool, u64, [u8; 16]), CacheError> {
    let file = File::open(path).map_err(|source| CacheError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut size = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((key.to_owned(), true, size, hasher.finalize().into()))
}

fn add_executable_fingerprint(hasher: &mut Md5) {
    if let Ok(executable) = env::current_exe() {
        add_metadata_fingerprint(hasher, &executable);
    }
}

fn add_tool_fingerprints(hasher: &mut Md5) {
    let ffmpeg_names: &[&str] = if cfg!(windows) {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg"]
    };
    for names in [
        ffmpeg_names,
        &["HLSLCompiler.exe"] as &[&str],
        &["D3D11ShaderParser.exe"] as &[&str],
    ] {
        if let Some(path) = find_tool(names) {
            add_metadata_fingerprint(hasher, &path);
        }
    }
}

fn add_metadata_fingerprint(hasher: &mut Md5, path: &Path) {
    hasher.update(path_key(path).as_bytes());
    if let Ok(metadata) = fs::metadata(path) {
        hasher.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            hasher.update(duration.as_secs().to_le_bytes());
            hasher.update(duration.subsec_nanos().to_le_bytes());
        }
    }
    hasher.update([0]);
}

#[derive(Debug, Clone)]
struct SavedDependency {
    path: PathBuf,
    stamp: DependencyStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DependencyStamp {
    kind: u8,
    size: u64,
    modified_seconds: u64,
    modified_nanos: u32,
}

impl DependencyStamp {
    fn reliable(self) -> bool {
        self.kind != 1
    }
}

fn dependency_stamp(path: &Path) -> Result<DependencyStamp, CacheError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return Ok(DependencyStamp {
                kind: 0,
                size: 0,
                modified_seconds: 0,
                modified_nanos: 0,
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(DependencyStamp {
                kind: 0,
                size: 0,
                modified_seconds: 0,
                modified_nanos: 0,
            });
        }
        Err(source) => {
            return Err(CacheError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(DependencyStamp {
            kind: 1,
            size: metadata.len(),
            modified_seconds: 0,
            modified_nanos: 0,
        });
    };
    let Ok(modified) = modified.duration_since(UNIX_EPOCH) else {
        return Ok(DependencyStamp {
            kind: 1,
            size: metadata.len(),
            modified_seconds: 0,
            modified_nanos: 0,
        });
    };
    Ok(DependencyStamp {
        kind: 2,
        size: metadata.len(),
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    })
}

fn read_saved_dependencies(path: &Path) -> Result<Option<Vec<SavedDependency>>, CacheError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CacheError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !data.starts_with(DEPENDENCY_MAGIC) || data.len() < DEPENDENCY_MAGIC.len() + 4 {
        return Err(CacheError::InvalidDependencyFile(path.to_path_buf()));
    }
    let mut offset = DEPENDENCY_MAGIC.len();
    let count = read_u32(&data, &mut offset, path)? as usize;
    let mut dependencies = Vec::with_capacity(count);
    for _ in 0..count {
        let length = read_u32(&data, &mut offset, path)? as usize;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| CacheError::InvalidDependencyFile(path.to_path_buf()))?;
        let value = std::str::from_utf8(&data[offset..end])
            .map_err(|_| CacheError::InvalidDependencyFile(path.to_path_buf()))?;
        offset = end;
        let kind = read_u8(&data, &mut offset, path)?;
        if kind > 2 {
            return Err(CacheError::InvalidDependencyFile(path.to_path_buf()));
        }
        let size = read_u64(&data, &mut offset, path)?;
        let modified_seconds = read_u64(&data, &mut offset, path)?;
        let modified_nanos = read_u32(&data, &mut offset, path)?;
        dependencies.push(SavedDependency {
            path: PathBuf::from(value),
            stamp: DependencyStamp {
                kind,
                size,
                modified_seconds,
                modified_nanos,
            },
        });
    }
    if offset != data.len() {
        return Err(CacheError::InvalidDependencyFile(path.to_path_buf()));
    }
    Ok(Some(dependencies))
}

fn read_u8(data: &[u8], offset: &mut usize, path: &Path) -> Result<u8, CacheError> {
    let value = *data
        .get(*offset)
        .ok_or_else(|| CacheError::InvalidDependencyFile(path.to_path_buf()))?;
    *offset += 1;
    Ok(value)
}

fn read_u32(data: &[u8], offset: &mut usize, path: &Path) -> Result<u32, CacheError> {
    let end = offset
        .checked_add(4)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| CacheError::InvalidDependencyFile(path.to_path_buf()))?;
    let value = u32::from_le_bytes(data[*offset..end].try_into().unwrap());
    *offset = end;
    Ok(value)
}

fn read_u64(data: &[u8], offset: &mut usize, path: &Path) -> Result<u64, CacheError> {
    let end = offset
        .checked_add(8)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| CacheError::InvalidDependencyFile(path.to_path_buf()))?;
    let value = u64::from_le_bytes(data[*offset..end].try_into().unwrap());
    *offset = end;
    Ok(value)
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, CacheError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| CacheError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| CacheError::Io {
                path: directory.clone(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| CacheError::Io {
                path: entry.path(),
                source,
            })?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty()
            && value != "0"
            && !value.eq_ignore_ascii_case("false")
            && !value.eq_ignore_ascii_case("no")
    })
}

fn validate_key(key: &str) -> Result<(), CacheError> {
    if key.len() == 32 && key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CacheError::InvalidKey(key.to_owned()))
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug)]
pub enum CacheError {
    Io { path: PathBuf, source: io::Error },
    InvalidDependencyFile(PathBuf),
    InvalidEntry(PathBuf),
    InvalidKey(String),
    PathTooLong(PathBuf),
    TooManyDependencies(usize),
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidDependencyFile(path) => {
                write!(
                    formatter,
                    "invalid build-cache dependency file {}",
                    path.display()
                )
            }
            Self::InvalidEntry(path) => {
                write!(formatter, "invalid build-cache entry {}", path.display())
            }
            Self::InvalidKey(key) => write!(formatter, "invalid build-cache key {key:?}"),
            Self::PathTooLong(path) => {
                write!(
                    formatter,
                    "build-cache dependency path is too long: {}",
                    path.display()
                )
            }
            Self::TooManyDependencies(count) => {
                write!(formatter, "build cache has too many dependencies: {count}")
            }
        }
    }
}

impl Error for CacheError {
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{BuildCache, GeneratedFile, default_cache_root};

    #[test]
    fn default_cache_is_inside_the_project_build_directory() {
        let root = temp_dir("project-cache-root");
        fs::create_dir_all(&root).unwrap();
        let project = root.join("Game.project.gmx");
        fs::write(&project, "<assets/>").unwrap();

        assert_eq!(
            default_cache_root(&project),
            fs::canonicalize(&root)
                .unwrap()
                .join("build")
                .join(".gmx-cache")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restores_complete_builds_and_invalidates_changed_dependencies() {
        let root = temp_dir("build-cache");
        fs::create_dir_all(&root).unwrap();
        let project = root.join("Game.project.gmx");
        let source = root.join("script.gml");
        let wad = root.join("built.win");
        let output = root.join("restored/data.win");
        fs::write(&project, "<assets/>").unwrap();
        fs::write(&source, "return 1;").unwrap();
        fs::write(&wad, b"FORM-test").unwrap();
        let cache = BuildCache::at(root.join("cache"), &project, "Default");
        let dependencies = vec![project.clone(), source.clone()];
        let key = cache.key(&dependencies).unwrap();
        cache
            .store(
                &key,
                &wad,
                &[GeneratedFile {
                    path: PathBuf::from("music/theme.ogg"),
                    data: Arc::from(&b"audio"[..]),
                }],
            )
            .unwrap();
        cache.save_dependencies(&dependencies).unwrap();

        let known = cache.known_build().unwrap().unwrap();
        assert!(known.matches_dependencies(&dependencies).unwrap());
        assert!(
            !known
                .matches_dependencies(&[project.clone(), source.clone(), wad.clone()])
                .unwrap()
        );
        assert!(cache.known_build_if_unchanged().unwrap().is_some());
        assert_eq!(cache.known_key().unwrap().as_deref(), Some(key.as_str()));
        let hit = cache.restore(&key, &output).unwrap().unwrap();
        assert_eq!(hit.external_files, 1);
        assert_eq!(fs::read(&output).unwrap(), b"FORM-test");
        assert_eq!(
            fs::read(root.join("restored/music/theme.ogg")).unwrap(),
            b"audio"
        );

        let restored_audio = root.join("restored/music/theme.ogg");
        let output_permissions = fs::metadata(&output).unwrap().permissions();
        let audio_permissions = fs::metadata(&restored_audio).unwrap().permissions();
        let mut readonly = output_permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&output, readonly).unwrap();
        let mut readonly = audio_permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&restored_audio, readonly).unwrap();
        assert!(cache.restore(&key, &output).unwrap().is_some());
        fs::set_permissions(&output, output_permissions).unwrap();
        fs::set_permissions(&restored_audio, audio_permissions).unwrap();

        fs::write(&output, b"bad").unwrap();
        fs::write(&restored_audio, b"bad").unwrap();
        assert!(cache.restore(&key, &output).unwrap().is_some());
        assert_eq!(fs::read(&output).unwrap(), b"FORM-test");
        assert_eq!(fs::read(&restored_audio).unwrap(), b"audio");

        fs::write(&source, "return 200;").unwrap();
        assert!(cache.known_build_if_unchanged().unwrap().is_none());
        assert_ne!(cache.known_key().unwrap().unwrap(), key);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_dependencies_invalidate_the_cache_when_created() {
        let root = temp_dir("missing-dependency");
        fs::create_dir_all(&root).unwrap();
        let project = root.join("Game.project.gmx");
        let optional = root.join("missing.png");
        fs::write(&project, "<assets/>").unwrap();
        let cache = BuildCache::at(root.join("cache"), &project, "Default");
        let dependencies = vec![project, optional.clone()];

        let missing_key = cache.key(&dependencies).unwrap();
        cache.save_dependencies(&dependencies).unwrap();
        assert_eq!(cache.known_key().unwrap().unwrap(), missing_key);

        fs::write(&optional, b"png").unwrap();
        assert!(cache.known_build_if_unchanged().unwrap().is_none());
        assert_ne!(cache.known_key().unwrap().unwrap(), missing_key);
        fs::remove_file(&optional).unwrap();
        assert!(cache.known_build_if_unchanged().unwrap().is_some());
        assert_eq!(cache.known_key().unwrap().unwrap(), missing_key);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_payloads_are_shared_between_build_entries() {
        let root = temp_dir("shared-build-cache-files");
        fs::create_dir_all(&root).unwrap();
        let project = root.join("Game.project.gmx");
        let wad = root.join("built.win");
        fs::write(&project, "<assets/>").unwrap();
        fs::write(&wad, b"FORM-test").unwrap();
        let cache = BuildCache::at(root.join("cache"), &project, "Default");
        let files = [GeneratedFile {
            path: PathBuf::from("music/theme.ogg"),
            data: Arc::from(&b"shared audio"[..]),
        }];

        cache.store(&"1".repeat(32), &wad, &files).unwrap();
        cache.store(&"2".repeat(32), &wad, &files).unwrap();

        let objects = super::collect_files(&cache.root.join("objects")).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(fs::read(&objects[0]).unwrap(), b"shared audio");
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
