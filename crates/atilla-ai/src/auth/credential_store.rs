// straitjacket-allow-file[:duplication] — the inline `#[cfg(test)] mod tests`
// rebuilds the same `modify`/`read` fixture closures per case (each exercises a
// distinct store path in isolation). The clone detector reads the repeated test
// setup as duplication; it is deliberate, load-bearing per-case fixtures.
//! Credential storage, ported from pi-ai's
//! `packages/ai/src/auth/credential-store.ts` (the `InMemoryCredentialStore`)
//! and the `CredentialStore` interface in `types.ts:60-88`, at pinned commit
//! `3da591ab`.
//!
//! `modify` is the only write path: every mutation is a serialized
//! read-modify-write, keyed per provider id. `resolve_stored_oauth` runs OAuth
//! refresh *inside* `modify` so concurrent requests cannot double-refresh a
//! rotated token (`types.ts:81-84`).
//!
//! # Sync port deviations
//!
//! pi serializes writes per provider through a promise chain
//! (`credential-store.ts:13-24`). This port is synchronous, so serialization is
//! a per-provider [`std::sync::Mutex`]: `modify`/`delete` take the provider's
//! lock for the whole read-modify-write. Because the lock is per provider (not a
//! single global lock), a slow OAuth refresh for one provider does not block
//! resolution for another — matching pi's per-id chains, and deliberately not
//! holding a global lock across the network refresh the `modify` closure runs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::error::ModelsError;
use super::types::{AuthType, Credential, CredentialInfo};

/// A storage-layer failure from a [`CredentialStore`] operation.
///
/// `resolve` re-wraps these as [`ModelsError::auth`] (`resolve.ts:105-138`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    /// The failure message.
    pub message: String,
}

impl StoreError {
    /// Construct a `StoreError` from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

/// The failure mode of a [`CredentialStore::modify`] call.
///
/// pi's `modify` rejects for two distinct reasons — a storage failure or a
/// rejection thrown by the mutation callback — and `resolve` treats them
/// differently: a callback `ModelsError` propagates unchanged, while a storage
/// failure is wrapped as `ModelsError("auth", "Credential store modify failed
/// ...")` (`resolve.ts:94-108`). This enum keeps the two paths distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyError {
    /// The backing store failed.
    Store(StoreError),
    /// The mutation callback rejected (carries the caller's [`ModelsError`]).
    Callback(ModelsError),
}

/// The mutation callback `modify` runs under the provider lock: it sees the
/// current credential and returns the new one (or `None` to leave it unchanged).
/// A returned `Err` is surfaced as [`ModifyError::Callback`].
pub type ModifyFn<'a> =
    dyn FnMut(Option<Credential>) -> Result<Option<Credential>, ModelsError> + 'a;

/// App-owned credential storage, keyed by `Provider.id`, one credential per
/// provider (`types.ts:60-88`).
///
/// `modify` is the only write path — the locked read-modify-write substrate for
/// token refresh.
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired. `Ok(None)` for a missing
    /// entry.
    fn read(&self, provider_id: &str) -> Result<Option<Credential>, StoreError>;

    /// List stored credential metadata without resolving or exposing secrets.
    fn list(&self) -> Result<Vec<CredentialInfo>, StoreError>;

    /// Serialized write — the only write path. `f` sees the current credential
    /// and returns the new one (or `None` to leave the entry unchanged).
    /// Resolves with the post-write credential; a callback rejection surfaces as
    /// [`ModifyError::Callback`].
    fn modify(
        &self,
        provider_id: &str,
        f: &mut ModifyFn,
    ) -> Result<Option<Credential>, ModifyError>;

    /// Remove a credential (logout). Serialized against `modify`.
    fn delete(&self, provider_id: &str) -> Result<(), StoreError>;
}

#[derive(Default)]
struct StoreState {
    credentials: BTreeMap<String, Credential>,
    /// Per-provider serialization locks (pi's per-id promise chains).
    locks: BTreeMap<String, Arc<Mutex<()>>>,
}

/// Default in-memory credential store (`credential-store.ts:8-51`).
///
/// Backed by a [`Mutex`]-guarded [`BTreeMap`] with per-provider serialization.
/// Apps inject persistent stores; this is the default.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    state: Mutex<StoreState>,
}

