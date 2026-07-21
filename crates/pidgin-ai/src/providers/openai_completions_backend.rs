//! The OpenAI Chat Completions [`Provider`] backend: the transport-aware adapter
//! that binds the ported `openai-completions` driver into the provider registry's
//! [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request shaping, header assembly, and SSE decode — is
//! already ported at [`crate::api::openai_completions`] (the pure request/response
//! halves) and [`crate::api::openai_completions::driver`] (the transport-driving
//! request assembler + stream driver). This module is pure wiring: it adapts the
//! generic [`Provider`] seam (which speaks [`Model<Value>`] and [`StreamOptions`])
//! onto the driver's typed [`stream`](crate::api::openai_completions::driver::stream)
//! entry point (which speaks [`Model<OpenAICompletionsCompat>`] and
//! [`OpenAICompletionsOptions`]), threading an injected [`HttpTransport`] and
//! [`Clock`] so a live completions turn runs without wall-clock or ambient-network
//! access. Its shape mirrors [`crate::providers::anthropic_backend`].

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `Model<Value>` -> typed
// reserialize mirror the identical shells in the anthropic backend and the
// registry by design; the clone detector reads the shared boundary-type
// construction as duplicative.

use std::sync::Arc;

use crate::api::openai_completions::driver;
use crate::api::openai_completions::OpenAICompletionsOptions;
use crate::providers::clamp_thinking_level;
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    OpenAICompletionsCompat, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Usage,
    UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `openai-completions` [`Api`] discriminant.
///
/// Registering this in
/// [`backend_for_api`](crate::providers::builtins) auto-binds every
/// completions-only provider (deepseek / groq / cerebras / openrouter / ...) to
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) and contributes
/// the completions leg to mixed-dialect (`ByApi`) providers.
pub const OPENAI_COMPLETIONS_API: &str = "openai-completions";

/// A [`Provider`] backend that runs an OpenAI chat-completions turn over an
/// injected [`HttpTransport`], sourcing the request timestamp from an injected
/// [`Clock`].
///
/// Constructed via [`OpenAICompletionsBackend::new`] and installed by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct OpenAICompletionsBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl OpenAICompletionsBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl Provider for OpenAICompletionsBackend {
    fn api(&self) -> &str {
        OPENAI_COMPLETIONS_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's typed
        // `Model<OpenAICompletionsCompat>` via serde. A malformed compat blob is
        // surfaced as a clean pre-start error event, never a panic.
        let mut typed_model: Model<OpenAICompletionsCompat> = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("OpenAI model is not compatible with openai-completions: {error}"),
                )
            }
        };

        // A per-request base-URL override targets the driver's `request_url`
        // (`{base_url}/chat/completions`) at the right host. `applyAuth` has
        // already applied any per-credential `auth.baseUrl` onto `model.base_url`;
        // this honors an explicit `StreamOptions.base_url` on top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let openai_options = map_options(&typed_model, options);

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against; `signal` is accepted for
        // seam parity and left unobserved here (matching the anthropic backend).
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &openai_options,
            timestamp,
        )
    }

    fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        // Same model/options assembly as `stream`, but the request runs through the
        // driver's incremental `stream_streaming` entry point: the returned reader
        // pulls one chunk at a time off the transport, so a streaming transport
        // surfaces real per-frame timing while the buffered `stream` path is left
        // untouched. Mirrors the anthropic backend's override.
        let mut typed_model: Model<OpenAICompletionsCompat> = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                return AssistantEventReader::from_buffered(error_result(
                    model,
                    self.clock.now_ms(),
                    format!("OpenAI model is not compatible with openai-completions: {error}"),
                ));
            }
        };

        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let openai_options = map_options(&typed_model, options);

        let timestamp = self.clock.now_ms();
        driver::stream_streaming(
            self.transport.as_ref(),
            &typed_model,
            context,
            &openai_options,
            timestamp,
        )
    }

    /// Lower the simple, level-based `reasoning` onto the completions request as
    /// `reasoning_effort`, mirroring pi's `streamSimple`
    /// (`openai-completions.ts:513-530`): clamp the requested level to the model's
    /// supported ladder via `clampThinkingLevel`, drop a clamp to `off` (⇒ no
    /// `reasoning_effort`), and hand the effort to the driver, whose
    /// `thinkingFormat` switch (`openai-completions.ts:638-712`) shapes the per-
    /// provider field (zai / qwen / deepseek / openrouter / together / ...).
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

        let mut typed_model: Model<OpenAICompletionsCompat> = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("OpenAI model is not compatible with openai-completions: {error}"),
                )
            }
        };

        if let Some(base_url) = simple.base.base_url.as_ref() {
            typed_model.base_url = base_url.clone();
        }

        let mut openai_options = map_options(&typed_model, Some(&simple.base));
        // pi `openai-completions.ts:521-522`:
        //   clampedReasoning = clampThinkingLevel(model, options.reasoning)
        //   reasoningEffort  = clampedReasoning === "off" ? undefined : clampedReasoning
        openai_options.reasoning_effort = simple.reasoning.and_then(|level| {
            to_thinking_level(clamp_thinking_level(
                &typed_model,
                to_model_thinking_level(level),
            ))
        });

        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &openai_options,
            timestamp,
        )
    }
}

