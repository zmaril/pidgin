//! The Azure OpenAI **Responses API** [`Provider`] backend: the transport-aware
//! adapter that binds the ported `azure-openai-responses` driver into the provider
//! registry's [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request shaping, Azure base-URL/deployment resolution,
//! and SSE decode — is already ported at [`crate::api::azure_openai_responses`]
//! (the pure request half, which reuses [`crate::api::openai_responses_shared`]'s
//! stream decoder) and [`crate::api::azure_openai_responses::driver`] (the
//! transport-driving request assembler + buffered/streaming drivers). This module
//! is pure wiring: it adapts the generic [`Provider`] seam (which speaks
//! [`Model<Value>`] and [`StreamOptions`]) onto the driver's typed
//! [`stream`](crate::api::azure_openai_responses::driver::stream) /
//! [`stream_streaming`](crate::api::azure_openai_responses::driver::stream_streaming)
//! entry points (which speak [`Model<OpenAIResponsesCompat>`] and
//! [`AzureOpenAIResponsesOptions`]), threading an injected [`HttpTransport`] and
//! [`Clock`] so a live responses turn runs without wall-clock or ambient-network
//! access. Its shape mirrors [`crate::providers::openai_responses_backend`].

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`), the `Model<Value>` -> typed
// reserialize, and the StreamOptions -> options bridge mirror the identical shells
// in the openai-responses backend it is the Azure sibling of; the clone detector
// reads the shared boundary-type construction as duplicative.

use std::sync::Arc;

use crate::api::azure_openai_responses::driver;
use crate::api::azure_openai_responses::AzureOpenAIResponsesOptions;
use crate::providers::clamp_thinking_level;
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    OpenAIResponsesCompat, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Usage,
    UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `azure-openai-responses` [`Api`]
/// discriminant.
///
/// Registering this in [`backend_for_api`](crate::providers::builtins) binds the
/// single-dialect `azure-openai-responses` provider to
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single).
pub const AZURE_OPENAI_RESPONSES_API: &str = "azure-openai-responses";

/// A [`Provider`] backend that runs an Azure OpenAI Responses turn over an injected
/// [`HttpTransport`], sourcing the request timestamp from an injected [`Clock`].
///
/// Constructed via [`AzureOpenAIResponsesBackend::new`] and installed by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct AzureOpenAIResponsesBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl AzureOpenAIResponsesBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }

    /// Re-present the untyped boundary `Model<Value>` as the driver's typed
    /// `Model<OpenAIResponsesCompat>`, applying any per-request `base_url`
    /// override, or return the pre-start error message for an incompatible compat
    /// blob. Shared by `stream` and `stream_incremental`.
    fn typed_model(
        &self,
        model: &Model,
        options: Option<&StreamOptions>,
    ) -> Result<Model<OpenAIResponsesCompat>, String> {
        let mut typed_model: Model<OpenAIResponsesCompat> =
            reserialize_model(model).map_err(|error| {
                format!("Azure model is not compatible with azure-openai-responses: {error}")
            })?;

        // A per-request base-URL override lands on `model.base_url`, the lowest
        // priority input to the driver's `resolve_azure_config` (below
        // `azureBaseUrl` / `azureResourceName`, which the generic StreamOptions
        // does not carry). `applyAuth` has already applied any per-credential
        // `auth.baseUrl` onto `model.base_url`; this honors an explicit
        // `StreamOptions.base_url` on top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }
        Ok(typed_model)
    }
}

/// Map the generic [`StreamOptions`] onto the driver's typed
/// [`AzureOpenAIResponsesOptions`].
///
/// # #192 threading
///
/// pi's Azure `buildParams` reads `options.maxTokens` and `options.temperature`
/// directly off the (StreamOptions-extending) options — there is no model-default
/// fallback for either in the Azure Responses shaper
/// (`azure-openai-responses.ts:268-274`), so both flow straight from
/// [`StreamOptions`]. pi's Responses shaper does **not** map `metadata` (unlike
/// some dialects), so `StreamOptions.metadata` is deliberately dropped here to
/// match pi's Azure Responses behavior — identical to the openai-responses
/// sibling. The Azure-specific knobs (`azureApiVersion` / `azureResourceName` /
/// `azureBaseUrl` / `azureDeploymentName` / the deployment-name map) are not
/// carried by the generic `StreamOptions`, so they stay defaulted: the api-version
/// defaults to `v1` and the base URL resolves from `model.base_url`.
fn responses_options(options: Option<&StreamOptions>) -> AzureOpenAIResponsesOptions {
    AzureOpenAIResponsesOptions {
        // #192: StreamOptions overrides the model; the Azure Responses shaper
        // reads options.maxTokens / options.temperature with no model fallback.
        max_tokens: options.and_then(|o| o.max_tokens),
        temperature: options.and_then(|o| o.temperature),
        // Azure's buildParams sets prompt_cache_key from sessionId (ungated by
        // cache retention, unlike openai-responses).
        session_id: options.and_then(|o| o.session_id.clone()),
        api_key: options.and_then(|o| o.api_key.clone()),
        headers: options.and_then(|o| o.headers.clone()),
        ..AzureOpenAIResponsesOptions::default()
    }
}

/// Compute the Responses `reasoningEffort` string from the requested reasoning
/// level, mirroring pi's Azure `streamSimple` (`azure-openai-responses.ts:155-156`):
/// `clampedReasoning = reasoning ? clampThinkingLevel(model, reasoning) :
/// undefined`, then `reasoningEffort = clampedReasoning === "off" ? undefined :
/// clampedReasoning`. Identical to the openai-responses sibling — Azure's
/// `streamSimple` differs from openai-responses only in requiring an apiKey (the
/// `stream`/`error_result` path already enforces that) and in having no
/// xai/github-copilot special-casing (absent here and in the ported driver
/// `buildParams`, `azure_openai_responses.rs:274-299`). Returns `None` when no
/// level is requested or the clamp lands on `off`, so the driver applies its
/// off-model fallback branch.
fn reasoning_effort<C>(model: &Model<C>, reasoning: Option<ThinkingLevel>) -> Option<String> {
    let clamped = clamp_thinking_level(model, to_model_thinking_level(reasoning?));
    if clamped == ModelThinkingLevel::Off {
        return None;
    }
    Some(model_thinking_level_str(clamped).to_string())
}

