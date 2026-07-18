//! Shared `#[cfg(test)]` filesystem fixtures for the tool tests.
//!
//! The `find` and `grep` tools both exercise their pure search layers against
//! real temporary directories. The `TempDir` scaffolding used to be copy-pasted
//! into each module's test block; hoisting it here keeps a single source of
//! truth for the setup helper.

use std::fs;
use std::path::PathBuf;

/// Build a unique path under the system temp dir, tagged with `prefix` plus the
/// current process id and a nanosecond timestamp. Shared by every test fixture
/// that needs a collision-free scratch directory.
pub fn unique_temp_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// A self-cleaning temporary directory rooted under the system temp dir.
pub struct TempDir {
    /// Absolute path to the directory root.
    pub path: PathBuf,
}

impl TempDir {
    /// Create a uniquely-named temp directory tagged for the calling test.
    pub fn new(tag: &str) -> Self {
        let path = unique_temp_path(&format!("atilla-tool-{tag}"));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    /// Create a subdirectory (recursively) relative to the root.
    pub fn mkdir(&self, rel: &str) {
        fs::create_dir_all(self.path.join(rel)).unwrap();
    }

    /// Write `content` to `name` (creating parents), returning the file path.
    pub fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.path.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }

    /// The root path as a `&str`, for use as a `cwd` argument.
    pub fn cwd(&self) -> &str {
        self.path.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
