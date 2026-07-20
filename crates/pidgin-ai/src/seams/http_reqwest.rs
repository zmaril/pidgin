//! The native, `reqwest`-backed [`HttpTransport`] implementation.
//!
//! The [`http`](crate::seams::http) seam abstracts every provider's network I/O
//! behind a synchronous, fully-buffered trait. In the shipped Node target that
//! trait is satisfied by [`HostTransport`](crate::seams::HostTransport), which
//! delegates to the runtime's `fetch`. This module supplies the *native* Rust
//! implementation the seam's docs promise: a `reqwest::blocking` client that
//! performs the request in-process, with no JS host in the loop.
//!
//! # Feature gate
//!
//! reqwest (plus its rustls TLS stack) is a heavier dependency tree than the
//! lean default build needs, so — mirroring the way `pidgin-extensions` gates
//! `deno_core`/V8 behind its non-default `deno` feature — this whole module and
//! its `reqwest` dependency live behind the non-default `native-http` feature.
//! The default `cargo build/test --workspace` never pulls reqwest.
//!
//! # Buffered, not streaming
//!
//! The [`HttpTransport::send`] signature returns a whole [`HttpResponse`] whose
//! `body` is a complete `String`, so this transport reads the entire response
//! body to end before returning — correct for one-shot (`-p`) and conformance
//! runs, where the driver hands the full SSE body to the parser. Live token
//! streaming would require an incremental variant of the seam (a chunk callback
//! or iterator on `send`); that does not exist yet and is out of scope here.

use std::io;
use std::time::Duration;

use reqwest::blocking::Client;

use super::http::{HttpRequest, HttpResponse, HttpTransport};

/// Builder for [`ReqwestTransport`], configuring the underlying blocking client.
///
/// Obtained via [`ReqwestTransport::builder`]. Defaults match pi/undici: no
/// total request timeout, and the transport honors the ambient proxy
/// environment (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`) exactly as reqwest does
/// by default. Call [`ReqwestTransportBuilder::no_proxy`] to bypass any ambient
/// proxy — tests use this to avoid the sandbox's TLS-intercepting egress proxy.
#[derive(Debug, Clone, Default)]
pub struct ReqwestTransportBuilder {
    timeout: Option<Duration>,
    no_proxy: bool,
}

impl ReqwestTransportBuilder {
    /// Set a total request timeout. Defaults to none (no total timeout), so a
    /// long-lived SSE response is not cut off, matching undici's default.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Disable all proxying, ignoring `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`.
    ///
    /// Tests bind an in-process listener on loopback and set this so the
    /// sandbox's ambient egress proxy is never consulted.
    pub fn no_proxy(mut self) -> Self {
        self.no_proxy = true;
        self
    }

    /// Build the transport, constructing the backing `reqwest` blocking client.
    ///
    /// # Panics
    ///
    /// Panics only if the TLS backend fails to initialize — the same
    /// unrecoverable condition `reqwest::blocking::Client::new` panics on.
    pub fn build(self) -> ReqwestTransport {
        let mut builder = Client::builder();
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        if self.no_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .expect("failed to build reqwest blocking client");
        ReqwestTransport { client }
    }
}

/// The native, `reqwest::blocking`-backed [`HttpTransport`].
///
/// Performs each [`HttpRequest`] in-process over reqwest + rustls and returns
/// the fully-buffered [`HttpResponse`]. HTTP error statuses (4xx/5xx) come back
/// as `Ok(HttpResponse { .. })` — the driver, not the transport, decides what a
/// non-2xx status means — exactly like `fetch`. Only transport-level failures
/// (connect refused, timeout, DNS, body read) surface as [`io::Error`].
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: Client,
}

