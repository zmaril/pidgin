// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `model-runtime.ts`. The provider-composition lifecycle (`recomposeProvider`
// fast paths, the `providerIds` union, register/unregister mirrors) and the
// snapshot recomputation repeat the same provider-keyed traversal shapes; the
// clone detector reads that deliberate parallel structure as duplication.
//! The stateful configured-model runtime.
//!
//! Ported from pi's `core/model-runtime.ts` at pinned commit `3da591ab`.
//! [`ModelRuntime`] loads `models.json` ([`ModelConfig`]), wires the credential
//! store ([`RuntimeCredentials`] over [`AuthStorage`]) and the catalog store
//! ([`ModelsStore`]), composes the built-in / config / extension provider layers
//! into a runtime [`Models`] collection, and tracks the "all models" and
//! "available models" snapshot. It exposes `reload_config`, `refresh`, the
//! provider-registration lifecycle, and error aggregation.
//!
//! # Scope of this slice
//!
//! pi's `ModelRuntime implements Models` — pi-ai's full runtime interface:
//! credential-aware `getAuth`/`checkAuth`/`getAvailable`, `login`/`logout`,
//! streaming (`stream`/`streamSimple`/`complete`/`completeSimple` via
//! `lazyStream`), and a network `refresh` that drives each provider's
//! `refreshModels` and persists results to the [`ModelsStore`]. atilla's landed
//! seams do not carry that surface — atilla-ai's [`Models`] collection is the
//! synchronous provider-view (`get_models`/`get_provider`/`set_provider`), with
//! no credential-aware availability, streaming completion, or async refresh, and
//! [`RegistryProvider`]'s auth is the pared-down [`ProviderAuth`] rather than
//! pi's composable `auth.apiKey`/`auth.oauth` handlers (see
//! [`provider_composer`](super::provider_composer)).
//!
//! This port therefore covers the credential-blind, synchronously computable
//! runtime surface:
//!
//! - config load + `reload_config`, radius-builtin reset;
//! - the provider-composition lifecycle: `builtins`/native/extension layering
//!   via [`compose_model_provider`], the `recompose_provider` fast paths, and
//!   `rebuild_providers`;
//! - the model snapshot (`all` / `available`), where "available" is derived
//!   from the **synchronously determinable** configured-auth set: runtime
//!   overrides, stored credentials ([`CredentialStore::list`]), and
//!   [`configured_request_auth_status`] over `models.json` / extension config;
//! - error aggregation (`get_error`): config error plus per-provider
//!   composition errors;
//! - the registration lifecycle (`register_provider` / `register_native_provider`
//!   / `unregister_provider`) and the registered-provider accessors;
//! - `get_provider_auth_status` and `get_compatibility_request_config`.
//!
//! The following are **deferred** (they require pi-ai's rich `Provider` /
//! streaming surface, not on atilla main — see the port report):
//!
//! - **auth resolution**: `get_auth`, `check_auth`, `get_available(provider)`
//!   (the async, credential-aware, per-provider scoped read), and the
//!   `snapshot.auth` env-based auth-check map that backs `is_using_oauth` and the
//!   environment fallback of `get_provider_auth_status`;
//! - **streaming**: `stream`/`stream_simple`/`complete`/`complete_simple`,
//!   `prepare_request`, header transforms;
//! - **login/logout** and the credential-mutation refresh side effects;
//! - **live refresh**: the network `refresh` that fetches each provider's
//!   `refreshModels` and persists to the [`ModelsStore`] (the store is wired and
//!   held for that future path; `refresh` here recomputes the snapshot only);
//! - the per-config **parametrized radius provider** (pi's
//!   `radiusProvider({ id, name, gateway })`): atilla-ai's [`radius_provider`]
//!   is not parametrizable, so a `oauth: "radius"` config provider is composed
//!   as a config-only provider rather than rebuilt as a gateway provider.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use atilla_ai::auth::{CredentialInfo, CredentialStore};
use atilla_ai::providers::registry::{
    create_provider, ApiRouting, CreateProviderOptions, Models, MutableModels, ProviderAuth,
    RegistryProvider,
};
use atilla_ai::{builtin_providers, create_models, Model};
use indexmap::{IndexMap, IndexSet};

