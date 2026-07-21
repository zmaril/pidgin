//! The Mistral conversations [`Provider`] backend: the transport-aware adapter
//! that binds the ported `mistral-conversations` driver into the provider
//! registry's [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request build, header assembly, and `chat.stream`
//! decode — is already ported at [`crate::api::mistral`]. This module is pure
//! wiring: it adapts the generic [`Provider`] seam (which speaks [`Model<Value>`]
//! and [`StreamOptions`]) onto the driver's typed
//! [`stream`](crate::api::mistral::driver::stream) entry point (which speaks
//! [`MistralModel`] and [`MistralOptions`]), threading an injected
//! [`HttpTransport`] and [`Clock`] so a live Mistral turn runs without wall-clock
//! or ambient-network access.

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `Model<Value>` -> typed
// re-serialization mirror the sibling `anthropic_backend` by design; the clone
// detector reads the shared boundary-type construction as duplicative.

use std::sync::Arc;

use crate::api::mistral::driver;
use crate::api::mistral::{MistralModel, MistralOptions, SimpleMistralOptions};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Usage, UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `mistral-conversations` [`Api`]
/// discriminant.
pub const MISTRAL_CONVERSATIONS_API: &str = "mistral-conversations";

/// A [`Provider`] backend that runs a Mistral `chat.stream` turn over an injected
/// [`HttpTransport`], sourcing the request timestamp from an injected [`Clock`].
///
/// Constructed via [`MistralBackend::new`] and installed as
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct MistralBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl MistralBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl Provider for MistralBackend {
    fn api(&self) -> &str {
        MISTRAL_CONVERSATIONS_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's
        // `MistralModel` via serde. A malformed model is surfaced as a clean
        // pre-start error event, never a panic.
        let mut typed_model: MistralModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Mistral model is not compatible with mistral-conversations: {error}"),
                )
            }
        };

        // A per-request base-URL override targets the driver's `request_url`
        // (`{base_url}/v1/chat/completions`) at the right host, honoring an
        // explicit `StreamOptions.base_url` on top of any `auth.baseUrl` already
        // applied onto `model.base_url`.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let mistral_options = build_mistral_options(&typed_model, options);
        let api_key = options.and_then(|o| o.api_key.as_deref());

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against; `signal` is accepted for
        // seam parity and left unobserved here.
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &mistral_options,
            api_key,
            timestamp,
        )
    }

    fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        // Reasoning requested: lower it onto the incremental request through the
        // driver's `stream_streaming_simple`, mirroring the buffered
        // `stream_simple`. This is full pi parity — pi's single
        // `streamAssistantResponse` (`agent-loop.ts:281`) streams incrementally
        // AND honors reasoning through the one `streamSimple` path.
        if let Some(simple) = options.filter(|o| o.reasoning.is_some()) {
            let mut typed_model: MistralModel = match reserialize_model(model) {
                Ok(typed_model) => typed_model,
                Err(error) => {
                    return AssistantEventReader::from_buffered(error_result(
                        model,
                        self.clock.now_ms(),
                        format!(
                            "Mistral model is not compatible with mistral-conversations: {error}"
                        ),
                    ));
                }
            };
            if let Some(base_url) = simple.base.base_url.as_ref() {
                typed_model.base_url = base_url.clone();
            }
            let simple_options = mistral_simple_options(simple);
            let api_key = simple.base.api_key.as_deref();
            let timestamp = self.clock.now_ms();
            return driver::stream_streaming_simple(
                self.transport.as_ref(),
                &typed_model,
                context,
                &simple_options,
                api_key,
                timestamp,
            );
        }

        // No reasoning: byte-identical to the pre-widening base incremental path.
        // Same model/options assembly as `stream`, but the request runs through
        // the driver's incremental `stream_streaming` entry point: the returned
        // reader pulls one chunk at a time off the transport, so a streaming
        // transport surfaces real per-frame timing while the buffered `stream`
        // path is left untouched.
        let options = options.map(|o| &o.base);
        let mut typed_model: MistralModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                return AssistantEventReader::from_buffered(error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Mistral model is not compatible with mistral-conversations: {error}"),
                ));
            }
        };

        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let mistral_options = build_mistral_options(&typed_model, options);
        let api_key = options.and_then(|o| o.api_key.as_deref());

        let timestamp = self.clock.now_ms();
        driver::stream_streaming(
            self.transport.as_ref(),
            &typed_model,
            context,
            &mistral_options,
            api_key,
            timestamp,
        )
    }

    /// Route the simple, level-based options through the driver's
    /// `stream_simple` so `reasoning` reaches the request as the model's
    /// prompt-mode / reasoning-effort configuration, mirroring pi's
    /// `streamSimple` (`mistral-conversations.ts:110`).
    ///
    /// When no `reasoning` level is requested, this falls back to the raw
    /// [`stream`](Self::stream) on the base options, so the outgoing request is
    /// byte-identical to the pre-seam path.
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // No reasoning: keep the raw request unchanged (map None -> base options).
        let simple = match options {
            Some(simple) if simple.reasoning.is_some() => simple,
            other => return self.stream(model, context, other.map(|o| &o.base), signal),
        };

        let mut typed_model: MistralModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Mistral model is not compatible with mistral-conversations: {error}"),
                )
            }
        };

        if let Some(base_url) = simple.base.base_url.as_ref() {
            typed_model.base_url = base_url.clone();
        }

        let simple_options = mistral_simple_options(simple);
        let api_key = simple.base.api_key.as_deref();
        let timestamp = self.clock.now_ms();
        driver::stream_simple(
            self.transport.as_ref(),
            &typed_model,
            context,
            &simple_options,
            api_key,
            timestamp,
        )
    }
}

