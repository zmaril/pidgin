// straitjacket-allow-file:duplication — `AuthStorage` is a faithful mirror of
// pi's file-backed `CredentialStore`, so its `read`/`list`/`modify`/`delete`
// members repeat the trait's method shapes and the backends' `with_lock`
// scaffolding, and the inline tests rebuild the same `write_auth_json` + create
// fixture per case (each exercises a distinct store path in isolation). The
// clone detector reads this parallel structure as duplication; it is deliberate.
//! File-backed credential storage, ported from pi's `core/auth-storage.ts`.
//!
//! [`AuthStorage`] is the coding-agent's [`CredentialStore`] over an `auth.json`
//! file: a keyed map of one [`Credential`] per provider. Writes are the only
//! mutating path and go through a locked read-modify-write on the backing
//! [`AuthStorageBackend`] ([`FileAuthStorageBackend`] for real files,
//! [`InMemoryAuthStorageBackend`] for tests), so concurrent processes serialize
//! on a `<auth.json>.lock` file. Reads resolve api-key values through
//! [`resolve_config_value`] (env/command interpolation) while OAuth credentials
//! pass through untouched. [`read_stored_credential`] is a one-off reader that
//! never instantiates a store or resolves configured values.
//!
//! # Sync port deviations from pi
//!
//! pi's [`CredentialStore`] is async (`Promise<...>`); pidgin's trait is
//! synchronous, so this port is too. Consequently pi's split of the backend into
//! `withLock` (sync) and `withLockAsync` (async, used by `modify`/`delete`)
//! collapses into a single [`AuthStorageBackend::with_lock`]. pi's async lock
//! uses `proper-lockfile` with an `onCompromised` stale-lock callback and
//! exponential backoff; the sync lock here mirrors the crate's existing
//! `FileSettingsStorage` pattern — a `<path>.lock` sentinel file acquired with
//! bounded retry (10 attempts, 20ms apart), released on drop. The
//! `onCompromised` mechanism has no sync analogue, so pi's
//! "compromised OAuth refresh lock" test (which drives it through the full
//! `Models.getAuth` OAuth-refresh integration) is not ported — the invariant it
//! shares with the lock-acquisition-failure test, "a bad lock never writes and a
//! later retry succeeds", is covered by [`tests`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use indexmap::IndexMap;
use pidgin_ai::auth::{
    Credential, CredentialInfo, CredentialStore, ModifyError, ModifyFn, StoreError,
};

use crate::core::resolve_config_value::resolve_config_value;
use crate::core::skills::get_agent_dir;
use crate::utils::paths::{normalize_path, PathInputOptions};

/// The `auth.json` file mode: owner read/write only (`AUTH_FILE_WRITE_OPTIONS`).
const AUTH_FILE_MODE: u32 = 0o600;
/// The agent-dir mode when created: owner rwx only.
const AGENT_DIR_MODE: u32 = 0o700;

/// The keyed `auth.json` map: one [`Credential`] per provider id, insertion
/// order preserved (pi's `Record<string, Credential>`).
type AuthStorageData = IndexMap<String, Credential>;

/// The locked storage substrate under [`AuthStorage`].
///
/// `with_lock` yields the current serialized contents (or `None` when absent)
/// and persists whatever `Some` string the callback returns, all under an
/// exclusive lock. Returns `Err` when the lock cannot be acquired or a write
/// fails, so callers never write against a lock they do not hold.
pub trait AuthStorageBackend: Send + Sync {
    /// Run `f` against the current contents under the lock, writing back its
    /// `Some` result.
    fn with_lock(
        &self,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), StoreError>;
}

/// A lock guard that removes its sentinel file on drop, mirroring the release
/// closure returned by `proper-lockfile`.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// File-backed storage over `auth.json` (pi's `FileAuthStorageBackend`).
pub struct FileAuthStorageBackend {
    auth_path: String,
}

impl FileAuthStorageBackend {
    /// Construct a backend over `auth_path`, normalizing it (tilde expansion,
    /// `file://` conversion) as pi does.
    pub fn new(auth_path: &str) -> Self {
        let normalized = normalize_path(auth_path, &PathInputOptions::default())
            .unwrap_or_else(|_| auth_path.to_string());
        Self {
            auth_path: normalized,
        }
    }

    /// Construct a backend over the default `<agent_dir>/auth.json` path.
    pub fn default_path() -> Self {
        Self::new(&default_auth_path())
    }

