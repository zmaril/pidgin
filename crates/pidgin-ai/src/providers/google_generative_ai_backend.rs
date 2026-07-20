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

        // The request-shaping options come from the MODEL for now: `max_tokens`
        // maps onto `maxOutputTokens`, and there is no model temperature to
        // thread.
        // Follow-up (#192): thread StreamOptions.temperature/max_tokens/metadata
        // once merged.
        let request_options = GoogleRequestOptions {
            temperature: None,
            max_tokens: (model.max_tokens > 0).then_some(model.max_tokens),
            tool_choice: None,
            thinking: None,
            aborted: false,
        };

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
mod tests {
    use super::*;

    use serde_json::json;

    use crate::seams::clock::FakeClock;
    use crate::seams::http::{HttpResponse, ScriptedTransport};
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
}
