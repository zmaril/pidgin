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
//! `refreshModels` and persists results to the [`ModelsStore`]. The
//! credential-aware read/refresh half is wired against atilla-ai's landed [`Models`]
//! runtime methods, which resolve the **env-key / override auth subset** through
//! the injected auth context. The models.json-configured-key and OAuth read paths
//! through the rich composed `auth.apiKey`/`auth.oauth` handlers stay deferred
//! inside atilla-ai (its own residual deferral): the [`RegistryProvider`] the
//! runtime stores carries the pared-down [`ProviderAuth`] env-key descriptor, not
//! the rich composed handlers (see [`provider_composer`](super::provider_composer)).
//!
//! This port therefore covers:
//!
//! - config load + `reload_config`, radius-builtin reset;
//! - the provider-composition lifecycle: `builtins`/native/extension layering via
//!   the credential-blind [`compose_model_provider`] plus the AUTH-layer composer
//!   (`compose_rich_provider`) that composes the rich [`ProviderAuth`](atilla_ai::auth::ProviderAuth)
//!   handlers and delegates `filterModels`, the `recompose_provider` fast paths,
//!   and `rebuild_providers`;
//! - the model snapshot (`all` / `available`), where "available" is derived from
//!   the **synchronously determinable** configured-auth set: runtime overrides,
//!   stored credentials ([`CredentialStore::list`]), and
//!   [`configured_request_auth_status`] over `models.json` / extension config;
//! - the **env-based auth-check map** (`snapshot.auth`) from [`Models::check_auth`],
//!   backing [`ModelRuntime::is_using_oauth`] and the environment fallback of
//!   `get_provider_auth_status`;
//! - **auth resolution / availability**: `get_auth_for_provider` /
//!   `get_auth_for_model` / `check_auth` / `get_available` over the env-key subset;
//! - **live refresh**: `refresh` drives each refreshable provider's `refreshModels`
//!   via [`Models::refresh`] under the runtime's network policy;
//! - error aggregation (`get_error`): config error plus per-provider composition
//!   errors;
//! - the registration lifecycle (`register_provider` / `register_native_provider`
//!   / `unregister_provider`) and the registered-provider accessors;
//! - `get_provider_auth_status` and `get_compatibility_request_config`.
//!
//! The following are **deferred**:
//!
//! - the models.json-configured-key and OAuth **rich-handler read paths** (the
//!   auth values resolve to the ambient/keyless result; atilla-ai's own residual
//!   deferral, not re-portable here without modifying that crate);
//! - **streaming**: `stream`/`stream_simple`/`complete`/`complete_simple`,
//!   `prepare_request`, header transforms;
//! - **login/logout** and the credential-mutation refresh side effects;
//! - **catalog persistence**: `refresh` drives `refreshModels` but does not yet
//!   persist the fetched catalogs to the [`ModelsStore`] (the store is wired and
//!   held for that path);
//! - the per-config **parametrized radius provider** (pi's
//!   `radiusProvider({ id, name, gateway })`): atilla-ai's [`radius_provider`]
//!   is not parametrizable, so a `oauth: "radius"` config provider is composed
//!   as a config-only provider rather than rebuilt as a gateway provider.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use atilla_ai::auth::{
    env_api_key_auth, ApiKeyAuth, AuthCheck, AuthResolutionOverrides, AuthResult, AuthType,
    Credential, CredentialInfo, CredentialStore, DefaultAuthContext, ModelsError, ProviderHeaders,
};
use atilla_ai::compose_model_provider as compose_rich_provider;
use atilla_ai::providers::composer::{ComposeAuthError, ComposeModelProviderInput};
use atilla_ai::providers::registry::{
    create_provider, ApiRouting, CreateProviderOptions, FilterModels, Models, MutableModels,
    ProviderAuth, RefreshOptions, RegistryProvider,
};
use atilla_ai::providers::ConfigValueResolver;
use atilla_ai::seams::storage::SystemEnv;
use atilla_ai::{builtin_providers, Model};
use indexmap::{IndexMap, IndexSet};

