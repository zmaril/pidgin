//! A file-backed [`CredentialStore`] over pi's `auth.json`.
//!
//! pi's `radius.ts` reads a stored OAuth credential through
//! `readStoredCredential("radius")` from `@earendil-works/pi-coding-agent`
//! (`core/auth-storage.ts`). That helper does a one-off synchronous read of the
//! coding-agent's `auth.json` — a `Record<providerId, Credential>` JSON map —
//! and returns `data[providerId]`, swallowing any read/parse failure.
//!
//! atilla-ai owns the credential model: the [`CredentialStore`] trait plus the
//! [`Credential`] / [`OAuthCredential`] types (`crates/atilla-ai/src/auth`). It
//! ships an [`atilla_ai::auth::InMemoryCredentialStore`] but no file-backed store
//! that opens the on-disk `auth.json`. [`FileCredentialStore`] fills that gap by
//! implementing atilla-ai's existing trait, so radius (and any future caller)
//! resolves stored credentials through the same seam a test can swap for the
//! in-memory store.
//!
//! # On-disk shape
//!
//! The file is a JSON object keyed by provider id, each value a type-tagged
//! [`Credential`] (`{"type":"oauth",...}` / `{"type":"api_key",...}`), matching
//! pi's `AuthStorageData` and atilla-ai's `Credential` serde tag exactly.
//!
//! # Path
//!
//! pi's `readStoredCredential` defaults to `join(getAgentDir(), "auth.json")` —
//! the *coding-agent* agent directory (`~/.pi/agent`, overridable via
//! `PI_CODING_AGENT_DIR`), **not** the orchestrator directory. [`FileCredentialStore::default_agent`]
//! reproduces that resolution so radius reads the same file pi does.
//!
//! # Deviations
//!
//! pi's writing store (`AuthStorage`) serializes writes with `proper-lockfile`
//! for cross-process safety. This store's write path ([`CredentialStore::modify`]
//! / [`CredentialStore::delete`]) is a plain read-modify-write without an
//! inter-process lock — radius only ever *reads*, and the cross-process lock is
//! out of scope for this port. It is called out here so a future writer caller
//! knows to revisit it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use atilla_ai::auth::{
    Credential, CredentialInfo, CredentialStore, ModifyError, ModifyFn, StoreError,
};

/// Environment variable overriding the coding-agent directory (pi's
/// `ENV_AGENT_DIR`, `${APP_NAME}_CODING_AGENT_DIR`).
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

/// Name of the pi config directory (`CONFIG_DIR_NAME`).
const CONFIG_DIR_NAME: &str = ".pi";

/// The parsed contents of an `auth.json`: pi's `AuthStorageData`
/// (`Record<string, Credential>`).
type AuthStorageData = BTreeMap<String, Credential>;

/// A [`CredentialStore`] backed by an on-disk `auth.json` file.
///
/// Reads parse the whole file per call, mirroring pi's stateless
/// `readStoredCredential`. A missing file reads as "no credentials"; a malformed
/// file surfaces as a [`StoreError`] (radius maps that to "not configured", the
/// same observable behaviour as pi's swallow-and-`undefined`).
pub struct FileCredentialStore {
    auth_path: PathBuf,
}

impl FileCredentialStore {
    /// A store reading the given `auth.json` path.
    pub fn new(auth_path: impl Into<PathBuf>) -> Self {
        Self {
            auth_path: auth_path.into(),
        }
    }

    /// A store reading the coding-agent default path,
    /// `join(getAgentDir(), "auth.json")` (`~/.pi/agent/auth.json`, or under
    /// `PI_CODING_AGENT_DIR`). This is the exact file pi's radius reads.
    pub fn default_agent() -> Self {
        Self::new(Self::default_agent_auth_path())
    }

    /// The default `auth.json` path pi's `readStoredCredential` resolves.
    pub fn default_agent_auth_path() -> PathBuf {
        agent_dir().join("auth.json")
    }

    /// The path this store reads.
    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }

    /// Load and parse the file. A missing file is an empty map; an I/O or parse
    /// failure is a [`StoreError`].
    fn load(&self) -> Result<AuthStorageData, StoreError> {
        if !self.auth_path.exists() {
            return Ok(AuthStorageData::new());
        }
        let raw = fs::read_to_string(&self.auth_path)
            .map_err(|error| StoreError::new(format!("failed to read auth.json: {error}")))?;
        serde_json::from_str(&raw)
            .map_err(|error| StoreError::new(format!("failed to parse auth.json: {error}")))
    }

    /// Persist the map as 2-space-indented JSON (pi's
    /// `JSON.stringify(data, null, 2)`), creating the parent directory and
    /// tightening file permissions to `0600` on unix.
    fn store(&self, data: &AuthStorageData) -> Result<(), StoreError> {
        if let Some(parent) = self.auth_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                StoreError::new(format!("failed to create auth.json directory: {error}"))
            })?;
        }
        let serialized = serde_json::to_string_pretty(data)
            .map_err(|error| StoreError::new(format!("failed to serialize auth.json: {error}")))?;
        fs::write(&self.auth_path, serialized)
            .map_err(|error| StoreError::new(format!("failed to write auth.json: {error}")))?;
        set_owner_only_permissions(&self.auth_path);
        Ok(())
    }
}

impl CredentialStore for FileCredentialStore {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>, StoreError> {
        Ok(self.load()?.get(provider_id).cloned())
    }