impl ReqwestTransport {
    /// Build a transport with default configuration: no total timeout, honoring
    /// the ambient proxy environment (reqwest's default behavior).
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Start configuring a transport (timeout, `no_proxy`).
    pub fn builder() -> ReqwestTransportBuilder {
        ReqwestTransportBuilder::default()
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, request: &HttpRequest) -> io::Result<HttpResponse> {
        // 1. Method (uppercased, per the seam convention).
        let method = reqwest::Method::from_bytes(request.method.to_uppercase().as_bytes())
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid HTTP method {:?}: {e}", request.method),
                )
            })?;

        // 2. Headers (BTreeMap -> request headers, keys as-is).
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        // 3. Body, if any, as the raw string bytes.
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }

        // 4. Execute (blocking). A non-2xx status is NOT an error here (no
        //    `.error_for_status()`); only transport failures are.
        let response = builder.send().map_err(map_reqwest_err)?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();

        // 5. Read the WHOLE body to a String. `text()` decodes per the response
        //    charset and consumes the response.
        let body = response.text().map_err(map_reqwest_err)?;

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Map a transport-level [`reqwest::Error`] to an [`io::Error`], choosing an
/// [`io::ErrorKind`] that reflects the failure and preserving reqwest's message.
fn map_reqwest_err(error: reqwest::Error) -> io::Error {
    let kind = if error.is_timeout() {
        io::ErrorKind::TimedOut
    } else if error.is_connect() {
        io::ErrorKind::ConnectionRefused
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, error.to_string())
}

