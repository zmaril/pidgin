//! The OpenAI **Responses API** [`Provider`] backend: the transport-aware adapter
//! that binds the ported `openai-responses` driver into the provider registry's
//! [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request shaping, header assembly, and SSE decode — is
//! already ported at [`crate::api::openai_responses`] (the pure request half plus
//! [`crate::api::openai_responses_shared`]'s stream decoder) and
//! [`crate::api::openai_responses::driver`] (the transport-driving request
//! assembler + buffered/streaming drivers). This module is pure wiring: it adapts
//! the generic [`Provider`] seam (which speaks [`Model<Value>`] and
//! [`StreamOptions`]) onto the driver's typed
//! [`stream`](crate::api::openai_responses::driver::stream) /
//! [`stream_streaming`](crate::api::openai_responses::driver::stream_streaming)
//! entry points (which speak [`Model<OpenAIResponsesCompat>`] and
//! [`OpenAIResponsesOptions`]), threading an injected [`HttpTransport`] and
//! [`Clock`] so a live responses turn runs without wall-clock or ambient-network
//! access. Its shape mirrors [`crate::providers::anthropic_backend`].

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `Model<Value>` -> typed
// reserialize mirror the identical shells in the anthropic / openai-completions
// backends and the registry by design; the clone detector reads the shared
// boundary-type construction as duplicative.

use std::sync::Arc;

use crate::api::openai_responses::driver;
use crate::api::openai_responses::OpenAIResponsesOptions;
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, OpenAIResponsesCompat,
    StopReason, StreamOptions, Usage, UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `openai-responses` [`Api`] discriminant.
///
/// Registering this in [`backend_for_api`](crate::providers::builtins) binds the
/// single-dialect `openai` provider to
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) and contributes
/// the responses leg to mixed-dialect (`ByApi`) providers (cloudflare / copilot /
/// opencode / xai).
pub const OPENAI_RESPONSES_API: &str = "openai-responses";

/// A [`Provider`] backend that runs an OpenAI Responses turn over an injected
/// [`HttpTransport`], sourcing the request timestamp from an injected [`Clock`].
///
/// Constructed via [`OpenAIResponsesBackend::new`] and installed by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct OpenAIResponsesBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl OpenAIResponsesBackend {
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
                format!("OpenAI model is not compatible with openai-responses: {error}")
            })?;

        // A per-request base-URL override targets the driver's `request_url`
        // (`{base_url}/responses`) at the right host. `applyAuth` has already
        // applied any per-credential `auth.baseUrl` onto `model.base_url`; this
        // honors an explicit `StreamOptions.base_url` on top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }
        Ok(typed_model)
    }
}

/// Map the generic [`StreamOptions`] onto the driver's typed
/// [`OpenAIResponsesOptions`].
///
/// # #192 threading
///
/// pi's Responses `buildParams` reads `options.maxTokens` and `options.temperature`
/// directly off the (StreamOptions-extending) options — there is no model-default
/// fallback for either in the Responses shaper (`openai-responses.ts:250-255`), so
/// both flow straight from [`StreamOptions`]. pi's Responses shaper does **not**
/// map `metadata` (unlike some dialects), so `StreamOptions.metadata` is
/// deliberately dropped here to match pi's Responses behavior.
fn responses_options(options: Option<&StreamOptions>) -> OpenAIResponsesOptions {
    OpenAIResponsesOptions {
        // #192: StreamOptions overrides the model; the Responses shaper reads
        // options.maxTokens / options.temperature with no model fallback.
        max_tokens: options.and_then(|o| o.max_tokens),
        temperature: options.and_then(|o| o.temperature),
        cache_retention: options.and_then(|o| o.cache_retention),
        session_id: options.and_then(|o| o.session_id.clone()),
        api_key: options.and_then(|o| o.api_key.clone()),
        headers: options.and_then(|o| o.headers.clone()),
        ..OpenAIResponsesOptions::default()
    }
}

impl Provider for OpenAIResponsesBackend {
    fn api(&self) -> &str {
        OPENAI_RESPONSES_API
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

    fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
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
/// OpenAI-responses compat map the driver reads.
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

    /// A minimal OpenAI Responses streaming body: a `response.created`, a text
    /// item lifecycle with two deltas, and a `response.completed` terminal. Each
    /// frame carries the real `event:` name plus the `data:` JSON the decoder
    /// dispatches on. The event shapes mirror `openai_responses/tests.rs`'s
    /// `full_text_lifecycle_event_ordering` fixture.
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

    /// A neutral non-reasoning openai-responses `Model<Value>` targeting
    /// `base_url`. The backend re-serializes this into
    /// `Model<OpenAIResponsesCompat>`.
    fn openai_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gpt-5",
            "name": "GPT-5",
            "api": "openai-responses",
            "provider": "openai",
            "baseUrl": base_url,
            "reasoning": false,
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
    // backend built carries `Authorization: Bearer <key>`, `Content-Type:
    // application/json`, and the `/responses` URL.
    #[test]
    fn backend_streams_hello_and_sets_sdk_headers() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

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
                text: "Hello world".to_string(),
                text_signature: Some(r#"{"v":1,"id":"msg_1"}"#.to_string()),
            }]
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].url, "https://api.openai.test/v1/responses");
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
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            base_url: Some("https://proxy.test/v1".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1/responses"
        );
    }

    // (c) The getClientApiKey fallback: with no api_key but a caller-supplied
    // `authorization` header, the apiKey becomes the "unused" sentinel and no
    // Bearer is minted over the caller's credential — the caller header reaches
    // the wire verbatim.
    #[test]
    fn backend_caller_authorization_header_suppresses_bearer() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

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

    // #192: StreamOptions.temperature / max_tokens land in the outgoing request
    // body (`temperature` and `max_output_tokens`, the latter clamped to the
    // Responses minimum by the shaper).
    #[test]
    fn backend_threads_temperature_and_max_tokens_into_body() {
        let (scripted, transport) = scripted_hello();
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
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
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());
        let model = openai_model("https://api.openai.test/v1");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            ..StreamOptions::default()
        };

        let buffered = backend.stream(&model, &user_context(), Some(&options), None);

        let (_scripted2, transport2) = scripted_hello();
        let backend2 = OpenAIResponsesBackend::new(transport2, fake_clock());
        let mut reader = backend2.stream_incremental(&model, &user_context(), Some(&options), None);
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
            body: json!({ "error": { "message": "Invalid API key" } }).to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

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

    // A model whose compat blob cannot decode into `OpenAIResponsesCompat`
    // surfaces a clean pre-start error event, never a panic, and never a request.
    #[test]
    fn backend_incompatible_model_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = OpenAIResponsesBackend::new(transport, fake_clock());

        let mut model = openai_model("https://api.openai.test/v1");
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
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving a minimal OpenAI Responses SSE reply
/// in delayed chunks and drives the backend's incremental path through
/// [`ReqwestTransport`] with `.no_proxy()`, asserting real inter-event spacing.
/// Mirrors the sibling backends' loopback tests.
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
        let backend = OpenAIResponsesBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "gpt-5",
            "name": "GPT-5",
            "api": "openai-responses",
            "provider": "openai",
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
        use crate::api::openai_responses::driver;
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
            "id": "gpt-5",
            "name": "GPT-5",
            "api": "openai-responses",
            "provider": "openai",
            "baseUrl": "https://api.openai.test/v1",
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
        let options = OpenAIResponsesOptions {
            api_key: Some("sk-test-key".to_string()),
            ..OpenAIResponsesOptions::default()
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
