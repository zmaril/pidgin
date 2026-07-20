// straitjacket-allow-file[:duplication] — `RuntimeCredentials` is a faithful
// overlay mirror of the wrapped `CredentialStore`: its `read`/`list`/`modify`/
// `delete` members repeat the trait's method shapes (and the `ApiKey`
// credential construction) by design, and the tests rebuild the same
// `AuthStorage::in_memory` + override fixture per case. The clone detector reads
// this parallel structure as duplication; it is deliberate.
//! Non-persistent runtime credential overlay, ported from pi's
//! `core/runtime-credentials.ts`.
//!
//! [`RuntimeCredentials`] wraps another [`CredentialStore`] and layers in-memory
//! api-key overrides on top of it. Overrides mask the wrapped store for
//! [`read`](RuntimeCredentials::read) / [`list`](RuntimeCredentials::list)
//! without ever being persisted, while [`modify`](RuntimeCredentials::modify)
//! delegates straight through. Used for session-scoped `--api-key` overrides
//! that must not touch `auth.json`.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pidgin_ai::auth::{
    ApiKeyCredential, Credential, CredentialInfo, CredentialStore, ModifyError, ModifyFn,
    StoreError,
};

/// A credential-store overlay for non-persistent runtime API keys
/// (`runtime-credentials.ts:4`).
pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Mutex<IndexMap<String, String>>,
}

impl RuntimeCredentials {
    /// Wrap `store` with an empty override overlay.
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            overrides: Mutex::new(IndexMap::new()),
        }
    }

    /// Set an in-memory api-key override for `provider_id`
    /// (`runtime-credentials.ts:12-14`).
    pub fn set_runtime_api_key(&self, provider_id: &str, api_key: &str) {
        self.overrides
            .lock()
            .expect("runtime credentials overrides mutex poisoned")
            .insert(provider_id.to_string(), api_key.to_string());
    }

    /// Remove the in-memory override for `provider_id`
    /// (`runtime-credentials.ts:16-18`).
    pub fn remove_runtime_api_key(&self, provider_id: &str) {
        self.overrides
            .lock()
            .expect("runtime credentials overrides mutex poisoned")
            .shift_remove(provider_id);
    }

    /// Whether an in-memory override exists for `provider_id`
    /// (`runtime-credentials.ts:20-22`).
    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        self.overrides
            .lock()
            .expect("runtime credentials overrides mutex poisoned")
            .contains_key(provider_id)
    }

    fn override_for(&self, provider_id: &str) -> Option<String> {
        self.overrides
            .lock()
            .expect("runtime credentials overrides mutex poisoned")
            .get(provider_id)
            .cloned()
    }
}

impl CredentialStore for RuntimeCredentials {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>, StoreError> {
        match self.override_for(provider_id) {
            Some(key) => Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some(key),
                env: None,
            }))),
            None => self.store.read(provider_id),
        }
    }

    fn list(&self) -> Result<Vec<CredentialInfo>, StoreError> {
        // Merge stored metadata with the overrides, overrides winning, in
        // first-seen order (stored entries first, then override-only ids).
        let mut entries: IndexMap<String, CredentialInfo> = IndexMap::new();
        for entry in self.store.list()? {
            entries.insert(entry.provider_id.clone(), entry);
        }
        for provider_id in self
            .overrides
            .lock()
            .expect("runtime credentials overrides mutex poisoned")
            .keys()
        {
            entries.insert(
                provider_id.clone(),
                CredentialInfo {
                    provider_id: provider_id.clone(),
                    credential_type: pidgin_ai::auth::AuthType::ApiKey,
                },
            );
        }
        Ok(entries.into_values().collect())
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut ModifyFn,
    ) -> Result<Option<Credential>, ModifyError> {
        self.store.modify(provider_id, f)
    }

    fn delete(&self, provider_id: &str) -> Result<(), StoreError> {
        self.remove_runtime_api_key(provider_id);
        self.store.delete(provider_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::auth::auth_storage::AuthStorage;
    use indexmap::IndexMap as Map;
    use pidgin_ai::auth::{AuthType, OAuthCredential};
    use serde_json::Map as JsonMap;

    fn api_key(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_string()),
            env: None,
        })
    }

    fn seed(pairs: Vec<(&str, Credential)>) -> Arc<AuthStorage> {
        let mut data: Map<String, Credential> = Map::new();
        for (provider, credential) in pairs {
            data.insert(provider.to_string(), credential);
        }
        Arc::new(AuthStorage::in_memory(data))
    }

    #[test]
    fn runtime_overrides_mask_stored_credentials_without_persisting() {
        let storage = seed(vec![("anthropic", api_key("stored-key"))]);
        let credentials = RuntimeCredentials::new(storage.clone());

        credentials.set_runtime_api_key("anthropic", "runtime-key");
        assert_eq!(
            credentials.read("anthropic").unwrap(),
            Some(api_key("runtime-key"))
        );
        assert_eq!(
            storage.read("anthropic").unwrap(),
            Some(api_key("stored-key"))
        );

        credentials.remove_runtime_api_key("anthropic");
        assert_eq!(
            credentials.read("anthropic").unwrap(),
            Some(api_key("stored-key"))
        );
    }

    #[test]
    fn enumeration_merges_overrides_without_exposing_keys() {
        let storage = seed(vec![(
            "anthropic",
            Credential::OAuth(OAuthCredential {
                access: "access".into(),
                refresh: "refresh".into(),
                expires: 60_000,
                extra: JsonMap::new(),
            }),
        )]);
        let credentials = RuntimeCredentials::new(storage);
        credentials.set_runtime_api_key("anthropic", "runtime-key");
        credentials.set_runtime_api_key("openai", "other-runtime-key");

        let list = credentials.list().unwrap();
        assert_eq!(
            list,
            vec![
                CredentialInfo {
                    provider_id: "anthropic".into(),
                    credential_type: AuthType::ApiKey,
                },
                CredentialInfo {
                    provider_id: "openai".into(),
                    credential_type: AuthType::ApiKey,
                },
            ]
        );
    }

    #[test]
    fn delete_clears_both_the_override_and_persisted_credential() {
        let storage = seed(vec![("anthropic", api_key("stored-key"))]);
        let credentials = RuntimeCredentials::new(storage);
        credentials.set_runtime_api_key("anthropic", "runtime-key");

        credentials.delete("anthropic").unwrap();

        assert_eq!(credentials.read("anthropic").unwrap(), None);
        assert_eq!(credentials.list().unwrap(), vec![]);
    }
}
