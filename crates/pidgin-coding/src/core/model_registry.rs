//! Synchronous registry facade over [`ModelRuntime`], exposed to extensions.
//!
//! Ported from pi's `core/model-registry.ts` at pinned commit `3da591ab`.
//! Coding-agent internals drive [`ModelRuntime`] directly; extensions get this
//! narrower, mostly-synchronous view.
//!
//! # Scope of this slice
//!
//! The credential-resolving facade methods — `getApiKeyAndHeaders`,
//! `getProviderAuth`, `getApiKeyForProvider`, and `isUsingOAuth` — delegate to
//! [`ModelRuntime`]'s `get_auth_for_*` / `check_auth`, which resolve the
//! env-key/override auth subset through the injected auth context. The
//! models.json-configured-key and OAuth read paths through the rich composed
//! handlers stay deferred inside pidgin-ai (see [`model_runtime`](super::model_runtime)),
//! so those resolve to the ambient/keyless result today. The
//! composition/registration/lookup and auth-*status* methods are fully delegated.

use std::collections::BTreeMap;
use std::sync::Arc;

use pidgin_ai::auth::{AuthResult, ModelsError, ProviderHeaders};
use pidgin_ai::providers::registry::RegistryProvider;
use pidgin_ai::Model;

use super::model_runtime::ModelRuntime;
use super::provider_composer::{AuthStatus, ComposeError, ProviderConfigInput};

// Re-exported to mirror pi's `model-registry.ts` public surface.
pub use super::provider_composer::clear_api_key_cache;

/// The resolved request auth an extension receives (pi's `ResolvedRequestAuth`).
///
/// Produced by the deferred `get_api_key_and_headers`; retained so the return
/// shape is stable once credential resolution lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRequestAuth {
    /// Auth resolved successfully.
    Ok {
        /// The resolved api key, if any.
        api_key: Option<String>,
        /// The resolved request headers, if any.
        headers: Option<std::collections::BTreeMap<String, String>>,
        /// Extra request-scoped environment, if any.
        env: Option<std::collections::BTreeMap<String, String>>,
    },
    /// Auth could not be resolved.
    Err {
        /// The failure reason.
        error: String,
    },
}

/// Synchronous compatibility facade exposed to extensions (pi's `ModelRegistry`).
///
/// Owns the [`ModelRuntime`]; mutating operations (`refresh`, provider
/// registration) take `&mut self`.
pub struct ModelRegistry {
    runtime: ModelRuntime,
}

impl ModelRegistry {
    /// Wrap a runtime (pi's `new ModelRegistry(runtime)`).
    pub fn new(runtime: ModelRuntime) -> Self {
        Self { runtime }
    }

    /// The wrapped runtime.
    pub fn runtime(&self) -> &ModelRuntime {
        &self.runtime
    }

    /// The wrapped runtime, mutably.
    pub fn runtime_mut(&mut self) -> &mut ModelRuntime {
        &mut self.runtime
    }

    /// Reload `models.json` (pi's `refresh` -> `runtime.reloadConfig`).
    pub fn refresh(&mut self) {
        self.runtime.reload_config();
    }

    /// The aggregated runtime error (pi's `getError`).
    pub fn get_error(&self) -> Option<String> {
        self.runtime.get_error()
    }

    /// Every known model (pi's `getAll`).
    pub fn get_all(&self) -> Vec<Model> {
        self.runtime.get_models(None)
    }

    /// The available-model snapshot (pi's `getAvailable`, `model-registry.ts:40`,
    /// which reads `runtime.getAvailableSnapshot()`).
    pub fn get_available(&self) -> Vec<Model> {
        self.runtime.get_available_snapshot().to_vec()
    }

    /// The resolved request auth for a model (pi's `getApiKeyAndHeaders`,
    /// `model-registry.ts:52-89`). On a resolved auth it returns the api key plus
    /// the non-null request headers; on an unconfigured provider it falls back to
    /// the compatibility request config (failing when `authHeader` is set); a
    /// resolution error becomes an [`Err`](ResolvedRequestAuth::Err), translating
    /// the `"authHeader requires a resolved API key"` throw into
    /// `No API key found for "<provider>"`.
    pub fn get_api_key_and_headers(&self, model: &Model) -> ResolvedRequestAuth {
        match self.runtime.get_auth_for_model(model, None) {
            Ok(Some(resolution)) => ResolvedRequestAuth::Ok {
                api_key: resolution.auth.api_key,
                headers: non_null_headers(resolution.auth.headers),
                env: resolution.env,
            },
            Ok(None) => {
                let compatibility = self.runtime.get_compatibility_request_config(model);
                if compatibility.auth_header {
                    return ResolvedRequestAuth::Err {
                        error: format!("No API key found for \"{}\"", model.provider),
                    };
                }
                ResolvedRequestAuth::Ok {
                    api_key: None,
                    headers: compatibility.headers,
                    env: None,
                }
            }
            Err(error) => {
                let message = error.cause.unwrap_or(error.message);
                let error = if message == "authHeader requires a resolved API key" {
                    format!("No API key found for \"{}\"", model.provider)
                } else {
                    message
                };
                ResolvedRequestAuth::Err { error }
            }
        }
    }

