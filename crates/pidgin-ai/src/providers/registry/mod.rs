// straitjacket-allow-file:duplication — the provider registry mirrors pi's `models.ts`; parallel structure with the ported dialects is intentional
//! The provider registry, ported from pi's `packages/ai/src/models.ts`
//! (pinned commit `3da591ab`).
//!
//! This mirrors pi's registry core: the [`RegistryProvider`] runtime unit (pi's
//! `Provider<TApi>`), [`create_provider`] (provider → dialect routing on
//! `model.api`, with the "no API implementation" stream error and dynamic
//! in-flight refresh), and the [`Models`] collection (pi's
//! `Models`/`MutableModels`: `createModels`, `setProvider` upsert-by-id,
//! `deleteProvider`, `clearProviders`, `getProviders`, `getModels`, `getModel`),
//! plus the streaming convenience surface (`stream`/`complete`/`stream_simple`/
//! `complete_simple`, pi's `ModelsImpl`): resolve the owning provider, apply
//! auth, and delegate to the provider's [`stream`](RegistryProvider::stream).
//!
//! # Scope of this slice
//!
//! [`Models::stream`] applies pi's `applyAuth` (`models.ts:463`) in full: it
//! resolves the credential through the injected [`AuthContext`], gates on the
//! configured/unconfigured outcome, and threads the resolved api key, merged
//! headers, per-credential `baseUrl`, and env into the request model / options
//! handed to the backend (a bound dialect — see
//! [`crate::providers::AnthropicMessagesBackend`] — then reads them). Two narrow
//! deviations remain, documented on [`Models::apply_auth`]: the ported
//! [`StreamOptions::headers`] carries plain values only (pi's suppressing `null`
//! is not representable) and pi's `transformHeaders` stream transform is not part
//! of the ported options. The OAuth-refreshing `getAuth`/`getAvailable` read
//! paths remain deferred (see the crate's port notes).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::auth::{AuthContext, AuthResolutionOverrides, Credential, DefaultAuthContext};
use crate::seams::provider::{AbortSignal, Provider as StreamBackend, StreamResult};
use crate::seams::storage::SystemEnv;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, Usage, UsageCost,
};
use crate::utils::sse::AssistantEventReader;

mod models_runtime;

pub use models_runtime::{RefreshOptions, RefreshResult};

/// A stream backend: pi's `ProviderStreams`. In Rust this is the model/streaming
/// seam [`crate::seams::provider::Provider`], shared behind an [`Arc`].
pub type StreamBackendRef = Arc<dyn StreamBackend>;

/// Provider-level headers, pi's `ProviderHeaders` (`Record<string, string | null>`).
/// A `None` value marks a header the provider explicitly clears downstream.
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

/// Auth metadata for a provider.
///
/// A pared-down stand-in for pi's `ProviderAuth`: it names the credential and
/// lists the environment variables (in precedence order) that can supply an API
/// key. The full resolve/login/oauth machinery is deferred for this slice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    /// Human-readable credential name (pi's `apiKey.name`).
    pub name: String,
    /// API-key environment variables in precedence order (pi's `envApiKeyAuth`).
    #[serde(default)]
    pub api_key_env_vars: Vec<String>,
}

