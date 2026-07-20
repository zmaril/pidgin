//! The HTTP transport seam: injectable request transport, including the
//! WebSocket path.
//!
//! # What this abstracts in pi
//!
//! This is the largest seam by mock-site count: the mock-seam inventory
//! (`notes/mock-inventory.md`) attributes 80 sites to it — 13 collaborator
//! mocks plus 67 `vi.stubGlobal("fetch")` global stubs. pi's tests overwhelmingly
//! do not hit the network: they stub `fetch` (OAuth, token refresh, model-catalog
//! fetches) or feed a hand-built `Response` straight into a provider's stream
//! parser. The seam lets a test inject the transport a provider calls so those
//! stubs keep working when the provider is Rust, instead of a JS `fetch` stub a
//! Rust HTTP client would never consult.
//!
//! The seam also carries the **WebSocket** path (some providers stream over a
//! socket rather than SSE), so [`HttpTransport::connect_websocket`] and
//! [`WebSocket`] sit alongside the request/response surface: the inventory calls
//! out the WebSocket sites explicitly, and a request-only transport would leave
//! them uncovered.
//!
//! # Implementations
//!
//! - [`HostTransport`] — the production transport for the Node/host binding.
//!   Rather than embedding a Rust HTTP stack (which Stage 2 deliberately left in
//!   JS — the host supplies `fetch`/`WebSocket` across the napi boundary), it
//!   delegates to injected closures. This is the real transport the shipped Node
//!   target uses today; a `reqwest`-backed transport lands with the provider HTTP
//!   port and implements this same trait.
//! - [`ScriptedTransport`] — the deterministic test transport: queued canned
//!   responses matched to requests, recording every request so a test can assert
//!   on the URL/method/headers/body exactly as a `fetch` stub does, plus scripted
//!   WebSocket frame sequences.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

/// An HTTP request the transport is asked to perform. Mirrors the argument shape
/// of a `fetch(url, init)` call.
///
/// Serde-serializable so the type can cross the napi JSON boundary as the
/// argument to the host `fetch` shim: `headers` becomes a JSON object and `body`
/// a JSON string (the boundary assumes text bodies — OAuth/JSON/SSE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    /// HTTP method, uppercase (`GET`, `POST`, ...).
    pub method: String,
    /// Absolute request URL.
    pub url: String,
    /// Request headers, lowercased keys by convention.
    pub headers: BTreeMap<String, String>,
    /// Request body, if any (JSON, form-encoded, ...).
    pub body: Option<String>,
}

impl HttpRequest {
    /// A `GET` for `url` with no headers or body.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET".to_string(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// A `POST` for `url` carrying `body`.
    pub fn post(url: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: "POST".to_string(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: Some(body.into()),
        }
    }

    /// Set a header (builder style).
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

/// An HTTP response. Mirrors the parts of a `fetch` `Response` the ported code
/// reads: status, headers, and the body as text (SSE bodies are consumed as text
/// and handed to the parser, exactly as the Stage-2 Anthropic shim does).
///
/// Serde-serializable so the host shim can hand a `{status, headers, body}` JSON
/// object back across the napi boundary: `headers` is a JSON object and `body`
/// the response text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers, lowercased keys by convention.
    pub headers: BTreeMap<String, String>,
    /// Response body as text.
    pub body: String,
}

impl HttpResponse {
    /// A `200 OK` carrying `body` and no headers.
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: BTreeMap::new(),
            body: body.into(),
        }
    }

    /// Whether the status is in the 2xx range (`Response.ok`).
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// A single WebSocket frame the ported code exchanges. Text and binary mirror
/// the two `WebSocket` message payload kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// A UTF-8 text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
}

/// An open WebSocket connection: send frames and receive the next frame.
///
/// The minimal duplex surface the ported providers need. Returning
/// `Ok(None)` from [`WebSocket::recv`] signals a clean close (the socket ran out
/// of frames), matching a `close` event with no further messages.
pub trait WebSocket: Send {
    /// Send a frame to the peer.
    fn send(&mut self, message: WsMessage) -> io::Result<()>;
    /// Receive the next frame, or `Ok(None)` once the socket has closed.
    fn recv(&mut self) -> io::Result<Option<WsMessage>>;
    /// Close the connection.
    fn close(&mut self) -> io::Result<()>;
}

