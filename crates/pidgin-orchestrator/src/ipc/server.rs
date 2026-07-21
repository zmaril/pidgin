//! The IPC server, mirroring `packages/orchestrator/src/ipc/server.ts`.
//!
//! pi's `startIpcServer` creates a `node:net` server on the orchestrator's Unix
//! socket. Each connection reads one newline-framed request and dispatches it to
//! the injected [`IpcRequestHandler`]. Ordinary requests get a single response
//! and the socket closes; an `rpc_stream` request instead flips the socket into a
//! **bidirectional JSONL RPC bridge**, relaying frames both ways until the socket
//! closes. Before listening, the server probes any pre-existing socket file and
//! removes it if it is stale (`removeStaleSocketIfNeeded` + `isSocketLive`).
//!
//! # The socket seam
//!
//! Like pi, the production server binds a real [`tokio::net::UnixListener`]
//! (through [`UnixSocketListener`]), but the accept loop and per-connection
//! framing are written generically over the [`IpcListener`] seam and any
//! `AsyncRead + AsyncWrite` stream, so a test can drive the whole server — including
//! the `rpc_stream` bridge — over in-memory duplex pipes with no socket file.
//!
//! # Relay seam and the deferred streaming caveat
//!
//! pi types the bridge's three server-to-client callbacks as `RpcResponse`,
//! `AgentSessionEvent`, and `RpcExtensionUIRequest`. Per the coordinator-approved
//! seam decision the orchestrator only *relays* these, so they are
//! [`serde_json::Value`] frames here (see [`super::protocol`]). Full streaming
//! parity — real [`AgentSessionEvent`] payloads flowing live from a running
//! agent — is **deferred** until pidgin-coding emits live agent events; until
//! then the bridge faithfully relays whatever `Value` frames the handler produces.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::get_socket_path;
use crate::ipc::protocol::{
    encode_message, parse_request_line, AgentSessionEvent, ErrorResponse, OrchestratorRequest,
    OrchestratorResponse, RpcClientMessage, RpcExtensionUIRequest, RpcResponse, RpcStreamRequest,
};
use crate::ipc::transport::{IpcListener, SocketProbe, UnixSocketListener, UnixSocketProbe};

/// A boxed, `Send` future — the return shape for the object-safe handler traits
/// (which must be usable behind `Arc<dyn …>` / `Box<dyn …>`).
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Dispatches decoded orchestrator requests, mirroring pi's `IpcRequestHandler`
/// interface.
///
/// pi's interface is a callable overloaded per request type plus an
/// `openRpcStream` method. In Rust that is one [`handle`](IpcRequestHandler::handle)
/// method over the [`OrchestratorRequest`] union and one
/// [`open_rpc_stream`](IpcRequestHandler::open_rpc_stream) method. The concrete
/// handler (the supervisor-backed dispatch) is ported in a later stage; the
/// server here only *calls* this trait.
pub trait IpcRequestHandler: Send + Sync {
    /// Handle a single request, resolving to the response to send back.
    fn handle(&self, request: OrchestratorRequest) -> BoxFuture<'_, OrchestratorResponse>;

    /// Open a bidirectional RPC stream for `instance_id`, or return `None` if no
    /// such instance exists (pi returns `undefined`). The returned session's
    /// frames are written to the client through `sink`.
    fn open_rpc_stream(
        &self,
        instance_id: String,
        sink: RpcStreamSink,
    ) -> Option<Box<dyn RpcStreamSession>>;
}

/// A live RPC-stream session, mirroring the object pi's `openRpcStream` returns
/// (`{ handleRequest, close }`).
pub trait RpcStreamSession: Send {
    /// Handle one client-to-server frame (an `RpcCommand` or an
    /// `extension_ui_response`). An `Err(message)` is relayed to the client as an
    /// error frame, mirroring pi's per-request `try/catch`.
    fn handle_request(&mut self, message: RpcClientMessage) -> BoxFuture<'_, Result<(), String>>;

    /// Tear the session down, mirroring pi's `rpcStream.close()` on socket close.
    fn close(&mut self);
}

/// The server-to-client frame sink handed to an [`RpcStreamSession`].
///
/// pi passes `openRpcStream` three callbacks (`onResponse`, `onSessionEvent`,
/// `onUiRequest`), each of which does `socket.write(encodeMessage(x))`. Since all
/// three relay opaque [`serde_json::Value`] frames onto the same socket, they are
/// modelled as three named methods over one frame channel; a single writer task
/// drains the channel so frames are written in FIFO order.
#[derive(Debug, Clone)]
pub struct RpcStreamSink {
    tx: mpsc::UnboundedSender<serde_json::Value>,
}