impl ProviderAuth {
    /// An env-API-key auth descriptor, mirroring pi's `envApiKeyAuth(name, vars)`.
    pub fn env_api_key(name: impl Into<String>, vars: &[&str]) -> Self {
        Self {
            name: name.into(),
            api_key_env_vars: vars.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// How a provider routes a model to a stream backend, mirroring pi's
/// `api: ProviderStreams | Record<TApi, ProviderStreams>` normalization
/// (`models.ts:570`).
#[derive(Clone)]
pub enum ApiRouting {
    /// One backend streams every model (single-API providers, e.g. Anthropic).
    Single(StreamBackendRef),
    /// Backends keyed by `model.api` (mixed-API providers, e.g. OpenCode).
    ByApi(BTreeMap<String, StreamBackendRef>),
    /// No wired backend yet — every model yields the "no API implementation"
    /// stream error. Used by catalog-backed builtins whose HTTP clients have not
    /// been ported.
    Unimplemented,
}

impl ApiRouting {
    fn backend_for(&self, api: &str) -> Option<&StreamBackendRef> {
        match self {
            ApiRouting::Single(backend) => Some(backend),
            ApiRouting::ByApi(map) => map.get(api),
            ApiRouting::Unimplemented => None,
        }
    }
}

/// Context handed to a dynamic provider's model fetch, mirroring pi's
/// `RefreshModelsContext` (the ported subset).
#[derive(Debug, Clone)]
pub struct RefreshContext {
    /// False during offline/cache-only initialization.
    pub allow_network: bool,
    /// Bypass freshness checks and fetch immediately when the network is allowed.
    pub force: bool,
    /// The effective configured credential resolved by [`Models::refresh`] before
    /// the fetch (pi's `RefreshModelsContext.credential`, `models.ts:36`). `None`
    /// when a provider is refreshed outside the auth-resolving [`Models::refresh`]
    /// path (e.g. the provider-level tests). pi resolves this via
    /// `resolveRefreshCredential` (`models.ts:330`) and OAuth credentials are
    /// refreshed before network access; this port resolves the api-key/ambient
    /// credential (OAuth refresh-before-fetch is deferred with the rest of the
    /// OAuth read path).
    pub credential: Option<Credential>,
}

type FetchModels = Arc<dyn Fn(&RefreshContext) -> Vec<Model> + Send + Sync>;

/// A provider's optional credential-specific model filter, pi's
/// `Provider.filterModels` (`models.ts:111`). Given the full synchronous catalog
/// and the effective credential, it returns the subset a credential may use.
///
/// # Port deviation
///
/// pi carries `filterModels` as a field of `CreateProviderOptions`. To keep this
/// addition crate-local (adding a field to the public [`CreateProviderOptions`]
/// would break every exhaustive struct literal in downstream crates), the Rust
/// port attaches it post-construction via
/// [`RegistryProvider::with_filter_models`] instead. Purely a construction-shape
/// difference; the runtime behavior in [`Models::get_available`] is identical.
pub type FilterModels = Arc<dyn Fn(&[Model], Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// Options for [`create_provider`], mirroring pi's `CreateProviderOptions`.
pub struct CreateProviderOptions {
    /// Unique provider id.
    pub id: String,
    /// Display name. Defaults to `id` when empty.
    pub name: Option<String>,
    /// Provider base URL.
    pub base_url: Option<String>,
    /// Provider-level headers.
    pub headers: Option<ProviderHeaders>,
    /// Auth metadata.
    pub auth: ProviderAuth,
    /// Static baseline model list (empty for purely dynamic providers).
    pub models: Vec<Model>,
    /// Optional dynamic model fetch (makes the provider refreshable).
    pub fetch_models: Option<FetchModels>,
    /// Single backend, or a map keyed by `model.api`.
    pub api: ApiRouting,
}

/// A serializable snapshot of one provider: its identity, metadata, auth, and
/// last-known models. This is the JSON-persistable unit pi rebuilds custom
/// providers from (the `models.json` config that also flows through
/// [`create_provider`]). Backends ([`ApiRouting`]) and the dynamic-fetch hook are
/// runtime-only wiring and are not part of the snapshot; a provider rebuilt from
/// one routes via [`ApiRouting::Unimplemented`] until backends are re-attached.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    /// Unique provider id.
    pub id: String,
    /// Display name. Defaults to `id` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Provider base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Provider-level headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    /// Auth metadata.
    #[serde(default)]
    pub auth: ProviderAuth,
    /// Last-known models for this provider.
    #[serde(default)]
    pub models: Vec<Model>,
}

impl ProviderSnapshot {
    /// Rebuild a runtime [`RegistryProvider`] from this snapshot. Streaming uses
    /// the "no API implementation" path (backends are not carried by the
    /// snapshot); model listing, lookup, pricing, and metadata are fully restored.
    pub fn into_provider(self) -> RegistryProvider {
        create_provider(CreateProviderOptions {
            id: self.id,
            name: self.name,
            base_url: self.base_url,
            headers: self.headers,
            auth: self.auth,
            models: self.models,
            fetch_models: None,
            api: ApiRouting::Unimplemented,
        })
    }
}

/// The concrete runtime provider unit, pi's `Provider<TApi>` (`models.ts:75`).
///
/// Owns id/name/base metadata, auth, the model list (baseline plus any
/// dynamically fetched overlay), and stream dispatch by `model.api`.
pub struct RegistryProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    baseline_models: Vec<Model>,
    dynamic_models: Mutex<Vec<Model>>,
    fetch_models: Option<FetchModels>,
    filter_models: Option<FilterModels>,
    api: ApiRouting,
}

impl RegistryProvider {
    /// Provider id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Base URL, if any.
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// Provider-level headers, if any.
    pub fn headers(&self) -> Option<&ProviderHeaders> {
        self.headers.as_ref()
    }

    /// Auth metadata.
    pub fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    /// Whether this provider can fetch a dynamic model list (pi's presence of
    /// `refreshModels`).
    pub fn is_refreshable(&self) -> bool {
        self.fetch_models.is_some()
    }

    /// The current known models: the baseline list with the dynamic overlay
    /// merged in by id (pi's `currentModels`, `models.ts:561`). Never throws.
    pub fn get_models(&self) -> Vec<Model> {
        let dynamic = self.dynamic_models.lock().unwrap();
        if dynamic.is_empty() {
            return self.baseline_models.clone();
        }
        let mut merged = self.baseline_models.clone();
        for model in dynamic.iter() {
            if let Some(existing) = merged.iter_mut().find(|m| m.id == model.id) {
                *existing = model.clone();
            } else {
                merged.push(model.clone());
            }
        }
        merged
    }

    /// Restore/fetch the dynamic model overlay (pi's `refreshModels`,
    /// `models.ts:596`). Returns whether a network fetch happened.
    ///
    /// The ported streaming seam is synchronous, so pi's in-flight promise
    /// dedup (`inflightRefresh ??=`) has no async window to collapse against;
    /// each call that is allowed to fetch does so.
    pub fn refresh_models(&self, ctx: &RefreshContext) -> bool {
        let Some(fetch) = &self.fetch_models else {
            return false;
        };
        if !ctx.allow_network {
            return false;
        }
        let refreshed = fetch(ctx);
        *self.dynamic_models.lock().unwrap() = refreshed;
        true
    }

    /// Attach a credential-specific model filter, pi's `filterModels`
    /// (`models.ts:111`). Builder form: pi passes this through
    /// `createProvider`'s options, but the Rust port attaches it here so the
    /// public [`CreateProviderOptions`] stays unchanged for downstream callers
    /// (see [`FilterModels`]).
    pub fn with_filter_models(mut self, filter: FilterModels) -> Self {
        self.filter_models = Some(filter);
        self
    }

    /// Whether this provider declares a credential-specific model filter (pi's
    /// presence of `filterModels`).
    pub fn has_filter(&self) -> bool {
        self.filter_models.is_some()
    }

    /// Apply the provider's credential-specific model filter, pi's
    /// `provider.filterModels?.(models, credential) ?? models`
    /// (`models.ts:407`). A provider with no filter returns `models` unchanged.
    pub fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        match &self.filter_models {
            Some(filter) => filter(&models, credential),
            None => models,
        }
    }

    /// Stream a response for `model`, dispatching to the backend for `model.api`.
    /// A model whose api has no backend yields the "no API implementation"
    /// stream error (pi's `dispatch`, `models.ts:576`).
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        match self.api.backend_for(&model.api) {
            Some(backend) => backend.stream(model, context, options, signal),
            None => no_api_implementation(&self.id, model),
        }
    }