    fn ensure_parent_dir(&self) -> Result<(), StoreError> {
        if let Some(dir) = Path::new(&self.auth_path).parent() {
            if !dir.exists() {
                fs::create_dir_all(dir).map_err(|e| {
                    StoreError::new(format!("failed to create {}: {e}", dir.display()))
                })?;
                set_mode(dir, AGENT_DIR_MODE)?;
            }
        }
        Ok(())
    }

    fn ensure_file_exists(&self) -> Result<(), StoreError> {
        let path = Path::new(&self.auth_path);
        if !path.exists() {
            write_auth_file(path, "{}")?;
        }
        Ok(())
    }

    /// Acquire an exclusive lock on the auth path, retrying on contention.
    /// Mirrors pi's `acquireLockSyncWithRetry` (10 attempts, 20ms apart).
    fn acquire_lock_with_retry(&self) -> Result<LockGuard, StoreError> {
        let lock_path = {
            let mut p = OsString::from(&self.auth_path);
            p.push(".lock");
            PathBuf::from(p)
        };
        let max_attempts = 10;
        let delay = Duration::from_millis(20);

        for attempt in 1..=max_attempts {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(LockGuard { path: lock_path }),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    if attempt == max_attempts {
                        return Err(StoreError::new(format!(
                            "failed to acquire auth storage lock: {err}"
                        )));
                    }
                    thread::sleep(delay);
                }
                Err(err) => {
                    return Err(StoreError::new(format!(
                        "failed to acquire auth storage lock: {err}"
                    )));
                }
            }
        }

        Err(StoreError::new("failed to acquire auth storage lock"))
    }
}

impl AuthStorageBackend for FileAuthStorageBackend {
    fn with_lock(
        &self,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), StoreError> {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        let _guard = self.acquire_lock_with_retry()?;
        let path = Path::new(&self.auth_path);
        let current = fs::read_to_string(path).ok();
        let next = f(current.as_deref());
        if let Some(contents) = next {
            write_auth_file(path, &contents)?;
        }
        Ok(())
    }
}

/// In-memory storage backend for tests (pi's `InMemoryAuthStorageBackend`).
#[derive(Default)]
pub struct InMemoryAuthStorageBackend {
    value: Mutex<Option<String>>,
}

impl InMemoryAuthStorageBackend {
    /// An empty in-memory backend.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AuthStorageBackend for InMemoryAuthStorageBackend {
    fn with_lock(
        &self,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), StoreError> {
        let mut value = self
            .value
            .lock()
            .expect("auth storage value mutex poisoned");
        let next = f(value.as_deref());
        if let Some(contents) = next {
            *value = Some(contents);
        }
        Ok(())
    }
}

/// Credential storage backed by a JSON file (pi's `AuthStorage`).
pub struct AuthStorage {
    storage: Box<dyn AuthStorageBackend>,
    data: Mutex<AuthStorageData>,
}

impl AuthStorage {
    /// Construct a file-backed store, defaulting to `<agent_dir>/auth.json`.
    pub fn create(auth_path: Option<&str>) -> AuthStorage {
        let backend = match auth_path {
            Some(path) => FileAuthStorageBackend::new(path),
            None => FileAuthStorageBackend::default_path(),
        };
        AuthStorage::from_storage(Box::new(backend))
    }

    /// Construct a store over an arbitrary backend, loading its current state.
    pub fn from_storage(storage: Box<dyn AuthStorageBackend>) -> AuthStorage {
        let store = AuthStorage {
            storage,
            data: Mutex::new(AuthStorageData::new()),
        };
        store.reload();
        store
    }

    /// Construct an in-memory store seeded with `data` (pi's `inMemory`).
    pub fn in_memory(data: AuthStorageData) -> AuthStorage {
        let backend = InMemoryAuthStorageBackend::new();
        let serialized = serde_json::to_string_pretty(&data).unwrap_or_else(|_| "{}".to_string());
        let _ = backend.with_lock(&mut |_| Some(serialized.clone()));
        AuthStorage::from_storage(Box::new(backend))
    }

    /// Reload credentials from storage, preserving the last valid snapshot on
    /// any lock or parse failure (pi's `reload`).
    pub fn reload(&self) {
        let mut content: Option<String> = None;
        let lock_result = self.storage.with_lock(&mut |current| {
            content = current.map(str::to_string);
            None
        });
        if lock_result.is_ok() {
            if let Ok(parsed) = parse_storage_data(content.as_deref()) {
                *self.data.lock().expect("auth storage data mutex poisoned") = parsed;
            }
        }
    }
}