use super::auth::auth_storage::AuthStorage;
use super::auth::runtime_credentials::RuntimeCredentials;
use super::model_config::ModelConfig;
use super::models_store::{FileModelsStore, InMemoryCodingAgentModelsStore, ModelsStore};
use super::provider_composer::{
    compose_model_provider, configured_request_auth_status, resolve_compatibility_request_config,
    validate_extension_provider, AuthSource, AuthStatus, CompatibilityRequestConfig, ComposeError,
    ComposedProvider, ProviderConfigInput,
};
use super::skills::get_agent_dir;

/// Where the runtime should look for `models.json` (pi's `modelsPath` sentinel:
/// `undefined` -> default path, `null` -> no file).
#[derive(Debug, Clone, Default)]
pub enum ModelsPath {
    /// The default `<agent_dir>/models.json`.
    #[default]
    Default,
    /// No config file (pi's `modelsPath: null`).
    Disabled,
    /// An explicit path.
    Path(String),
}

/// Options for [`ModelRuntime::create`], mirroring pi's
/// `CreateModelRuntimeOptions` (the ported subset).
#[derive(Default)]
pub struct CreateModelRuntimeOptions {
    /// Credential store. Defaults to [`AuthStorage`] over `auth_path`.
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// Path to `auth.json` when the default [`AuthStorage`] is used.
    pub auth_path: Option<String>,
    /// Where to load `models.json` from.
    pub models_path: ModelsPath,
    /// Catalog store. Defaults to a file store next to `models.json`, or an
    /// in-memory store when `models_path` is [`ModelsPath::Disabled`].
    pub models_store: Option<Box<dyn ModelsStore>>,
    /// Explicit `models-store.json` path (overrides the default location).
    pub models_store_path: Option<String>,
    /// Whether model network access is allowed. Defaults to `PI_OFFLINE` unset.
    pub allow_model_network: Option<bool>,
    /// The base built-in provider set. Defaults to [`builtin_providers`].
    pub builtins: Option<Vec<RegistryProvider>>,
}

/// The runtime's model snapshot: the available subset and the provider sets that
/// classify availability.
///
/// pi's snapshot also caches the full `all` model list; here `get_models`/
/// `get_available_snapshot` read the composed [`Models`] collection and the
/// `available` cache directly, so no separate `all` cache is kept.
#[derive(Default)]
struct ModelRuntimeSnapshot {
    available: Vec<Model>,
    configured_providers: BTreeSet<String>,
    stored_providers: BTreeSet<String>,
}

/// The configured pi-ai model collection used by coding-agent and SDK consumers
/// (pi's `ModelRuntime`).
pub struct ModelRuntime {
    credentials: RuntimeCredentials,
    models_store: Box<dyn ModelsStore>,
    default_builtins: IndexMap<String, Arc<RegistryProvider>>,
    builtins: IndexMap<String, Arc<RegistryProvider>>,
    native_extension_providers: IndexMap<String, Arc<RegistryProvider>>,
    extension_providers: IndexMap<String, ProviderConfigInput>,
    composition_errors: IndexMap<String, String>,
    models_path: Option<String>,
    allow_model_network: bool,
    config: ModelConfig,
    models: Models,
    snapshot: ModelRuntimeSnapshot,
}