    /// The incremental counterpart to [`stream`](Self::stream): dispatch to the
    /// backend for `model.api` via its
    /// [`stream_incremental`](StreamBackend::stream_incremental) and return the
    /// pull reader. A model whose api has no backend yields the "no API
    /// implementation" error, replayed through
    /// [`AssistantEventReader::from_buffered`].
    pub fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        match self.api.backend_for(&model.api) {
            Some(backend) => backend.stream_incremental(model, context, options, signal),
            None => AssistantEventReader::from_buffered(no_api_implementation(&self.id, model)),
        }
    }

    /// The simple, level-based counterpart to [`stream`](Self::stream): dispatch
    /// to the backend for `model.api` via its
    /// [`stream_simple`](StreamBackend::stream_simple), carrying the requested
    /// `reasoning`/`thinking_budgets`. A model whose api has no backend yields the
    /// "no API implementation" stream error.
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        match self.api.backend_for(&model.api) {
            Some(backend) => backend.stream_simple(model, context, options, signal),
            None => no_api_implementation(&self.id, model),
        }
    }
}

/// Build a [`RegistryProvider`] from parts, mirroring pi's `createProvider`
/// (`models.ts:556`).
pub fn create_provider(options: CreateProviderOptions) -> RegistryProvider {
    let name = match options.name {
        Some(name) if !name.is_empty() => name,
        _ => options.id.clone(),
    };
    RegistryProvider {
        id: options.id,
        name,
        base_url: options.base_url,
        headers: options.headers,
        auth: options.auth,
        baseline_models: options.models,
        dynamic_models: Mutex::new(Vec::new()),
        fetch_models: options.fetch_models,
        filter_models: None,
        api: options.api,
    }
}

/// The stream error pi raises for a model whose api has no backend
/// (`models.ts:583`): `Provider {id} has no API implementation for "{api}"`.
fn no_api_implementation(provider_id: &str, model: &Model) -> StreamResult {
    let message = format!(
        "Provider {provider_id} has no API implementation for \"{}\"",
        model.api
    );
    error_result(model, message)
}

fn error_result(model: &Model, message: String) -> StreamResult {
    let error = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp: 0,
    };
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error.clone(),
        }],
        message: error,
    }
}

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

/// Merge the resolved auth headers with the caller's request headers for
/// dispatch, pi's `mergeHeaders(auth.headers, options?.headers)`
/// (`models.ts:483`). The caller's plain-valued headers lift to the provider
/// `ProviderHeaders` overlay, merge (override wins, case-insensitive), then
/// collapse back to plain values — a `null`/`None` entry (a suppressed provider
/// default header) is dropped, since the ported [`StreamOptions::headers`] cannot
/// carry it (see [`Models::apply_auth`]).
fn merge_request_headers(
    auth_headers: Option<&ProviderHeaders>,
    option_headers: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let overlay: Option<ProviderHeaders> = option_headers.map(|headers| {
        headers
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone())))
            .collect()
    });
    let merged = models_runtime::merge_headers(auth_headers, overlay.as_ref())?;
    let collapsed: BTreeMap<String, String> = merged
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
    Some(collapsed)
}

/// Merge the resolved auth env with the caller's request env for dispatch, pi's
/// `env = resolution.env || options?.env ? { ...resolution.env, ...options.env }
/// : undefined` (`models.ts:485`). The caller's values win.
fn merge_request_env(
    auth_env: Option<&BTreeMap<String, String>>,
    option_env: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    if auth_env.is_none() && option_env.is_none() {
        return None;
    }
    let mut merged = auth_env.cloned().unwrap_or_default();
    if let Some(option_env) = option_env {
        for (name, value) in option_env {
            merged.insert(name.clone(), value.clone());
        }
    }
    Some(merged)
}

/// A runtime collection of providers, pi's `Models`/`MutableModels`
/// (`models.ts:127`). Providers are keyed by unique id; `set_provider` upserts.
///
/// Beyond the provider-registry read/mutate half, this owns the streaming
/// convenience surface (`stream`/`complete`/`stream_simple`/`complete_simple`,
/// pi's `ModelsImpl`, `models.ts:492-525`): it resolves the provider that owns a
/// model, applies auth, and delegates to the provider's
/// [`stream`](RegistryProvider::stream). The auth-application step is gated
/// through the injected [`AuthContext`], the same seam pi's `ModelsImpl` reaches
/// through `getAuth`/`applyAuth`.
#[derive(Clone)]
pub struct Models {
    providers: Vec<Arc<RegistryProvider>>,
    /// Environment access for provider auth resolution, pi's `ModelsImpl`
    /// `authContext` (`models.ts:222`). Defaults to the process environment; a
    /// test injects an in-memory context.
    auth_context: Arc<dyn AuthContext + Send + Sync>,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            auth_context: Arc::new(DefaultAuthContext::new(SystemEnv::new())),
        }
    }
}

