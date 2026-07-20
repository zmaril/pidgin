//! The api-provider registry — the Rust port of pi-ai's `compat` entrypoint
//! (`packages/ai/src/compat.ts`).
//!
//! pi's `compat.ts` keeps a runtime `Map<Api, provider>` that api ids resolve
//! through: `registerApiProvider` populates it, `getApiProvider` reads it, and
//! `registerFauxProvider` registers pi's scripted test provider into it. The
//! coding-agent provider-composer calls `getApiProvider(model.api)` to obtain the
//! provider it streams a request through. This module is that registry.
//!
//! # Ported surface vs. deferred
//!
//! An [`ApiProvider`] wraps the [`Provider`] streaming seam (pi's
//! `ApiProvider.stream`/`streamSimple`). [`register_api_provider`],
//! [`get_api_provider`], [`get_api_providers`], [`unregister_api_providers`], and
//! [`reset_api_providers`] mirror pi's registry mutators 1:1.
//! [`register_faux_provider`] consumes the foundations faux provider
//! ([`crate::providers::faux::FauxProvider`]) — it is registered, not
//! reimplemented.
//!
//! pi's top-level `stream`/`complete` additionally short-circuit through the
//! builtin model catalog and inject env api keys before dispatch. Those two
//! branches depend on the builtin catalog (`builtinModels()`, the registry slice)
//! and `env-api-keys.ts`, neither ported yet; [`stream`]/[`complete`] here port
//! the registry-dispatch fallback (pi `compat.ts:340-347`) and defer those
//! branches.

// straitjacket-allow-file:duplication — this registry's `OnceLock<Mutex<BTreeMap>>`
// + monotonic-id idiom is faithfully mirrored by `session_resources.rs`; cpd pairs
// the two and honors the marker only on this alphabetically-first fragment.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::providers::faux::{
    FauxProvider, FauxResponseStep, FauxState, RegisterFauxProviderOptions,
};
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{AssistantMessage, Context, Model, StreamOptions};

/// A dispatch error raised by the compat entrypoint — the value analog of pi's
/// synchronous `throw new Error(...)` on the two `stream` guard paths.
///
/// pi's `stream`/`complete` *throw* (a catchable JS `Error`) when the api is
/// mismatched (`wrapStream`, `compat.ts:108`) or no provider is registered
/// (`resolveApiProvider`, `compat.ts:245`). Modelling those as a returned `Err`
/// keeps that behavior a value: at the napi boundary it maps to a catchable JS
/// throw, whereas a Rust `panic!` would cross as an uncatchable abort and crash
/// the test runner. Its [`Display`](std::fmt::Display) text is byte-for-byte
/// pi's `Error` message, so a caller (or the binding shim) reproduces pi's
/// thrown message exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatError {
    /// pi's `wrapStream` mismatch throw (`compat.ts:108`): the dispatched
    /// model's api does not match the provider it was routed to.
    MismatchedApi {
        /// The api the provider serves (pi's `expected`).
        expected: String,
        /// The dispatched model's api (pi's `model.api`).
        actual: String,
    },
    /// pi's `resolveApiProvider` throw (`compat.ts:245`): no provider is
    /// registered for the dispatched model's api.
    NoApiProvider {
        /// The dispatched model's api with no registered provider.
        api: String,
    },
}

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompatError::MismatchedApi { expected, actual } => {
                write!(f, "Mismatched api: {actual} expected {expected}")
            }
            CompatError::NoApiProvider { api } => {
                write!(f, "No API provider registered for api: {api}")
            }
        }
    }
}

impl std::error::Error for CompatError {}

// Re-export the faux content helpers so `pidgin_ai::compat` mirrors pi's compat
// entrypoint, whose faux-driven tests import `fauxText`/`fauxThinking`/
// `fauxToolCall`/`fauxAssistantMessage` from it. Consumed from the foundations
// faux module, not redefined.
pub use crate::providers::faux::{
    faux_assistant_message, faux_text, faux_thinking, faux_tool_call, FauxAssistantOptions,
};

/// A registered api provider — pi's `ApiProvider` (`compat.ts:78-82`).
///
/// Carries the api id it serves and the [`Provider`] seam that streams requests
/// for it. `stream` guards against an api mismatch exactly as pi's `wrapStream`
/// does (`compat.ts:98-107`). Clone is a cheap `Arc` bump, which is how
/// [`get_api_provider`] hands an entry back out (see the note there).
#[derive(Clone)]
pub struct ApiProvider {
    api: String,
    provider: Arc<dyn Provider>,
}

