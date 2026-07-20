//! The Google Generative AI [`Provider`] backend: the transport-aware adapter
//! that binds the ported `google-generative-ai` driver into the provider
//! registry's [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request assembly, `x-goog-api-key` header injection,
//! and the shared Google stream decode — is already ported at
//! [`crate::api::google_generative_ai`] and [`crate::api::google_shared`]. This
//! module is pure wiring: it adapts the generic [`Provider`] seam (which speaks
//! [`Model<Value>`](crate::types::Model) and [`StreamOptions`]) onto the driver's
//! typed [`stream`](crate::api::google_generative_ai::driver::stream) entry point
//! (which speaks [`GoogleModel`] and [`GoogleRequestOptions`]), threading an
//! injected [`HttpTransport`] and [`Clock`] so a live Gemini turn runs without
//! wall-clock or ambient-network access.

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `reserialize_model` /
// `StreamOptions` bridging mirror the identical wiring in the anthropic backend
// by design; the clone detector reads the shared boundary-type construction as
// duplicative.

use std::sync::Arc;

use crate::api::google_generative_ai::driver;
use crate::api::google_generative_ai::API;
use crate::api::google_shared::{GoogleModel, GoogleRequestOptions};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, StopReason,
    StreamOptions, Usage, UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `google-generative-ai` [`Api`] discriminant.
pub const GOOGLE_GENERATIVE_AI_API: &str = API;

/// A [`Provider`] backend that runs a Google Generative AI (direct Gemini) turn
/// over an injected [`HttpTransport`], sourcing the request timestamp from an
/// injected [`Clock`].
///
/// Constructed via [`GoogleGenerativeAiBackend::new`] and installed as
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct GoogleGenerativeAiBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl GoogleGenerativeAiBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl Provider for GoogleGenerativeAiBackend {
    fn api(&self) -> &str {
        GOOGLE_GENERATIVE_AI_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's typed
        // `GoogleModel` via serde. A malformed model is surfaced as a clean
        // pre-start error event, never a panic.
        let mut typed_model: GoogleModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Google model is not compatible with google-generative-ai: {error}"),
                )
            }
        };

        // A per-request base-URL override targets the driver's request URL
        // (`{base_url}/models/{model}:streamGenerateContent`) at the right host.
        // `applyAuth` has already applied any per-credential `auth.baseUrl` onto
        // `model.base_url`; this honors an explicit `StreamOptions.base_url` on
        // top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        // Google authenticates with `x-goog-api-key`, not a Bearer token; the
        // resolved `StreamOptions.api_key` threads into that header inside the
        // driver's request assembler.
        let api_key = options.and_then(|o| o.api_key.clone());
        let headers = options.and_then(|o| o.headers.clone()).unwrap_or_default();
        let request_options = request_options_from(model, options);

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against (pi aborts an async SSE
        // read); `signal` is accepted for seam parity and left unobserved here.
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            api_key.as_deref(),
            &headers,
            &request_options,
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
        let mut typed_model: GoogleModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                return AssistantEventReader::from_buffered(error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Google model is not compatible with google-generative-ai: {error}"),
                ));
            }
        };

        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let api_key = options.and_then(|o| o.api_key.clone());
        let headers = options.and_then(|o| o.headers.clone()).unwrap_or_default();
        let request_options = request_options_from(model, options);

        let timestamp = self.clock.now_ms();
        driver::stream_streaming(
            self.transport.as_ref(),
            &typed_model,
            context,
            api_key.as_deref(),
            &headers,
            &request_options,
            timestamp,
        )
    }
}

/// Map [`StreamOptions`] onto the driver's [`GoogleRequestOptions`], threading the
/// #192 request-shaping fields with pi's Google precedence.
///
/// pi's Google `buildParams` (`google-generative-ai.ts:343`) reads only
/// `options.temperature` and `options.maxTokens`, mapping them into
/// `generationConfig.temperature` / `generationConfig.maxOutputTokens`; there is
/// no model temperature in pi's Google request. Precedence here mirrors that:
/// - `temperature` comes solely from `StreamOptions.temperature` (pi has no model
///   default to fall back to).
/// - `max_tokens` prefers `StreamOptions.max_tokens`; when the caller omits it we
///   fall back to the model's `maxTokens` default (`> 0`), the pidgin seam's
///   stand-in for the `streamSimple`/`buildBaseOptions` layer pi fills it from.
/// - `metadata` is intentionally NOT threaded: the Google dialect never consumes
///   it in pi (only anthropic reads `metadata.user_id`), so mapping it into the
///   request would diverge from pi.
///
/// `model` is the boundary [`Model`] (carrying the `maxTokens` default); the
/// per-request shaping fields come from `options`.
fn request_options_from(model: &Model, options: Option<&StreamOptions>) -> GoogleRequestOptions {
    let temperature = options.and_then(|o| o.temperature);
    let max_tokens = options
        .and_then(|o| o.max_tokens)
        .or_else(|| (model.max_tokens > 0).then_some(model.max_tokens));
    GoogleRequestOptions {
        temperature,
        max_tokens,
        tool_choice: None,
        thinking: None,
        aborted: false,
    }
}