/// The mutating half of pi's `MutableModels` (`models.ts:189`): upsert/replace,
/// remove, and clear providers. Kept as a trait so the read-only [`Models`]
/// surface and the mutators stay distinct, mirroring pi's `Models` vs
/// `MutableModels` split.
pub trait MutableModels {
    /// Upsert a provider by id (pi's `setProvider`). Replaces any existing
    /// provider with the same id, preserving its position.
    fn set_provider(&mut self, provider: RegistryProvider);
    /// [`set_provider`](MutableModels::set_provider) for an already-shared provider.
    fn set_provider_arc(&mut self, provider: Arc<RegistryProvider>);
    /// Remove a provider by id (pi's `deleteProvider`).
    fn delete_provider(&mut self, id: &str);
    /// Remove every provider (pi's `clearProviders`).
    fn clear_providers(&mut self);
}

impl MutableModels for Models {
    fn set_provider(&mut self, provider: RegistryProvider) {
        self.set_provider_arc(Arc::new(provider));
    }

    fn set_provider_arc(&mut self, provider: Arc<RegistryProvider>) {
        if let Some(slot) = self.providers.iter_mut().find(|p| p.id() == provider.id()) {
            *slot = provider;
        } else {
            self.providers.push(provider);
        }
    }

    fn delete_provider(&mut self, id: &str) {
        self.providers.retain(|p| p.id() != id);
    }

    fn clear_providers(&mut self) {
        self.providers.clear();
    }
}

impl Models {
    /// An empty collection (pi's `createModels`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a populated collection from provider snapshots, the JSON-snapshot
    /// counterpart to [`create_models`] + [`crate::builtin_providers`]. Mirrors
    /// pi's `createModels` followed by `setProvider` over each `models.json`
    /// custom provider: every snapshot is upserted by id (later snapshots win),
    /// so a `Models` can be reconstructed from a serialized form as well as from
    /// the builtins path.
    pub fn from_providers(providers: impl IntoIterator<Item = ProviderSnapshot>) -> Self {
        let mut models = Models::new();
        for snapshot in providers {
            models.set_provider(snapshot.into_provider());
        }
        models
    }

    /// All providers, in registration order (pi's `getProviders`).
    pub fn get_providers(&self) -> &[Arc<RegistryProvider>] {
        &self.providers
    }