impl ModelRuntime {
    /// Build and initialize a runtime (pi's `ModelRuntime.create`). The initial
    /// network availability refresh pi performs is deferred (see module docs);
    /// the snapshot is computed from the offline configured-auth set.
    pub fn create(options: CreateModelRuntimeOptions) -> ModelRuntime {
        let credentials = RuntimeCredentials::new(
            options
                .credentials
                .unwrap_or_else(|| Arc::new(AuthStorage::create(options.auth_path.as_deref()))),
        );

        let models_path: Option<String> = match options.models_path {
            ModelsPath::Disabled => None,
            ModelsPath::Path(path) => Some(path),
            ModelsPath::Default => Some(format!("{}/models.json", get_agent_dir())),
        };

        let config = ModelConfig::load(models_path.as_deref());

        let models_store: Box<dyn ModelsStore> =
            options.models_store.unwrap_or_else(|| match &models_path {
                Some(path) => {
                    let store_path = options
                        .models_store_path
                        .unwrap_or_else(|| default_models_store_path(path));
                    Box::new(FileModelsStore::new(&store_path))
                }
                None => Box::new(InMemoryCodingAgentModelsStore::new()),
            });

        let providers = options.builtins.unwrap_or_else(builtin_providers);
        let allow_model_network = options
            .allow_model_network
            .unwrap_or_else(|| std::env::var("PI_OFFLINE").is_err());

        let default_builtins: IndexMap<String, Arc<RegistryProvider>> = providers
            .into_iter()
            .map(|provider| (provider.id().to_string(), Arc::new(provider)))
            .collect();

        let mut runtime = ModelRuntime {
            credentials,
            models_store,
            builtins: default_builtins.clone(),
            default_builtins,
            native_extension_providers: IndexMap::new(),
            extension_providers: IndexMap::new(),
            composition_errors: IndexMap::new(),
            models_path,
            allow_model_network,
            config,
            models: create_models(),
            snapshot: ModelRuntimeSnapshot::default(),
        };
        runtime.configure_radius_providers();
        runtime.rebuild_providers();
        runtime
    }

    /// Reset `builtins` to the default set (pi's `configureRadiusProviders`).
    ///
    /// The per-config parametrized radius provider construction is deferred (see
    /// module docs); only the builtin reset is performed here.
    fn configure_radius_providers(&mut self) {
        self.builtins.clear();
        for (id, provider) in &self.default_builtins {
            self.builtins.insert(id.clone(), provider.clone());
        }
    }

    /// The union of every provider id from all layers, in first-seen order
    /// (pi's `providerIds`).
    fn provider_ids(&self) -> Vec<String> {
        let mut seen: IndexSet<String> = IndexSet::new();
        for id in self.builtins.keys() {
            seen.insert(id.clone());
        }
        for id in self.native_extension_providers.keys() {
            seen.insert(id.clone());
        }
        for id in self.config.get_provider_ids() {
            seen.insert(id.to_string());
        }
        for id in self.extension_providers.keys() {
            seen.insert(id.clone());
        }
        seen.into_iter().collect()
    }

    /// Recompose one provider from its layers (pi's `recomposeProvider`).
    fn recompose_provider(&mut self, provider_id: &str) {
        let base: Option<Arc<RegistryProvider>> = self
            .native_extension_providers
            .get(provider_id)
            .or_else(|| self.builtins.get(provider_id))
            .cloned();
        let has_config = self.config.get_provider(provider_id).is_some();
        let extension = self.extension_providers.get(provider_id).cloned();

        if !has_config && extension.is_none() {
            // No overlays: reuse the base provider untouched so its identity and
            // metadata are exact (pi keeps the builtin's auth/stream behavior); a
            // provider that exists in no layer is dropped.
            match base {
                Some(base) => self.models.set_provider_arc(base),
                None => self.models.delete_provider(provider_id),
            }
            self.composition_errors.shift_remove(provider_id);
            return;
        }
        match compose_model_provider(
            provider_id,
            base.as_deref(),
            &self.config,
            extension.as_ref(),
        ) {
            Ok(composed) => {
                let provider = composed_into_provider(composed, base.as_ref());
                self.models.set_provider(provider);
                self.composition_errors.shift_remove(provider_id);
            }
            Err(error) => {
                self.composition_errors
                    .insert(provider_id.to_string(), error.0);
                match base {
                    Some(base) => self.models.set_provider_arc(base),
                    None => self.models.delete_provider(provider_id),
                }
            }
        }
    }