impl ApiProvider {
    /// Wrap a [`Provider`] seam as an api-registry entry keyed by its own api id.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            api: provider.api().to_string(),
            provider,
        }
    }

    /// The api id this provider serves (pi's `provider.api`).
    pub fn api(&self) -> &str {
        &self.api
    }

    /// Stream a request through the wrapped provider. Returns
    /// [`CompatError::MismatchedApi`] on an api mismatch, mirroring pi's
    /// `wrapStream` throw (`compat.ts:108`) as a catchable value; the normal
    /// [`get_api_provider`] dispatch path never mismatches, so the `Err` only
    /// fires on a caller contract violation.
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> Result<StreamResult, CompatError> {
        if model.api != self.api {
            return Err(CompatError::MismatchedApi {
                expected: self.api.clone(),
                actual: model.api.clone(),
            });
        }
        Ok(self.provider.stream(model, context, options, signal))
    }
}

impl std::fmt::Debug for ApiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiProvider")
            .field("api", &self.api)
            .finish()
    }
}

/// A registry entry: the provider plus the optional source id that scopes bulk
/// unregistration (pi's `RegisteredApiProvider`, `compat.ts:88-91`).
struct RegisteredApiProvider {
    provider: ApiProvider,
    source_id: Option<String>,
}

fn registry() -> &'static Mutex<BTreeMap<String, RegisteredApiProvider>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, RegisteredApiProvider>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register `provider` under its api id, optionally tagged with `source_id` for
/// scoped removal. pi's `registerApiProvider` (`compat.ts:126-138`).
pub fn register_api_provider(provider: ApiProvider, source_id: Option<&str>) {
    registry().lock().unwrap().insert(
        provider.api().to_string(),
        RegisteredApiProvider {
            provider,
            source_id: source_id.map(str::to_string),
        },
    );
}

/// Look up the provider registered for `api`. pi's `getApiProvider`
/// (`compat.ts:140-142`).
///
/// Returns an owned (`Arc`-backed) clone rather than pi's `&'static` shape:
/// pi's registry is a runtime-mutable `Map` — `register`/`unregister`/`reset`
/// rewrite it — so a borrowed-static handle cannot be handed out safely. The
/// clone is a cheap `Arc` bump and is the faithful Rust analog.
pub fn get_api_provider(api: &str) -> Option<ApiProvider> {
    registry()
        .lock()
        .unwrap()
        .get(api)
        .map(|e| e.provider.clone())
}

/// Every registered provider. pi's `getApiProviders` (`compat.ts:144-146`).
pub fn get_api_providers() -> Vec<ApiProvider> {
    registry()
        .lock()
        .unwrap()
        .values()
        .map(|e| e.provider.clone())
        .collect()
}

/// Remove every provider tagged with `source_id`. pi's `unregisterApiProviders`
/// (`compat.ts:148-154`).
pub fn unregister_api_providers(source_id: &str) {
    registry()
        .lock()
        .unwrap()
        .retain(|_, e| e.source_id.as_deref() != Some(source_id));
}

/// Register the builtin api implementations without clobbering existing entries.
/// pi's `registerBuiltInApiProviders` (`compat.ts:186-193`).
///
/// The ten builtin apis (anthropic-messages, openai-completions, …) are real
/// wire providers not yet ported (the api implementations and registry slice);
/// this is the seam that will register them as they land. It currently registers
/// nothing, so the registry holds only providers registered at runtime (e.g. via
/// [`register_faux_provider`]).
pub fn register_builtin_api_providers() {}

/// Clear the registry and re-register the builtins. pi's `resetApiProviders`
/// (`compat.ts:200-204`).
pub fn reset_api_providers() {
    registry().lock().unwrap().clear();
    register_builtin_api_providers();
}

