//! The Amazon Bedrock `ConverseStream` [`Provider`] backend: the
//! transport-aware adapter that binds the ported `bedrock-converse-stream`
//! driver into the provider registry's
//! [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request build, client-config/endpoint resolution,
//! binary `vnd.amazon.eventstream` decode, and Converse event accumulation — is
//! already ported at [`crate::api::bedrock`]. This module is pure wiring: it
//! adapts the generic [`Provider`] seam (which speaks [`Model<Value>`] and
//! [`StreamOptions`]) onto the driver's typed
//! [`stream`](crate::api::bedrock::driver::stream) entry point (which speaks
//! [`BedrockModel`] and [`BedrockOptions`]), threading an injected
//! [`HttpTransport`] and [`Clock`] so a live Bedrock turn runs without
//! wall-clock or ambient-network access.
//!
//! # Scope: buffered + bearer only
//!
//! This backend drives the BEARER-TOKEN auth path (pi's Bedrock API-key bypass)
//! over a buffered decode of the whole response body. The non-bearer
//! AWS-credentials path (SigV4 signing) and true per-chunk incremental streaming
//! are documented follow-ups on the driver; `stream_incremental` here inherits
//! the default buffered wrapper.

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `Model<Value>` -> typed
// re-serialization mirror the sibling `mistral_backend` by design; the clone
// detector reads the shared boundary-type construction as duplicative.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::bedrock::driver;
use crate::api::bedrock::{BedrockModel, BedrockOptions, ThinkingBudgets};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, SimpleStreamOptions,
    StopReason, StreamOptions, ThinkingBudgets as SeamThinkingBudgets, ThinkingLevel, Usage,
    UsageCost,
};
use crate::utils::provider_env::ProviderEnv;

/// The api id this backend serves, pi's `bedrock-converse-stream` [`Api`]
/// discriminant.
pub const BEDROCK_CONVERSE_STREAM_API: &str = "bedrock-converse-stream";

/// A [`Provider`] backend that runs a Bedrock `ConverseStream` turn over an
/// injected [`HttpTransport`], sourcing the request timestamp from an injected
/// [`Clock`].
///
/// Constructed via [`BedrockBackend::new`] and installed as
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport)
/// (amazon-bedrock is a single-dialect provider).
pub struct BedrockBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl BedrockBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl Provider for BedrockBackend {
    fn api(&self) -> &str {
        BEDROCK_CONVERSE_STREAM_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's
        // `BedrockModel` via serde. A malformed model is surfaced as a clean
        // pre-start error event, never a panic.
        let mut typed_model: BedrockModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!(
                        "Bedrock model is not compatible with bedrock-converse-stream: {error}"
                    ),
                )
            }
        };

        // A per-request base-URL override targets the client-config endpoint
        // resolution at the right host, honoring an explicit
        // `StreamOptions.base_url` on top of any `auth.baseUrl` already applied
        // onto `model.base_url`.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = Some(base_url.clone());
        }

        let bedrock_options = bedrock_options_from(options);

        // The ambient environment snapshot is the not-yet-wired provider seam
        // (see the `build_command_input` divergence note); scoped provider config
        // and the bearer token flow through `bedrock_options` (from
        // `StreamOptions`), so an empty ambient env keeps resolution
        // deterministic here.
        let process_env = ProviderEnv::new();

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against; `signal` is accepted for
        // seam parity and left unobserved here.
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &bedrock_options,
            &process_env,
            timestamp,
        )
    }

    /// Route the simple, level-based options through the driver's
    /// `stream_simple` so `reasoning`/`thinking_budgets` reach the request,
    /// mirroring pi's `streamSimple` (`bedrock-converse-stream.ts:392`).
    ///
    /// When no `reasoning` level is requested, this falls back to the raw
    /// [`stream`](Self::stream) on the base options, so the outgoing request is
    /// byte-identical to the pre-seam path (no thinking config is added).
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

        let mut typed_model: BedrockModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!(
                        "Bedrock model is not compatible with bedrock-converse-stream: {error}"
                    ),
                )
            }
        };

        if let Some(base_url) = simple.base.base_url.as_ref() {
            typed_model.base_url = Some(base_url.clone());
        }

        let bedrock_options = bedrock_simple_options_from(simple);
        let process_env = ProviderEnv::new();
        let timestamp = self.clock.now_ms();
        driver::stream_simple(
            self.transport.as_ref(),
            &typed_model,
            context,
            &bedrock_options,
            &process_env,
            timestamp,
        )
    }
}

