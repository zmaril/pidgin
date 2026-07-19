//! The Anthropic Messages [`Provider`] backend: the transport-aware adapter that
//! binds the ported `anthropic-messages` driver into the provider registry's
//! [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — request assembly, header switching, and SSE decode —
//! is already ported at [`crate::api::anthropic`]. This module is pure wiring: it
//! adapts the generic [`Provider`] seam (which speaks [`Model<Value>`] and
//! [`StreamOptions`]) onto the driver's typed
//! [`stream`](crate::api::anthropic::driver::stream) entry point (which speaks
//! [`Model<AnthropicMessagesCompat>`] and [`AnthropicOptions`]), threading an
//! injected [`HttpTransport`] and [`Clock`] so a live messages turn runs without
//! wall-clock or ambient-network access.

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) mirrors the identical shells in
// the registry and the anthropic driver by design; the clone detector reads the
// shared boundary-type construction as duplicative.

use std::sync::Arc;

use crate::api::anthropic::driver;
use crate::api::anthropic::request::AnthropicOptions;
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, AssistantMessageEvent, AssistantRole, Context,
    Model, StopReason, StreamOptions, Usage, UsageCost,
};

/// The api id this backend serves, pi's `anthropic-messages` [`Api`] discriminant.
pub const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";

/// A [`Provider`] backend that runs an Anthropic Messages turn over an injected
/// [`HttpTransport`], sourcing the request timestamp from an injected [`Clock`].
///
/// Constructed via [`AnthropicMessagesBackend::new`] and installed as
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct AnthropicMessagesBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl AnthropicMessagesBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl Provider for AnthropicMessagesBackend {
    fn api(&self) -> &str {
        ANTHROPIC_MESSAGES_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's typed
        // `Model<AnthropicMessagesCompat>` via serde. A malformed compat blob is
        // surfaced as a clean pre-start error event, never a panic.
        let mut typed_model: Model<AnthropicMessagesCompat> = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Anthropic model is not compatible with anthropic-messages: {error}"),
                )
            }
        };

        // A per-request base-URL override targets the driver's `request_url`
        // (`{base_url}/v1/messages`) at the right host. `applyAuth` has already
        // applied any per-credential `auth.baseUrl` onto `model.base_url`; this
        // honors an explicit `StreamOptions.base_url` on top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let anthropic_options = AnthropicOptions {
            cache_retention: options.and_then(|o| o.cache_retention),
            session_id: options.and_then(|o| o.session_id.clone()),
            env: options.and_then(|o| o.env.clone()),
            api_key: options.and_then(|o| o.api_key.clone()),
            headers: options.and_then(|o| o.headers.clone()),
            ..AnthropicOptions::default()
        };

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against (pi aborts an async SSE
        // read); `signal` is accepted for seam parity and left unobserved here.
        let timestamp = self.clock.now_ms();
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            &anthropic_options,
            timestamp,
        )
    }
}