use super::auth::auth_storage::AuthStorage;
use super::auth::runtime_credentials::RuntimeCredentials;
use super::model_config::ModelConfig;
use super::models_store::{FileModelsStore, InMemoryCodingAgentModelsStore, ModelsStore};
use super::provider_composer::{
    compose_model_provider, configured_request_auth_status, extension_auth_config,
    provider_auth_config, resolve_compatibility_request_config, resolve_configured_model_headers,
    validate_extension_provider, AuthSource, AuthStatus, CompatibilityRequestConfig, ComposeError,
    ComposedProvider, ConfigValueResolverAdapter, ProviderConfigInput,
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
    /// The env-based auth-check map, pi's `snapshot.auth` (`model-runtime.ts:104`),
    /// populated from [`Models::check_auth`]. Backs [`ModelRuntime::is_using_oauth`]
    /// and the environment fallback of [`ModelRuntime::get_provider_auth_status`].
    auth: BTreeMap<String, AuthCheck>,
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
    /// The config-value resolution seam threaded into the AUTH-layer composer
    /// (`compose_rich_provider`) so `$ENV` / `!command` / literal keys resolve
    /// through pi's ported resolver.
    resolver: Arc<dyn ConfigValueResolver>,
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
            models: Models::with_auth_context(Arc::new(DefaultAuthContext::new(SystemEnv::new()))),
            snapshot: ModelRuntimeSnapshot::default(),
            resolver: Arc::new(ConfigValueResolverAdapter),
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
        // The credential-blind half layers models and resolves identity; the
        // AUTH half (atilla-ai) then composes the rich `ProviderAuth` handlers.
        // Both halves surface composition errors into `composition_errors`, and
        // both fall back to the untouched base provider on failure — mirroring
        // pi's single `composeModelProvider` try/catch (`model-runtime.ts:212-219`).
        let composed = compose_model_provider(
            provider_id,
            base.as_deref(),
            &self.config,
            extension.as_ref(),
        )
        .map_err(|error| error.0)
        .and_then(|blind| {
            self.composed_into_provider(provider_id, blind, base.as_ref())
                .map_err(|error| error.0)
        });
        match composed {
            Ok(provider) => {
                self.models.set_provider(provider);
                self.composition_errors.shift_remove(provider_id);
            }
            Err(message) => {
                self.composition_errors
                    .insert(provider_id.to_string(), message);
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
        // Populate the env-based auth-check map (pi's `runAvailabilityRefresh`
        // `checks`, `model-runtime.ts:238-249`): each provider's side-effect-free
        // `checkAuth` over the injected auth context. This backs the environment
        // fallback of `get_provider_auth_status` and `is_using_oauth` without
        // changing the offline `configured_providers` set (the models.json-key
        // path stays resolved by `compute_configured_providers`).
        let mut auth: BTreeMap<String, AuthCheck> = BTreeMap::new();
        for provider in self.models.get_providers() {
            if let Ok(Some(check)) = self.models.check_auth(provider.id()) {
                auth.insert(provider.id().to_string(), check);
            }
        }
        self.snapshot = ModelRuntimeSnapshot {
            available,
            configured_providers: configured,
            stored_providers: stored,
            auth,
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

    /// Whether a provider resolves to an OAuth credential (pi's `isUsingOAuth`,
    /// `model-runtime.ts:360`): `snapshot.auth.get(id)?.type === "oauth"`.
    ///
    /// The env-key/ambient auth subset atilla-ai's [`Models::check_auth`] resolves
    /// only ever reports [`AuthType::ApiKey`]; the OAuth read path (stored-credential
    /// ownership) stays deferred with the rest of the OAuth surface, so this is
    /// `false` today. The comparison is kept verbatim so it lights up for free when
    /// that path lands.
    pub fn is_using_oauth(&self, provider_id: &str) -> bool {
        self.snapshot
            .auth
            .get(provider_id)
            .map(|check| check.check_type == AuthType::Oauth)
            .unwrap_or(false)
    }

    /// A provider's configured-auth status (pi's `getProviderAuthStatus`,
    /// `model-runtime.ts:416-426`). After the runtime / stored / models.json
    /// checks, the env-based `checkAuth` map is consulted last, marking a provider
    /// whose ambient env key resolves as `source: environment`.
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
        if let Some(check) = self.snapshot.auth.get(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Environment),
                label: check.source.clone(),
            };
        }
        AuthStatus {
            configured: false,
            source: None,
            label: None,
        }
    }

    /// Resolve provider-scoped auth by provider id, pi's `getAuth(providerId, ...)`
    /// overload (`model-runtime.ts:368`). Delegates to [`Models::get_auth_for_provider`],
    /// which resolves the env-key/override subset through the injected auth context.
    /// `Ok(None)` for an unknown or unconfigured provider.
    pub fn get_auth_for_provider(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        self.models.get_auth_for_provider(provider_id, overrides)
    }

    /// Resolve auth for a model, pi's `getAuth(model, ...)` overload
    /// (`model-runtime.ts:375-389`): resolve the owning provider's auth, then merge
    /// the model's configured request headers (`resolveConfiguredModelHeaders` over
    /// the resolved + override env) into the result. `Ok(None)` for an unknown or
    /// unconfigured provider.
    pub fn get_auth_for_model(
        &self,
        model: &Model,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(mut resolution) = self.models.get_auth_for_model(model, overrides)? else {
            return Ok(None);
        };
        // `{ ...(resolution.env ?? {}), ...(overrides.env ?? {}) }`.
        let mut env: HashMap<String, String> = HashMap::new();
        for (key, value) in resolution.env.iter().flatten() {
            env.insert(key.clone(), value.clone());
        }
        if let Some(overrides) = overrides {
            for (key, value) in overrides.env.iter().flatten() {
                env.insert(key.clone(), value.clone());
            }
        }
        let env = if env.is_empty() { None } else { Some(&env) };
        let configured = resolve_configured_model_headers(
            model,
            self.config.get_provider(&model.provider),
            self.extension_providers.get(&model.provider),
            env,
        )
        .map_err(|error| {
            ModelsError::auth(format!(
                "Failed to resolve configured headers for provider {}",
                model.provider
            ))
            .with_cause(error.0)
        })?;
        resolution.auth.headers = merge_model_headers(resolution.auth.headers.take(), configured);
        Ok(Some(resolution))
    }

    /// A provider's side-effect-free auth check, pi's `checkAuth`
    /// (`model-runtime.ts:303`). `Ok(None)` for an unknown or unconfigured provider.
    pub fn check_auth(&self, provider_id: &str) -> Result<Option<AuthCheck>, ModelsError> {
        self.models.check_auth(provider_id)
    }

    /// The available models, pi's `getAvailable` (`model-runtime.ts:307`). Without a
    /// provider id it returns the cached snapshot (the offline configured-auth set);
    /// scoped to a provider it resolves live through [`Models::get_available`], gated
    /// by the provider's env-key/ambient auth and narrowed by its `filterModels`.
    pub fn get_available(&self, provider_id: Option<&str>) -> Result<Vec<Model>, ModelsError> {
        match provider_id {
            Some(id) => self.models.get_available(Some(id)),
            None => Ok(self.snapshot.available.clone()),
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

    /// Live-refresh dynamic providers and recompute the snapshot (pi's `refresh`,
    /// `model-runtime.ts:513-531`). [`Models::refresh`] drives each refreshable
    /// provider's `refreshModels` with the resolved credential and the runtime's
    /// network policy; the snapshot recomputation then repopulates the env-based
    /// auth-check map. Persisting refreshed catalogs to the [`ModelsStore`] remains
    /// deferred (the store is wired and held for that path).
    pub fn refresh(&mut self) {
        self.models.refresh(&RefreshOptions {
            allow_network: self.allow_model_network,
            force: false,
        });
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

    /// Build a [`RegistryProvider`] from the credential-blind [`ComposedProvider`],
    /// composing the rich [`ProviderAuth`](atilla_ai::auth::ProviderAuth) handlers
    /// through the AUTH-layer composer (pi's `composeModelProvider` auth half,
    /// `provider-composer.ts:412-499`).
    ///
    /// The AUTH composer validates that at least one auth method is configured
    /// (throwing pi's verbatim `"Provider {id}: no authentication method
    /// configured."`) and, when the base provider declares one, delegates the
    /// credential-specific `filterModels` to it (`provider-composer.ts:493-495`).
    /// The [`RegistryProvider`] the runtime stores carries the pared-down
    /// [`ProviderAuth`](atilla_ai::providers::registry::ProviderAuth) env-key
    /// descriptor (the resolution path [`Models::get_auth_for_provider`] reads); the
    /// rich composed handlers gate configuration and would drive the eager stream,
    /// but the models.json-configured-key and OAuth read paths through them stay
    /// deferred inside atilla-ai (see the module docs).
    fn composed_into_provider(
        &self,
        provider_id: &str,
        composed: ComposedProvider,
        base: Option<&Arc<RegistryProvider>>,
    ) -> Result<RegistryProvider, ComposeAuthError> {
        let name = composed.name.clone();
        let base_url = composed.base_url.clone();
        let headers = composed.headers.clone();
        let models = composed.into_models();

        // pi's `base?.auth.apiKey`: the builtin's env-key handler, rebuilt from the
        // pared-down descriptor. `base?.auth.oauth` is absent (the RegistryProvider
        // carries no OAuth handler), so it is `None`.
        let base_api_key: Option<Box<dyn ApiKeyAuth>> = base.map(|base| {
            let auth = base.auth();
            let vars: Vec<&str> = auth.api_key_env_vars.iter().map(String::as_str).collect();
            Box::new(env_api_key_auth(auth.name.clone(), &vars)) as Box<dyn ApiKeyAuth>
        });

        let rich = compose_rich_provider(ComposeModelProviderInput {
            provider_id: provider_id.to_string(),
            base: base.cloned(),
            base_api_key,
            base_oauth: None,
            config: self
                .config
                .get_provider(provider_id)
                .map(provider_auth_config),
            extension: self
                .extension_providers
                .get(provider_id)
                .map(extension_auth_config),
            models,
            name: name.clone(),
            base_url: base_url.clone(),
            headers: headers.clone(),
            resolver: self.resolver.clone(),
        })?;

        let auth: ProviderAuth = base.map(|base| base.auth().clone()).unwrap_or_default();
        // pi's `filterModels: base?.filterModels ? (models, cred) =>
        // base.filterModels!(models, cred) : undefined` — delegate to the base
        // provider's filter when it declares one, else no filter.
        let filter_models: Option<FilterModels> =
            base.filter(|base| base.has_filter()).map(|base| {
                let base = base.clone();
                Arc::new(move |models: &[Model], credential: Option<&Credential>| {
                    base.filter_models(models.to_vec(), credential)
                }) as FilterModels
            });
        let mut provider = create_provider(CreateProviderOptions {
            id: rich.id.clone(),
            name: Some(name),
            base_url,
            headers,
            auth,
            models: rich.get_models().to_vec(),
            fetch_models: None,
            api: ApiRouting::Unimplemented,
        });
        if let Some(filter_models) = filter_models {
            provider = provider.with_filter_models(filter_models);
        }
        Ok(provider)
    }
}

/// Merge configured request headers onto a resolved [`ModelAuth`]'s headers, pi's
/// `mergeHeaders` (`models.ts:202`): the override wins, replacing any base entry
/// whose name matches case-insensitively while keeping the override's own casing.
/// `None`/`None` yields `None`.
fn merge_model_headers(
    base: Option<ProviderHeaders>,
    override_headers: Option<BTreeMap<String, String>>,
) -> Option<ProviderHeaders> {
    if base.is_none() && override_headers.is_none() {
        return None;
    }
    let mut merged = base.unwrap_or_default();
    if let Some(override_headers) = override_headers {
        for (name, value) in override_headers {
            let lower = name.to_lowercase();
            merged.retain(|existing, _| existing.to_lowercase() != lower);
            merged.insert(name, Some(value));
        }
    }
    Some(merged)
}

/// The default `models-store.json` path: alongside `models.json`
/// (pi's `join(dirname(modelsPath), "models-store.json")`).
fn default_models_store_path(models_path: &str) -> String {
    match Path::new(models_path).parent() {
        Some(dir) => dir.join("models-store.json").to_string_lossy().into_owned(),
        None => "models-store.json".to_string(),
    }
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
