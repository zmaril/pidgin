//! Path, environment, and version helpers mirroring
//! `packages/orchestrator/src/config.ts`.
//!
//! pi resolves the orchestrator data directory from `PI_ORCHESTRATOR_DIR`, then
//! `PI_CONFIG_DIR`, then `~/.pi`, and derives the auth/machine/instances/socket
//! paths beneath an `orchestrator/` subdirectory. `isBunBinary` detects a Bun
//! compiled binary from the module URL, and `VERSION` reflects the package
//! version (`env!("CARGO_PKG_VERSION")` is the Rust-native analog of pi walking
//! up to read its own `package.json`, falling back to `"0.0.0"`).

// straitjacket-allow-file[:duplication] — the `home_dir()` resolver and the
// `EnvGuard` test scaffold here are faithfully mirrored by the parallel ports in
// `credential_store.rs` and `radius.rs`; the shared shape is deliberate.

use std::path::PathBuf;

/// Name of the pi config directory (`CONFIG_DIR_NAME` in `config.ts`).
const CONFIG_DIR_NAME: &str = ".pi";

/// Environment variable overriding the orchestrator directory.
const ENV_ORCHESTRATOR_DIR: &str = "PI_ORCHESTRATOR_DIR";

/// Environment variable overriding the pi config directory.
const ENV_CONFIG_DIR: &str = "PI_CONFIG_DIR";

/// Detect a Bun compiled binary from a module URL.
///
/// Bun binaries have an `import.meta.url` containing `"$bunfs"`, `"~BUN"`, or
/// `"%7EBUN"` (Bun's virtual filesystem path). Mirrors pi's `isBunBinary`
/// expression, factored into a pure helper so it is directly testable.
pub fn is_bun_binary_url(url: &str) -> bool {
    url.contains("$bunfs") || url.contains("~BUN") || url.contains("%7EBUN")
}

/// Whether this process is running as a Bun compiled binary.
///
/// pi evaluates `isBunBinary` from `import.meta.url` at module load. Rust has no
/// module URL, so we evaluate the same substring test against the current
/// executable path (the closest analog to Bun's virtual-filesystem module URL).
pub fn is_bun_binary() -> bool {
    std::env::current_exe()
        .ok()
        .map(|path| is_bun_binary_url(&path.to_string_lossy()))
        .unwrap_or(false)
}

/// Package version, mirroring pi's `VERSION` (`pkg.version || "0.0.0"`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Home directory (`os.homedir()`), matching the resolution used elsewhere in
/// atilla: `USERPROFILE` on Windows, `HOME` otherwise.
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key).map(PathBuf::from).unwrap_or_default()
}

/// Resolve the orchestrator data directory.
///
/// Precedence mirrors `config.ts:getOrchestratorDir`:
/// 1. `PI_ORCHESTRATOR_DIR` if set.
/// 2. `(PI_CONFIG_DIR || ~/.pi)/orchestrator`.
pub fn get_orchestrator_dir() -> PathBuf {
    if let Some(env_dir) = non_empty_env(ENV_ORCHESTRATOR_DIR) {
        return PathBuf::from(env_dir);
    }

    let pi_dir = match non_empty_env(ENV_CONFIG_DIR) {
        Some(dir) => PathBuf::from(dir),
        None => home_dir().join(CONFIG_DIR_NAME),
    };
    pi_dir.join("orchestrator")
}

/// Path to `auth.json` (`config.ts:getAuthPath`).
pub fn get_auth_path() -> PathBuf {
    get_orchestrator_dir().join("auth.json")
}

/// Path to `machine.json` (`config.ts:getMachinePath`).
pub fn get_machine_path() -> PathBuf {
    get_orchestrator_dir().join("machine.json")
}

/// Path to `instances.json` (`config.ts:getInstancesPath`).
pub fn get_instances_path() -> PathBuf {
    get_orchestrator_dir().join("instances.json")
}

/// Path to the orchestrator Unix socket (`config.ts:getSocketPath`).
pub fn get_socket_path() -> PathBuf {
    get_orchestrator_dir().join("orchestrator.sock")
}

/// Read an environment variable, treating unset and empty as absent.
///
/// pi uses `process.env[X]` truthiness, so an empty string is falsy and falls
/// through to the next branch.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Environment mutation is process-global; serialize the env-dependent tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            EnvGuard { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn is_bun_binary_url_detects_markers() {
        assert!(is_bun_binary_url("/$bunfs/root/orchestrator.js"));
        assert!(is_bun_binary_url("file:///~BUN/root/index.js"));
        assert!(is_bun_binary_url("file:///%7EBUN/root/index.js"));
        assert!(!is_bun_binary_url(
            "file:///home/user/.pi/orchestrator/index.js"
        ));
        assert!(!is_bun_binary_url(""));
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn orchestrator_dir_prefers_explicit_env() {
        let _guard = EnvGuard::new(&[ENV_ORCHESTRATOR_DIR, ENV_CONFIG_DIR, "HOME"]);
        std::env::set_var(ENV_ORCHESTRATOR_DIR, "/custom/orchestrator");
        std::env::set_var(ENV_CONFIG_DIR, "/should/be/ignored");
        std::env::set_var("HOME", "/home/ignored");
        assert_eq!(
            get_orchestrator_dir(),
            PathBuf::from("/custom/orchestrator")
        );
    }

    #[test]
    fn orchestrator_dir_falls_back_to_config_dir() {
        let _guard = EnvGuard::new(&[ENV_ORCHESTRATOR_DIR, ENV_CONFIG_DIR, "HOME"]);
        std::env::set_var(ENV_CONFIG_DIR, "/opt/pi-config");
        std::env::set_var("HOME", "/home/ignored");
        assert_eq!(
            get_orchestrator_dir(),
            PathBuf::from("/opt/pi-config/orchestrator")
        );
    }

    #[test]
    fn orchestrator_dir_falls_back_to_home() {
        let _guard = EnvGuard::new(&[ENV_ORCHESTRATOR_DIR, ENV_CONFIG_DIR, "HOME"]);
        std::env::set_var("HOME", "/home/pi-user");
        assert_eq!(
            get_orchestrator_dir(),
            PathBuf::from("/home/pi-user/.pi/orchestrator")
        );
    }

    #[test]
    fn empty_env_is_treated_as_unset() {
        let _guard = EnvGuard::new(&[ENV_ORCHESTRATOR_DIR, ENV_CONFIG_DIR, "HOME"]);
        std::env::set_var(ENV_ORCHESTRATOR_DIR, "");
        std::env::set_var(ENV_CONFIG_DIR, "");
        std::env::set_var("HOME", "/home/pi-user");
        assert_eq!(
            get_orchestrator_dir(),
            PathBuf::from("/home/pi-user/.pi/orchestrator")
        );
    }

    #[test]
    fn derived_paths_nest_under_orchestrator_dir() {
        let _guard = EnvGuard::new(&[ENV_ORCHESTRATOR_DIR, ENV_CONFIG_DIR, "HOME"]);
        std::env::set_var(ENV_ORCHESTRATOR_DIR, "/data/orch");
        assert_eq!(get_auth_path(), PathBuf::from("/data/orch/auth.json"));
        assert_eq!(get_machine_path(), PathBuf::from("/data/orch/machine.json"));
        assert_eq!(
            get_instances_path(),
            PathBuf::from("/data/orch/instances.json")
        );
        assert_eq!(
            get_socket_path(),
            PathBuf::from("/data/orch/orchestrator.sock")
        );
    }
}
