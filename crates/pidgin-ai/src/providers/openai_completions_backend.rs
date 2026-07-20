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
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model,
    OpenAICompletionsCompat, StopReason, StreamOptions, Usage, UsageCost,
};

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

        let openai_options = OpenAICompletionsOptions {
            // Follow-up (#192): thread StreamOptions.temperature/max_tokens/metadata
            // once merged; the base branch's StreamOptions carries none of them yet,
            // so max_tokens is populated from the MODEL and temperature is left unset.
            max_tokens: Some(typed_model.max_tokens),
            temperature: None,
            cache_retention: options.and_then(|o| o.cache_retention),
            session_id: options.and_then(|o| o.session_id.clone()),
            api_key: options.and_then(|o| o.api_key.clone()),
            headers: options.and_then(|o| o.headers.clone()),
            // pi's `getProviderEnvValue("PI_CACHE_RETENTION", env)` — the only env
            // value the request shaper reads.
            cache_retention_env: options.and_then(|o| {
                o.env
                    .as_ref()
                    .and_then(|env| env.get("PI_CACHE_RETENTION").cloned())
            }),
            ..OpenAICompletionsOptions::default()
        };

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
    use crate::seams::http::ScriptedTransport;
    use crate::types::{ContentBlock, StopReason};

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
}
