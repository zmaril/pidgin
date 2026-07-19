//! Shared `#[cfg(test)]` helpers for the `core` module's unit tests.
//!
//! `project_trust` and `trust_manager` both stand up throwaway temp
//! directories and stringify paths for their store/resource fixtures. That
//! setup used to be copy-pasted into each module's test block; hoisting it here
//! keeps a single source of truth for the scaffolding.

use std::path::{Path, PathBuf};

/// Create a uniquely-named scratch directory under the system temp dir, tagged
/// for the calling test so parallel runs never collide.
pub fn scratch_dir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "atilla-core-test-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// Borrow a path as an owned `String`, for the many APIs here that take `&str`
/// paths.
pub fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Write `contents` to `path`, creating any missing parent directories first.
/// Shared by the many tests that stage on-disk resource fixtures.
pub fn write(path: &str, contents: &str) {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}