/// Widen a caller's [`ThinkingLevel`] to the model-level [`ModelThinkingLevel`]
/// the clamp expects (pi's `SimpleStreamOptions.reasoning`, which extends the base
/// ladder with `off`; a requested level is never `off`).
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

/// The lowercase effort string the driver's `buildParams` reads (pi passes the
/// clamped level verbatim as `reasoningEffort`).
fn model_thinking_level_str(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

impl Provider for AzureOpenAIResponsesBackend {
    fn api(&self) -> &str {
        AZURE_OPENAI_RESPONSES_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let typed_model = match self.typed_model(model, options) {
            Ok(typed_model) => typed_model,
            Err(message) => return error_result(model, self.clock.now_ms(), message),
        };
        let responses_options = responses_options(options);

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against; `signal` is accepted for
        // seam parity and left unobserved here (matching the sibling backends).
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &responses_options,
            timestamp,
        )
    }

    /// Route the simple, level-based options through the Azure Responses driver so
    /// `reasoning` reaches the request as `reasoning={effort,summary}` +
    /// `include:["reasoning.encrypted_content"]`, mirroring pi's Azure `streamSimple`
    /// (`azure-openai-responses.ts:144-162`).
    ///
    /// pi's Azure `streamSimple` clamps the requested level and drops `"off"`
    /// exactly like the openai-responses sibling, then passes `{ ...base,
    /// reasoningEffort }` to `stream`. The `reasoning`/`include`/off-model-fallback
    /// shaping lives in the already-ported driver `buildParams`
    /// (`azure_openai_responses.rs:274-299`, mirroring `azure-openai-responses.ts:280-295`),
    /// which — unlike openai-responses — has no xai/github-copilot special-casing.
    ///
    /// When no `reasoning` level is requested, this falls back to the raw
    /// [`stream`](Self::stream) on the base options, so the outgoing request is
    /// byte-identical to the pre-seam path (the raw path already enforces Azure's
    /// api-key requirement).
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

        let typed_model = match self.typed_model(model, Some(&simple.base)) {
            Ok(typed_model) => typed_model,
            Err(message) => return error_result(model, self.clock.now_ms(), message),
        };

        // pi `azure-openai-responses.ts:155-156`: clamp the requested level to a
        // supported one, then drop "off" so the driver's off-model fallback applies.
        let mut responses_options = responses_options(Some(&simple.base));
        responses_options.reasoning_effort = reasoning_effort(&typed_model, simple.reasoning);

        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &responses_options,
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
        // The incremental driver path cannot lower `reasoning` yet (per-driver
        // incremental lowering is a follow-up, tracked with the buffered
        // `stream_simple` override for this dialect), so guard against silently
        // dropping a reasoning request before streaming on the base options.
        crate::seams::provider::debug_assert_incremental_reasoning_unlowered(options, self.api());
        let options = options.map(|o| &o.base);
        // Same model/options assembly as `stream`, but the request runs through
        // the driver's incremental `stream_streaming` entry point: the returned
        // reader pulls one chunk at a time off the transport, so a streaming
        // transport surfaces real per-frame timing while the buffered `stream`
        // path is left untouched.
        let typed_model = match self.typed_model(model, options) {
            Ok(typed_model) => typed_model,
            Err(message) => {
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                return AssistantEventReader::from_buffered(error_result(
                    model,
                    self.clock.now_ms(),
                    message,
                ));
            }
        };
        let responses_options = responses_options(options);

        let timestamp = self.clock.now_ms();
        driver::stream_streaming(
            self.transport.as_ref(),
            &typed_model,
            context,
            &responses_options,
            timestamp,
        )
    }
}

