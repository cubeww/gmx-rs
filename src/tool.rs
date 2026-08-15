use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn find_tool(names: &[&str]) -> Option<PathBuf> {
    if let Ok(executable) = env::current_exe()
        && let Some(directory) = executable.parent()
        && let Some(path) = find_in(directory, names)
    {
        return Some(path);
    }

    env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .find_map(|directory| find_in(&directory, names))
}

fn find_in(directory: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .find_map(|name| executable_at(&directory.join(name)))
}

fn executable_at(path: &Path) -> Option<PathBuf> {
    path.is_file()
        .then(|| fs::canonicalize(path).ok())
        .flatten()
}