impl CredentialStore for AuthStorage {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>, StoreError> {
        let credential = self
            .data
            .lock()
            .expect("auth storage data mutex poisoned")
            .get(provider_id)
            .cloned();
        match credential {
            Some(Credential::ApiKey(mut api_key)) => {
                if let Some(key) = api_key.key.clone() {
                    let env: Option<HashMap<String, String>> = api_key
                        .env
                        .as_ref()
                        .map(|e| e.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                    api_key.key = resolve_config_value(&key, env.as_ref());
                }
                Ok(Some(Credential::ApiKey(api_key)))
            }
            other => Ok(other),
        }
    }

    fn list(&self) -> Result<Vec<CredentialInfo>, StoreError> {
        Ok(self
            .data
            .lock()
            .expect("auth storage data mutex poisoned")
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.auth_type(),
            })
            .collect())
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut ModifyFn,
    ) -> Result<Option<Credential>, ModifyError> {
        let mut result: Result<Option<Credential>, ModifyError> = Ok(None);
        let mut new_data: Option<AuthStorageData> = None;

        let lock_result = self.storage.with_lock(&mut |content| {
            let current_data = match parse_storage_data(content) {
                Ok(data) => data,
                Err(err) => {
                    result = Err(ModifyError::Store(StoreError::new(err)));
                    return None;
                }
            };
            let current = current_data.get(provider_id).cloned();
            match f(current) {
                Err(err) => {
                    result = Err(ModifyError::Callback(err));
                    None
                }
                Ok(None) => {
                    // pi: `this.data = currentData; return { result: currentData[provider] }`
                    let existing = current_data.get(provider_id).cloned();
                    new_data = Some(current_data);
                    result = Ok(existing);
                    None
                }
                Ok(Some(next)) => {
                    let mut merged = current_data;
                    merged.insert(provider_id.to_string(), next.clone());
                    match serde_json::to_string_pretty(&merged) {
                        Ok(serialized) => {
                            new_data = Some(merged);
                            result = Ok(Some(next));
                            Some(serialized)
                        }
                        Err(err) => {
                            result = Err(ModifyError::Store(StoreError::new(err.to_string())));
                            None
                        }
                    }
                }
            }
        });

        if let Some(data) = new_data {
            *self.data.lock().expect("auth storage data mutex poisoned") = data;
        }
        if let Err(store_err) = lock_result {
            return Err(ModifyError::Store(store_err));
        }
        result
    }

    fn delete(&self, provider_id: &str) -> Result<(), StoreError> {
        let mut new_data: Option<AuthStorageData> = None;
        let mut parse_err: Option<String> = None;

        let lock_result = self.storage.with_lock(&mut |content| {
            match parse_storage_data(content) {
                Ok(mut current_data) => {
                    current_data.shift_remove(provider_id);
                    let serialized =
                        serde_json::to_string_pretty(&current_data).unwrap_or_else(|_| {
                            // A parsed map always re-serializes; fall back defensively.
                            "{}".to_string()
                        });
                    new_data = Some(current_data);
                    Some(serialized)
                }
                Err(err) => {
                    parse_err = Some(err);
                    None
                }
            }
        });

        lock_result?;
        if let Some(err) = parse_err {
            return Err(StoreError::new(err));
        }
        if let Some(data) = new_data {
            *self.data.lock().expect("auth storage data mutex poisoned") = data;
        }
        Ok(())
    }
}

/// The default auth path: `<agent_dir>/auth.json` (pi's `getAuthPath`).
pub fn default_auth_path() -> String {
    format!("{}/auth.json", get_agent_dir())
}

/// Parse the serialized `auth.json` contents into the keyed map. Empty/absent
/// contents parse to an empty map (pi's `parseStorageData`).
fn parse_storage_data(content: Option<&str>) -> Result<AuthStorageData, String> {
    match content {
        None => Ok(AuthStorageData::new()),
        Some("") => Ok(AuthStorageData::new()),
        Some(raw) => serde_json::from_str(raw).map_err(|e| e.to_string()),
    }
}