    /// Recompose every provider from scratch (pi's `rebuildProviders`).
    fn rebuild_providers(&mut self) {
        self.models.clear_providers();
        self.composition_errors.clear();
        for provider_id in self.provider_ids() {
            self.recompose_provider(&provider_id);
        }
        self.update_snapshot();
    }

    /// The set of providers whose auth is configured through a synchronously
    /// determinable source: a runtime override, a stored credential, or a
    /// resolvable `models.json` / extension api key ([`configured_request_auth_status`]).
    fn compute_configured_providers(&self, stored: &BTreeSet<String>) -> BTreeSet<String> {
        let mut configured: BTreeSet<String> = BTreeSet::new();
        for provider_id in self.provider_ids() {
            if self.credentials.has_runtime_api_key(&provider_id) {
                configured.insert(provider_id);
                continue;
            }
            if stored.contains(&provider_id) {
                configured.insert(provider_id);
                continue;
            }
            let status = configured_request_auth_status(
                self.config.get_provider(&provider_id),
                self.extension_providers.get(&provider_id),
            );
            if status.map(|status| status.configured).unwrap_or(false) {
                configured.insert(provider_id);
            }
        }
        configured
    }

    /// Recompute the model snapshot (pi's `updateModelSnapshot` combined with the
    /// offline portion of `runAvailabilityRefresh`).
    fn update_snapshot(&mut self) {
        let stored: BTreeSet<String> = self
            .credentials
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|info| info.provider_id)
            .collect();
        let configured = self.compute_configured_providers(&stored);
        let available: Vec<Model> = self
            .models
            .get_models(None)
            .into_iter()
            .filter(|model| configured.contains(&model.provider))
            .collect();
        self.snapshot = ModelRuntimeSnapshot {
            available,
            configured_providers: configured,
            stored_providers: stored,
        };
    }

    /// All providers, in registration order (pi's `getProviders`).
    pub fn get_providers(&self) -> &[Arc<RegistryProvider>] {
        self.models.get_providers()
    }

    /// A provider by id (pi's `getProvider`).
    pub fn get_provider(&self, provider_id: &str) -> Option<&Arc<RegistryProvider>> {
        self.models.get_provider(provider_id)
    }

    /// The models from one provider, or every provider when `provider` is `None`
    /// (pi's `getModels`).
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        self.models.get_models(provider)
    }

    /// Runtime model lookup (pi's `getModel`).
    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.models.get_model(provider_id, model_id)
    }

    /// The last-computed available-model snapshot (pi's `getAvailableSnapshot`).
    pub fn get_available_snapshot(&self) -> &[Model] {
        &self.snapshot.available
    }

    /// Whether `provider_id`'s auth is configured (pi's `hasConfiguredAuth`).
    pub fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.snapshot.configured_providers.contains(provider_id)
    }

    /// Aggregate config + composition errors (pi's `getError`). The
    /// availability-refresh error strand is part of the deferred credential-aware
    /// path.
    pub fn get_error(&self) -> Option<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(config_error) = self.config.get_error() {
            errors.push(config_error.to_string());
        }
        for (provider_id, error) in &self.composition_errors {
            errors.push(format!("Provider \"{provider_id}\": {error}"));
        }
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n\n"))
        }
    }

    /// The stored config for an extension-registered provider
    /// (pi's `getRegisteredProviderConfig`).
    pub fn get_registered_provider_config(
        &self,
        provider_id: &str,
    ) -> Option<&ProviderConfigInput> {
        self.extension_providers.get(provider_id)
    }

    /// The registered extension + native provider ids
    /// (pi's `getRegisteredProviderIds`).
    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        let mut seen: IndexSet<String> = IndexSet::new();
        for id in self.extension_providers.keys() {
            seen.insert(id.clone());
        }
        for id in self.native_extension_providers.keys() {
            seen.insert(id.clone());
        }
        seen.into_iter().collect()
    }

    /// A registered native provider by id (pi's `getRegisteredNativeProvider`).
    pub fn get_registered_native_provider(
        &self,
        provider_id: &str,
    ) -> Option<&Arc<RegistryProvider>> {
        self.native_extension_providers.get(provider_id)
    }

    /// The compatibility request config for a model, pi's
    /// `getCompatibilityRequestConfig` (the credential-blind projection used by
    /// the registry facade's deferred `get_api_key_and_headers` fallback).
    pub fn get_compatibility_request_config(&self, model: &Model) -> CompatibilityRequestConfig {
        resolve_compatibility_request_config(
            model,
            self.config.get_provider(&model.provider),
            self.extension_providers.get(&model.provider),
        )
        .unwrap_or_default()
    }

    /// The credential list from the underlying store (pi's `listCredentials`).
    pub fn list_credentials(&self) -> Vec<CredentialInfo> {
        self.credentials.list().unwrap_or_default()
    }

    /// A provider's configured-auth status (pi's `getProviderAuthStatus`).
    ///
    /// The env-based `checkAuth` fallback pi consults last is deferred; a
    /// provider with no runtime/stored/config-resolvable key reads as
    /// unconfigured.
    pub fn get_provider_auth_status(&self, provider_id: &str) -> AuthStatus {
        if self.credentials.has_runtime_api_key(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Runtime),
                label: None,
            };
        }
        if self.snapshot.stored_providers.contains(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Stored),
                label: None,
            };
        }
        if let Some(status) = configured_request_auth_status(
            self.config.get_provider(provider_id),
            self.extension_providers.get(provider_id),
        ) {
            return status;
        }
        AuthStatus {
            configured: false,
            source: None,
            label: None,
        }
    }

    /// Set a non-persistent runtime api key and recompute the snapshot
    /// (pi's `setRuntimeApiKey`, minus the deferred network refresh).
    pub fn set_runtime_api_key(&mut self, provider_id: &str, api_key: &str) {
        self.credentials.set_runtime_api_key(provider_id, api_key);
        self.update_snapshot();
    }

    /// Remove a runtime api key and recompute the snapshot
    /// (pi's `removeRuntimeApiKey`, minus the deferred network refresh).
    pub fn remove_runtime_api_key(&mut self, provider_id: &str) {
        self.credentials.remove_runtime_api_key(provider_id);
        self.update_snapshot();
    }

    /// Reload `models.json` and recompose every provider (pi's `reloadConfig`).
    pub fn reload_config(&mut self) {
        self.config = ModelConfig::load(self.models_path.as_deref());
        self.configure_radius_providers();
        self.rebuild_providers();
    }

    /// Recompute the model + availability snapshot (pi's `refresh`, reduced to
    /// its offline snapshot recomputation; the network model refresh is deferred).
    pub fn refresh(&mut self) {
        self.update_snapshot();
    }

    /// Register (or replace) a native pi-ai provider (pi's `registerNativeProvider`).
    pub fn register_native_provider(
        &mut self,
        provider: RegistryProvider,
    ) -> Result<(), ComposeError> {
        if provider.id().trim().is_empty() {
            return Err(ComposeError("Provider id must not be empty.".to_string()));
        }
        let provider_id = provider.id().to_string();
        self.extension_providers.shift_remove(&provider_id);
        self.native_extension_providers
            .insert(provider_id.clone(), Arc::new(provider));
        self.recompose_provider(&provider_id);
        self.update_snapshot();
        Ok(())
    }

    /// Register (or merge into) an extension provider config (pi's `registerProvider`).
    ///
    /// The incoming config is validated on its own first: a broken
    /// re-registration throws without touching the stored config. Re-registration
    /// merges defined values over the previous registration.
    pub fn register_provider(
        &mut self,
        provider_id: &str,
        config: ProviderConfigInput,
    ) -> Result<(), ComposeError> {
        let base_models = self
            .builtins
            .get(provider_id)
            .map(|base| base.get_models())
            .unwrap_or_default();
        validate_extension_provider(
            provider_id,
            &base_models,
            self.config.get_provider(provider_id),
            &config,
        )?;
        self.native_extension_providers.shift_remove(provider_id);
        let previous = self.extension_providers.get(provider_id).cloned();
        let effective = merge_provider_config(previous, config);
        self.extension_providers
            .insert(provider_id.to_string(), effective);
        self.recompose_provider(provider_id);
        self.update_snapshot();
        Ok(())
    }

    /// Remove an extension / native provider (pi's `unregisterProvider`).
    pub fn unregister_provider(&mut self, provider_id: &str) {
        self.extension_providers.shift_remove(provider_id);
        self.native_extension_providers.shift_remove(provider_id);
        self.recompose_provider(provider_id);
        self.update_snapshot();
    }

    /// The catalog store backing the deferred live-refresh path.
    pub fn models_store(&self) -> &dyn ModelsStore {
        self.models_store.as_ref()
    }

    /// Whether model network access is allowed (drives the deferred live refresh).
    pub fn allow_model_network(&self) -> bool {
        self.allow_model_network
    }
}