/// Map the seam-level [`SimpleStreamOptions`] onto the driver's [`BedrockOptions`]
/// (the reasoning-lowering input pi's `streamSimple` reads). The base
/// [`StreamOptions`] fields are projected as in [`bedrock_options_from`], plus the
/// `reasoning` level and the per-level `thinking_budgets` carried into the request.
fn bedrock_simple_options_from(simple: &SimpleStreamOptions) -> BedrockOptions {
    let base = &simple.base;
    BedrockOptions {
        api_key: base.api_key.clone(),
        temperature: base.temperature,
        max_tokens: base.max_tokens,
        cache_retention: base.cache_retention,
        headers: base.headers.as_ref().map(to_provider_headers),
        env: base.env.clone(),
        reasoning: simple.reasoning,
        thinking_budgets: simple
            .thinking_budgets
            .as_ref()
            .map(to_bedrock_thinking_budgets),
        ..BedrockOptions::default()
    }
}

/// Convert the seam's struct-shaped [`SeamThinkingBudgets`] into the driver's
/// per-level budget map (pi's `ThinkingBudgets`, `Partial<Record<ThinkingLevel,
/// number>>`), keying only the levels the caller set.
fn to_bedrock_thinking_budgets(budgets: &SeamThinkingBudgets) -> ThinkingBudgets {
    let mut map = ThinkingBudgets::new();
    if let Some(minimal) = budgets.minimal {
        map.insert(ThinkingLevel::Minimal, minimal);
    }
    if let Some(low) = budgets.low {
        map.insert(ThinkingLevel::Low, low);
    }
    if let Some(medium) = budgets.medium {
        map.insert(ThinkingLevel::Medium, medium);
    }
    if let Some(high) = budgets.high {
        map.insert(ThinkingLevel::High, high);
    }
    map
}

/// Bridge the generic [`StreamOptions`] onto the driver's [`BedrockOptions`].
///
/// #192 threading: `StreamOptions.temperature` / `max_tokens` flow into the
/// Bedrock `inferenceConfig` (pi's `buildCommandInput` reads
/// `options.temperature` / `options.maxTokens`). `StreamOptions.metadata` is
/// INERT for Bedrock — pi's Bedrock `requestMetadata` is a distinct
/// provider-specific option (`BedrockOptions.request_metadata`), not the generic
/// `StreamOptions.metadata` bag, consistent with the sibling dialects that also
/// leave `metadata` unthreaded.
fn bedrock_options_from(options: Option<&StreamOptions>) -> BedrockOptions {
    let Some(options) = options else {
        return BedrockOptions::default();
    };
    BedrockOptions {
        // The resolved credential (pi's `options.apiKey`) is the bearer token on
        // the API-key path.
        api_key: options.api_key.clone(),
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        cache_retention: options.cache_retention,
        headers: options.headers.as_ref().map(to_provider_headers),
        // Scoped provider-env overrides (AWS_REGION / AWS_BEARER_TOKEN_BEDROCK /
        // credentials), pi's `options.env`.
        env: options.env.clone(),
        ..BedrockOptions::default()
    }
}

/// Convert the seam's string-valued header map into pi's `ProviderHeaders`
/// (`BTreeMap<String, Option<String>>`) by wrapping each present value in
/// `Some`.
fn to_provider_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, Option<String>> {
    headers
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect()
}

