//! Shared filesystem/path helpers for pidgin's integration test suites.
//!
//! These are the generic, crate-agnostic path/write/symlink helpers that the
//! package-manager resolve/discovery tests (`pidgin-coding`) and the `deno`-gated
//! resource-loader acceptance tests (`pidgin-extensions`) both need. They were
//! previously copied between `crates/pidgin-coding/tests/common/mod.rs` and
//! `crates/pidgin-extensions/tests/common/mod.rs` — a pidgin-own duplication (the
//! extensions copy's header said it was "Ported from
//! `crates/pidgin-coding/tests/common/mod.rs`"), not a pi mirror. Hoisting them
//! into this dev-only crate lets the two suites share one copy across the crate
//! boundary. Crate-specific fixtures (e.g. the package-manager `Fixture`) stay in
//! each crate's own `tests/common/mod.rs`.

use std::fs;
use std::path::Path;

/// Canonicalize a path, falling back to the input on error.
pub fn canonical(path: &str) -> String {
    fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// Join `parts` onto `base` and return the resulting path as a string.
pub fn join(base: &str, parts: &[&str]) -> String {
    let mut p = std::path::PathBuf::from(base);
    for part in parts {
        p.push(part);
    }
    p.to_string_lossy().into_owned()
}

/// Write `content` to `path`, creating parent directories as needed.
pub fn write(path: &str, content: &str) {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Create `path` and all missing parent directories.
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