impl RpcStreamSink {
    /// Build a sink over a frame sender.
    ///
    /// The server builds one internally per `rpc_stream` connection; exposed to
    /// the crate so the [`crate::handler`] adapter can be driven in tests without
    /// standing up a full connection.
    #[cfg(test)]
    pub(crate) fn from_sender(tx: mpsc::UnboundedSender<serde_json::Value>) -> Self {
        Self { tx }
    }

    /// Relay an RPC response frame (pi's `onResponse`).
    pub fn send_response(&self, response: RpcResponse) {
        let _ = self.tx.send(response);
    }

    /// Relay a streaming session-event frame (pi's `onSessionEvent`).
    pub fn send_session_event(&self, event: AgentSessionEvent) {
        let _ = self.tx.send(event);
    }

    /// Relay an extension-UI request frame (pi's `onUiRequest`).
    pub fn send_ui_request(&self, request: RpcExtensionUIRequest) {
        let _ = self.tx.send(request);
    }
}

/// A running IPC server, mirroring the `node:net` `Server` pi returns.
///
/// Dropping the server (or calling [`abort`](IpcServer::abort)) stops the accept
/// loop; in-flight connection tasks finish on their own.
#[derive(Debug)]
pub struct IpcServer {
    accept_task: JoinHandle<()>,
}

impl IpcServer {
    /// Stop accepting new connections.
    pub fn abort(&self) {
        self.accept_task.abort();
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// Start the IPC server on the orchestrator's Unix socket.
///
/// Mirrors pi's `startIpcServer`: remove a stale socket if present, bind the
/// socket, and begin accepting connections. Returns once the socket is bound.
pub async fn start_ipc_server(handler: Arc<dyn IpcRequestHandler>) -> io::Result<IpcServer> {
    let socket_path = get_socket_path();
    remove_stale_socket_if_needed(&socket_path, &UnixSocketProbe).await?;
    let listener = UnixSocketListener::bind(&socket_path)?;
    Ok(start_ipc_server_with(listener, handler))
}

/// Start the server over an already-bound [`IpcListener`].
///
/// The seam entry point behind [`start_ipc_server`]: production passes a
/// [`UnixSocketListener`], tests pass the in-memory listener so the accept loop
/// and every connection run without a real socket.
pub fn start_ipc_server_with<L>(mut listener: L, handler: Arc<dyn IpcRequestHandler>) -> IpcServer
where
    L: IpcListener + 'static,
{
    let accept_task = tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
            let handler = handler.clone();
            tokio::spawn(handle_connection(stream, handler));
        }
    });
    IpcServer { accept_task }
}

/// Handle a single accepted connection.
///
/// Mirrors the `createServer` connection callback: read the first framed request,
/// dispatch it, and either send one response (ordinary requests) or flip into the
/// bidirectional RPC bridge (`rpc_stream`).
pub(crate) async fn handle_connection<S>(stream: S, handler: Arc<dyn IpcRequestHandler>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    let first_line = match read_next_line(&mut reader).await {
        Ok(Some(line)) => line,
        // Socket closed (or errored) before any request arrived.
        Ok(None) | Err(_) => return,
    };

    let request = match parse_request_line(&first_line) {
        Ok(request) => request,
        Err(error) => {
            // pi's outer `catch`: reply with an error frame and end the socket.
            let _ = write_frame(&mut write_half, &error_response(error.to_string())).await;
            let _ = write_half.shutdown().await;
            return;
        }
    };

    if let OrchestratorRequest::RpcStream(RpcStreamRequest { instance_id }) = &request {
        let instance_id = instance_id.clone();
        run_rpc_stream(reader, write_half, handler, request, instance_id).await;
        return;
    }

    let response = handler.handle(request).await;
    let _ = write_frame(&mut write_half, &response).await;
    let _ = write_half.shutdown().await;
}