/// Re-present a `Model<Value>` as a [`BedrockModel`] via a serde JSON round-trip,
/// so the untyped boundary model is decoded into the lean identity/pricing slice
/// the driver reads.
fn reserialize_model(model: &Model) -> Result<BedrockModel, serde_json::Error> {
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

    use crate::api::bedrock::eventstream::test_support::{encode_event, ScriptedBytesTransport};
    use crate::auth::DefaultAuthContext;
    use crate::providers::registry::{
        create_provider, ApiRouting, CreateProviderOptions, Models, MutableModels, ProviderAuth,
    };
    use crate::seams::clock::FakeClock;
    use crate::seams::storage::MemoryEnv;
    use crate::types::ContentBlock;

    /// A `Hello` ConverseStream turn as real binary eventstream frames.
    fn hello_eventstream() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(encode_event(
            "messageStart",
            &json!({ "role": "assistant" }),
        ));
        bytes.extend(encode_event(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "text": "Hello" } }),
        ));
        bytes.extend(encode_event(
            "contentBlockStop",
            &json!({ "contentBlockIndex": 0 }),
        ));
        bytes.extend(encode_event(
            "messageStop",
            &json!({ "stopReason": "end_turn" }),
        ));
        bytes
    }

    /// A neutral bedrock `Model<Value>` targeting `base_url`.
    fn bedrock_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "name": "Claude 3.5 Sonnet",
            "api": "bedrock-converse-stream",
            "provider": "amazon-bedrock",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
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

    fn scripted_eventstream(body: Vec<u8>) -> (ScriptedBytesTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedBytesTransport::new();
        scripted.push(200, body);
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    fn fake_clock() -> Arc<dyn Clock> {
        Arc::new(FakeClock::new(1_700_000_000_000))
    }

    #[test]
    fn backend_streams_hello_and_threads_bearer_and_url() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());

        let model = bedrock_model("https://bedrock-runtime.us-east-1.amazonaws.com");
        let options = StreamOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
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
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer bedrock-bearer-token")
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());

        let model = bedrock_model("https://bedrock-runtime.us-east-1.amazonaws.com");
        let options = StreamOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            base_url: Some("https://proxy.test".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream"
        );
    }

    #[test]
    fn backend_threads_temperature_and_max_tokens() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());

        let model = bedrock_model("https://bedrock.test");
        let options = StreamOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            temperature: Some(0.7),
            max_tokens: Some(555),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["inferenceConfig"]["temperature"], json!(0.7));
        assert_eq!(body["inferenceConfig"]["maxTokens"], json!(555));
    }

    #[test]
    fn backend_missing_credentials_errors_without_request() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());

        let model = bedrock_model("https://bedrock.test");
        // No bearer token and no AWS credentials anywhere (the backend's ambient
        // env is empty) -> clean no-credentials error, no request sent.
        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("no usable credentials"));
        assert!(scripted.requests().is_empty());
    }

    #[test]
    fn backend_non_2xx_surfaces_error_body() {
        let scripted = ScriptedBytesTransport::new();
        scripted.push(403, b"{\"message\":\"forbidden\"}".to_vec());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = BedrockBackend::new(transport, fake_clock());

        let model = bedrock_model("https://bedrock.test");
        let options = StreamOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Bedrock API error (403): {\"message\":\"forbidden\"}")
        );
        assert_eq!(scripted.requests().len(), 1);
    }

    /// A `Models` collection whose sole provider (`amazon-bedrock`) routes
    /// through the backend over `transport`, resolving auth against `env`.
    fn models_with_bedrock_backend(
        env: MemoryEnv,
        transport: Arc<dyn HttpTransport>,
        model: &Model,
    ) -> Models {
        let mut models = Models::with_auth_context(Arc::new(DefaultAuthContext::new(env)));
        models.set_provider(create_provider(CreateProviderOptions {
            id: "amazon-bedrock".to_string(),
            name: Some("Amazon Bedrock".to_string()),
            base_url: None,
            headers: None,
            auth: ProviderAuth::env_api_key(
                "Amazon Bedrock API key",
                &["AWS_BEARER_TOKEN_BEDROCK"],
            ),
            models: vec![model.clone()],
            fetch_models: None,
            api: ApiRouting::Single(Arc::new(BedrockBackend::new(transport, fake_clock()))),
        }));
        models
    }

    // The `Models::stream` applyAuth path: a configured env key resolves and
    // reaches the outbound request as `authorization: Bearer`, proving the
    // resolved apiKey threads through `StreamOptions.api_key` into the backend.
    #[test]
    fn models_stream_threads_resolved_bearer_to_request() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let model = bedrock_model("https://bedrock-runtime.us-east-1.amazonaws.com");
        let env = MemoryEnv::new().with_env("AWS_BEARER_TOKEN_BEDROCK", "env-bearer-secret");
        let models = models_with_bedrock_backend(env, transport, &model);

        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer env-bearer-secret")
        );
    }

    // -----------------------------------------------------------------------
    // streamSimple: reasoning lowering (pi `bedrock-converse-stream.ts:392`)
    // -----------------------------------------------------------------------

    use crate::types::SimpleStreamOptions;

    /// A reasoning-capable bedrock `Model<Value>` at `model_id`, with the given
    /// context window and output cap so the streamSimple budget/clamp math is
    /// exercisable with exact numbers.
    fn bedrock_reasoning_model(model_id: &str, context_window: u64, max_tokens: u64) -> Model {
        serde_json::from_value(json!({
            "id": model_id,
            "name": model_id,
            "api": "bedrock-converse-stream",
            "provider": "amazon-bedrock",
            "baseUrl": "https://bedrock.test",
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": context_window,
            "maxTokens": max_tokens,
        }))
        .unwrap()
    }

    /// The decoded JSON body of the single request the transport recorded.
    fn request_body(scripted: &ScriptedBytesTransport) -> Value {
        serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap()
    }

    // Adaptive Claude (`anthropic.claude-opus-4-8`) + reasoning `high` -> pi's
    // adaptive sub-branch (`:403-408`): reasoning passes through and the driver
    // emits `thinking.type = "adaptive"` + `output_config.effort` (no budget math).
    #[test]
    fn stream_simple_adaptive_claude_passes_reasoning() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());
        let model =
            bedrock_reasoning_model("anthropic.claude-opus-4-8-20250101-v1:0", 200_000, 32_000);
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("bedrock-bearer-token".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let fields = &request_body(&scripted)["additionalModelRequestFields"];
        assert_eq!(fields["thinking"]["type"], json!("adaptive"));
        assert_eq!(fields["output_config"], json!({ "effort": "high" }));
    }

    // Budget-based (non-adaptive) Claude + reasoning `medium` with an explicit
    // caller cap: pi's `:413-430` path. base = clamp(maxTokens=2000) = 2000;
    // adjust(base=2000, model=32000, medium=8192) -> maxTokens = min(2000+8192,
    // 32000) = 10192, budget = 8192 (fits, no shrink); re-clamp(10192) = 10192;
    // budget override = min(8192, 10192-1024=9168) = 8192. So the request carries
    // `inferenceConfig.maxTokens = 10192` and `thinking.budget_tokens = 8192`.
    #[test]
    fn stream_simple_budget_claude_adjusts_max_tokens_and_budget() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());
        let model =
            bedrock_reasoning_model("anthropic.claude-3-5-sonnet-20241022-v2:0", 200_000, 32_000);
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("bedrock-bearer-token".to_string()),
                max_tokens: Some(2000),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::Medium),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body = request_body(&scripted);
        assert_eq!(body["inferenceConfig"]["maxTokens"], json!(10192));
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"]["type"],
            json!("enabled")
        );
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"]["budget_tokens"],
            json!(8192)
        );
    }

    // Budget-based Claude where the override `min(adjusted, maxTokens-1024)` bites:
    // model cap 16000, custom `medium` budget 20000, no caller cap. base =
    // clamp(model.maxTokens=16000) = 16000; adjust -> maxTokens = min(16000+20000,
    // 16000) = 16000, and since 16000 <= 20000 the helper shrinks budget to
    // max(0, 16000-1024) = 14976; re-clamp(16000) = 16000; override = min(14976,
    // 16000-1024=14976) = 14976. So `budget_tokens = 14976`, `maxTokens = 16000`.
    #[test]
    fn stream_simple_budget_claude_override_clamps_to_max_tokens_minus_1024() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());
        let model =
            bedrock_reasoning_model("anthropic.claude-3-5-sonnet-20241022-v2:0", 200_000, 16_000);
        let budgets = SeamThinkingBudgets {
            medium: Some(20_000),
            ..SeamThinkingBudgets::default()
        };
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("bedrock-bearer-token".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::Medium),
            Some(budgets),
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body = request_body(&scripted);
        assert_eq!(body["inferenceConfig"]["maxTokens"], json!(16000));
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"]["budget_tokens"],
            json!(14976)
        );
    }

    // Non-Claude reasoning model (`meta.llama...`) + reasoning `high`: pi's
    // non-Claude passthrough (`:433-437`). `reasoning` is present but the driver
    // ignores it (`build_additional_model_request_fields` returns None for
    // non-Claude), so NO `additionalModelRequestFields` reaches the request.
    #[test]
    fn stream_simple_non_claude_passthrough_omits_thinking_config() {
        let (scripted, transport) = scripted_eventstream(hello_eventstream());
        let backend = BedrockBackend::new(transport, fake_clock());
        let model = bedrock_reasoning_model("meta.llama3-1-405b-instruct-v1:0", 128_000, 8_192);
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("bedrock-bearer-token".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body = request_body(&scripted);
        assert!(body.get("additionalModelRequestFields").is_none());
    }

    // NO-REASONING zero-regression: `stream_simple` with `reasoning: None` builds
    // a request byte-identical to the raw `stream` path -- no thinking config is
    // added. Guards the None -> base mapping.
    #[test]
    fn stream_simple_without_reasoning_matches_raw_stream() {
        let model = bedrock_model("https://bedrock.test");
        let base = StreamOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            ..StreamOptions::default()
        };

        let (raw_scripted, raw_transport) = scripted_eventstream(hello_eventstream());
        let raw_backend = BedrockBackend::new(raw_transport, fake_clock());
        raw_backend.stream(&model, &user_context(), Some(&base), None);

        let (simple_scripted, simple_transport) = scripted_eventstream(hello_eventstream());
        let simple_backend = BedrockBackend::new(simple_transport, fake_clock());
        let simple = SimpleStreamOptions::from_base(base.clone());
        simple_backend.stream_simple(&model, &user_context(), Some(&simple), None);

        assert_eq!(request_body(&raw_scripted), request_body(&simple_scripted));
        assert!(request_body(&simple_scripted)
            .get("additionalModelRequestFields")
            .is_none());
    }

    // Unconfigured provider: applyAuth gates before dispatch with the exact
    // "Provider is not configured" error, no panic and no network request.
    #[test]
    fn models_stream_unconfigured_provider_errors_without_request() {
        let scripted = ScriptedBytesTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let model = bedrock_model("https://bedrock.test");
        let models = models_with_bedrock_backend(MemoryEnv::new(), transport, &model);

        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Provider is not configured: amazon-bedrock")
        );
        assert!(scripted.requests().is_empty());
    }
}