    /// A provider by id (pi's `getProvider`).
    pub fn get_provider(&self, id: &str) -> Option<&Arc<RegistryProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    /// Last-known models from one provider, or every provider when `provider`
    /// is `None` (pi's `getModels`). Best-effort: unknown providers yield none.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        match provider {
            Some(id) => self
                .get_provider(id)
                .map(|p| p.get_models())
                .unwrap_or_default(),
            None => self.providers.iter().flat_map(|p| p.get_models()).collect(),
        }
    }

    /// Runtime model lookup against last-known lists (pi's `getModel`).
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == id)
    }

    /// Build a collection with an injected auth context, pi's
    /// `createModels({ authContext })` (`models.ts:527`). Providers are added
    /// afterwards via [`set_provider`](MutableModels::set_provider).
    pub fn with_auth_context(auth_context: Arc<dyn AuthContext + Send + Sync>) -> Self {
        Self {
            providers: Vec::new(),
            auth_context,
        }
    }

    /// Resolve the provider that owns `model`, pi's `requireProvider`
    /// (`models.ts:455`). `Err` carries pi's `Unknown provider: {id}` message.
    fn require_provider(&self, model: &Model) -> Result<&Arc<RegistryProvider>, String> {
        self.get_provider(&model.provider)
            .ok_or_else(|| format!("Unknown provider: {}", model.provider))
    }

    /// Resolve provider auth and build the request model + options ahead of
    /// dispatch, mirroring pi's `applyAuth` (`models.ts:463`).
    ///
    /// pi resolves the credential via `getAuth(model, { apiKey, env })`, throws
    /// `Provider is not configured: {id}` when nothing resolves, then threads the
    /// result into the request:
    ///
    /// ```text
    /// apiKey       = options?.apiKey ?? auth.apiKey
    /// headers      = mergeHeaders(auth.headers, options?.headers)
    /// env          = merge(resolution.env, options?.env)
    /// requestModel = auth.baseUrl ? { ...model, baseUrl: auth.baseUrl } : model
    /// requestOptions = { ...providerOptions, apiKey, headers, env }
    /// ```
    ///
    /// This port reproduces that end to end: the credential is resolved through
    /// the same [`get_auth_for_model`](Models::get_auth_for_model) path pi's
    /// `getAuth` reaches (so the ambient/keyless provider — pi's `ambientAuth` —
    /// is always configured, and a provider with env vars must have one set or a
    /// stored credential in the injected [`AuthContext`]). The resolved
    /// `{apiKey, headers, env}` are merged into a cloned [`StreamOptions`] and the
    /// per-credential `baseUrl` override is applied to a cloned [`Model`], both of
    /// which are handed to [`provider.stream`](RegistryProvider::stream).
    ///
    /// # Port deviations
    ///
    /// - The Rust [`StreamOptions::headers`] carries plain `string` values, while
    ///   pi's `ProviderHeaders` allows a `null` value to suppress a provider
    ///   default header. The merged result is collapsed to plain values by
    ///   dropping any `null` entry; the env-API-key / ambient auth path resolved
    ///   here never produces a suppressing `null`, so the collapse is lossless.
    /// - pi's `transformHeaders` (a `Models`-only stream transform) is not part
    ///   of the ported [`StreamOptions`] and is therefore not applied.
    fn apply_auth(
        &self,
        model: &Model,
        options: Option<&StreamOptions>,
    ) -> Result<(Model, StreamOptions), String> {
        // pi: getAuth(model, { apiKey: options?.apiKey, env: options?.env }).
        let overrides = AuthResolutionOverrides {
            api_key: options.and_then(|o| o.api_key.clone()),
            env: options.and_then(|o| o.env.clone()),
        };
        let resolution = match self.get_auth_for_model(model, Some(&overrides)) {
            Ok(Some(resolution)) => resolution,
            Ok(None) => return Err(format!("Provider is not configured: {}", model.provider)),
            Err(error) => {
                return Err(format!(
                    "Auth resolution failed for {}: {error}",
                    model.provider
                ))
            }
        };
        let auth = resolution.auth;

        // Explicit request options win per-field (pi: `options?.apiKey ?? auth.apiKey`).
        let api_key = options
            .and_then(|o| o.api_key.clone())
            .or_else(|| auth.api_key.clone());
        // mergeHeaders(auth.headers, options?.headers), then collapse to plain values.
        let headers = merge_request_headers(
            auth.headers.as_ref(),
            options.and_then(|o| o.headers.as_ref()),
        );
        // env = resolution.env || options?.env ? { ...resolution.env, ...options.env } : undefined.
        let env = merge_request_env(
            resolution.env.as_ref(),
            options.and_then(|o| o.env.as_ref()),
        );

        // requestOptions = { ...providerOptions, apiKey, headers, env }.
        let mut request_options = options.cloned().unwrap_or_default();
        request_options.api_key = api_key;
        request_options.headers = headers;
        request_options.env = env;

        // requestModel = auth.baseUrl ? { ...model, baseUrl: auth.baseUrl } : model.
        let request_model = match auth.base_url {
            Some(base_url) => {
                let mut request_model = model.clone();
                request_model.base_url = base_url;
                request_model
            }
            None => model.clone(),
        };

        Ok((request_model, request_options))
    }

    /// Stream a response for `model`, pi's `Models.stream` (`models.ts:492`):
    /// resolve the owning provider, apply auth, and delegate to the provider's
    /// [`stream`](RegistryProvider::stream). A pre-dispatch failure (unknown
    /// provider, unconfigured auth) becomes a single-`error` [`StreamResult`]
    /// rather than an `Err`, matching pi's `lazyStream` catch handler and the
    /// [`crate::api::anthropic`] driver's pre-start error path.
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let provider = match self.require_provider(model) {
            Ok(provider) => provider.clone(),
            Err(message) => return error_result(model, message),
        };
        let (request_model, request_options) = match self.apply_auth(model, options) {
            Ok(resolved) => resolved,
            Err(message) => return error_result(model, message),
        };
        provider.stream(&request_model, context, Some(&request_options), signal)
    }

    /// The incremental counterpart to [`stream`](Self::stream): resolve the
    /// owning provider and apply auth exactly as [`stream`](Self::stream) does,
    /// then delegate to the provider's
    /// [`stream_incremental`](RegistryProvider::stream_incremental) and return the
    /// pull reader. A pre-dispatch failure (unknown provider, unconfigured auth)
    /// becomes a single-`error` reader replayed through
    /// [`AssistantEventReader::from_buffered`], matching [`stream`](Self::stream)'s
    /// error shape byte for byte.
    ///
    /// This is the resolved entry point the future agent-loop wiring will call;
    /// it is additive and not yet on any hot path. The reader borrows `self`
    /// (through the resolved backend's transport), so a streaming backend yields
    /// per-frame timing while the buffered [`stream`](Self::stream) is untouched.
    pub fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        let provider = match self.require_provider(model) {
            Ok(provider) => provider,
            Err(message) => {
                return AssistantEventReader::from_buffered(error_result(model, message))
            }
        };
        let (request_model, request_options) = match self.apply_auth(model, options) {
            Ok(resolved) => resolved,
            Err(message) => {
                return AssistantEventReader::from_buffered(error_result(model, message))
            }
        };
        provider.stream_incremental(&request_model, context, Some(&request_options), signal)
    }

    /// Stream and resolve the final message, pi's `Models.complete`
    /// (`models.ts:504`): `this.stream(...).result()`.
    pub fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantMessage {
        self.stream(model, context, options, signal).message
    }

    /// Stream a response from the simple, level-based options, pi's
    /// `Models.streamSimple` (`models.ts:512`): resolve the owning provider, apply
    /// auth, and delegate to the provider's
    /// [`stream_simple`](RegistryProvider::stream_simple), carrying the requested
    /// `reasoning`/`thinking_budgets` so the drivers that consume them (Anthropic,
    /// Mistral) can lower reasoning onto the request. A pre-dispatch failure
    /// (unknown provider, unconfigured auth) becomes a single-`error`
    /// [`StreamResult`], matching [`stream`](Self::stream)'s error shape.
    ///
    /// Auth resolution runs against the base [`StreamOptions`] (auth carries no
    /// reasoning), and the resolved base is recombined with the caller's
    /// `reasoning`/`thinking_budgets` before dispatch, so the two-field simple
    /// distinction pi keeps survives the seam.
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let provider = match self.require_provider(model) {
            Ok(provider) => provider.clone(),
            Err(message) => return error_result(model, message),
        };
        let base = options.map(|o| &o.base);
        let (request_model, request_base) = match self.apply_auth(model, base) {
            Ok(resolved) => resolved,
            Err(message) => return error_result(model, message),
        };
        let request_options = SimpleStreamOptions {
            base: request_base,
            reasoning: options.and_then(|o| o.reasoning),
            thinking_budgets: options.and_then(|o| o.thinking_budgets),
        };
        provider.stream_simple(&request_model, context, Some(&request_options), signal)
    }

    /// Stream and resolve the final message, pi's `Models.completeSimple`
    /// (`models.ts:520`): `this.streamSimple(...).result()`.
    pub fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantMessage {
        self.stream_simple(model, context, options, signal).message
    }
}