fn next_source_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("faux-provider-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// A handle to a registered faux provider — pi's `FauxProviderRegistration`
/// (`compat.ts:157-181`, `faux.ts` return shape).
///
/// It exposes the wrapped [`FauxProvider`]'s scripting surface (queue responses,
/// read the model catalog and call count) and [`unregister`](Self::unregister)s
/// itself from the api registry. Built by [`register_faux_provider`].
pub struct FauxProviderRegistration {
    api: String,
    inner: Arc<FauxProvider>,
    source_id: String,
}

impl FauxProviderRegistration {
    /// The provider's api id (pi's `registration.api`).
    pub fn api(&self) -> &str {
        &self.api
    }

    /// The model catalog (pi's `registration.models`).
    pub fn models(&self) -> &[Model] {
        self.inner.models()
    }

    /// The first model, or the model with `id` (pi's `registration.getModel`).
    pub fn get_model(&self, id: Option<&str>) -> Option<Model> {
        self.inner.get_model(id)
    }

    /// A snapshot of the mutable call state (pi's `registration.state`).
    pub fn state(&self) -> FauxState {
        FauxState {
            call_count: self.inner.call_count(),
        }
    }

    /// The running call count (pi's `registration.state.callCount`).
    pub fn call_count(&self) -> u64 {
        self.inner.call_count()
    }

    /// Replace the queued responses (pi's `registration.setResponses`).
    pub fn set_responses(&self, responses: impl IntoIterator<Item = FauxResponseStep>) {
        self.inner.set_responses(responses);
    }

    /// Append queued responses (pi's `registration.appendResponses`).
    pub fn append_responses(&self, responses: impl IntoIterator<Item = FauxResponseStep>) {
        self.inner.append_responses(responses);
    }

    /// Pending queued-response count (pi's `registration.getPendingResponseCount`).
    pub fn pending_response_count(&self) -> usize {
        self.inner.pending_response_count()
    }

    /// Remove this provider from the api registry (pi's `registration.unregister`).
    pub fn unregister(&self) {
        unregister_api_providers(&self.source_id);
    }
}

/// Register a scripted faux provider into the api registry and return a handle.
/// pi's `registerFauxProvider` (`compat.ts:156-181`).
///
/// The faux streaming/cache core is the foundations [`FauxProvider`], consumed
/// here — this only wraps it as an [`ApiProvider`], assigns a source id, and
/// registers it.
pub fn register_faux_provider(options: RegisterFauxProviderOptions) -> FauxProviderRegistration {
    let inner = Arc::new(FauxProvider::new(options));
    let api = inner.api().to_string();
    let source_id = next_source_id();
    let provider = ApiProvider {
        api: api.clone(),
        provider: inner.clone() as Arc<dyn Provider>,
    };
    register_api_provider(provider, Some(&source_id));
    FauxProviderRegistration {
        api,
        inner,
        source_id,
    }
}

/// Stream a request for `model` through its registered api provider. The
/// registry-dispatch fallback of pi's `stream` (`compat.ts:250-263`); see the
/// module doc for the deferred builtin-catalog and env-api-key branches.
/// Returns [`CompatError::NoApiProvider`] when no provider is registered for
/// `model.api`, mirroring pi's `resolveApiProvider` throw (`compat.ts:245`) as
/// a catchable value.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&StreamOptions>,
    signal: Option<&AbortSignal>,
) -> Result<StreamResult, CompatError> {
    match get_api_provider(&model.api) {
        Some(provider) => provider.stream(model, context, options, signal),
        None => Err(CompatError::NoApiProvider {
            api: model.api.clone(),
        }),
    }
}

