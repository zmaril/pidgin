//! Settings storage backends, ported from the `SettingsStorage` implementations
//! in `packages/coding-agent/src/core/settings-manager.ts`.

use std::cell::RefCell;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::utils::paths::{resolve_path, PathInputOptions};

use super::{SettingsScope, CONFIG_DIR_NAME};

/// A storage backend for a settings scope. `with_lock` yields the current
/// serialized contents (or `None` when absent) and persists whatever `Some`
/// value the callback returns. Mirrors pi's `SettingsStorage` interface.
pub trait SettingsStorage {
    /// Run `f` against the current contents of `scope`, writing back its result
    /// when it returns `Some`.
    fn with_lock(&self, scope: SettingsScope, f: &mut dyn FnMut(Option<&str>) -> Option<String>);
}

/// A lock guard that removes its lock file on drop, mirroring the release
/// closure returned by `proper-lockfile`.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// File-backed storage. Global settings live at `<agent_dir>/settings.json`;
/// project settings at `<cwd>/.pi/settings.json`. Mirrors pi's
/// `FileSettingsStorage`.
pub struct FileSettingsStorage {
    global_settings_path: PathBuf,
    project_settings_path: PathBuf,
}

impl FileSettingsStorage {
    /// Construct a storage rooted at `cwd` (project scope) and `agent_dir`
    /// (global scope). Both are resolved to absolute paths, as pi does.
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        let opts = PathInputOptions::default();
        let resolved_cwd = resolve_path(cwd, ".", &opts).unwrap_or_else(|_| cwd.to_string());
        let resolved_agent_dir =
            resolve_path(agent_dir, ".", &opts).unwrap_or_else(|_| agent_dir.to_string());
        Self {
            global_settings_path: Path::new(&resolved_agent_dir).join("settings.json"),
            project_settings_path: Path::new(&resolved_cwd)
                .join(CONFIG_DIR_NAME)
                .join("settings.json"),
        }
    }

    /// Acquire an exclusive lock on `path`, retrying on contention. Mirrors pi's
    /// `acquireLockSyncWithRetry` (10 attempts, 20ms apart, on `ELOCKED`).
    fn acquire_lock_with_retry(path: &Path) -> std::io::Result<LockGuard> {
        let lock_path = {
            let mut p = path.as_os_str().to_os_string();
            p.push(".lock");
            PathBuf::from(p)
        };
        let max_attempts = 10;
        let delay = std::time::Duration::from_millis(20);
        let mut last_err: Option<std::io::Error> = None;

        for attempt in 1..=max_attempts {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(LockGuard { path: lock_path }),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    if attempt == max_attempts {
                        return Err(err);
                    }
                    last_err = Some(err);
                    std::thread::sleep(delay);
                }
                Err(err) => return Err(err),
            }
        }

        Err(last_err.unwrap_or_else(|| std::io::Error::other("Failed to acquire settings lock")))
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn with_lock(&self, scope: SettingsScope, f: &mut dyn FnMut(Option<&str>) -> Option<String>) {
        let path = match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        };
        let dir = path.parent().map(Path::to_path_buf);

        // Only create the directory and take the lock when the file already
        // exists or we actually need to write (pi comment: avoid creating the
        // `.pi` folder just to read).
        let file_exists = path.exists();
        let mut guard = if file_exists {
            Self::acquire_lock_with_retry(path).ok()
        } else {
            None
        };

        let current = if file_exists {
            fs::read_to_string(path).ok()
        } else {
            None
        };

        let next = f(current.as_deref());
        if let Some(contents) = next {
            if let Some(dir) = &dir {
                if !dir.exists() {
                    let _ = fs::create_dir_all(dir);
                }
            }
            if guard.is_none() {
                guard = Self::acquire_lock_with_retry(path).ok();
            }
            let _ = fs::write(path, contents);
        }

        drop(guard);
    }
}

/// In-memory storage with no filesystem I/O. Mirrors pi's
/// `InMemorySettingsStorage`.
#[derive(Default)]
pub struct InMemorySettingsStorage {
    global: RefCell<Option<String>>,
    project: RefCell<Option<String>>,
}

impl InMemorySettingsStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(&self, scope: SettingsScope, f: &mut dyn FnMut(Option<&str>) -> Option<String>) {
        let cell = match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        };
        let current = cell.borrow().clone();
        let next = f(current.as_deref());
        if let Some(contents) = next {
            *cell.borrow_mut() = Some(contents);
        }
    }
}