/// An empty [`Models`] collection, pi's `createModels` (`models.ts:529`).
pub fn create_models() -> Models {
    Models::new()
}

/// The thinking levels in ascending order, pi's `EXTENDED_THINKING_LEVELS`
/// (`models.ts:661`).
const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// The thinking levels a model supports, pi's `getSupportedThinkingLevels`
/// (`models.ts:663`). A non-reasoning model supports only `off`. A level whose
/// `thinkingLevelMap` entry is `null` is unsupported; `xhigh`/`max` are supported
/// only when explicitly present in the map.
pub fn get_supported_thinking_levels<C>(model: &Model<C>) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    EXTENDED_THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| {
            let mapped = model.thinking_level_map.as_ref().and_then(|m| m.get(level));
            // `mapped === null` (present but disabled) → unsupported.
            if matches!(mapped, Some(None)) {
                return false;
            }
            if matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max) {
                // Supported only when explicitly present (`mapped !== undefined`).
                return mapped.is_some();
            }
            true
        })
        .collect()
}

/// Clamp a requested thinking level to the nearest supported one, pi's
/// `clampThinkingLevel` (`models.ts:674`): prefer the exact level, else the
/// next higher supported level, else the next lower, else the first supported.
pub fn clamp_thinking_level<C>(model: &Model<C>, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let Some(requested_index) = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level) else {
        return available
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
    };
    for candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