/// Re-present a `Model<Value>` as a `Model<OpenAIResponsesCompat>` via a serde
/// JSON round-trip, so the untyped `compat` blob is decoded into the typed
/// responses compat map the driver reads.
fn reserialize_model(model: &Model) -> Result<Model<OpenAIResponsesCompat>, serde_json::Error> {
    let json = serde_json::to_value(model)?;
    serde_json::from_value(json)
}

/// A single-`error`-event result for a failure before the driver's stream start
/// (an incompatible model), matching the registry's and driver's pre-start error
/// shape.
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

    use crate::seams::clock::FakeClock;
    use crate::seams::http::ScriptedTransport;
    use crate::types::{AssistantMessageEvent, ContentBlock, StopReason};

    /// A minimal Azure OpenAI Responses streaming body: a `response.created`, a
    /// text item lifecycle with two deltas, and a `response.completed` terminal.
    /// Azure's responses SSE frames are byte-identical to openai-responses', so
    /// this mirrors the openai-responses backend's `hello_sse_body` fixture.
    fn hello_sse_body() -> String {
        let events = [
            json!({ "type": "response.created", "response": { "id": "resp_1" } }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
            }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Hello" }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": " world" }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Hello world" }]
                }
            }),
            json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed" } }),
        ];
        events
            .iter()
            .map(|event| {
                let name = event.get("type").and_then(Value::as_str).unwrap();
                format!("event: {name}\ndata: {event}\n\n")
            })
            .collect()
    }

    /// A neutral non-reasoning azure-openai-responses `Model<Value>` targeting
    /// `base_url` (a non-Azure host so `normalize_azure_base_url` preserves it
    /// verbatim). The backend re-serializes this into
    /// `Model<OpenAIResponsesCompat>`.
    fn azure_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gpt-5-mini",
            "name": "GPT-5 mini",
            "api": "azure-openai-responses",
            "provider": "azure-openai-responses",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 400000,
            "maxTokens": 128000,
        }))
        .unwrap()
    }

    /// A reasoning-enabled azure-openai-responses `Model<Value>`. With no
    /// `thinkingLevelMap` every base level (minimal/low/medium/high) is supported,
    /// so `clampThinkingLevel(high)` is `high`. Uses a non-Azure host so the base
    /// URL is preserved verbatim.
    fn reasoning_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gpt-5-mini",
            "name": "GPT-5 mini",
            "api": "azure-openai-responses",
            "provider": "azure-openai-responses",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 400000,
            "maxTokens": 128000,
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

    // (a) Drives the backend end to end through ScriptedTransport: the `hello`
    // fixture yields a single "Hello world" text block, and (b) the request the
    // backend built carries the Azure `api-key: <key>` HEADER (not Bearer),
    // `Content-Type: application/json`, and the `/responses?api-version=v1` URL.
    #[test]
    fn backend_streams_hello_and_sets_azure_auth() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: Some(r#"{"v":1,"id":"msg_1"}"#.to_string()),
            }]
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        // api-version query on the /responses endpoint under the resolved base URL.
        assert_eq!(
            requests[0].url,
            "https://my-proxy.example.com/v1/responses?api-version=v1"
        );
        // Azure key auth: `api-key` header, not `authorization: Bearer`.
        assert_eq!(
            requests[0].headers.get("api-key").map(String::as_str),
            Some("azure-test-key")
        );
        assert!(
            !requests[0].headers.contains_key("authorization"),
            "azure key auth must not mint an authorization: Bearer header"
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    // An Azure-host base URL is normalized to /openai/v1 before /responses is
    // appended (the driver reuses the ported resolve_azure_config).
    #[test]
    fn backend_normalizes_azure_host_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-resource.openai.azure.com");
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1"
        );
    }

    // A per-request `base_url` override targets the request at the right host (it
    // lands on model.base_url, the driver's lowest-priority base URL input).
    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            base_url: Some("https://override-proxy.test/v1".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://override-proxy.test/v1/responses?api-version=v1"
        );
    }

    // A caller-supplied `api-key` header wins over the SDK-injected default: the
    // backend never overwrites it.
    #[test]
    fn backend_caller_api_key_header_wins() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("api-key".to_string(), "caller-key".to_string());
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            headers: Some(headers),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0]
                .headers
                .get("api-key")
                .map(String::as_str),
            Some("caller-key")
        );
    }

    // No api_key is the pre-start `No API key` failure, and no request is made
    // (Azure has no caller-header sentinel fallback, unlike openai-responses).
    #[test]
    fn backend_missing_api_key_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("No API key for provider"));
        assert!(scripted.requests().is_empty());
    }

    // #192: StreamOptions.temperature / max_tokens land in the outgoing request
    // body (`temperature` and `max_output_tokens`, the latter clamped to the
    // Responses minimum by the shaper).
    #[test]
    fn backend_threads_temperature_and_max_tokens_into_body() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            temperature: Some(0.42),
            max_tokens: Some(4096),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body.get("temperature").and_then(Value::as_f64), Some(0.42));
        assert_eq!(
            body.get("max_output_tokens").and_then(Value::as_u64),
            Some(4096)
        );
    }

    // The incremental path yields the SAME events and terminal message as the
    // buffered path over the same one-chunk body.
    #[test]
    fn backend_stream_incremental_matches_buffered() {
        let (_scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());
        let model = azure_model("https://my-proxy.example.com/v1");
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            ..StreamOptions::default()
        };

        let buffered = backend.stream(&model, &user_context(), Some(&options), None);

        let (_scripted2, transport2) = scripted_hello();
        let backend2 = AzureOpenAIResponsesBackend::new(transport2, fake_clock());
        let mut reader = backend2.stream_incremental(
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
    }

    // A non-2xx create surfaces the API's error body through format_api_error, and
    // makes exactly one request.
    #[test]
    fn backend_non_2xx_surfaces_error_body() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(crate::seams::http::HttpResponse {
            status: 401,
            headers: std::collections::BTreeMap::new(),
            body: json!({ "error": { "message": "Access denied due to invalid api-key" } })
                .to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = azure_model("https://my-proxy.example.com/v1");
        let options = StreamOptions {
            api_key: Some("bad-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("401 Access denied due to invalid api-key")
        );
        assert_eq!(scripted.requests().len(), 1);
    }

    // A model whose compat blob cannot decode into `OpenAIResponsesCompat`
    // surfaces a clean pre-start error event, never a panic, and never a request.
    #[test]
    fn backend_incompatible_model_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let mut model = azure_model("https://my-proxy.example.com/v1");
        // `supportsToolSearch` is a bool flag; a number here fails the typed decode.
        model.compat = Some(json!({ "supportsToolSearch": 12345 }) as Value);

        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(
            result.events[0],
            AssistantMessageEvent::Error { .. }
        ));
        assert!(scripted.requests().is_empty());
    }

    // stream_simple with a reasoning level lowers `reasoning={effort,summary:"auto"}`
    // + `include:["reasoning.encrypted_content"]` into the request body, per pi's
    // Azure `streamSimple` (`azure-openai-responses.ts:155-156`) + `buildParams`
    // (`azure-openai-responses.ts:280-295`). `high` clamps to `high` on this model.
    #[test]
    fn stream_simple_lowers_reasoning_effort_and_include() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = reasoning_model("https://my-proxy.example.com/v1");
        let options = SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("azure-test-key".to_string()),
                ..StreamOptions::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..SimpleStreamOptions::default()
        };
        backend.stream_simple(&model, &user_context(), Some(&options), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body.get("reasoning"),
            Some(&json!({ "effort": "high", "summary": "auto" }))
        );
        assert_eq!(
            body.get("include"),
            Some(&json!(["reasoning.encrypted_content"]))
        );
    }

    // No reasoning requested falls to the driver's off-model fallback branch
    // (`reasoning={effort: map.off ?? "none"}`, `azure-openai-responses.ts:290-293`):
    // the effort is `"none"` (no `off` map entry) and no `include` is emitted.
    // Azure has no xai/github-copilot special-casing, so `include` never appears
    // outside the effort branch.
    #[test]
    fn stream_simple_without_reasoning_uses_off_fallback() {
        let (scripted, transport) = scripted_hello();
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = reasoning_model("https://my-proxy.example.com/v1");
        let options = SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("azure-test-key".to_string()),
                ..StreamOptions::default()
            },
            reasoning: None,
            ..SimpleStreamOptions::default()
        };
        backend.stream_simple(&model, &user_context(), Some(&options), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body.get("reasoning"), Some(&json!({ "effort": "none" })));
        assert!(body.get("include").is_none());
    }

    // With no reasoning, stream_simple produces a request byte-identical to the raw
    // stream on the base options (the compatibility default's guarantee, preserved
    // by the override's None short-circuit).
    #[test]
    fn stream_simple_without_reasoning_matches_raw_stream() {
        let (scripted_simple, transport_simple) = scripted_hello();
        let backend_simple = AzureOpenAIResponsesBackend::new(transport_simple, fake_clock());
        let model = azure_model("https://my-proxy.example.com/v1");
        let base = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
            temperature: Some(0.3),
            max_tokens: Some(2048),
            ..StreamOptions::default()
        };
        let simple = SimpleStreamOptions {
            base: base.clone(),
            reasoning: None,
            ..SimpleStreamOptions::default()
        };
        backend_simple.stream_simple(&model, &user_context(), Some(&simple), None);

        let (scripted_raw, transport_raw) = scripted_hello();
        let backend_raw = AzureOpenAIResponsesBackend::new(transport_raw, fake_clock());
        backend_raw.stream(&model, &user_context(), Some(&base), None);

        assert_eq!(
            scripted_simple.requests()[0].body,
            scripted_raw.requests()[0].body
        );
    }

    // Azure's hard api-key requirement (pi `azure-openai-responses.ts:149-152`) is
    // preserved on the reasoning-lowering path: a reasoning request with no api key
    // is the pre-start `No API key` failure and makes no request.
    #[test]
    fn stream_simple_missing_api_key_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = AzureOpenAIResponsesBackend::new(transport, fake_clock());

        let model = reasoning_model("https://my-proxy.example.com/v1");
        let options = SimpleStreamOptions {
            base: StreamOptions::default(),
            reasoning: Some(ThinkingLevel::High),
            ..SimpleStreamOptions::default()
        };
        let result = backend.stream_simple(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("No API key for provider"));
        assert!(scripted.requests().is_empty());
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving a minimal Azure OpenAI Responses SSE
/// reply in delayed chunks and drives the backend's incremental path through
/// [`ReqwestTransport`] with `.no_proxy()`, asserting real inter-event spacing.
/// Mirrors the openai-responses backend's loopback test.
#[cfg(all(test, feature = "native-http"))]
mod native_http_tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use crate::seams::clock::FakeClock;
    use crate::seams::http_reqwest::ReqwestTransport;
    use crate::types::{AssistantMessageEvent, ContentBlock};

    /// The Responses SSE frames, one per element, for the `hello` lifecycle.
    fn hello_frames() -> Vec<String> {
        let events = [
            json!({ "type": "response.created", "response": { "id": "resp_1" } }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
            }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Hello" }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": " world" }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Hello world" }]
                }
            }),
            json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed" } }),
        ];
        events
            .iter()
            .map(|event| {
                let name = event.get("type").and_then(Value::as_str).unwrap();
                format!("event: {name}\r\ndata: {event}\r\n\r\n")
            })
            .collect()
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

    // A delayed chunked-transfer server writes each SSE frame `delay` apart; the
    // incremental reader must observe non-zero inter-event spread, proving the
    // events arrive per-frame rather than all at once.
    #[test]
    fn backend_incremental_streams_over_reqwest_loopback_with_spread() {
        let frames = hello_frames();
        let delay = Duration::from_millis(20);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("local addr"));

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .expect("write headers");
            stream.flush().ok();
            for frame in &frames {
                thread::sleep(delay);
                let chunk = format!("{:x}\r\n{}\r\n", frame.len(), frame);
                stream.write_all(chunk.as_bytes()).expect("write chunk");
                stream.flush().ok();
            }
            stream.write_all(b"0\r\n\r\n").expect("write terminator");
            stream.flush().ok();
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = AzureOpenAIResponsesBackend::new(transport, clock);

        // A non-Azure host base URL so resolve_azure_config preserves it verbatim.
        let model: Model = serde_json::from_value(json!({
            "id": "gpt-5-mini",
            "name": "GPT-5 mini",
            "api": "azure-openai-responses",
            "provider": "azure-openai-responses",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 400000,
            "maxTokens": 128000,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("azure-test-key".to_string()),
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

        let message = reader.result().and_then(|r| r.as_ref().ok()).cloned();
        assert_eq!(
            message.map(|m| m.content),
            Some(vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: Some(r#"{"v":1,"id":"msg_1"}"#.to_string()),
            }])
        );

        // PULL timing: events span multiple inter-frame delays rather than
        // arriving together.
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread (incremental), got {spread:?}",
        );
    }

    // A deterministic sleeping-chunk drive (no network): feed the reader one frame
    // per sleep and assert the yielded events span the sleeps.
    #[test]
    fn backend_incremental_sleeping_chunks_have_spread() {
        use crate::api::azure_openai_responses::driver;
        use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, HttpTransport};

        struct SleepingTransport {
            frames: Vec<String>,
            delay: Duration,
        }

        impl HttpTransport for SleepingTransport {
            fn send(&self, _request: &HttpRequest) -> std::io::Result<HttpResponse> {
                unreachable!("streaming path only")
            }

            fn send_streaming(
                &self,
                _request: &HttpRequest,
            ) -> std::io::Result<HttpStreamResponse<'_>> {
                let frames = self.frames.clone();
                let delay = self.delay;
                let chunks = frames.into_iter().map(move |frame| {
                    thread::sleep(delay);
                    Ok(frame.into_bytes())
                });
                Ok(HttpStreamResponse {
                    status: 200,
                    headers: std::collections::BTreeMap::new(),
                    chunks: Box::new(chunks),
                })
            }
        }

        let transport = SleepingTransport {
            frames: hello_frames(),
            delay: Duration::from_millis(15),
        };
        let model: Model<OpenAIResponsesCompat> = serde_json::from_value(json!({
            "id": "gpt-5-mini",
            "name": "GPT-5 mini",
            "api": "azure-openai-responses",
            "provider": "azure-openai-responses",
            "baseUrl": "https://my-proxy.example.com/v1",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 400000,
            "maxTokens": 128000,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = AzureOpenAIResponsesOptions {
            api_key: Some("azure-test-key".to_string()),
            ..AzureOpenAIResponsesOptions::default()
        };

        let mut reader = driver::stream_streaming(&transport, &model, &context, &options, 0);
        let start = Instant::now();
        let mut stamped = Vec::new();
        for event in reader.by_ref() {
            stamped.push(start.elapsed());
            let _ = event;
        }
        let spread = *stamped.last().unwrap() - *stamped.first().unwrap();
        assert!(
            spread >= Duration::from_millis(30),
            "expected sleeping-chunk spread, got {spread:?}",
        );
    }
}