    /// A provider's resolved auth (pi's `getProviderAuth`, `model-registry.ts:101`):
    /// `runtime.getAuth(provider)`.
    pub fn get_provider_auth(&self, provider: &str) -> Result<Option<AuthResult>, ModelsError> {
        self.runtime.get_auth_for_provider(provider, None)
    }

    /// A provider's resolved api key (pi's `getApiKeyForProvider`,
    /// `model-registry.ts:104-110`): the resolved auth's api key, swallowing errors.
    pub fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        self.runtime
            .get_auth_for_provider(provider, None)
            .ok()
            .flatten()
            .and_then(|resolution| resolution.auth.api_key)
    }

    /// Whether a model's provider resolves to an OAuth credential (pi's
    /// `isUsingOAuth`, `model-registry.ts:112`).
    pub fn is_using_oauth(&self, model: &Model) -> bool {
        self.runtime.is_using_oauth(&model.provider)
    }

    /// A model by provider + id (pi's `find`).
    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.runtime.get_model(provider, model_id)
    }

    /// Whether a model's provider has configured auth (pi's `hasConfiguredAuth`).
    pub fn has_configured_auth(&self, model: &Model) -> bool {
        self.runtime.has_configured_auth(&model.provider)
    }

    /// A provider's configured-auth status (pi's `getProviderAuthStatus`).
    pub fn get_provider_auth_status(&self, provider: &str) -> AuthStatus {
        self.runtime.get_provider_auth_status(provider)
    }

    /// A provider by id (pi's `getProvider`).
    pub fn get_provider(&self, provider: &str) -> Option<&Arc<RegistryProvider>> {
        self.runtime.get_provider(provider)
    }

    /// A provider's display name, falling back to its id (pi's `getProviderDisplayName`).
    pub fn get_provider_display_name(&self, provider: &str) -> String {
        self.runtime
            .get_provider(provider)
            .map(|entry| entry.name().to_string())
            .unwrap_or_else(|| provider.to_string())
    }

    /// Register a native pi-ai provider (pi's `registerProvider(provider)`).
    pub fn register_native_provider(
        &mut self,
        provider: RegistryProvider,
    ) -> Result<(), ComposeError> {
        self.runtime.register_native_provider(provider)
    }

    /// Register an extension provider by name + config
    /// (pi's `registerProvider(name, config)`).
    pub fn register_provider(
        &mut self,
        provider_name: &str,
        config: ProviderConfigInput,
    ) -> Result<(), ComposeError> {
        self.runtime.register_provider(provider_name, config)
    }

    /// Unregister a provider (pi's `unregisterProvider`).
    pub fn unregister_provider(&mut self, provider_name: &str) {
        self.runtime.unregister_provider(provider_name);
    }

    /// The stored config for a registered provider (pi's `getRegisteredProviderConfig`).
    pub fn get_registered_provider_config(
        &self,
        provider_name: &str,
    ) -> Option<&ProviderConfigInput> {
        self.runtime.get_registered_provider_config(provider_name)
    }

    /// A registered native provider (pi's `getRegisteredNativeProvider`).
    pub fn get_registered_native_provider(
        &self,
        provider_name: &str,
    ) -> Option<&Arc<RegistryProvider>> {
        self.runtime.get_registered_native_provider(provider_name)
    }

    /// The registered provider ids (pi's `getRegisteredProviderIds`).
    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        self.runtime.get_registered_provider_ids()
    }
}

/// Drop the null-valued entries of a resolved [`ProviderHeaders`] into a plain
/// header map, pi's `Object.entries(...).filter(entry[1] !== null)`
/// (`model-registry.ts:60-66`). A present-but-empty header map still yields
/// `Some` (pi returns `{}` rather than `undefined`).
fn non_null_headers(headers: Option<ProviderHeaders>) -> Option<BTreeMap<String, String>> {
    headers.map(|headers| {
        headers
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect()
    })
}