/// Stream to completion, returning the final message. pi's `complete`
/// (`compat.ts:266-273`), i.e. `stream(...).result()`; the dispatch error is
/// propagated as an `Err`, mirroring pi's rejected promise.
pub fn complete(
    model: &Model,
    context: &Context,
    options: Option<&StreamOptions>,
) -> Result<AssistantMessage, CompatError> {
    Ok(stream(model, context, options, None)?.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, Message, StopReason, UserContent, UserMessage, UserRole};
    use std::sync::MutexGuard;

    const TS: i64 = 1_700_000_000_000;

    // The api registry is process-global (pi's module-level `Map`), so registry
    // tests must not run concurrently. Each takes this lock and starts from a
    // cleared registry; the lock is poison-tolerant so a panicking test (e.g. an
    // `unwrap` on an unexpected value) does not wedge the others.
    fn serialized() -> MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_api_providers();
        guard
    }

    fn user_context(text: &str) -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text(text.to_string()),
                timestamp: TS,
            })],
            tools: None,
        }
    }

    fn text_message(text: &str) -> FauxResponseStep {
        faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), TS).into()
    }

    // Dispatch `model` through the registry and assert its single text block, the
    // shape most of these ported cases assert on.
    fn assert_completes_to_text(model: &Model, expected: &str) {
        let response = complete(model, &user_context("hi"), None).unwrap();
        assert_eq!(response.content, vec![faux_text(expected)]);
    }

    fn text_blocks(message: &AssistantMessage) -> Vec<(&'static str, String)> {
        message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text, .. } => ("text", text.clone()),
                ContentBlock::Thinking { thinking, .. } => ("thinking", thinking.clone()),
                ContentBlock::ToolCall { name, .. } => ("toolCall", name.clone()),
                _ => ("other", String::new()),
            })
            .collect()
    }

    // faux-provider.test.ts:31-46 — "registers a custom provider and estimates
    // usage": register, dispatch through the registry, and read call state back.
    #[test]
    fn register_faux_provider_registers_streams_and_estimates_usage() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions::default());
        registration.set_responses([text_message("hello world")]);

        // The provider is discoverable in the api registry under its api id.
        assert!(get_api_provider(registration.api()).is_some());

        let model = registration.get_model(None).unwrap();
        let response = complete(&model, &user_context("hi there"), None).unwrap();

        assert_eq!(response.content, vec![faux_text("hello world")]);
        assert!(response.usage.input > 0);
        assert!(response.usage.output > 0);
        assert_eq!(
            response.usage.total_tokens,
            response.usage.input + response.usage.output
        );
        assert_eq!(registration.call_count(), 1);
        assert_eq!(registration.state().call_count, 1);
        registration.unregister();
    }

    // faux-provider.test.ts:48-68 — "supports helper blocks for text, thinking,
    // and tool calls".
    #[test]
    fn helper_blocks_thinking_toolcall_text() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions::default());
        registration.set_responses([faux_assistant_message(
            vec![
                faux_thinking("think"),
                faux_tool_call("echo", serde_json::json!({ "text": "hi" }), None),
                faux_text("done"),
            ],
            FauxAssistantOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
            TS,
        )
        .into()]);

        let model = registration.get_model(None).unwrap();
        let response = complete(&model, &user_context("hi"), None).unwrap();

        assert_eq!(
            text_blocks(&response),
            vec![
                ("thinking", "think".to_string()),
                ("toolCall", "echo".to_string()),
                ("text", "done".to_string()),
            ]
        );
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        registration.unregister();
    }

    // faux-provider.test.ts:70-98 — "supports multiple models with per-model
    // reasoning and model-aware factories".
    #[test]
    fn multiple_models_and_model_aware_factories() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions {
            models: Some(vec![
                model_def("faux-fast", "Faux Fast", false),
                model_def("faux-thinker", "Faux Thinker", true),
            ]),
            ..Default::default()
        });
        registration.set_responses((0..2).map(|_| reasoning_echo_factory()));

        let ids: Vec<String> = registration.models().iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec!["faux-fast", "faux-thinker"]);
        assert_eq!(registration.get_model(None).unwrap().id, "faux-fast");
        assert!(!registration.get_model(Some("faux-fast")).unwrap().reasoning);
        assert!(
            registration
                .get_model(Some("faux-thinker"))
                .unwrap()
                .reasoning
        );

        assert_completes_to_text(
            &registration.get_model(Some("faux-fast")).unwrap(),
            "faux-fast:false",
        );
        assert_completes_to_text(
            &registration.get_model(Some("faux-thinker")).unwrap(),
            "faux-thinker:true",
        );
        registration.unregister();
    }

    // faux-provider.test.ts:100-116 — "rewrites api, provider, and model on
    // returned messages".
    #[test]
    fn rewrites_api_provider_and_model() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions {
            api: Some("faux:test".to_string()),
            provider: Some("faux-provider".to_string()),
            models: Some(vec![model_def("faux-model", "faux-model", false)]),
            ..Default::default()
        });
        registration.set_responses([text_message("hello")]);

        let model = registration.get_model(None).unwrap();
        let response = complete(&model, &user_context("hi"), None).unwrap();

        assert_eq!(response.api, "faux:test");
        assert_eq!(response.provider, "faux-provider");
        assert_eq!(response.model, "faux-model");
        registration.unregister();
    }

    // faux-provider.test.ts:118-135 — "consumes queued responses in order and
    // errors when exhausted".
    #[test]
    fn consumes_queue_in_order_then_errors_when_exhausted() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions::default());
        registration.set_responses([text_message("first"), text_message("second")]);

        let model = registration.get_model(None).unwrap();
        assert_completes_to_text(&model, "first");
        assert_completes_to_text(&model, "second");
        let exhausted = complete(&model, &user_context("hi"), None).unwrap();

        assert_eq!(exhausted.stop_reason, StopReason::Error);
        assert_eq!(
            exhausted.error_message.as_deref(),
            Some("No more faux responses queued")
        );
        assert_eq!(registration.pending_response_count(), 0);
        assert_eq!(registration.call_count(), 3);
        registration.unregister();
    }

    // faux-provider.test.ts:137-158 — "can replace and append queued responses".
    #[test]
    fn replace_and_append_queued_responses() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions::default());
        registration.set_responses([text_message("first")]);
        let model = registration.get_model(None).unwrap();

        assert_completes_to_text(&model, "first");
        assert_eq!(registration.pending_response_count(), 0);

        registration.set_responses([text_message("second")]);
        assert_eq!(registration.pending_response_count(), 1);
        assert_completes_to_text(&model, "second");

        registration.append_responses([text_message("third"), text_message("fourth")]);
        assert_eq!(registration.pending_response_count(), 2);
        assert_completes_to_text(&model, "third");
        assert_completes_to_text(&model, "fourth");
        assert_eq!(registration.pending_response_count(), 0);
        registration.unregister();
    }

    // Registry mutators, mirroring compat-env.test.ts's register/reset use and
    // pi's registry contract (`compat.ts:126-204`): register, enumerate,
    // scoped-unregister, and reset.
    #[test]
    fn registry_register_enumerate_unregister_reset() {
        let _guard = serialized();
        assert!(get_api_providers().is_empty());

        let a = register_faux_provider(RegisterFauxProviderOptions {
            api: Some("faux:a".to_string()),
            ..Default::default()
        });
        let b = register_faux_provider(RegisterFauxProviderOptions {
            api: Some("faux:b".to_string()),
            ..Default::default()
        });

        assert_eq!(get_api_providers().len(), 2);
        assert_eq!(get_api_provider("faux:a").unwrap().api(), "faux:a");
        assert_eq!(get_api_provider("faux:b").unwrap().api(), "faux:b");
        assert!(get_api_provider("faux:missing").is_none());

        a.unregister();
        assert!(get_api_provider("faux:a").is_none());
        assert!(get_api_provider("faux:b").is_some());

        reset_api_providers();
        assert!(get_api_provider("faux:b").is_none());
        assert!(get_api_providers().is_empty());
        b.unregister();
    }

    // pi's `resolveApiProvider` throw (`compat.ts:245`), asserted by
    // faux-provider.test.ts:587-594 as `.rejects.toThrow("No API provider
    // registered for api: ...")`: dispatching an api with no registered provider
    // is a catchable error. Here it is returned as a `CompatError` value (so the
    // napi boundary maps it to a JS throw, not an uncatchable abort).
    #[test]
    fn stream_errors_when_no_provider_registered() {
        let _guard = serialized();
        let model = lone_model("nope");
        let err = stream(&model, &user_context("hi"), None, None).unwrap_err();
        assert_eq!(
            err,
            CompatError::NoApiProvider {
                api: "nope".to_string(),
            }
        );
        assert_eq!(err.to_string(), "No API provider registered for api: nope");
    }

    // pi's `wrapStream` mismatch guard (`compat.ts:108`), a catchable
    // `throw new Error("Mismatched api: ...")`. Here it is returned as a
    // `CompatError` value rather than a panic, for the same napi-boundary reason.
    #[test]
    fn stream_errors_on_api_mismatch() {
        let _guard = serialized();
        let registration = register_faux_provider(RegisterFauxProviderOptions {
            api: Some("faux:guard".to_string()),
            ..Default::default()
        });
        let provider = get_api_provider("faux:guard").unwrap();
        let mismatched = lone_model("other");
        let err = provider
            .stream(&mismatched, &user_context("hi"), None, None)
            .unwrap_err();
        assert_eq!(
            err,
            CompatError::MismatchedApi {
                expected: "faux:guard".to_string(),
                actual: "other".to_string(),
            }
        );
        assert_eq!(err.to_string(), "Mismatched api: other expected faux:guard");
        registration.unregister();
    }

    fn model_def(
        id: &str,
        name: &str,
        reasoning: bool,
    ) -> crate::providers::faux::FauxModelDefinition {
        crate::providers::faux::FauxModelDefinition {
            id: id.to_string(),
            name: Some(name.to_string()),
            reasoning: Some(reasoning),
            input: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }
    }

    fn reasoning_echo_factory() -> FauxResponseStep {
        FauxResponseStep::Factory(Box::new(|_context, _options, _state, model| {
            faux_assistant_message(
                vec![faux_text(format!("{}:{}", model.id, model.reasoning))],
                FauxAssistantOptions::default(),
                TS,
            )
        }))
    }

    fn lone_model(api: &str) -> Model {
        Model {
            id: "m".to_string(),
            name: "m".to_string(),
            api: api.to_string(),
            provider: "p".to_string(),
            base_url: "https://example.test".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![],
            cost: crate::types::ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 1000,
            max_tokens: 100,
            headers: None,
            compat: None,
        }
    }
}