#[cfg(all(test, feature = "native-http"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    /// A parsed request line + headers + body read off one client connection.
    struct Served {
        method: String,
        path: String,
        headers: std::collections::BTreeMap<String, String>,
        body: String,
    }

    /// Read one HTTP/1.1 request off `stream`, returning the parsed pieces.
    ///
    /// Reads headers, then exactly `content-length` bytes of body (the tests
    /// only send bodies with an explicit length).
    fn read_request(stream: &mut TcpStream) -> Served {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        // Read until the header terminator is seen.
        let header_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut tmp).expect("read request");
            if n == 0 {
                break buf.len();
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        let mut headers = std::collections::BTreeMap::new();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let value = v.trim().to_string();
                if key == "content-length" {
                    content_length = value.parse().unwrap_or(0);
                }
                headers.insert(key, value);
            }
        }

        // Body bytes already read past the header terminator.
        let mut body = buf[(header_end + 4).min(buf.len())..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).expect("read body");
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&body[..content_length.min(body.len())]).to_string();

        Served {
            method,
            path,
            headers,
            body,
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Bind a one-shot server on `127.0.0.1:0`, hand the accepted stream to
    /// `handler` on a background thread, and return the bound base URL
    /// (`http://127.0.0.1:PORT`). The shared bind/accept core behind the request
    /// servers below.
    fn spawn_on_loopback<F>(handler: F) -> String
    where
        F: FnOnce(TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handler(stream);
            }
        });
        format!("http://{addr}")
    }

    /// Spawn a one-shot server whose `handler` receives the parsed request and
    /// the accepted stream, and is responsible for writing the whole response.
    fn spawn_server<F>(handler: F) -> String
    where
        F: FnOnce(Served, &mut TcpStream) + Send + 'static,
    {
        spawn_on_loopback(move |mut stream| {
            let served = read_request(&mut stream);
            handler(served, &mut stream);
        })
    }

    /// A server that never reads/responds until after `delay`, then closes —
    /// used to exercise the client timeout path. Accepts the connection so the
    /// failure is a read timeout, not a connect refusal.
    fn spawn_slow_server(delay: Duration) -> String {
        spawn_on_loopback(move |mut stream| {
            let mut tmp = [0u8; 1024];
            let _ = stream.read(&mut tmp);
            thread::sleep(delay);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
        })
    }

    fn transport() -> ReqwestTransport {
        ReqwestTransport::builder().no_proxy().build()
    }

    #[test]
    fn get_happy_path_round_trips_status_body_headers() {
        let url = spawn_server(|served, stream| {
            assert_eq!(served.method, "GET");
            assert_eq!(served.path, "/hello");
            let body = "hello world";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Custom: Yes\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let response = transport()
            .send(&HttpRequest::get(format!("{url}/hello")))
            .unwrap();

        assert_eq!(response.status, 200);
        assert!(response.is_ok());
        assert_eq!(response.body, "hello world");
        // Response headers land lowercased in the BTreeMap.
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("text/plain")
        );
        assert_eq!(
            response.headers.get("x-custom").map(String::as_str),
            Some("Yes")
        );
    }

    #[test]
    fn request_headers_reach_server_and_response_headers_lowercased() {
        let url = spawn_server(|served, stream| {
            // Echo the received authorization header back as a response header.
            let auth = served
                .headers
                .get("authorization")
                .cloned()
                .unwrap_or_default();
            let response =
                format!("HTTP/1.1 200 OK\r\nX-Echo-Auth: {auth}\r\nContent-Length: 0\r\n\r\n");
            stream.write_all(response.as_bytes()).unwrap();
        });

        let response = transport()
            .send(
                &HttpRequest::get(format!("{url}/")).with_header("authorization", "Bearer secret"),
            )
            .unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("x-echo-auth").map(String::as_str),
            Some("Bearer secret")
        );
    }

    #[test]
    fn post_body_is_sent_and_echoed() {
        let url = spawn_server(|served, stream| {
            assert_eq!(served.method, "POST");
            let echoed = served.body.clone();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{echoed}",
                echoed.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let sent = "{\"stream\":true,\"n\":42}";
        let response = transport()
            .send(&HttpRequest::post(format!("{url}/v1"), sent))
            .unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, sent);
    }

    #[test]
    fn non_2xx_status_returns_ok_response_not_err() {
        for status in [429u16, 500u16] {
            let url = spawn_server(move |_served, stream| {
                let body = format!("error {status}");
                let response = format!(
                    "HTTP/1.1 {status} Whatever\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            });

            let response = transport()
                .send(&HttpRequest::get(format!("{url}/")))
                .expect("non-2xx must be Ok, not Err");
            assert_eq!(response.status, status);
            assert!(!response.is_ok());
            assert_eq!(response.body, format!("error {status}"));
        }
    }

    #[test]
    fn chunked_multi_write_body_is_fully_reassembled() {
        // Server writes an SSE-shaped body across several TCP writes with a tiny
        // sleep between them, using Transfer-Encoding: chunked. The transport
        // must return the full concatenated body regardless of chunk boundaries.
        let pieces = [
            "event: message_start\n",
            "data: {\"a\":1}\n\n",
            "event: content_block_delta\n",
            "data: {\"b\":2}\n\n",
            "event: message_stop\n\n",
        ];
        let url = spawn_server(move |_served, stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap();
            for piece in pieces {
                let chunk = format!("{:X}\r\n{piece}\r\n", piece.len());
                stream.write_all(chunk.as_bytes()).unwrap();
                stream.flush().unwrap();
                thread::sleep(Duration::from_millis(5));
            }
            stream.write_all(b"0\r\n\r\n").unwrap();
            stream.flush().unwrap();
        });

        let response = transport()
            .send(&HttpRequest::get(format!("{url}/sse")))
            .unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, pieces.concat());
    }

    #[test]
    fn connection_refused_maps_to_err() {
        // Bind then drop the listener to obtain a definitely-free port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = transport()
            .send(&HttpRequest::get(format!("http://127.0.0.1:{port}/")))
            .expect_err("expected a connection error");
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::Other
            ),
            "unexpected error kind: {:?} ({err})",
            err.kind()
        );
    }

    #[test]
    fn timeout_maps_to_err() {
        let url = spawn_slow_server(Duration::from_secs(30));
        let client = ReqwestTransport::builder()
            .no_proxy()
            .timeout(Duration::from_millis(150))
            .build();

        let err = client
            .send(&HttpRequest::get(format!("{url}/slow")))
            .expect_err("expected a timeout error");
        assert!(
            matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::Other),
            "unexpected error kind: {:?} ({err})",
            err.kind()
        );
    }
}
