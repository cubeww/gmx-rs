use std::path::PathBuf;

pub(crate) fn gmx_path(value: &str) -> PathBuf {
    let mut result = PathBuf::new();
    push_gmx_path(&mut result, value);
    result
}

pub(crate) fn push_gmx_path(path: &mut PathBuf, value: &str) {
    for component in value.split(['\\', '/']).filter(|part| !part.is_empty()) {
        path.push(component);
    }
}
