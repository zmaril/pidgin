//! The provider registry, ported from pi's `packages/ai/src/models.ts`
//! (pinned commit `3da591ab`).
//!
//! This mirrors pi's registry core: the [`RegistryProvider`] runtime unit (pi's
//! `Provider<TApi>`), [`create_provider`] (provider → dialect routing on
//! `model.api`, with the "no API implementation" stream error and dynamic
//! in-flight refresh), and the [`Models`] collection (pi's
//! `Models`/`MutableModels`: `createModels`, `setProvider` upsert-by-id,
//! `deleteProvider`, `clearProviders`, `getProviders`, `getModels`, `getModel`).
//!
//! # Scope of this slice
//!
//! pi's `Models` also resolves auth and applies it before dispatch
//! (`getAuth`/`applyAuth`) and refreshes OAuth credentials. That auth subsystem
//! (`auth/resolve.ts`, credential stores, provider-owned login flows) is not
//! ported here — [`ProviderAuth`] carries only the env-var metadata needed to
//! describe a provider. Streaming through [`Models`] therefore dispatches
//! directly to the provider without an auth-application step; the auth-resolving
//! `getAuth`/`getAvailable` read paths are deferred (see the crate's port notes).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::seams::provider::{AbortSignal, Provider as StreamBackend, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    StopReason, StreamOptions, Usage, UsageCost,
};

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
#[derive(Debug, Clone, Default)]
pub struct ProviderAuth {
    /// Human-readable credential name (pi's `apiKey.name`).
    pub name: String,
    /// API-key environment variables in precedence order (pi's `envApiKeyAuth`).
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
}

type FetchModels = Arc<dyn Fn(&RefreshContext) -> Vec<Model> + Send + Sync>;

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

/// A runtime collection of providers, pi's `Models`/`MutableModels`
/// (`models.ts:127`). Providers are keyed by unique id; `set_provider` upserts.
#[derive(Default)]
pub struct Models {
    providers: Vec<Arc<RegistryProvider>>,
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
            id: "mixed".to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![
                test_model("api-a", "model-a"),
                test_model("api-b", "model-b"),
            ],
            fetch_models: None,
            api: ApiRouting::ByApi(by_api),
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
            id: "mixed".to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![test_model("api-a", "model-a")],
            fetch_models: None,
            api: ApiRouting::ByApi(by_api),
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
            id: "dynamic".to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![],
            fetch_models: Some(Arc::new(move |_ctx| {
                *fetches_inner.lock().unwrap() += 1;
                vec![test_model("api-a", "listed")]
            })),
            api: ApiRouting::Unimplemented,
        });

        assert!(provider.get_models().is_empty());
        provider.refresh_models(&RefreshContext {
            allow_network: true,
            force: false,
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
        });
        assert_eq!(*fetches.lock().unwrap(), 2);
    }

    // Offline refresh does not fetch (pi models.ts:606 allowNetwork gate).
    #[test]
    fn dynamic_provider_offline_does_not_fetch() {
        let provider = create_provider(CreateProviderOptions {
            id: "dynamic".to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![],
            fetch_models: Some(Arc::new(|_ctx| vec![test_model("api-a", "listed")])),
            api: ApiRouting::Unimplemented,
        });
        assert!(!provider.refresh_models(&RefreshContext {
            allow_network: false,
            force: false,
        }));
        assert!(provider.get_models().is_empty());
    }

    // setProvider upserts by id (models.ts:230).
    #[test]
    fn set_provider_upserts_by_id() {
        let mut models = create_models();
        let make = |name: &str| {
            create_provider(CreateProviderOptions {
                id: "p".to_string(),
                name: Some(name.to_string()),
                base_url: None,
                headers: None,
                auth: ProviderAuth::default(),
                models: vec![],
                fetch_models: None,
                api: ApiRouting::Unimplemented,
            })
        };
        models.set_provider(make("first"));
        models.set_provider(make("second"));
        assert_eq!(models.get_providers().len(), 1);
        assert_eq!(models.get_provider("p").unwrap().name(), "second");

        models.delete_provider("p");
        assert!(models.get_provider("p").is_none());
    }
}