    fn list(&self) -> Result<Vec<CredentialInfo>, StoreError> {
        Ok(self
            .load()?
            .into_iter()
            .map(|(provider_id, credential)| CredentialInfo {
                credential_type: credential.auth_type(),
                provider_id,
            })
            .collect())
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut ModifyFn,
    ) -> Result<Option<Credential>, ModifyError> {
        let mut data = self.load().map_err(ModifyError::Store)?;
        let current = data.get(provider_id).cloned();
        let next = f(current.clone()).map_err(ModifyError::Callback)?;
        match next {
            Some(next) => {
                data.insert(provider_id.to_string(), next.clone());
                self.store(&data).map_err(ModifyError::Store)?;
                Ok(Some(next))
            }
            None => Ok(current),
        }
    }

    fn delete(&self, provider_id: &str) -> Result<(), StoreError> {
        let mut data = self.load()?;
        data.remove(provider_id);
        self.store(&data)
    }
}

/// Resolve the coding-agent directory, mirroring pi's `getAgentDir`:
/// `PI_CODING_AGENT_DIR` if set, else `~/.pi/agent`.
fn agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var(ENV_AGENT_DIR)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(dir);
    }
    home_dir().join(CONFIG_DIR_NAME).join("agent")
}

/// Home directory (`os.homedir()`): `USERPROFILE` on Windows, `HOME` otherwise.
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key).map(PathBuf::from).unwrap_or_default()
}

/// Tighten `auth.json` to owner-only (`0600`) on unix, matching pi's
/// `chmodSync(authPath, 0o600)`. A best-effort no-op elsewhere.
#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use atilla_ai::auth::{AuthType, Credential};

    fn write_auth_json(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn reads_oauth_credential_from_sample_auth_json() {
        let (_dir, path) = write_auth_json(
            r#"{
              "radius": {
                "type": "oauth",
                "refresh": "refresh-tok",
                "access": "access-tok",
                "expires": 1700000000000
              },
              "anthropic": { "type": "api_key", "key": "sk-ant" }
            }"#,
        );
        let store = FileCredentialStore::new(&path);

        match store.read("radius").unwrap() {
            Some(Credential::OAuth(oauth)) => {
                assert_eq!(oauth.access, "access-tok");
                assert_eq!(oauth.refresh, "refresh-tok");
                assert_eq!(oauth.expires, 1_700_000_000_000);
            }
            other => panic!("expected radius oauth credential, got {other:?}"),
        }
        assert_eq!(
            store.read("anthropic").unwrap().map(|c| c.auth_type()),
            Some(AuthType::ApiKey)
        );
    }

    #[test]
    fn missing_provider_is_none() {
        let (_dir, path) = write_auth_json(r#"{ "anthropic": { "type": "api_key" } }"#);
        let store = FileCredentialStore::new(&path);
        assert_eq!(store.read("radius").unwrap(), None);
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("does-not-exist.json"));
        assert_eq!(store.read("radius").unwrap(), None);
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn malformed_file_is_a_store_error() {
        let (_dir, path) = write_auth_json("{ this is not json");
        let store = FileCredentialStore::new(&path);
        let error = store.read("radius").unwrap_err();
        assert!(error.message.contains("parse"));
    }

    #[test]
    fn list_reports_metadata_without_secrets() {
        let (_dir, path) = write_auth_json(
            r#"{
              "radius": { "type": "oauth", "refresh": "r", "access": "a", "expires": 1 },
              "openai": { "type": "api_key", "key": "sk-openai" }
            }"#,
        );
        let store = FileCredentialStore::new(&path);
        let mut list = store.list().unwrap();
        list.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].provider_id, "openai");
        assert_eq!(list[0].credential_type, AuthType::ApiKey);
        assert_eq!(list[1].provider_id, "radius");
        assert_eq!(list[1].credential_type, AuthType::Oauth);
    }

    #[test]
    fn modify_then_read_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("auth.json");
        let store = FileCredentialStore::new(&path);

        let mut set = |_current: Option<Credential>| {
            Ok(Some(Credential::OAuth(atilla_ai::auth::OAuthCredential {
                refresh: "r".into(),
                access: "written".into(),
                expires: 42,
                extra: serde_json::Map::new(),
            })))
        };
        store.modify("radius", &mut set).unwrap();

        // Reload from a fresh store to prove it hit disk.
        let reloaded = FileCredentialStore::new(&path);
        match reloaded.read("radius").unwrap() {
            Some(Credential::OAuth(oauth)) => assert_eq!(oauth.access, "written"),
            other => panic!("expected persisted oauth, got {other:?}"),
        }

        store.delete("radius").unwrap();
        assert_eq!(reloaded.read("radius").unwrap(), None);
    }

    #[test]
    fn default_agent_auth_path_honors_env_override() {
        // The env is process-global; this asserts the shape without disturbing
        // other tests (restored immediately).
        let saved = std::env::var(ENV_AGENT_DIR).ok();
        std::env::set_var(ENV_AGENT_DIR, "/custom/agent");
        assert_eq!(
            FileCredentialStore::default_agent_auth_path(),
            PathBuf::from("/custom/agent/auth.json")
        );
        match saved {
            Some(value) => std::env::set_var(ENV_AGENT_DIR, value),
            None => std::env::remove_var(ENV_AGENT_DIR),
        }
    }
}