/// Drive the bidirectional RPC bridge for an `rpc_stream` request.
///
/// Mirrors pi's `rpc_stream` branch: run the ready handshake, open the stream,
/// then relay client frames into the session and session frames back to the
/// client until the socket closes.
async fn run_rpc_stream<R, W>(
    mut reader: BufReader<R>,
    mut write_half: W,
    handler: Arc<dyn IpcRequestHandler>,
    request: OrchestratorRequest,
    instance_id: String,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Ready handshake. pi ends the socket unless the response is a truthy
    // `rpc_ready` carrying an instance.
    let response = handler.handle(request).await;
    let ready = matches!(
        &response,
        OrchestratorResponse::RpcReady(ready) if ready.base.ok && ready.instance.is_some()
    );
    if !ready {
        let _ = write_frame(&mut write_half, &response).await;
        let _ = write_half.shutdown().await;
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let sink = RpcStreamSink { tx: tx.clone() };
    let mut session = match handler.open_rpc_stream(instance_id.clone(), sink) {
        Some(session) => session,
        None => {
            let message = format!("Unknown instance: {instance_id}");
            let _ = write_frame(&mut write_half, &error_response(message)).await;
            let _ = write_half.shutdown().await;
            return;
        }
    };

    // The ready frame goes out first, ahead of any session frame (FIFO channel).
    let _ = tx.send(serde_json::to_value(&response).expect("serialize rpc_ready response"));

    // A single writer task owns the write half and flushes every queued frame,
    // so response/event/ui frames and error frames never interleave mid-line.
    let drain = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_half
                .write_all(encode_message(&frame).as_bytes())
                .await
                .is_err()
            {
                break;
            }
            if write_half.flush().await.is_err() {
                break;
            }
        }
        let _ = write_half.shutdown().await;
    });

    // Relay client frames into the session, one at a time (pi chains them on a
    // promise queue; awaiting each in turn is the same sequential behaviour).
    // Read until the socket closes (or errors); either ends the loop, then the
    // session is torn down (pi's `close`).
    while let Ok(Some(line)) = read_next_line(&mut reader).await {
        match serde_json::from_str::<RpcClientMessage>(&line) {
            Ok(message) => {
                if let Err(error) = session.handle_request(message).await {
                    let _ = tx.send(error_frame(&error));
                }
            }
            // pi parses the line inside the `try`, so a bad line is caught and
            // relayed as an error frame too.
            Err(error) => {
                let _ = tx.send(error_frame(&error.to_string()));
            }
        }
    }

    // Tear the session down and drop every frame sender (the local `tx` plus the
    // clone the session holds inside its sink) so the drain task's channel closes
    // and the writer half is released.
    session.close();
    drop(session);
    drop(tx);
    let _ = drain.await;
}

/// Remove a stale socket file before binding, mirroring pi's
/// `removeStaleSocketIfNeeded`.
///
/// If the path does not exist, there is nothing to do. If a live server answers
/// the probe, the orchestrator is already running and this errors. Otherwise the
/// stale file is removed. The [`SocketProbe`] seam lets the decision be tested
/// without a real `connect`.
pub(crate) async fn remove_stale_socket_if_needed<P>(
    socket_path: &Path,
    probe: &P,
) -> io::Result<()>
where
    P: SocketProbe,
{
    if !socket_path.exists() {
        return Ok(());
    }

    if probe.is_socket_live(socket_path).await? {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("orchestrator is already running: {}", socket_path.display()),
        ));
    }

    std::fs::remove_file(socket_path)?;
    Ok(())
}

/// Read the next non-empty, trimmed line, or `None` at end of stream.
///
/// Mirrors pi's buffer scan: split on `\n`, `trim`, and skip blank lines.
async fn read_next_line<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Ok(Some(trimmed.to_string()));
    }
}

/// Frame and write one message, mirroring `socket.write(encodeMessage(message))`.
async fn write_frame<W, T>(writer: &mut W, message: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    writer.write_all(encode_message(message).as_bytes()).await?;
    writer.flush().await
}

/// Build the `{ type: "error", ok: false, error }` response pi sends on failures.
fn error_response(message: String) -> OrchestratorResponse {
    OrchestratorResponse::Error(ErrorResponse {
        ok: false,
        error: message,
    })
}