/// Whether two models are equal by id and provider, pi's `modelsAreEqual`
/// (`models.ts:699`). `None` on either side is not equal.
pub fn models_are_equal<C>(a: Option<&Model<C>>, b: Option<&Model<C>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::storage::MemoryEnv;
    use crate::types::{Modality, ModelCost};

    /// A backend that records the model ids it is asked to stream and returns a
    /// deterministic "ok" message — the Rust analog of the test's
    /// `recordingStreams` (providers.test.ts:272).
    struct RecordingBackend {
        label: String,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl StreamBackend for RecordingBackend {
        fn api(&self) -> &str {
            &self.label
        }

        fn stream(
            &self,
            model: &Model,
            _context: &Context,
            _options: Option<&StreamOptions>,
            _signal: Option<&AbortSignal>,
        ) -> StreamResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:{}", self.label, model.id));
            let message = AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: zero_usage(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            StreamResult {
                events: vec![AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: message.clone(),
                }],
                message,
            }
        }
    }

    fn recording(label: &str, calls: Arc<Mutex<Vec<String>>>) -> StreamBackendRef {
        Arc::new(RecordingBackend {
            label: label.to_string(),
            calls,
        })
    }

    fn test_model(api: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: api.to_string(),
            provider: "mixed".to_string(),
            base_url: "https://example.test/v1".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 10000,
            max_tokens: 1000,
            headers: None,
            compat: None,
        }
    }

    fn context() -> Context {
        Context::default()
    }

    /// Baseline [`CreateProviderOptions`] for tests; override the fields a case
    /// cares about via struct-update (`..default_opts(id)`).
    fn default_opts(id: &str) -> CreateProviderOptions {
        CreateProviderOptions {
            id: id.to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![],
            fetch_models: None,
            api: ApiRouting::Unimplemented,
        }
    }

    // providers.test.ts:300 — dispatches on model.api for mixed-API providers.
    // Adapted to dispatch at the provider level (pi routes through
    // `models.completeSimple`, which additionally applies auth — the auth
    // application step is deferred for this slice).
    #[test]
    fn dispatches_on_model_api() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut by_api = BTreeMap::new();
        by_api.insert("api-a".to_string(), recording("a", calls.clone()));
        by_api.insert("api-b".to_string(), recording("b", calls.clone()));
        let provider = create_provider(CreateProviderOptions {
            models: vec![
                test_model("api-a", "model-a"),
                test_model("api-b", "model-b"),
            ],
            api: ApiRouting::ByApi(by_api),
            ..default_opts("mixed")
        });

        provider.stream(&test_model("api-a", "model-a"), &context(), None, None);
        provider.stream(&test_model("api-b", "model-b"), &context(), None, None);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["a:model-a".to_string(), "b:model-b".to_string()]
        );
    }

    // providers.test.ts:357 — stream error for a model whose api has no backend.
    #[test]
    fn errors_for_model_with_no_implementation() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut by_api = BTreeMap::new();
        by_api.insert("api-a".to_string(), recording("a", calls));
        let provider = create_provider(CreateProviderOptions {
            models: vec![test_model("api-a", "model-a")],
            api: ApiRouting::ByApi(by_api),
            ..default_opts("mixed")
        });
        let result = provider.stream(&test_model("api-ghost", "model-x"), &context(), None, None);
        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("no API implementation"));
    }

    // providers.test.ts:369 — dynamic providers: empty until refreshed, and a
    // later refresh fetches again. The concurrent in-flight dedup assertion
    // (fetches === 1 for two overlapping refreshes) is deferred: the ported
    // seam is synchronous and has no async in-flight promise to collapse
    // against (pi models.ts:598 `inflightRefresh ??=`).
    #[test]
    fn dynamic_provider_empty_until_refreshed() {
        let fetches = Arc::new(Mutex::new(0u32));
        let fetches_inner = fetches.clone();
        let provider = create_provider(CreateProviderOptions {
            fetch_models: Some(Arc::new(move |_ctx| {
                *fetches_inner.lock().unwrap() += 1;
                vec![test_model("api-a", "listed")]
            })),
            ..default_opts("dynamic")
        });

        assert!(provider.get_models().is_empty());
        provider.refresh_models(&RefreshContext {
            allow_network: true,
            force: false,
            credential: None,
        });
        assert_eq!(*fetches.lock().unwrap(), 1);
        assert_eq!(
            provider
                .get_models()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec!["listed".to_string()]
        );

        provider.refresh_models(&RefreshContext {
            allow_network: true,
            force: false,
            credential: None,
        });
        assert_eq!(*fetches.lock().unwrap(), 2);
    }

    // Offline refresh does not fetch (pi models.ts:606 allowNetwork gate).
    #[test]
    fn dynamic_provider_offline_does_not_fetch() {
        let provider = create_provider(CreateProviderOptions {
            fetch_models: Some(Arc::new(|_ctx| vec![test_model("api-a", "listed")])),
            ..default_opts("dynamic")
        });
        assert!(!provider.refresh_models(&RefreshContext {
            allow_network: false,
            force: false,
            credential: None,
        }));
        assert!(provider.get_models().is_empty());
    }

    /// A minimal providers-JSON snapshot with one provider and one model whose
    /// `compat` blob must survive the round-trip. Shared by the snapshot-path
    /// tests so the JSON boilerplate lives in one place.
    fn snapshot_json() -> serde_json::Value {
        serde_json::json!([{
            "id": "acme",
            "name": "Acme",
            "baseUrl": "https://acme.test/v1",
            "auth": { "name": "Acme API key", "apiKeyEnvVars": ["ACME_API_KEY"] },
            "models": [{
                "id": "acme-1",
                "name": "Acme One",
                "api": "acme-api",
                "provider": "acme",
                "baseUrl": "https://acme.test/v1",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.0 },
                "contextWindow": 10000,
                "maxTokens": 1000,
                "compat": { "supportsTemperature": false }
            }]
        }])
    }

    // The napi ModelsCore snapshot path: deserialize a providers-JSON snapshot,
    // build a `Models` from it, and confirm getModel round-trips and the model's
    // compat blob survives re-serialization out (napi getModel serializes the
    // Model<Value> back to JSON).
    #[test]
    fn from_providers_snapshot_round_trips() {
        let snapshots: Vec<ProviderSnapshot> =
            serde_json::from_value(snapshot_json()).expect("snapshot deserializes");
        let models = Models::from_providers(snapshots);

        assert_eq!(models.get_providers().len(), 1);
        assert_eq!(models.get_models(Some("acme")).len(), 1);

        let model = models.get_model("acme", "acme-1").expect("acme-1 resolves");
        assert_eq!(model.provider, "acme");
        assert_eq!(model.api, "acme-api");

        // Serialize the resulting Model<Value> out and confirm the compat blob
        // survives verbatim.
        let out = serde_json::to_value(&model).expect("model serializes");
        assert_eq!(
            out["compat"]["supportsTemperature"],
            serde_json::json!(false)
        );

        // A model whose api has no wired backend streams the "no API
        // implementation" error (snapshot-rebuilt providers are Unimplemented).
        let result = models
            .get_provider("acme")
            .unwrap()
            .stream(&model, &context(), None, None);
        assert_eq!(result.message.stop_reason, StopReason::Error);
    }

    // modelsAreEqual over two deserialized Model<Value> — the napi
    // modelsAreEqual(a_json, b_json) shape: same id+provider are equal, a
    // differing id is not.
    #[test]
    fn models_are_equal_over_deserialized_models() {
        let model_json = snapshot_json()[0]["models"][0].clone();
        let a: Model = serde_json::from_value(model_json.clone()).expect("a deserializes");
        let b: Model = serde_json::from_value(model_json).expect("b deserializes");
        assert!(models_are_equal(Some(&a), Some(&b)));

        let mut other = b.clone();
        other.id = "acme-2".to_string();
        assert!(!models_are_equal(Some(&a), Some(&other)));
        assert!(!models_are_equal(Some(&a), None));
    }

    // setProvider upserts by id (models.ts:230).
    #[test]
    fn set_provider_upserts_by_id() {
        let mut models = create_models();
        let make = |name: &str| {
            create_provider(CreateProviderOptions {
                name: Some(name.to_string()),
                ..default_opts("p")
            })
        };
        models.set_provider(make("first"));
        models.set_provider(make("second"));
        assert_eq!(models.get_providers().len(), 1);
        assert_eq!(models.get_provider("p").unwrap().name(), "second");

        models.delete_provider("p");
        assert!(models.get_provider("p").is_none());
    }

    // -----------------------------------------------------------------------
    // Models streaming surface (pi's `models-runtime.test.ts` completeSimple /
    // streamSimple assertions, translated onto the eager Rust seam).
    // -----------------------------------------------------------------------

    /// A backend that emits pi's `start` + `done` pair, mirroring the test
    /// provider's `respond()` (models-runtime.test.ts:64).
    struct StartDoneBackend;

    impl StreamBackend for StartDoneBackend {
        fn api(&self) -> &str {
            "test-api"
        }

        fn stream(
            &self,
            model: &Model,
            _context: &Context,
            _options: Option<&StreamOptions>,
            _signal: Option<&AbortSignal>,
        ) -> StreamResult {
            let message = AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: zero_usage(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            StreamResult {
                events: vec![
                    AssistantMessageEvent::Start {
                        partial: message.clone(),
                    },
                    AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: message.clone(),
                    },
                ],
                message,
            }
        }
    }

    /// [`test_model`] with `provider` set so it resolves against a registered
    /// provider id (the base helper hardcodes `"mixed"`).
    fn model_for(provider: &str, api: &str, id: &str) -> Model {
        let mut model = test_model(api, id);
        model.provider = provider.to_string();
        model
    }

    // models-runtime.test.ts:675 — "streams through the provider": streamSimple
    // emits `start`+`done` and result() resolves a `stop` message. Adapted to the
    // eager seam: the events live on the returned StreamResult.
    #[test]
    fn stream_simple_emits_start_and_done() {
        let mut models = create_models();
        models.set_provider(create_provider(CreateProviderOptions {
            api: ApiRouting::Single(Arc::new(StartDoneBackend)),
            ..default_opts("p1")
        }));

        let result = models.stream_simple(
            &model_for("p1", "test-api", "model-a"),
            &context(),
            None,
            None,
        );
        assert_eq!(result.events.len(), 2);
        assert!(matches!(
            result.events[0],
            AssistantMessageEvent::Start { .. }
        ));
        assert!(matches!(
            result.events[1],
            AssistantMessageEvent::Done { .. }
        ));
        assert_eq!(result.message.stop_reason, StopReason::Stop);
    }

    // completeSimple resolves the streamed result to the final message and
    // reaches the provider that owns `model.provider`.
    #[test]
    fn complete_simple_returns_final_message() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut models = create_models();
        models.set_provider(create_provider(CreateProviderOptions {
            api: ApiRouting::Single(recording("p1", calls.clone())),
            ..default_opts("p1")
        }));

        let message = models.complete_simple(
            &model_for("p1", "test-api", "model-a"),
            &context(),
            None,
            None,
        );
        assert_eq!(message.stop_reason, StopReason::Stop);
        assert_eq!(*calls.lock().unwrap(), vec!["p1:model-a".to_string()]);
    }

    // models-runtime.test.ts:668 — "produces an error stream for unknown
    // providers instead of throwing".
    #[test]
    fn complete_simple_unknown_provider_streams_error() {
        let models = create_models();
        let result = models.complete_simple(
            &model_for("ghost", "test-api", "model-a"),
            &context(),
            None,
            None,
        );
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(result
            .error_message
            .as_deref()
            .unwrap()
            .contains("Unknown provider: ghost"));
    }

    // Auth is applied ahead of dispatch (pi's applyAuth gate, models.ts:463): a
    // provider that declares api-key env vars with none set is unconfigured and
    // never reaches the backend; setting the var lets the stream through.
    #[test]
    fn complete_simple_applies_auth_gate() {
        let model = model_for("p1", "test-api", "model-a");

        // Unconfigured: the declared env var is unset in the injected context.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut unconfigured =
            Models::with_auth_context(Arc::new(DefaultAuthContext::new(MemoryEnv::new())));
        unconfigured.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Test key", &["PIDGIN_TEST_KEY"]),
            api: ApiRouting::Single(recording("p1", calls.clone())),
            ..default_opts("p1")
        }));
        let error = unconfigured.complete_simple(&model, &context(), None, None);
        assert_eq!(error.stop_reason, StopReason::Error);
        assert!(error
            .error_message
            .as_deref()
            .unwrap()
            .contains("Provider is not configured: p1"));
        assert!(calls.lock().unwrap().is_empty());

        // Configured: the env var resolves through the auth context.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut configured = Models::with_auth_context(Arc::new(DefaultAuthContext::new(
            MemoryEnv::new().with_env("PIDGIN_TEST_KEY", "secret"),
        )));
        configured.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Test key", &["PIDGIN_TEST_KEY"]),
            api: ApiRouting::Single(recording("p1", calls.clone())),
            ..default_opts("p1")
        }));
        let ok = configured.complete_simple(&model, &context(), None, None);
        assert_eq!(ok.stop_reason, StopReason::Stop);
        assert_eq!(*calls.lock().unwrap(), vec!["p1:model-a".to_string()]);
    }

    // Provider resolution routes each request to the provider named by
    // `model.provider` (pi's requireProvider keys on `model.provider`).
    #[test]
    fn stream_resolves_provider_by_model_provider() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut models = create_models();
        models.set_provider(create_provider(CreateProviderOptions {
            api: ApiRouting::Single(recording("p1", calls.clone())),
            ..default_opts("p1")
        }));
        models.set_provider(create_provider(CreateProviderOptions {
            api: ApiRouting::Single(recording("p2", calls.clone())),
            ..default_opts("p2")
        }));

        models.complete_simple(&model_for("p2", "test-api", "m"), &context(), None, None);
        models.complete_simple(&model_for("p1", "test-api", "m"), &context(), None, None);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["p2:m".to_string(), "p1:m".to_string()]
        );
    }
}