/// Map the seam-level [`SimpleStreamOptions`] onto the Mistral driver's local
/// simple-options struct (the reasoning-mode input pi's `streamSimple` reads).
fn mistral_simple_options(simple: &SimpleStreamOptions) -> SimpleMistralOptions {
    let base = &simple.base;
    SimpleMistralOptions {
        reasoning: simple.reasoning.map(to_model_thinking_level),
        temperature: base.temperature,
        max_tokens: base.max_tokens,
        session_id: base.session_id.clone(),
        cache_retention: base.cache_retention,
    }
}

/// Widen a caller's [`ThinkingLevel`] to the model-level [`ModelThinkingLevel`]
/// the reasoning-mode resolver expects (pi's `SimpleStreamOptions.reasoning`,
/// which extends the base ladder with `off`; a requested level is never `off`).
fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// Map the generic [`StreamOptions`] onto the driver's [`MistralOptions`],
/// threading the #192 per-request tuning knobs with pi's precedence.
///
/// Precedence (`mistral-conversations.ts` `buildChatPayload`, lines 253-254):
/// - `temperature` comes straight from `StreamOptions` (`payload.temperature =
///   options.temperature` when set); Mistral models carry no temperature default,
///   so an unset value leaves `temperature` off the payload.
/// - `max_tokens` follows the "StreamOptions overrides model" rule: an explicit
///   `StreamOptions.max_tokens` wins, else the model's `maxTokens` default is used
///   (pi's `buildBaseOptions` `options?.maxTokens ?? model.maxTokens`), else the
///   field is omitted.
/// - `metadata` is intentionally NOT threaded into the request body: pi's Mistral
///   driver never reads `options.metadata` into the `chat.stream` payload (only
///   `anthropic-messages` maps `metadata.user_id`), so emitting it here would
///   diverge from pi. `StreamOptions.metadata` stays inert for this dialect.
fn build_mistral_options(model: &MistralModel, options: Option<&StreamOptions>) -> MistralOptions {
    MistralOptions {
        temperature: options.and_then(|o| o.temperature),
        max_tokens: options
            .and_then(|o| o.max_tokens)
            .or_else(|| (model.max_tokens > 0).then_some(model.max_tokens)),
        session_id: options.and_then(|o| o.session_id.clone()),
        cache_retention: options.and_then(|o| o.cache_retention),
        headers: options.and_then(|o| o.headers.clone()),
        ..MistralOptions::default()
    }
}

/// Re-present a `Model<Value>` as a [`MistralModel`] via a serde JSON round-trip,
/// so the untyped boundary model is decoded into the lean identity/pricing slice
/// the driver reads.
fn reserialize_model(model: &Model) -> Result<MistralModel, serde_json::Error> {
    let json = serde_json::to_value(model)?;
    serde_json::from_value(json)
}