/// The error response as a raw JSON frame, for relaying on the RPC bridge.
fn error_frame(message: &str) -> serde_json::Value {
    serde_json::to_value(error_response(message.to_string())).expect("serialize error response")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ipc::protocol::{
        encode_message, InstanceSummary, ListResponse, ResponseBase, RpcBridgeResponse,
        RpcReadyResponse, SpawnResponse, StatusResponse, StopResponse,
    };
    use crate::ipc::transport::in_memory_transport;
    use crate::types::InstanceStatus;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncWriteExt, DuplexStream};

    /// Build a summary for `instance_id` in the given status.
    fn summary(instance_id: &str, status: InstanceStatus) -> InstanceSummary {
        InstanceSummary {
            id: instance_id.to_string(),
            status,
            cwd: "/work".to_string(),
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        }
    }

    fn ok_base() -> ResponseBase {
        ResponseBase {
            ok: true,
            error: None,
        }
    }

    // --- test handlers ------------------------------------------------------

    /// A handler that answers every request with a canonical success response and
    /// opens an echoing RPC stream. `closed` records whether the session's
    /// `close` ran.
    pub(crate) struct EchoHandler {
        closed: Arc<AtomicBool>,
    }

    impl EchoHandler {
        pub(crate) fn new() -> Self {
            Self {
                closed: Arc::new(AtomicBool::new(false)),
            }
        }

        /// Shared flag flipped when the opened session is closed.
        pub(crate) fn closed_flag(&self) -> Arc<AtomicBool> {
            self.closed.clone()
        }
    }

    impl IpcRequestHandler for EchoHandler {
        fn handle(&self, request: OrchestratorRequest) -> BoxFuture<'_, OrchestratorResponse> {
            Box::pin(async move {
                match request {
                    OrchestratorRequest::List(_) => OrchestratorResponse::List(ListResponse {
                        base: ok_base(),
                        instances: Some(vec![]),
                    }),
                    OrchestratorRequest::Spawn(_) => OrchestratorResponse::Spawn(SpawnResponse {
                        base: ok_base(),
                        instance: None,
                    }),
                    OrchestratorRequest::Stop(stop) => OrchestratorResponse::Stop(StopResponse {
                        base: ok_base(),
                        instance_id: Some(stop.instance_id),
                    }),
                    OrchestratorRequest::Status(status) => {
                        OrchestratorResponse::Status(StatusResponse {
                            base: ok_base(),
                            instance: Some(summary(&status.instance_id, InstanceStatus::Online)),
                        })
                    }
                    OrchestratorRequest::Rpc(rpc) => OrchestratorResponse::Rpc(RpcBridgeResponse {
                        base: ok_base(),
                        response: rpc.command,
                    }),
                    OrchestratorRequest::RpcStream(stream) => {
                        OrchestratorResponse::RpcReady(RpcReadyResponse {
                            base: ok_base(),
                            instance: Some(summary(&stream.instance_id, InstanceStatus::Online)),
                        })
                    }
                }
            })
        }

        fn open_rpc_stream(
            &self,
            _instance_id: String,
            sink: RpcStreamSink,
        ) -> Option<Box<dyn RpcStreamSession>> {
            Some(Box::new(EchoSession {
                sink,
                closed: self.closed.clone(),
            }))
        }
    }

    /// An RPC session that echoes each client frame straight back as a response.
    struct EchoSession {
        sink: RpcStreamSink,
        closed: Arc<AtomicBool>,
    }

    impl RpcStreamSession for EchoSession {
        fn handle_request(
            &mut self,
            message: RpcClientMessage,
        ) -> BoxFuture<'_, Result<(), String>> {
            self.sink.send_response(message);
            Box::pin(async { Ok(()) })
        }

        fn close(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// A handler that acknowledges `rpc_stream` at the handshake but has no such
    /// instance, so `open_rpc_stream` returns `None`.
    pub(crate) struct RejectingStreamHandler;

    impl IpcRequestHandler for RejectingStreamHandler {
        fn handle(&self, request: OrchestratorRequest) -> BoxFuture<'_, OrchestratorResponse> {
            Box::pin(async move {
                match request {
                    OrchestratorRequest::RpcStream(stream) => {
                        OrchestratorResponse::RpcReady(RpcReadyResponse {
                            base: ok_base(),
                            instance: Some(summary(&stream.instance_id, InstanceStatus::Online)),
                        })
                    }
                    _ => error_response("unexpected request".to_string()),
                }
            })
        }

        fn open_rpc_stream(
            &self,
            _instance_id: String,
            _sink: RpcStreamSink,
        ) -> Option<Box<dyn RpcStreamSession>> {
            None
        }
    }

    /// Read one framed line from `stream` and JSON-parse it.
    async fn read_frame(stream: &mut DuplexStream) -> serde_json::Value {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    // --- accept loop over the in-memory transport ---------------------------

    #[tokio::test]
    async fn accept_loop_dispatches_each_connection() {
        let (connector, listener) = in_memory_transport();
        let handler = Arc::new(EchoHandler::new());
        let server = start_ipc_server_with(listener, handler);

        // Two independent connections each get their own response.
        for id in ["a", "b"] {
            use crate::ipc::client::send_ipc_request_via;
            let request = OrchestratorRequest::Status(crate::ipc::protocol::StatusRequest {
                instance_id: id.to_string(),
            });
            let response = send_ipc_request_via(&connector, &request).await.unwrap();
            match response {
                OrchestratorResponse::Status(status) => {
                    assert_eq!(status.instance.unwrap().id, id);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        drop(server);
    }

    #[tokio::test]
    async fn malformed_request_line_yields_an_error_frame() {
        let (mut client_end, server_end) = tokio::io::duplex(4096);
        let handler = Arc::new(EchoHandler::new());
        let server = tokio::spawn(handle_connection(server_end, handler));

        client_end
            .write_all(b"{ this is not json }\n")
            .await
            .unwrap();
        let frame = read_frame(&mut client_end).await;
        assert_eq!(frame["type"], "error");
        assert_eq!(frame["ok"], false);

        server.await.unwrap();
    }

    // --- rpc_stream bridge, both directions ---------------------------------

    #[tokio::test]
    async fn rpc_stream_bridge_relays_frames_both_directions() {
        let (mut client_end, server_end) = tokio::io::duplex(64 * 1024);
        let handler = Arc::new(EchoHandler::new());
        let closed = handler.closed_flag();
        let server = tokio::spawn(handle_connection(server_end, handler));

        // Open the stream and read the ready handshake (server -> client).
        let open = OrchestratorRequest::RpcStream(RpcStreamRequest {
            instance_id: "i-1".to_string(),
        });
        client_end
            .write_all(encode_message(&open).as_bytes())
            .await
            .unwrap();
        let ready = read_frame(&mut client_end).await;
        assert_eq!(ready["type"], "rpc_ready");
        assert_eq!(ready["ok"], true);
        assert_eq!(ready["instance"]["id"], "i-1");

        // Send a client frame (client -> server) and read the echoed response
        // frame (server -> client), proving both directions relay.
        let command = serde_json::json!({ "id": "7", "method": "ping" });
        client_end
            .write_all(encode_message(&command).as_bytes())
            .await
            .unwrap();
        let echoed = read_frame(&mut client_end).await;
        assert_eq!(echoed, command);

        // Closing the client tears the session down (pi's socket "close").
        drop(client_end);
        server.await.unwrap();
        assert!(closed.load(Ordering::SeqCst), "session close should run");
    }

    #[tokio::test]
    async fn rpc_stream_unknown_instance_ends_with_error() {
        let (mut client_end, server_end) = tokio::io::duplex(4096);
        let handler = Arc::new(RejectingStreamHandler);
        let server = tokio::spawn(handle_connection(server_end, handler));

        let open = OrchestratorRequest::RpcStream(RpcStreamRequest {
            instance_id: "ghost".to_string(),
        });
        client_end
            .write_all(encode_message(&open).as_bytes())
            .await
            .unwrap();
        let frame = read_frame(&mut client_end).await;
        assert_eq!(frame["type"], "error");
        assert_eq!(frame["error"], "Unknown instance: ghost");

        server.await.unwrap();
    }

    // --- stale-socket probe, injected (no real socket) ----------------------

    /// A [`SocketProbe`] returning a scripted liveness answer.
    struct FakeProbe {
        live: bool,
    }

    impl SocketProbe for FakeProbe {
        async fn is_socket_live(&self, _path: &Path) -> io::Result<bool> {
            Ok(self.live)
        }
    }

    #[tokio::test]
    async fn stale_probe_no_op_when_socket_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orchestrator.sock");
        // Missing file: Ok, and the probe is never consulted.
        remove_stale_socket_if_needed(&path, &FakeProbe { live: true })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stale_probe_errors_when_a_live_server_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orchestrator.sock");
        std::fs::write(&path, b"").unwrap();
        let error = remove_stale_socket_if_needed(&path, &FakeProbe { live: true })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(error.to_string().contains("already running"));
        // The file is left in place when a live server owns it.
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stale_probe_removes_a_dead_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orchestrator.sock");
        std::fs::write(&path, b"").unwrap();
        remove_stale_socket_if_needed(&path, &FakeProbe { live: false })
            .await
            .unwrap();
        assert!(!path.exists(), "stale socket file should be removed");
    }

    #[tokio::test]
    async fn unix_probe_reports_missing_socket_as_not_live() {
        // A path with no listener maps ENOENT -> false, with no socket bound.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.sock");
        let live = UnixSocketProbe.is_socket_live(&path).await.unwrap();
        assert!(!live);
    }
}