/// Write `contents` to `path` and force owner-only (`0o600`) permissions.
fn write_auth_file(path: &Path, contents: &str) -> Result<(), StoreError> {
    fs::write(path, contents)
        .map_err(|e| StoreError::new(format!("failed to write {}: {e}", path.display())))?;
    set_mode(path, AUTH_FILE_MODE)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| StoreError::new(format!("failed to chmod {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), StoreError> {
    Ok(())
}

/// One-off synchronous read of a stored credential from an `auth.json` file,
/// without instantiating a store or resolving configured key values (pi's
/// `readStoredCredential`). Defaults to `<agent_dir>/auth.json`.
pub fn read_stored_credential(provider_id: &str, auth_path: Option<&str>) -> Option<Credential> {
    let raw = auth_path
        .map(str::to_string)
        .unwrap_or_else(default_auth_path);
    let normalized = normalize_path(&raw, &PathInputOptions::default()).unwrap_or(raw);
    let content = fs::read_to_string(&normalized).ok()?;
    let data: AuthStorageData = serde_json::from_str(&content).ok()?;
    data.get(provider_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::auth::{ApiKeyCredential, AuthType, OAuthCredential};
    use serde_json::{json, Map as JsonMap, Value};
    use std::sync::Mutex as StdMutex;

    /// Serializes tests that mutate process-global environment variables so they
    /// do not race one another.
    static ENV_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn api_key(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_string()),
            env: None,
        })
    }

    fn temp_auth_path() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json").to_string_lossy().into_owned();
        (dir, path)
    }

    fn write_auth_json(path: &str, data: Value) {
        fs::write(path, serde_json::to_string(&data).unwrap()).unwrap();
    }

    fn read_json(path: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn data_map(pairs: Vec<(&str, Credential)>) -> AuthStorageData {
        let mut data = AuthStorageData::new();
        for (provider, credential) in pairs {
            data.insert(provider.to_string(), credential);
        }
        data
    }

    #[test]
    fn reads_and_resolves_stored_api_key_credentials() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("TEST_AUTH_STORAGE_KEY", "environment-key");
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "$TEST_AUTH_STORAGE_KEY" } }),
        );
        let storage = AuthStorage::create(Some(&path));
        assert_eq!(
            storage.read("anthropic").unwrap(),
            Some(api_key("environment-key"))
        );
        std::env::remove_var("TEST_AUTH_STORAGE_KEY");
    }

    #[test]
    fn resolves_command_backed_api_key_credentials() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "!printf 'command-key'" } }),
        );
        let storage = AuthStorage::create(Some(&path));
        assert_eq!(
            storage.read("anthropic").unwrap(),
            Some(api_key("command-key"))
        );
    }

    #[test]
    fn returns_oauth_credentials_unchanged() {
        let credential = Credential::OAuth(OAuthCredential {
            access: "access-token".into(),
            refresh: "refresh-token".into(),
            expires: 60_000,
            extra: JsonMap::new(),
        });
        let storage = AuthStorage::in_memory(data_map(vec![("anthropic", credential.clone())]));
        assert_eq!(storage.read("anthropic").unwrap(), Some(credential));
    }

    #[test]
    fn credential_scoped_env_takes_precedence_and_remains_inspectable() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({
                "anthropic": {
                    "type": "api_key",
                    "key": "$SCOPED_KEY",
                    "env": { "SCOPED_KEY": "scoped-value", "REGION": "test-region" }
                }
            }),
        );
        let storage = AuthStorage::create(Some(&path));
        match storage.read("anthropic").unwrap().unwrap() {
            Credential::ApiKey(api_key) => {
                assert_eq!(api_key.key.as_deref(), Some("scoped-value"));
                let env = api_key.env.unwrap();
                assert_eq!(
                    env.get("SCOPED_KEY").map(String::as_str),
                    Some("scoped-value")
                );
                assert_eq!(env.get("REGION").map(String::as_str), Some("test-region"));
            }
            other => panic!("expected api_key, got {other:?}"),
        }
    }

    #[test]
    fn modify_persists_a_credential_while_preserving_unrelated_external_edits() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "old" } }),
        );
        let storage = AuthStorage::create(Some(&path));
        write_auth_json(
            &path,
            json!({
                "anthropic": { "type": "api_key", "key": "old" },
                "openai": { "type": "api_key", "key": "external" }
            }),
        );

        let mut set = |_current: Option<Credential>| Ok(Some(api_key("new")));
        storage.modify("anthropic", &mut set).unwrap();

        assert_eq!(
            read_json(&path),
            json!({
                "anthropic": { "type": "api_key", "key": "new" },
                "openai": { "type": "api_key", "key": "external" }
            })
        );
    }

    #[test]
    fn modify_with_none_leaves_the_current_credential_unchanged() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        let storage = AuthStorage::create(Some(&path));

        let mut noop = |_current: Option<Credential>| Ok(None);
        assert_eq!(
            storage.modify("anthropic", &mut noop).unwrap(),
            Some(api_key("stored"))
        );
        assert_eq!(storage.read("anthropic").unwrap(), Some(api_key("stored")));
    }

    #[test]
    fn serializes_concurrent_modifications() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(&path, json!({}));

        let path_a = path.clone();
        let path_b = path.clone();
        let first = thread::spawn(move || {
            let storage = AuthStorage::create(Some(&path_a));
            let mut set = |_c: Option<Credential>| Ok(Some(api_key("anthropic-key")));
            storage.modify("anthropic", &mut set).unwrap();
        });
        let second = thread::spawn(move || {
            let storage = AuthStorage::create(Some(&path_b));
            let mut set = |_c: Option<Credential>| Ok(Some(api_key("openai-key")));
            storage.modify("openai", &mut set).unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();

        assert_eq!(
            read_json(&path),
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" }
            })
        );
    }

    #[test]
    fn delete_removes_one_credential_while_preserving_others() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" }
            }),
        );
        let storage = AuthStorage::create(Some(&path));
        write_auth_json(
            &path,
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" },
                "google": { "type": "api_key", "key": "external-key" }
            }),
        );
        storage.delete("anthropic").unwrap();
        assert_eq!(
            storage.list().unwrap(),
            vec![
                CredentialInfo {
                    provider_id: "openai".into(),
                    credential_type: AuthType::ApiKey,
                },
                CredentialInfo {
                    provider_id: "google".into(),
                    credential_type: AuthType::ApiKey,
                },
            ]
        );
        assert_eq!(storage.read("anthropic").unwrap(), None);
        assert_eq!(storage.read("openai").unwrap(), Some(api_key("openai-key")));
        assert_eq!(
            storage.read("google").unwrap(),
            Some(api_key("external-key"))
        );
    }

    #[test]
    fn in_memory_storage_implements_the_same_credential_store_behavior() {
        let storage = AuthStorage::in_memory(data_map(vec![("anthropic", api_key("initial"))]));
        assert_eq!(storage.read("anthropic").unwrap(), Some(api_key("initial")));

        let mut set = |_c: Option<Credential>| Ok(Some(api_key("updated")));
        storage.modify("anthropic", &mut set).unwrap();
        assert_eq!(storage.read("anthropic").unwrap(), Some(api_key("updated")));

        storage.delete("anthropic").unwrap();
        assert_eq!(storage.list().unwrap(), vec![]);
    }

    #[test]
    fn does_not_write_after_lock_acquisition_failure_and_recovers_on_retry() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        let storage = AuthStorage::create(Some(&path));

        // Hold the sentinel lock so every acquisition attempt fails, mirroring
        // pi's mocked one-shot `lockfile.lock` rejection.
        let lock_path = format!("{path}.lock");
        fs::write(&lock_path, "").unwrap();

        let mut set = |_c: Option<Credential>| Ok(Some(api_key("new")));
        let err = storage.modify("openai", &mut set).unwrap_err();
        assert!(matches!(err, ModifyError::Store(_)));
        assert_eq!(
            read_json(&path),
            json!({ "anthropic": { "type": "api_key", "key": "stored" } })
        );

        // Release the lock and retry: the write now lands.
        fs::remove_file(&lock_path).unwrap();
        let mut set = |_c: Option<Credential>| Ok(Some(api_key("new")));
        storage.modify("openai", &mut set).unwrap();
        assert_eq!(
            read_json(&path),
            json!({
                "anthropic": { "type": "api_key", "key": "stored" },
                "openai": { "type": "api_key", "key": "new" }
            })
        );
    }

    #[test]
    fn does_not_overwrite_malformed_auth_files() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        let storage = AuthStorage::create(Some(&path));
        fs::write(&path, "{invalid-json").unwrap();

        let mut set = |_c: Option<Credential>| Ok(Some(api_key("new")));
        let err = storage.modify("openai", &mut set).unwrap_err();
        assert!(matches!(err, ModifyError::Store(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{invalid-json");
    }

    #[test]
    fn read_stored_credential_reads_without_resolving() {
        let (_dir, path) = temp_auth_path();
        write_auth_json(
            &path,
            json!({ "anthropic": { "type": "api_key", "key": "$UNRESOLVED_ENV_XYZ" } }),
        );
        // Reads the raw stored value verbatim, without env resolution.
        assert_eq!(
            read_stored_credential("anthropic", Some(&path)),
            Some(api_key("$UNRESOLVED_ENV_XYZ"))
        );
        assert_eq!(read_stored_credential("missing", Some(&path)), None);
        assert_eq!(
            read_stored_credential("anthropic", Some("/no/such/file.json")),
            None
        );
    }
}