/// Performs HTTP requests and opens WebSocket connections — the boundary every
/// provider's network I/O sits behind.
///
/// Production code depends on `&dyn HttpTransport` so a test can inject
/// [`ScriptedTransport`] in place of the host's real transport, reproducing pi's
/// `vi.stubGlobal("fetch")` and WebSocket mocks.
pub trait HttpTransport: Send + Sync {
    /// Perform `request` and return the response (pi's `fetch`).
    fn send(&self, request: &HttpRequest) -> io::Result<HttpResponse>;
    /// Open a WebSocket to `url`. The default rejects the WebSocket path for
    /// transports that do not implement it, so a request-only transport is still
    /// a valid [`HttpTransport`].
    fn connect_websocket(&self, url: &str) -> io::Result<Box<dyn WebSocket>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("this transport does not support WebSocket ({url})"),
        ))
    }
}

type SendFn = dyn Fn(&HttpRequest) -> io::Result<HttpResponse> + Send + Sync;
type WsFn = dyn Fn(&str) -> io::Result<Box<dyn WebSocket>> + Send + Sync;

/// The production transport for the Node/host binding.
///
/// Delegates to closures the host supplies — on the Node target these wrap the
/// runtime's `fetch` and `WebSocket`, which is where Stage 2 keeps real network
/// I/O. A future `reqwest`-backed transport implements the same
/// [`HttpTransport`] trait and slots in without touching any caller.
#[derive(Clone)]
pub struct HostTransport {
    send: Arc<SendFn>,
    connect_ws: Option<Arc<WsFn>>,
}

impl HostTransport {
    /// Build a transport whose requests are served by `send`.
    pub fn new(
        send: impl Fn(&HttpRequest) -> io::Result<HttpResponse> + Send + Sync + 'static,
    ) -> Self {
        Self {
            send: Arc::new(send),
            connect_ws: None,
        }
    }

    /// Add WebSocket support by supplying a connect closure.
    pub fn with_websocket(
        mut self,
        connect: impl Fn(&str) -> io::Result<Box<dyn WebSocket>> + Send + Sync + 'static,
    ) -> Self {
        self.connect_ws = Some(Arc::new(connect));
        self
    }
}

impl HttpTransport for HostTransport {
    fn send(&self, request: &HttpRequest) -> io::Result<HttpResponse> {
        (self.send)(request)
    }
    fn connect_websocket(&self, url: &str) -> io::Result<Box<dyn WebSocket>> {
        match &self.connect_ws {
            Some(connect) => connect(url),
            None => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("HostTransport has no WebSocket connector ({url})"),
            )),
        }
    }
}

#[derive(Default)]
struct ScriptedState {
    responses: std::collections::VecDeque<io::Result<HttpResponse>>,
    requests: Vec<HttpRequest>,
    ws_scripts: std::collections::VecDeque<Vec<WsMessage>>,
}

/// A deterministic, scripted HTTP transport for tests.
///
/// Queue responses with [`ScriptedTransport::push_response`]; each
/// [`HttpTransport::send`] pops the next and records the request, so a test both
/// steers the response and asserts on the request — the Rust-side equivalent of
/// `vi.stubGlobal("fetch", ...)`. WebSocket connections replay a scripted frame
/// sequence queued with [`ScriptedTransport::push_websocket`].
#[derive(Clone, Default)]
pub struct ScriptedTransport {
    state: Arc<Mutex<ScriptedState>>,
}

impl ScriptedTransport {
    /// An empty scripted transport.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a `200 OK` response with `body`.
    pub fn push_ok(&self, body: impl Into<String>) -> &Self {
        self.push_response(Ok(HttpResponse::ok(body)))
    }

    /// Queue an arbitrary response or error.
    pub fn push_response(&self, response: io::Result<HttpResponse>) -> &Self {
        self.state.lock().unwrap().responses.push_back(response);
        self
    }

    /// Queue the frame sequence the next [`HttpTransport::connect_websocket`]
    /// will replay to its consumer.
    pub fn push_websocket(&self, frames: Vec<WsMessage>) -> &Self {
        self.state.lock().unwrap().ws_scripts.push_back(frames);
        self
    }

    /// Every request performed so far, in order — for request assertions.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.state.lock().unwrap().requests.clone()
    }
}