impl InMemoryCredentialStore {
    /// An empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch (creating if absent) the per-provider serialization lock.
    fn provider_lock(&self, provider_id: &str) -> Arc<Mutex<()>> {
        let mut state = self.state.lock().unwrap();
        state
            .locks
            .entry(provider_id.to_string())
            .or_default()
            .clone()
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .credentials
            .get(provider_id)
            .cloned())
    }

    fn list(&self) -> Result<Vec<CredentialInfo>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .credentials
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: match credential {
                    Credential::ApiKey(_) => AuthType::ApiKey,
                    Credential::OAuth(_) => AuthType::Oauth,
                },
            })
            .collect())
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut ModifyFn,
    ) -> Result<Option<Credential>, ModifyError> {
        // Serialize per provider, without holding the global map lock across the
        // (possibly network-bound) callback.
        let lock = self.provider_lock(provider_id);
        let _guard = lock.lock().unwrap();

        let current = self
            .state
            .lock()
            .unwrap()
            .credentials
            .get(provider_id)
            .cloned();

        let next = f(current.clone()).map_err(ModifyError::Callback)?;
        match next {
            Some(next) => {
                self.state
                    .lock()
                    .unwrap()
                    .credentials
                    .insert(provider_id.to_string(), next.clone());
                Ok(Some(next))
            }
            None => Ok(current),
        }
    }

    fn delete(&self, provider_id: &str) -> Result<(), StoreError> {
        let lock = self.provider_lock(provider_id);
        let _guard = lock.lock().unwrap();
        self.state.lock().unwrap().credentials.remove(provider_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential};
    use serde_json::Map;

    fn oauth(access: &str, expires: i64) -> Credential {
        Credential::OAuth(OAuthCredential {
            refresh: "r".into(),
            access: access.into(),
            expires,
            extra: Map::new(),
        })
    }

    #[test]
    fn read_missing_is_none_and_modify_persists() {
        let store = InMemoryCredentialStore::new();
        assert_eq!(store.read("anthropic").unwrap(), None);

        let mut set = |_current: Option<Credential>| Ok(Some(oauth("a1", 100)));
        let post = store.modify("anthropic", &mut set).unwrap();
        assert_eq!(post, Some(oauth("a1", 100)));
        assert_eq!(store.read("anthropic").unwrap(), Some(oauth("a1", 100)));
    }

    #[test]
    fn modify_returning_none_leaves_entry_unchanged() {
        let store = InMemoryCredentialStore::new();
        let mut set = |_c: Option<Credential>| Ok(Some(oauth("a1", 100)));
        store.modify("p", &mut set).unwrap();

        let mut noop = |_c: Option<Credential>| Ok(None);
        let post = store.modify("p", &mut noop).unwrap();
        // Returns the unchanged current credential.
        assert_eq!(post, Some(oauth("a1", 100)));
        assert_eq!(store.read("p").unwrap(), Some(oauth("a1", 100)));
    }

    #[test]
    fn modify_sees_current_credential() {
        let store = InMemoryCredentialStore::new();
        let mut set = |_c: Option<Credential>| Ok(Some(oauth("a1", 100)));
        store.modify("p", &mut set).unwrap();

        let mut seen = None;
        let mut inspect = |current: Option<Credential>| {
            seen = current.clone();
            Ok(None)
        };
        store.modify("p", &mut inspect).unwrap();
        assert_eq!(seen, Some(oauth("a1", 100)));
    }

    #[test]
    fn modify_callback_error_propagates_as_callback_variant() {
        let store = InMemoryCredentialStore::new();
        let mut fail = |_c: Option<Credential>| Err(ModelsError::oauth("refresh failed"));
        let err = store.modify("p", &mut fail).unwrap_err();
        match err {
            ModifyError::Callback(e) => assert_eq!(e, ModelsError::oauth("refresh failed")),
            ModifyError::Store(_) => panic!("expected callback error"),
        }
        // Nothing was persisted.
        assert_eq!(store.read("p").unwrap(), None);
    }

    #[test]
    fn list_reports_metadata_without_secrets() {
        let store = InMemoryCredentialStore::new();
        let mut oauth_set = |_c: Option<Credential>| Ok(Some(oauth("secret", 100)));
        store.modify("anthropic", &mut oauth_set).unwrap();
        let mut key_set = |_c: Option<Credential>| {
            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("sk-secret".into()),
                env: None,
            })))
        };
        store.modify("openai", &mut key_set).unwrap();

        let mut list = store.list().unwrap();
        list.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].provider_id, "anthropic");
        assert_eq!(list[0].credential_type, AuthType::Oauth);
        assert_eq!(list[1].provider_id, "openai");
        assert_eq!(list[1].credential_type, AuthType::ApiKey);
    }

    #[test]
    fn delete_removes_credential() {
        let store = InMemoryCredentialStore::new();
        let mut set = |_c: Option<Credential>| Ok(Some(oauth("a1", 100)));
        store.modify("p", &mut set).unwrap();
        store.delete("p").unwrap();
        assert_eq!(store.read("p").unwrap(), None);
    }
}