/// Widen a caller's [`ThinkingLevel`] to the model-level [`ModelThinkingLevel`]
/// pi's `clampThinkingLevel` expects (a requested level is never `off`).
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

/// Narrow a clamped [`ModelThinkingLevel`] back to the driver's [`ThinkingLevel`],
/// dropping `off` (pi's `reasoningEffort = clamped === "off" ? undefined : clamped`).
fn to_thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// Map the generic [`StreamOptions`] onto the driver's typed
/// [`OpenAICompletionsOptions`], shared by both `stream` and `stream_incremental` so
/// the two paths thread identical request-shaping inputs.
///
/// # `#192` precedence
///
/// - `max_tokens`: an explicit [`StreamOptions::max_tokens`] wins; otherwise the
///   model's own `max_tokens` is the default. pi writes `max_tokens` /
///   `max_completion_tokens` only from `options?.maxTokens`, and this port keeps the
///   model default as the floor so a caller that supplies none still bounds output.
/// - `temperature`: threaded straight from [`StreamOptions::temperature`] when set;
///   pi writes `temperature` only when `options?.temperature !== undefined`, so a
///   `None` here (as before) emits no `temperature` field at all.
/// - `metadata`: pi's `openai-completions` request shaper reads no `metadata`
///   fields (only `anthropic-messages` maps `metadata.user_id`), so it is
///   intentionally NOT threaded here -- emitting it would diverge from pi and break
///   conformance.
fn map_options(
    typed_model: &Model<OpenAICompletionsCompat>,
    options: Option<&StreamOptions>,
) -> OpenAICompletionsOptions {
    OpenAICompletionsOptions {
        max_tokens: options
            .and_then(|o| o.max_tokens)
            .or(Some(typed_model.max_tokens)),
        temperature: options.and_then(|o| o.temperature),
        cache_retention: options.and_then(|o| o.cache_retention),
        session_id: options.and_then(|o| o.session_id.clone()),
        api_key: options.and_then(|o| o.api_key.clone()),
        headers: options.and_then(|o| o.headers.clone()),
        // pi's `getProviderEnvValue("PI_CACHE_RETENTION", env)` — the only env value
        // the request shaper reads.
        cache_retention_env: options.and_then(|o| {
            o.env
                .as_ref()
                .and_then(|env| env.get("PI_CACHE_RETENTION").cloned())
        }),
        ..OpenAICompletionsOptions::default()
    }
}