/// The default `models-store.json` path: alongside `models.json`
/// (pi's `join(dirname(modelsPath), "models-store.json")`).
fn default_models_store_path(models_path: &str) -> String {
    match Path::new(models_path).parent() {
        Some(dir) => dir.join("models-store.json").to_string_lossy().into_owned(),
        None => "models-store.json".to_string(),
    }
}

/// Build a [`RegistryProvider`] from a [`ComposedProvider`], carrying the base
/// provider's [`ProviderAuth`] when present so env-var auth metadata survives
/// composition. Streaming routes via [`ApiRouting::Unimplemented`] (composed
/// providers carry no backend; streaming is deferred).
fn composed_into_provider(
    composed: ComposedProvider,
    base: Option<&Arc<RegistryProvider>>,
) -> RegistryProvider {
    let auth: ProviderAuth = base.map(|base| base.auth().clone()).unwrap_or_default();
    let id = composed.id.clone();
    let name = composed.name.clone();
    let base_url = composed.base_url.clone();
    let headers = composed.headers.clone();
    let models = composed.into_models();
    create_provider(CreateProviderOptions {
        id,
        name: Some(name),
        base_url,
        headers,
        auth,
        models,
        fetch_models: None,
        filter_models: None,
        api: ApiRouting::Unimplemented,
    })
}

/// Merge a re-registration over the previous extension config (pi's
/// "defined values over previous, preserve undefined"). Defined `Some` fields in
/// `config` win; otherwise the previous value is kept. The `stream_simple` /
/// `refresh_models` presence markers accumulate (a closure once present stays
/// until the registration is dropped).
fn merge_provider_config(
    previous: Option<ProviderConfigInput>,
    config: ProviderConfigInput,
) -> ProviderConfigInput {
    let Some(previous) = previous else {
        return config;
    };
    ProviderConfigInput {
        name: config.name.or(previous.name),
        base_url: config.base_url.or(previous.base_url),
        api_key: config.api_key.or(previous.api_key),
        api: config.api.or(previous.api),
        stream_simple: config.stream_simple || previous.stream_simple,
        headers: config.headers.or(previous.headers),
        auth_header: config.auth_header.or(previous.auth_header),
        oauth: config.oauth.or(previous.oauth),
        models: config.models.or(previous.models),
        refresh_models: config.refresh_models || previous.refresh_models,
    }
}

#[cfg(test)]
mod tests;