/// A single-`error`-event result for a failure before the driver's stream start
/// (an unserializable model), matching the driver's pre-start error shape.
fn error_result(model: &Model, timestamp: i64, message: String) -> StreamResult {
    let error = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp,
    };
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error.clone(),
        }],
        message: error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{json, Value};

    use crate::auth::DefaultAuthContext;
    use crate::providers::registry::{
        create_provider, ApiRouting, CreateProviderOptions, Models, MutableModels, ProviderAuth,
    };
    use crate::seams::clock::FakeClock;
    use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, ScriptedTransport};
    use crate::seams::storage::MemoryEnv;
    use crate::types::ContentBlock;
    use std::collections::BTreeMap;
    use std::io;
    use std::time::{Duration, Instant};

    /// A scripted `chat.stream` SSE body yielding a single `Hello` text block, in
    /// the SDK-shaped (camelCase) `CompletionChunk` frames the ported decoder
    /// reads.
    fn hello_sse_body() -> String {
        [
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finishReason\":\"stop\"}],\"usage\":{\"promptTokens\":10,\"completionTokens\":5,\"totalTokens\":15}}\n\n",
            "data: [DONE]\n\n",
        ]
        .concat()
    }

    /// A neutral non-reasoning mistral `Model<Value>` targeting `base_url`. The
    /// backend re-serializes this into [`MistralModel`].
    fn mistral_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "mistral-large-latest",
            "name": "Mistral Large",
            "api": "mistral-conversations",
            "provider": "mistral",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    fn user_context() -> Context {
        serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap()
    }

    fn scripted_hello() -> (ScriptedTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(hello_sse_body());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    fn fake_clock() -> Arc<dyn Clock> {
        Arc::new(FakeClock::new(1_700_000_000_000))
    }

    // Drives the backend end to end through ScriptedTransport: the `hello`
    // fixture yields a single "Hello" text block, and the request the backend
    // built carries the SDK Bearer credential, content-type, and the
    // `/v1/chat/completions` URL.
    #[test]
    fn backend_streams_hello_and_threads_bearer() {
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());

        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
                text_signature: None,
            }]
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-mistral-key")
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    // A per-request `base_url` override targets the request at the right host.
    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());

        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            base_url: Some("https://proxy.test".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1/chat/completions"
        );
    }

    // A non-2xx create surfaces the API's error body through the driver's
    // `format_api_error`, as a clean pre-start error event.
    #[test]
    fn backend_non_2xx_surfaces_error_body() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 401,
            headers: std::collections::BTreeMap::new(),
            body: "{\"message\":\"unauthorized\"}".to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = MistralBackend::new(transport, fake_clock());

        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Mistral API error (401): {\"message\":\"unauthorized\"}")
        );
        assert_eq!(scripted.requests().len(), 1);
    }

    /// A `Models` collection whose sole provider (`mistral`) routes through the
    /// backend over `transport`, resolving auth against `env`.
    fn models_with_mistral_backend(
        env: MemoryEnv,
        transport: Arc<dyn HttpTransport>,
        model: &Model,
    ) -> Models {
        let mut models = Models::with_auth_context(Arc::new(DefaultAuthContext::new(env)));
        models.set_provider(create_provider(CreateProviderOptions {
            id: "mistral".to_string(),
            name: Some("Mistral".to_string()),
            base_url: Some("https://api.mistral.ai".to_string()),
            headers: None,
            auth: ProviderAuth::env_api_key("Mistral API key", &["MISTRAL_API_KEY"]),
            models: vec![model.clone()],
            fetch_models: None,
            api: ApiRouting::Single(Arc::new(MistralBackend::new(transport, fake_clock()))),
        }));
        models
    }

    // The `Models::stream` applyAuth path: a configured env key resolves and
    // reaches the outbound request as `authorization: Bearer`, proving the
    // resolved apiKey threads through `StreamOptions.api_key` into the backend.
    #[test]
    fn models_stream_threads_resolved_api_key_to_request() {
        let (scripted, transport) = scripted_hello();
        let model = mistral_model("https://api.mistral.test");
        let env = MemoryEnv::new().with_env("MISTRAL_API_KEY", "sk-env-secret");
        let models = models_with_mistral_backend(env, transport, &model);

        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-env-secret")
        );
    }

    // #192: an explicit `StreamOptions.temperature` / `max_tokens` overrides the
    // model defaults and reaches the outgoing `chat.stream` body, on BOTH the
    // buffered and incremental mapping paths. `metadata` stays inert for Mistral
    // (pi's driver never maps it into the payload).
    #[test]
    fn backend_threads_stream_options_temperature_and_max_tokens() {
        let model = mistral_model("https://api.mistral.test");
        let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
        metadata.insert("user_id".to_string(), json!("u-42"));
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            temperature: Some(0.3),
            max_tokens: Some(256),
            metadata: Some(metadata),
            ..StreamOptions::default()
        };

        // Buffered path.
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());
        backend.stream(&model, &user_context(), Some(&options), None);
        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["temperature"], json!(0.3));
        assert_eq!(body["maxTokens"], json!(256));
        assert!(body.get("metadata").is_none());

        // Incremental path threads the identical knobs.
        let (scripted_incr, transport_incr) = scripted_hello();
        let backend_incr = MistralBackend::new(transport_incr, fake_clock());
        let mut reader = backend_incr.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let _: Vec<AssistantMessageEvent> = reader.by_ref().collect();
        let body_incr: Value =
            serde_json::from_str(scripted_incr.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body_incr["temperature"], json!(0.3));
        assert_eq!(body_incr["maxTokens"], json!(256));
        assert!(body_incr.get("metadata").is_none());
    }

    // Without a `StreamOptions.max_tokens`, the mapping falls back to the model's
    // `maxTokens` default (pi's `options?.maxTokens ?? model.maxTokens`), and no
    // temperature is emitted (Mistral models carry no temperature default).
    #[test]
    fn backend_max_tokens_falls_back_to_model_default() {
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());
        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);
        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["maxTokens"], json!(8192));
        assert!(body.get("temperature").is_none());
    }

    // -----------------------------------------------------------------------
    // streamSimple: reasoning mode (pi `mistral-conversations.ts:110`)
    // -----------------------------------------------------------------------

    /// A reasoning-capable mistral `Model<Value>` in the `reasoningEffort` class
    /// (`mistral-medium-3.5`), targeting `base_url`.
    fn mistral_reasoning_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "mistral-medium-3.5",
            "name": "Mistral Medium 3.5",
            "api": "mistral-conversations",
            "provider": "mistral",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    // Reasoning `high` on a `reasoningEffort`-class model lowers to
    // `reasoningEffort: "high"` in the outgoing body (cf. pi
    // `mistral-conversations.ts:121-129`).
    #[test]
    fn stream_simple_reasoning_sets_reasoning_effort() {
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());
        let model = mistral_reasoning_model("https://api.mistral.test");
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("sk-mistral-key".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["reasoningEffort"], json!("high"));
    }

    // FULL INCREMENTAL PARITY: `stream_incremental` carrying reasoning `high`
    // lowers to `reasoningEffort: "high"` param-exactly, identically to the
    // buffered `stream_simple` -- pi streams incrementally AND honors reasoning
    // through one `streamSimple` (`agent-loop.ts:281`).
    #[test]
    fn stream_incremental_reasoning_sets_reasoning_effort() {
        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());
        let model = mistral_reasoning_model("https://api.mistral.test");
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("sk-mistral-key".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        let mut reader = backend.stream_incremental(&model, &user_context(), Some(&simple), None);
        // Drain the reader so the request is actually issued.
        let _events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["reasoningEffort"], json!("high"));
    }

    // NO-REASONING zero-regression: `stream_simple` with `reasoning: None` builds
    // a request byte-identical to the raw `stream` path -- no `reasoningEffort` /
    // `promptMode` is added.
    #[test]
    fn stream_simple_without_reasoning_matches_raw_stream() {
        let model = mistral_model("https://api.mistral.test");
        let base = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };

        let (raw_scripted, raw_transport) = scripted_hello();
        let raw_backend = MistralBackend::new(raw_transport, fake_clock());
        raw_backend.stream(&model, &user_context(), Some(&base), None);

        let (simple_scripted, simple_transport) = scripted_hello();
        let simple_backend = MistralBackend::new(simple_transport, fake_clock());
        let simple = SimpleStreamOptions::from_base(base.clone());
        simple_backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let raw_body: Value =
            serde_json::from_str(raw_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        let simple_body: Value =
            serde_json::from_str(simple_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(raw_body, simple_body);
        assert!(simple_body.get("reasoningEffort").is_none());
        assert!(simple_body.get("promptMode").is_none());
    }

    // Incremental over the one-chunk ScriptedTransport (default `send_streaming`)
    // yields the SAME events and final message as the buffered `stream`, and
    // builds the same threaded request.
    #[test]
    fn backend_stream_incremental_matches_buffered_over_scripted() {
        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };

        let (_scripted_buffered, transport_buffered) = scripted_hello();
        let backend_buffered = MistralBackend::new(transport_buffered, fake_clock());
        let buffered = backend_buffered.stream(&model, &user_context(), Some(&options), None);

        let (scripted, transport) = scripted_hello();
        let backend = MistralBackend::new(transport, fake_clock());
        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        assert_eq!(events, buffered.events);
        assert_eq!(
            reader.result().and_then(|r| r.as_ref().ok()),
            Some(&buffered.message)
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://api.mistral.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-mistral-key")
        );
    }

    /// A transport whose `send_streaming` splits the SSE body into one chunk per
    /// frame and sleeps `delay` before yielding each, so the reader's per-frame
    /// PULL timing is observable. Its buffered `send` returns the whole body.
    struct SleepingStreamTransport {
        body: String,
        delay: Duration,
    }

    struct SleepingChunks {
        frames: std::vec::IntoIter<Vec<u8>>,
        delay: Duration,
    }

    impl Iterator for SleepingChunks {
        type Item = io::Result<Vec<u8>>;

        fn next(&mut self) -> Option<Self::Item> {
            let bytes = self.frames.next()?;
            std::thread::sleep(self.delay);
            Some(Ok(bytes))
        }
    }

    impl HttpTransport for SleepingStreamTransport {
        fn send(&self, _request: &HttpRequest) -> io::Result<HttpResponse> {
            Ok(HttpResponse::ok(self.body.clone()))
        }

        fn send_streaming(&self, _request: &HttpRequest) -> io::Result<HttpStreamResponse<'_>> {
            let frames: Vec<Vec<u8>> = self
                .body
                .split("\n\n")
                .filter(|part| !part.is_empty())
                .map(|part| format!("{part}\n\n").into_bytes())
                .collect();
            Ok(HttpStreamResponse {
                status: 200,
                headers: BTreeMap::new(),
                chunks: Box::new(SleepingChunks {
                    frames: frames.into_iter(),
                    delay: self.delay,
                }),
            })
        }
    }

    /// A scripted `chat.stream` SSE body yielding a `Hello world` text block over
    /// two frames, so per-frame streaming produces observable spread.
    fn hello_world_sse_body() -> String {
        [
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finishReason\":\"stop\"}],\"usage\":{\"promptTokens\":10,\"completionTokens\":5,\"totalTokens\":15}}\n\n",
            "data: [DONE]\n\n",
        ]
        .concat()
    }

    // Over a per-frame sleeping transport, the yielded events span multiple
    // sleeping chunks -- non-zero inter-event spread -- while resolving to the
    // same "Hello world" message as the buffered path.
    #[test]
    fn backend_stream_incremental_streams_with_inter_event_spread() {
        let delay = Duration::from_millis(12);
        let transport: Arc<dyn HttpTransport> = Arc::new(SleepingStreamTransport {
            body: hello_world_sse_body(),
            delay,
        });
        let backend = MistralBackend::new(transport, fake_clock());
        let model = mistral_model("https://api.mistral.test");
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }

        assert!(matches!(
            stamped.last().map(|(_, e)| e),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert_eq!(
            reader
                .result()
                .and_then(|r| r.as_ref().ok())
                .map(|m| m.content.clone()),
            Some(vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread from per-frame streaming, got {spread:?}",
        );
    }

    // Unconfigured provider: applyAuth gates before dispatch with the exact
    // "Provider is not configured" error, no panic and no network request.
    #[test]
    fn models_stream_unconfigured_provider_errors_without_request() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let model = mistral_model("https://api.mistral.test");
        let models = models_with_mistral_backend(MemoryEnv::new(), transport, &model);

        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Provider is not configured: mistral")
        );
        assert!(scripted.requests().is_empty());
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving a scripted `chat.stream` SSE body
/// and drives the backend through [`ReqwestTransport`] with `.no_proxy()`
/// (required in the sandbox).
#[cfg(all(test, feature = "native-http"))]
mod native_http_tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use crate::seams::clock::FakeClock;
    use crate::seams::http_reqwest::ReqwestTransport;
    use crate::types::ContentBlock;

    fn hello_sse_body() -> String {
        [
            "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finishReason\":\"stop\"}],\"usage\":{\"promptTokens\":10,\"completionTokens\":5,\"totalTokens\":15}}\n\n",
            "data: [DONE]\n\n",
        ]
        .concat()
    }

    /// Read one HTTP/1.1 request off `stream` up to the header terminator, then
    /// drain any declared body so the client's write completes cleanly.
    fn drain_request(stream: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut tmp).expect("read request");
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length = header_text
            .split("\r\n")
            .find_map(|line| {
                line.split_once(':').and_then(|(k, v)| {
                    (k.trim().eq_ignore_ascii_case("content-length"))
                        .then(|| v.trim().parse::<usize>().unwrap_or(0))
                })
            })
            .unwrap_or(0);
        let mut body_len = buf.len().saturating_sub(header_end + 4);
        while body_len < content_length {
            let n = stream.read(&mut tmp).expect("read body");
            if n == 0 {
                break;
            }
            body_len += n;
        }
    }

    #[test]
    fn backend_runs_over_reqwest_loopback() {
        let body = hello_sse_body();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));

        let server_body = body.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                server_body.len(),
                server_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().ok();
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = MistralBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "mistral-large-latest",
            "name": "Mistral Large",
            "api": "mistral-conversations",
            "provider": "mistral",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };

        let result = backend.stream(&model, &context, Some(&options), None);
        handle.join().expect("server thread");

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
                text_signature: None,
            }]
        );
    }

    /// The `chat.stream` SSE frames the incremental loopback serves, written as
    /// separate chunked writes with a delay between them so the reader observes
    /// real inter-frame timing.
    const SSE_FRAMES: [&str; 3] = [
        "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
        "data: {\"id\":\"resp_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finishReason\":\"stop\"}],\"usage\":{\"promptTokens\":10,\"completionTokens\":5,\"totalTokens\":15}}\n\n",
        "data: [DONE]\n\n",
    ];

    // Over the real reqwest transport driving a delayed `Transfer-Encoding:
    // chunked` loopback, `stream_incremental` delivers events across MULTIPLE
    // iterator steps with non-zero wall-clock spread, resolving to the same
    // "Hello world" message -- proving the pull loop streams per frame, not
    // buffered.
    #[test]
    fn backend_streams_incrementally_over_reqwest_loopback_with_timing() {
        let delay = Duration::from_millis(15);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .expect("write headers");
            for frame in SSE_FRAMES {
                let chunk = format!("{:X}\r\n{frame}\r\n", frame.len());
                stream.write_all(chunk.as_bytes()).expect("write chunk");
                stream.flush().expect("flush chunk");
                thread::sleep(delay);
            }
            stream.write_all(b"0\r\n\r\n").expect("write terminator");
            stream.flush().ok();
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = MistralBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "mistral-large-latest",
            "name": "Mistral Large",
            "api": "mistral-conversations",
            "provider": "mistral",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("sk-mistral-key".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(
            &model,
            &context,
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }
        handle.join().expect("server thread");

        assert!(matches!(
            stamped.last().map(|(_, e)| e),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert_eq!(
            reader
                .result()
                .and_then(|r| r.as_ref().ok())
                .map(|m| m.content.clone()),
            Some(vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread from chunked streaming, got {spread:?}",
        );
    }
}