/// Re-present a `Model<Value>` as a `Model<OpenAICompletionsCompat>` via a serde
/// JSON round-trip, so the untyped `compat` blob is decoded into the typed
/// OpenAI-completions compat map the driver reads.
fn reserialize_model(model: &Model) -> Result<Model<OpenAICompletionsCompat>, serde_json::Error> {
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
    use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, ScriptedTransport};
    use crate::types::{ContentBlock, StopReason};
    use std::collections::BTreeMap;
    use std::io;
    use std::time::{Duration, Instant};

    /// A minimal OpenAI-style streaming completion body: a text delta then a
    /// `stop` finish with usage, terminated by `[DONE]`. The frame shape is
    /// derived from the `parse_sse_chunks` fixtures in `openai_completions/tests.rs`
    /// (`parse_sse_chunks_decodes_data_lines_and_stops_at_done`).
    fn hello_sse_body() -> String {
        concat!(
            "data: {\"id\":\"chatcmpl-hello\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-hello\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string()
    }

    /// A neutral non-reasoning openai-completions `Model<Value>` targeting
    /// `base_url`. The backend re-serializes this into
    /// `Model<OpenAICompletionsCompat>`.
    fn openai_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gpt-4o-mini",
            "name": "GPT-4o mini",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 4096,
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
    // fixture yields a single "Hello" text block, and (b) the request the backend
    // built carries `Authorization: Bearer <key>`, `Content-Type: application/json`,
    // and the `/chat/completions` URL.
    #[test]
    fn backend_streams_hello_and_sets_sdk_headers() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
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
            "https://api.openai.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-test-key")
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
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            base_url: Some("https://proxy.test/v1".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1/chat/completions"
        );
    }

    // (c) The getClientApiKey fallback: with no api_key but a caller-supplied
    // `authorization` header, the apiKey becomes the "unused" sentinel and no
    // Bearer is minted over the caller's credential — the caller header reaches
    // the wire verbatim.
    #[test]
    fn backend_caller_authorization_header_suppresses_bearer() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer caller-token".to_string(),
        );
        let options = StreamOptions {
            // No api_key: the caller's authorization header stands in for it.
            headers: Some(headers),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        // The caller header wins; the "unused" sentinel never mints a Bearer over it.
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer caller-token")
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
            body: json!({ "error": { "message": "Invalid API key" } }).to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-bad-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("401 Invalid API key")
        );
        assert_eq!(scripted.requests().len(), 1);
    }

    // A model whose compat blob cannot decode into `OpenAICompletionsCompat`
    // surfaces a clean pre-start error event, never a panic, and never a request.
    #[test]
    fn backend_incompatible_model_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let mut model = openai_model("https://api.openai.test/v1");
        // `supportsStore` is a bool flag; a number here fails the typed decode.
        model.compat = Some(json!({ "supportsStore": 12345 }) as Value);

        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(
            result.events[0],
            AssistantMessageEvent::Error { .. }
        ));
        assert!(scripted.requests().is_empty());
    }

    /// A transport whose `send_streaming` splits the SSE body into one chunk per
    /// frame and sleeps `delay` before yielding each, so the reader's per-frame PULL
    /// timing is observable. Its buffered `send` returns the whole body. Mirrors the
    /// anthropic backend's `SleepingStreamTransport`.
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

    // Incremental over the one-chunk ScriptedTransport (default `send_streaming`)
    // yields the SAME events and final message as the buffered `stream`, and builds
    // the same threaded request -- proving the buffered and streaming paths share one
    // decoder and are byte-identical.
    #[test]
    fn backend_stream_incremental_matches_buffered_over_scripted() {
        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let (_scripted_buffered, transport_buffered) = scripted_hello();
        let backend_buffered = OpenAICompletionsBackend::new(transport_buffered, fake_clock());
        let buffered = backend_buffered.stream(&model, &user_context(), Some(&options), None);

        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());
        let mut reader = backend.stream_incremental(&model, &user_context(), Some(&options), None);
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
            "https://api.openai.test/v1/chat/completions"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer sk-test-key")
        );
    }

    // Over a per-frame sleeping transport, the yielded events span multiple sleeping
    // chunks -- non-zero inter-event spread -- while resolving to the same "Hello"
    // message as the buffered path.
    #[test]
    fn backend_stream_incremental_streams_with_inter_event_spread() {
        let delay = Duration::from_millis(12);
        let transport: Arc<dyn HttpTransport> = Arc::new(SleepingStreamTransport {
            body: hello_sse_body(),
            delay,
        });
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());
        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(&model, &user_context(), Some(&options), None);
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
                text: "Hello".to_string(),
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

    // #192: an explicit StreamOptions `temperature` and `max_tokens` thread into the
    // outgoing request body (temperature verbatim, max_tokens overriding the model
    // default). `metadata` is NOT threaded -- pi's openai-completions shaper reads
    // none, so the request carries no `metadata` field.
    #[test]
    fn stream_options_thread_temperature_and_max_tokens_into_request_body() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        // The model default is maxTokens: 4096; StreamOptions overrides it with 512.
        let model = openai_model("https://api.openai.test/v1");
        let mut metadata = BTreeMap::new();
        metadata.insert("user_id".to_string(), json!("u-123"));
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            temperature: Some(0.42),
            max_tokens: Some(512),
            metadata: Some(metadata),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_str(
            requests[0]
                .body
                .as_deref()
                .expect("request carries a JSON body"),
        )
        .expect("request body is valid JSON");

        // gpt-4o-mini's compat uses `max_completion_tokens`; StreamOptions.max_tokens
        // (512) overrides the model default (4096).
        assert_eq!(
            body.get("max_completion_tokens").and_then(Value::as_u64),
            Some(512)
        );
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body.get("temperature").and_then(Value::as_f64), Some(0.42));
        // Metadata is intentionally not mapped for openai-completions (pi parity).
        assert!(body.get("metadata").is_none());
    }

    // #192: with no StreamOptions overrides, max_tokens falls back to the model
    // default and no `temperature` is emitted -- the pre-#192 buffered behaviour.
    #[test]
    fn request_body_falls_back_to_model_max_tokens_without_stream_options() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let requests = scripted.requests();
        let body: Value = serde_json::from_str(requests[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body.get("max_completion_tokens").and_then(Value::as_u64),
            Some(4096)
        );
        assert!(body.get("temperature").is_none());
    }

    // -----------------------------------------------------------------------
    // stream_simple: reasoning lowering (pi `openai-completions.ts:513-530`)
    // -----------------------------------------------------------------------

    /// A reasoning-capable openai-completions `Model<Value>` on the default
    /// OpenAI-style `thinkingFormat` (provider `openai`, no thinking_level_map),
    /// targeting `base_url`.
    fn openai_reasoning_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "o3-mini",
            "name": "o3 mini",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 4096,
        }))
        .unwrap()
    }

    /// A reasoning-capable openrouter model (`thinkingFormat = openrouter`), so the
    /// driver shapes reasoning as the nested `reasoning: { effort }` object.
    fn openrouter_reasoning_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "some/reasoner",
            "name": "OpenRouter Reasoner",
            "api": "openai-completions",
            "provider": "openrouter",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 4096,
        }))
        .unwrap()
    }

    // Reasoning `high` on a default OpenAI-style reasoning model lowers to
    // `reasoning_effort: "high"` in the outgoing body (pi `openai-completions.ts:521-522`
    // clamp + drop-off, then the driver's default `reasoning_effort` branch
    // `:704-706`).
    #[test]
    fn stream_simple_reasoning_sets_reasoning_effort() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());
        let model = openai_reasoning_model("https://api.openai.test/v1");
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("sk-test-key".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["reasoning_effort"], json!("high"));
    }

    // A lower level (`low`) threads through the same path to `reasoning_effort: "low"`.
    #[test]
    fn stream_simple_reasoning_low_sets_reasoning_effort() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());
        let model = openai_reasoning_model("https://api.openai.test/v1");
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("sk-test-key".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::Low),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["reasoning_effort"], json!("low"));
    }

    // A `thinkingFormat = openrouter` model shapes the same lowered effort as the
    // nested `reasoning: { effort: "high" }` object (pi `openai-completions.ts:673-682`
    // / driver `:1300-1311`), asserted param-exact.
    #[test]
    fn stream_simple_reasoning_openrouter_variant() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAICompletionsBackend::new(transport, fake_clock());
        let model = openrouter_reasoning_model("https://openrouter.ai/api/v1");
        let simple = SimpleStreamOptions::new(
            StreamOptions {
                api_key: Some("sk-test-key".to_string()),
                ..StreamOptions::default()
            },
            Some(ThinkingLevel::High),
            None,
        );
        backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let body: Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        // The nested-object shape supersedes the flat field for this format.
        assert!(body.get("reasoning_effort").is_none());
    }

    // A reasoning level requested against a NON-reasoning model clamps to `off`
    // (pi `clampThinkingLevel` over `["off"]`), so `reasoningEffort` becomes
    // `undefined` and no reasoning field is emitted -- byte-identical to the raw
    // `stream` path.
    #[test]
    fn stream_simple_off_clamp_omits_reasoning_effort() {
        let model = openai_model("https://api.openai.test/v1");
        let base = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let (raw_scripted, raw_transport) = scripted_hello();
        let raw_backend = OpenAICompletionsBackend::new(raw_transport, fake_clock());
        raw_backend.stream(&model, &user_context(), Some(&base), None);

        let (simple_scripted, simple_transport) = scripted_hello();
        let simple_backend = OpenAICompletionsBackend::new(simple_transport, fake_clock());
        let simple = SimpleStreamOptions::new(base.clone(), Some(ThinkingLevel::High), None);
        simple_backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let raw_body: Value =
            serde_json::from_str(raw_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        let simple_body: Value =
            serde_json::from_str(simple_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(raw_body, simple_body);
        assert!(simple_body.get("reasoning_effort").is_none());
        assert!(simple_body.get("reasoning").is_none());
    }

    // NO-REASONING zero-regression: `stream_simple` with `reasoning: None` builds a
    // request byte-identical to the raw `stream` path -- no `reasoning_effort` is
    // added.
    #[test]
    fn stream_simple_without_reasoning_matches_raw_stream() {
        let model = openai_reasoning_model("https://api.openai.test/v1");
        let base = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let (raw_scripted, raw_transport) = scripted_hello();
        let raw_backend = OpenAICompletionsBackend::new(raw_transport, fake_clock());
        raw_backend.stream(&model, &user_context(), Some(&base), None);

        let (simple_scripted, simple_transport) = scripted_hello();
        let simple_backend = OpenAICompletionsBackend::new(simple_transport, fake_clock());
        let simple = SimpleStreamOptions::from_base(base.clone());
        simple_backend.stream_simple(&model, &user_context(), Some(&simple), None);

        let raw_body: Value =
            serde_json::from_str(raw_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        let simple_body: Value =
            serde_json::from_str(simple_scripted.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(raw_body, simple_body);
        assert!(simple_body.get("reasoning_effort").is_none());
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving a minimal OpenAI SSE completion and
/// drives the backend through [`ReqwestTransport`] with `.no_proxy()` (required in
/// the sandbox). Mirrors the anthropic backend's loopback test.
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
        concat!(
            "data: {\"id\":\"chatcmpl-hello\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-hello\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string()
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
        let base_url = format!("http://{}/v1", listener.local_addr().expect("local addr"));

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
        let backend = OpenAICompletionsBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "gpt-4o-mini",
            "name": "GPT-4o mini",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 4096,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
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

    // Drives `stream_incremental` over the real reqwest transport against a server
    // that writes each SSE frame with a bounded sleep between flushes: the events the
    // consumer pulls off the reader arrive spaced (non-zero inter-event spread),
    // proving the streaming path delivers per-frame rather than all-at-once, while
    // resolving to the same "Hello" message.
    #[test]
    fn backend_stream_incremental_over_reqwest_loopback_has_spread() {
        let delay = Duration::from_millis(20);
        let frames: Vec<String> = hello_sse_body()
            .split_inclusive("\n\n")
            .filter(|part| !part.trim().is_empty())
            .map(str::to_string)
            .collect();
        let total: usize = frames.iter().map(String::len).sum();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("local addr"));

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total}\r\n\r\n",
            );
            stream.write_all(head.as_bytes()).expect("write head");
            stream.flush().ok();
            for frame in &frames {
                thread::sleep(delay);
                stream.write_all(frame.as_bytes()).expect("write frame");
                stream.flush().ok();
            }
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = OpenAICompletionsBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "gpt-4o-mini",
            "name": "GPT-4o mini",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 4096,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(&model, &context, Some(&options), None);
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
                text: "Hello".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread from delayed chunked streaming, got {spread:?}",
        );
    }
}