/// Re-present a `Model<Value>` as a `Model<AnthropicMessagesCompat>` via a serde
/// JSON round-trip, so the untyped `compat` blob is decoded into the typed
/// Anthropic compat map the driver reads.
fn reserialize_model(model: &Model) -> Result<Model<AnthropicMessagesCompat>, serde_json::Error> {
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

    use crate::api::anthropic::driver_tests::hello_sse_body;
    use crate::auth::DefaultAuthContext;
    use crate::providers::registry::{
        create_provider, ApiRouting, CreateProviderOptions, Models, MutableModels, ProviderAuth,
    };
    use crate::seams::clock::FakeClock;
    use crate::seams::http::ScriptedTransport;
    use crate::seams::storage::MemoryEnv;
    use crate::types::ContentBlock;

    /// A neutral non-reasoning anthropic `Model<Value>` targeting `base_url`. The
    /// backend re-serializes this into `Model<AnthropicMessagesCompat>`.
    fn anthropic_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "claude-haiku-4-5",
            "name": "Claude Haiku 4.5",
            "api": "anthropic-messages",
            "provider": "anthropic",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 8000,
        }))
        .unwrap()
    }

    fn user_context() -> Context {
        serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap()
    }

    /// A scripted transport pre-loaded with the shared `hello` SSE body, plus a
    /// handle (sharing state) for later request assertions.
    fn scripted_hello() -> (ScriptedTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(hello_sse_body());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    fn fake_clock() -> Arc<dyn Clock> {
        Arc::new(FakeClock::new(1_700_000_000_000))
    }

    // Drives the NEW backend end to end through ScriptedTransport (default
    // features): the `hello` fixture yields a single "Hello" text block, and the
    // request the backend built carries the threaded `x-api-key` and the
    // `/v1/messages` URL.
    #[test]
    fn backend_streams_hello_and_threads_api_key() {
        let (scripted, transport) = scripted_hello();
        let backend = AnthropicMessagesBackend::new(transport, fake_clock());

        let model = anthropic_model("https://api.anthropic.test");
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
        assert_eq!(requests[0].url, "https://api.anthropic.test/v1/messages");
        assert_eq!(
            requests[0].headers.get("x-api-key").map(String::as_str),
            Some("sk-test-key")
        );
        assert!(!requests[0].headers.contains_key("authorization"));
    }

    // A per-request `base_url` override targets the request at the right host.
    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = AnthropicMessagesBackend::new(transport, fake_clock());

        let model = anthropic_model("https://api.anthropic.test");
        let options = StreamOptions {
            api_key: Some("sk-test-key".to_string()),
            base_url: Some("https://proxy.test".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(scripted.requests()[0].url, "https://proxy.test/v1/messages");
    }

    // A model whose compat blob cannot decode into `AnthropicMessagesCompat`
    // surfaces a clean pre-start error event, never a panic, and never a request.
    #[test]
    fn backend_incompatible_model_is_a_clean_error() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = AnthropicMessagesBackend::new(transport, fake_clock());

        let mut model = anthropic_model("https://api.anthropic.test");
        // `supportsTemperature` is a bool flag; a number here fails the typed decode.
        model.compat = Some(json!({ "supportsTemperature": 12345 }) as Value);

        let result = backend.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(result.events.len(), 1);
        assert!(matches!(
            result.events[0],
            AssistantMessageEvent::Error { .. }
        ));
        assert!(scripted.requests().is_empty());
    }

    /// A `Models` collection whose sole provider (`anthropic`) routes through the
    /// backend over `transport`, resolving auth against `env`.
    fn models_with_anthropic_backend(
        env: MemoryEnv,
        transport: Arc<dyn HttpTransport>,
        model: &Model,
    ) -> Models {
        let mut models = Models::with_auth_context(Arc::new(DefaultAuthContext::new(env)));
        models.set_provider(create_provider(CreateProviderOptions {
            id: "anthropic".to_string(),
            name: Some("Anthropic".to_string()),
            base_url: Some("https://api.anthropic.com".to_string()),
            headers: None,
            auth: ProviderAuth::env_api_key("Anthropic API key", &["ANTHROPIC_API_KEY"]),
            models: vec![model.clone()],
            fetch_models: None,
            api: ApiRouting::Single(Arc::new(AnthropicMessagesBackend::new(
                transport,
                fake_clock(),
            ))),
        }));
        models
    }

    // The `Models::stream` applyAuth path (models.ts:463): a configured env key
    // resolves and reaches the outbound request as `x-api-key`, proving the
    // resolved apiKey threads through `StreamOptions.api_key` into the backend.
    #[test]
    fn models_stream_threads_resolved_api_key_to_request() {
        let (scripted, transport) = scripted_hello();
        let model = anthropic_model("https://api.anthropic.test");
        let env = MemoryEnv::new().with_env("ANTHROPIC_API_KEY", "sk-env-secret");
        let models = models_with_anthropic_backend(env, transport, &model);

        // No per-request options: the api key comes purely from resolved auth.
        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "https://api.anthropic.test/v1/messages");
        assert_eq!(
            requests[0].headers.get("x-api-key").map(String::as_str),
            Some("sk-env-secret")
        );
    }

    // A per-request `base_url` threads through applyAuth's requestOptions clone
    // into the backend and overrides the request URL host.
    #[test]
    fn models_stream_threads_base_url_override() {
        let (scripted, transport) = scripted_hello();
        let model = anthropic_model("https://api.anthropic.test");
        let env = MemoryEnv::new().with_env("ANTHROPIC_API_KEY", "sk-env-secret");
        let models = models_with_anthropic_backend(env, transport, &model);

        let options = StreamOptions {
            base_url: Some("https://proxy.test".to_string()),
            ..StreamOptions::default()
        };
        models.stream(&model, &user_context(), Some(&options), None);

        assert_eq!(scripted.requests()[0].url, "https://proxy.test/v1/messages");
    }

    // Unconfigured provider: applyAuth gates before dispatch with the exact
    // "Provider is not configured" error, no panic and no network request.
    #[test]
    fn models_stream_unconfigured_provider_errors_without_request() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let model = anthropic_model("https://api.anthropic.test");
        // Env var unset -> the provider cannot resolve a credential.
        let models = models_with_anthropic_backend(MemoryEnv::new(), transport, &model);

        let result = models.stream(&model, &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Provider is not configured: anthropic")
        );
        assert!(scripted.requests().is_empty());
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving the shared `hello` SSE body and
/// drives the backend through [`ReqwestTransport`] with `.no_proxy()` (required
/// in the sandbox).
#[cfg(all(test, feature = "native-http"))]
mod native_http_tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use serde_json::json;

    use crate::api::anthropic::driver_tests::hello_sse_body;
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
        let backend = AnthropicMessagesBackend::new(transport, clock);

        let model: Model = serde_json::from_value(json!({
            "id": "claude-haiku-4-5",
            "name": "Claude Haiku 4.5",
            "api": "anthropic-messages",
            "provider": "anthropic",
            "baseUrl": base_url,
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 8000,
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