/// Re-present a `Model<Value>` as a [`GoogleModel`] via a serde JSON round-trip,
/// decoding the lenient Google model slice the driver reads.
fn reserialize_model(model: &Model) -> Result<GoogleModel, serde_json::Error> {
    let json = serde_json::to_value(model)?;
    serde_json::from_value(json)
}

/// A single-`error`-event result for a failure before the driver's stream start
/// (an undecodable model), matching the registry's and driver's pre-start error
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
pub(crate) fn hello_sse_body() -> String {
    // One `?alt=sse` frame: a single "Hello" text part with a STOP finish and
    // usage metadata. Mirrors the shape the Gemini streamGenerateContent endpoint
    // returns and the ported decoder's own fixtures.
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\
\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\
\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n"
        .to_string()
}

#[cfg(test)]
pub(crate) fn multi_frame_hello_sse_body() -> String {
    // Three `?alt=sse` frames streaming "He" / "llo" / "!" as text deltas into a
    // single accumulating block, the last carrying the STOP finish + usage. Split
    // per frame it exercises multi-chunk incremental timing.
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"He\"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"llo\"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\
\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\
\"candidatesTokenCount\":3,\"totalTokenCount\":4}}\n\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use std::collections::BTreeMap;
    use std::io;
    use std::time::{Duration, Instant};

    use crate::seams::clock::FakeClock;
    use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, ScriptedTransport};
    use crate::types::ContentBlock;

    /// A neutral google `Model<Value>` targeting `base_url`. The backend
    /// re-serializes this into [`GoogleModel`].
    fn google_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-generative-ai",
            "provider": "google",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
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

    // (a) Drives the backend end to end through ScriptedTransport (default
    // features): the `hello` fixture yields a single "Hello" text block.
    #[test]
    fn backend_streams_hello() {
        let (_scripted, transport) = scripted_hello();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
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
    }

    // (b) The request carries `x-goog-api-key: <key>` and the
    // `:streamGenerateContent?alt=sse` URL under the model's base URL.
    #[test]
    fn backend_request_carries_api_key_and_stream_url() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].url,
            "https://generativelanguage.googleapis.test/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            requests[0]
                .headers
                .get("x-goog-api-key")
                .map(String::as_str),
            Some("AIza-test-key")
        );
        assert!(!requests[0].headers.contains_key("authorization"));
    }

    // A per-request `base_url` override targets the request at the right host.
    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            base_url: Some("https://proxy.test/v1beta".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    // (c) A non-2xx create surfaces the API's error body through the error event.
    #[test]
    fn backend_non_2xx_surfaces_error_body() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 400,
            headers: std::collections::BTreeMap::new(),
            body: json!({ "error": { "code": 400, "message": "API key not valid" } }).to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("400 API key not valid")
        );
    }

    // A missing credential surfaces the exact pi error, no panic and no request.
    #[test]
    fn backend_missing_api_key_errors_without_request() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("No API key for provider: google")
        );
        assert!(scripted.requests().is_empty());
    }

    /// A scripted transport pre-loaded with the multi-frame SSE body.
    fn scripted_multi_frame() -> (ScriptedTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(multi_frame_hello_sse_body());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    // Incremental over the one-chunk ScriptedTransport (default `send_streaming`)
    // yields the SAME events and final message as the buffered `stream`, and
    // builds the same threaded request.
    #[test]
    fn backend_stream_incremental_matches_buffered_over_scripted() {
        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            ..StreamOptions::default()
        };

        let (_scripted_buffered, transport_buffered) = scripted_multi_frame();
        let backend_buffered = GoogleGenerativeAiBackend::new(transport_buffered, fake_clock());
        let buffered = backend_buffered.stream(&model, &user_context(), Some(&options), None);

        let (scripted, transport) = scripted_multi_frame();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());
        let mut reader = backend.stream_incremental(&model, &user_context(), Some(&options), None);
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        assert_eq!(events, buffered.events);
        assert_eq!(
            reader.result().and_then(|r| r.as_ref().ok()),
            Some(&buffered.message)
        );
        assert_eq!(
            buffered.message.content,
            vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                text_signature: None,
            }]
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://generativelanguage.googleapis.test/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            requests[0]
                .headers
                .get("x-goog-api-key")
                .map(String::as_str),
            Some("AIza-test-key")
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

    // Over a per-frame sleeping transport, the yielded events span multiple
    // sleeping chunks -- non-zero inter-event spread -- while resolving to the
    // same "Hello!" message as the buffered path.
    #[test]
    fn backend_stream_incremental_streams_with_inter_event_spread() {
        let delay = Duration::from_millis(12);
        let transport: Arc<dyn HttpTransport> = Arc::new(SleepingStreamTransport {
            body: multi_frame_hello_sse_body(),
            delay,
        });
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());
        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
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
                text: "Hello!".to_string(),
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

    // #192: `StreamOptions.temperature` and `StreamOptions.max_tokens` thread into
    // the outgoing `generationConfig` (`config.temperature` /
    // `config.maxOutputTokens`); `metadata` is per pi NOT mapped for Google.
    #[test]
    fn backend_threads_stream_options_into_generation_config() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());

        let model = google_model("https://generativelanguage.googleapis.test/v1beta");
        let mut metadata = BTreeMap::new();
        metadata.insert("user_id".to_string(), json!("u-123"));
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            temperature: Some(0.42),
            max_tokens: Some(1234),
            metadata: Some(metadata),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        let requests = scripted.requests();
        let body: serde_json::Value =
            serde_json::from_str(requests[0].body.as_deref().expect("body")).expect("json body");
        let config = &body["config"];
        assert_eq!(config["temperature"], json!(0.42));
        assert_eq!(config["maxOutputTokens"], json!(1234));
        // metadata is not consumed by the Google dialect in pi.
        assert!(config.get("metadata").is_none());
        assert!(!requests[0].body.as_deref().unwrap().contains("user_id"));
    }

    // #192 precedence: with no `StreamOptions.max_tokens`, the model's `maxTokens`
    // default fills `maxOutputTokens`; a `StreamOptions.max_tokens` overrides it.
    #[test]
    fn backend_max_tokens_prefers_stream_options_over_model_default() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleGenerativeAiBackend::new(transport, fake_clock());
        let model = google_model("https://generativelanguage.googleapis.test/v1beta");

        // No StreamOptions.max_tokens -> model default (8192) is used.
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);
        let body: serde_json::Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().expect("body"))
                .expect("json body");
        assert_eq!(body["config"]["maxOutputTokens"], json!(8192));
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving the `hello` SSE body and drives
/// the backend through [`ReqwestTransport`] with `.no_proxy()` (required in the
/// sandbox).
#[cfg(all(test, feature = "native-http"))]
mod native_http_tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use serde_json::json;

    use crate::seams::clock::FakeClock;
    use crate::seams::http_reqwest::ReqwestTransport;
    use crate::types::ContentBlock;

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
        let base_url = format!(
            "http://{}/v1beta",
            listener.local_addr().expect("local addr")
        );

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
        let backend = GoogleGenerativeAiBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-generative-ai",
            "provider": "google",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
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

    // Incremental over a real reqwest loopback whose server writes each SSE frame
    // with a bounded sleep between flushes: the reader's per-frame PULL surfaces
    // that spacing as a non-zero inter-event spread at the consumer.
    #[test]
    fn backend_stream_incremental_over_reqwest_loopback_has_spread() {
        use std::time::{Duration, Instant};

        let delay = Duration::from_millis(20);
        let frames: Vec<String> = multi_frame_hello_sse_body()
            .split("\n\n")
            .filter(|part| !part.is_empty())
            .map(|part| format!("{part}\n\n"))
            .collect();
        let total_len: usize = frames.iter().map(|f| f.len()).sum();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!(
            "http://{}/v1beta",
            listener.local_addr().expect("local addr")
        );

        let server_frames = frames.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total_len}\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).expect("write head");
            stream.flush().ok();
            for (i, frame) in server_frames.iter().enumerate() {
                if i > 0 {
                    thread::sleep(delay);
                }
                stream.write_all(frame.as_bytes()).expect("write frame");
                stream.flush().ok();
            }
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = GoogleGenerativeAiBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-generative-ai",
            "provider": "google",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let context: Context = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap();
        let options = StreamOptions {
            api_key: Some("AIza-test-key".to_string()),
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
                text: "Hello!".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread over reqwest loopback, got {spread:?}",
        );
    }
}
