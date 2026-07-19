//! The storage / execution-environment seam: injectable filesystem and
//! environment access.
//!
//! # What this abstracts in pi
//!
//! pi's `agent` package funnels its persistence and environment reads through a
//! `NodeExecutionEnv` interface (`readTextLines`, file read/write, `process.env`
//! lookups) rather than calling `node:fs` and `process.env` directly — the
//! testing strategy notes this "maps cleanly to a Rust trait"
//! (`notes/startup/testing-strategy.md` §1.3). The mock-seam inventory
//! (`notes/mock-inventory.md`) attributes three collaborator sites to this seam:
//! tests that steer where sessions are read from and written to, and what the
//! environment reports.
//!
//! # Implementations
//!
//! - [`SystemEnv`] — the production environment: real files under `std::fs` and
//!   real `std::env` variables. This is what ships.
//! - [`MemoryEnv`] — a deterministic in-memory environment: files live in a map,
//!   environment variables are a fixed table. Tests inject it to run session and
//!   storage logic with no disk and no ambient `process.env`, exactly as pi's
//!   tests swap in a memory-backed execution env.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Filesystem plus environment access for the ported agent/storage layer.
///
/// Every method mirrors an operation pi performs through its `NodeExecutionEnv`
/// or `process.env`. Production code depends on `&dyn ExecutionEnv` so a test can
/// inject [`MemoryEnv`] and make storage behavior fully deterministic.
pub trait ExecutionEnv: Send + Sync {
    /// Read a file's full contents as UTF-8 (`fs.readFile`).
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    /// Read a file split into lines, mirroring pi's `readTextLines`. The default
    /// splits [`ExecutionEnv::read_to_string`] on `\n` and drops a trailing empty
    /// segment, matching pi's line reader for `\n`-terminated session files.
    fn read_text_lines(&self, path: &Path) -> io::Result<Vec<String>> {
        let text = self.read_to_string(path)?;
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        Ok(lines)
    }
    /// Write `contents` to `path`, creating or truncating (`fs.writeFile`).
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    /// Whether `path` exists (`fs.existsSync`).
    fn exists(&self, path: &Path) -> bool;
    /// Look up an environment variable (`process.env[key]`).
    fn env_var(&self, key: &str) -> Option<String>;
}

/// The production execution environment: real disk, real `std::env`.
#[derive(Debug, Default, Clone)]
pub struct SystemEnv;

impl SystemEnv {
    /// Construct the production environment.
    pub fn new() -> Self {
        Self
    }
}

impl ExecutionEnv for SystemEnv {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[derive(Default)]
struct MemoryEnvState {
    files: BTreeMap<PathBuf, String>,
    env: BTreeMap<String, String>,
}

/// A deterministic in-memory execution environment for tests.
///
/// Files and environment variables live in maps; nothing touches disk or the
/// ambient process environment. Cloneable and shareable — clones share state, so
/// a file written through one handle is visible through another.
#[derive(Clone, Default)]
pub struct MemoryEnv {
    state: Arc<Mutex<MemoryEnvState>>,
}

impl MemoryEnv {
    /// An empty in-memory environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed an environment variable, mirroring a test that pins `process.env`.
    pub fn with_env(self, key: &str, value: &str) -> Self {
        self.state
            .lock()
            .unwrap()
            .env
            .insert(key.to_string(), value.to_string());
        self
    }

    /// Seed a file's contents.
    pub fn with_file(self, path: impl Into<PathBuf>, contents: &str) -> Self {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.into(), contents.to_string());
        self
    }
}

impl ExecutionEnv for MemoryEnv {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.state
            .lock()
            .unwrap()
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no such file: {path:?}"))
            })
    }
    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_path_buf(), contents.to_string());
        Ok(())
    }
    fn exists(&self, path: &Path) -> bool {
        self.state.lock().unwrap().files.contains_key(path)
    }
    fn env_var(&self, key: &str) -> Option<String> {
        self.state.lock().unwrap().env.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_env_round_trips_files_and_vars() {
        let env = MemoryEnv::new().with_env("PI_API_KEY", "secret");
        assert_eq!(env.env_var("PI_API_KEY").as_deref(), Some("secret"));
        assert_eq!(env.env_var("ABSENT"), None);

        let path = Path::new("/sessions/a.jsonl");
        assert!(!env.exists(path));
        env.write(path, "line1\nline2\n").unwrap();
        assert!(env.exists(path));
        assert_eq!(env.read_to_string(path).unwrap(), "line1\nline2\n");
        assert_eq!(env.read_text_lines(path).unwrap(), vec!["line1", "line2"]);
    }

    #[test]
    fn memory_env_missing_file_is_not_found() {
        let env = MemoryEnv::new();
        let err = env.read_to_string(Path::new("/nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn system_env_reads_real_process_env() {
        // PATH is present in every environment this runs in.
        assert!(SystemEnv::new().env_var("PATH").is_some());
    }
}
