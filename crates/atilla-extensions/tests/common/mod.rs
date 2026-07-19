//! Shared filesystem-fixture helpers for the `deno`-gated resource-loader
//! acceptance tests. Ported from `crates/atilla-coding/tests/common/mod.rs`
//! (only the path/write/symlink helpers the moved extension cases need).

// The test binary that includes this module uses a subset of these helpers, so
// per-binary `dead_code` is expected and allowed.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

pub fn canonical(path: &str) -> String {
    fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

pub fn join(base: &str, parts: &[&str]) -> String {
    let mut p = std::path::PathBuf::from(base);
    for part in parts {
        p.push(part);
    }
    p.to_string_lossy().into_owned()
}

pub fn write(path: &str, content: &str) {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub fn mkdir(path: &str) {
    fs::create_dir_all(path).unwrap();
}

#[cfg(unix)]
pub fn symlink_dir(src: &str, dst: &str) {
    std::os::unix::fs::symlink(src, dst).unwrap();
}

#[cfg(not(unix))]
pub fn symlink_dir(src: &str, dst: &str) {
    let _ = (src, dst);
    panic!("symlink tests require unix");
}