impl HttpTransport for ScriptedTransport {
    fn send(&self, request: &HttpRequest) -> io::Result<HttpResponse> {
        let mut state = self.state.lock().unwrap();
        state.requests.push(request.clone());
        state.responses.pop_front().unwrap_or_else(|| {
            Err(io::Error::other(format!(
                "ScriptedTransport: no scripted response for {} {}",
                request.method, request.url
            )))
        })
    }

    fn connect_websocket(&self, url: &str) -> io::Result<Box<dyn WebSocket>> {
        let frames = self.state.lock().unwrap().ws_scripts.pop_front();
        match frames {
            Some(frames) => Ok(Box::new(ScriptedWebSocket {
                incoming: frames.into(),
                sent: Vec::new(),
            })),
            None => Err(io::Error::other(format!(
                "ScriptedTransport: no scripted WebSocket for {url}"
            ))),
        }
    }
}

/// A scripted [`WebSocket`] that replays a fixed frame sequence and records what
/// was sent.
struct ScriptedWebSocket {
    incoming: std::collections::VecDeque<WsMessage>,
    sent: Vec<WsMessage>,
}

impl WebSocket for ScriptedWebSocket {
    fn send(&mut self, message: WsMessage) -> io::Result<()> {
        self.sent.push(message);
        Ok(())
    }
    fn recv(&mut self) -> io::Result<Option<WsMessage>> {
        Ok(self.incoming.pop_front())
    }
    fn close(&mut self) -> io::Result<()> {
        self.incoming.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_transport_replays_and_records_requests() {
        let transport = ScriptedTransport::new();
        transport.push_ok("event: message_start\ndata: {}\n");

        let response = transport
            .send(
                &HttpRequest::post("https://api.example/v1", "{\"stream\":true}")
                    .with_header("authorization", "Bearer k"),
            )
            .unwrap();
        assert!(response.is_ok());
        assert!(response.body.contains("message_start"));

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].headers.get("authorization").unwrap(),
            "Bearer k"
        );
    }

    #[test]
    fn scripted_transport_errors_when_unscripted() {
        let transport = ScriptedTransport::new();
        let err = transport.send(&HttpRequest::get("https://x")).unwrap_err();
        assert!(err.to_string().contains("no scripted response"));
    }

    #[test]
    fn scripted_websocket_replays_frames_then_closes() {
        let transport = ScriptedTransport::new();
        transport.push_websocket(vec![
            WsMessage::Text("hello".to_string()),
            WsMessage::Binary(vec![1, 2, 3]),
        ]);
        let mut socket = transport.connect_websocket("wss://x").unwrap();
        socket.send(WsMessage::Text("ping".to_string())).unwrap();
        assert_eq!(
            socket.recv().unwrap(),
            Some(WsMessage::Text("hello".to_string()))
        );
        assert_eq!(
            socket.recv().unwrap(),
            Some(WsMessage::Binary(vec![1, 2, 3]))
        );
        assert_eq!(socket.recv().unwrap(), None);
    }

    #[test]
    fn http_request_serde_round_trips_to_json_wire_shape() {
        // Pins the wire contract the host `fetch` shim consumes: `headers` is a
        // JSON object and `body` a JSON string.
        let request = HttpRequest::post("https://api.example/v1", "{\"stream\":true}")
            .with_header("authorization", "Bearer k")
            .with_header("content-type", "application/json");
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "method": "POST",
                "url": "https://api.example/v1",
                "headers": {
                    "authorization": "Bearer k",
                    "content-type": "application/json"
                },
                "body": "{\"stream\":true}"
            })
        );
        let back: HttpRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn http_response_serde_round_trips_to_json_wire_shape() {
        let response = HttpResponse {
            status: 200,
            headers: BTreeMap::from([(
                "content-type".to_string(),
                "text/event-stream".to_string(),
            )]),
            body: "event: message_start\ndata: {}\n".to_string(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "status": 200,
                "headers": { "content-type": "text/event-stream" },
                "body": "event: message_start\ndata: {}\n"
            })
        );
        let back: HttpResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn host_transport_delegates_to_closure() {
        let transport = HostTransport::new(|req| {
            assert_eq!(req.url, "https://api");
            Ok(HttpResponse::ok("pong"))
        });
        assert_eq!(
            transport
                .send(&HttpRequest::get("https://api"))
                .unwrap()
                .body,
            "pong"
        );
        // No WebSocket connector supplied -> Unsupported.
        match transport.connect_websocket("wss://x") {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported error"),
        }
    }
}
