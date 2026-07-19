//! The IPC client, mirroring `packages/orchestrator/src/ipc/client.ts`.
//!
//! pi's `sendIpcRequest` opens a connection to the orchestrator's Unix socket,
//! writes one newline-framed request, and resolves with the first response line
//! it reads back (rejecting if the socket errors or closes first). This port
//! keeps that one-shot request/response shape on a tokio stream.
//!
//! # The socket seam
//!
//! The connection is obtained through the [`IpcConnector`] seam (see
//! [`super::transport`]) rather than calling `UnixStream::connect` inline, so the
//! request/response framing can be exercised in-memory in tests. The public
//! [`send_ipc_request`] wires in the production [`UnixSocketConnector`] pointed
//! at [`crate::config::get_socket_path`] — the exact path pi's client uses.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::get_socket_path;
use crate::ipc::protocol::{
    encode_message, parse_response_line, OrchestratorRequest, OrchestratorResponse,
};
use crate::ipc::transport::{IpcConnector, UnixSocketConnector};

/// An error from [`send_ipc_request`], mirroring the ways pi's `sendIpcRequest`
/// promise rejects: a transport/`socket` error, a socket that closed before a
/// response arrived, or a malformed response line that failed to parse.
#[derive(Debug)]
pub enum IpcClientError {
    /// The underlying transport failed (pi's `socket.on("error")`).
    Io(std::io::Error),
    /// The socket closed before a response line was received (pi's
    /// `socket.on("end")` rejection).
    Closed(String),
    /// A response line was received but could not be parsed (pi's
    /// `parseResponseLine` throw).
    Parse(serde_json::Error),
}

impl std::fmt::Display for IpcClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcClientError::Io(error) => write!(f, "{error}"),
            IpcClientError::Closed(message) => f.write_str(message),
            IpcClientError::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for IpcClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IpcClientError::Io(error) => Some(error),
            IpcClientError::Parse(error) => Some(error),
            IpcClientError::Closed(_) => None,
        }
    }
}

impl From<std::io::Error> for IpcClientError {
    fn from(error: std::io::Error) -> Self {
        IpcClientError::Io(error)
    }
}

/// Send one request to the orchestrator and await the first response.
///
/// Mirrors pi's `sendIpcRequest`: connect to [`get_socket_path`], write the
/// framed request, and resolve with the first non-empty response line.
pub async fn send_ipc_request(
    request: &OrchestratorRequest,
) -> Result<OrchestratorResponse, IpcClientError> {
    let connector = UnixSocketConnector::new(get_socket_path());
    send_ipc_request_via(&connector, request).await
}

/// Send one request over a connection obtained from `connector`.
///
/// The seam entry point: production passes a [`UnixSocketConnector`], tests pass
/// the in-memory connector so the whole request/response round-trip runs without
/// a real socket.
pub(crate) async fn send_ipc_request_via<C: IpcConnector>(
    connector: &C,
    request: &OrchestratorRequest,
) -> Result<OrchestratorResponse, IpcClientError> {
    let stream = connector.connect().await?;
    send_ipc_request_over(stream, request).await
}

/// Write `request` to `stream` and read back the first response line.
///
/// The core framing logic, generic over the stream so it is identical for a real
/// [`tokio::net::UnixStream`] and an in-memory duplex pipe. Mirrors the body of
/// pi's `sendIpcRequest` promise: write `encodeMessage(request)`, then read until
/// the first newline, `trim`, skip blanks, and `parseResponseLine`.
pub(crate) async fn send_ipc_request_over<S>(
    stream: S,
    request: &OrchestratorRequest,
) -> Result<OrchestratorResponse, IpcClientError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_half
        .write_all(encode_message(request).as_bytes())
        .await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half);
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            // pi: `socket.on("end")` before a response → reject.
            return Err(IpcClientError::Closed(
                "Orchestrator socket closed before a response was received".to_string(),
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return parse_response_line(trimmed).map_err(IpcClientError::Parse);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::{
        ListRequest, ListResponse, ResponseBase, RpcStreamRequest, SpawnResponse,
    };
    use crate::ipc::server::handle_connection;
    use crate::ipc::server::tests::{EchoHandler, RejectingStreamHandler};
    use crate::ipc::transport::in_memory_transport;
    use crate::types::InstanceStatus;
    use std::sync::Arc;

    // --- send_ipc_request_over: core framing --------------------------------

    /// Pre-load `server_bytes` onto a duplex server end, then run the client core
    /// over the paired client end. Shared by the framing tests below.
    async fn request_over_preloaded(
        server_bytes: &[u8],
        request: &OrchestratorRequest,
    ) -> Result<OrchestratorResponse, IpcClientError> {
        let (client_end, mut server_end) = tokio::io::duplex(4096);
        tokio::io::AsyncWriteExt::write_all(&mut server_end, server_bytes)
            .await
            .unwrap();
        send_ipc_request_over(client_end, request).await
    }

    #[tokio::test]
    async fn reads_the_first_response_line_from_the_stream() {
        let response = OrchestratorResponse::List(ListResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            instances: Some(vec![]),
        });
        let request = OrchestratorRequest::List(ListRequest {});
        let received = request_over_preloaded(encode_message(&response).as_bytes(), &request)
            .await
            .unwrap();
        assert_eq!(received, response);
    }

    #[tokio::test]
    async fn skips_blank_lines_before_the_response() {
        let response = OrchestratorResponse::Spawn(SpawnResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            instance: None,
        });
        // Blank and whitespace-only lines precede the real response frame.
        let framed = format!("\n \n{}", encode_message(&response));
        let request = OrchestratorRequest::List(ListRequest {});
        let received = request_over_preloaded(framed.as_bytes(), &request)
            .await
            .unwrap();
        assert_eq!(received, response);
    }

    #[tokio::test]
    async fn errors_when_the_socket_closes_before_a_response() {
        let (client_end, mut server_end) = tokio::io::duplex(4096);
        // Accept the request, then close the socket without ever responding —
        // the client must reject with `Closed` on the resulting EOF.
        tokio::spawn(async move {
            let mut scratch = [0u8; 256];
            let _ = tokio::io::AsyncReadExt::read(&mut server_end, &mut scratch).await;
            drop(server_end);
        });
        let request = OrchestratorRequest::List(ListRequest {});
        let error = send_ipc_request_over(client_end, &request)
            .await
            .unwrap_err();
        assert!(matches!(error, IpcClientError::Closed(_)));
    }

    #[tokio::test]
    async fn errors_when_the_response_line_is_malformed() {
        let (client_end, mut server_end) = tokio::io::duplex(4096);
        tokio::io::AsyncWriteExt::write_all(&mut server_end, b"{not valid json}\n")
            .await
            .unwrap();
        let request = OrchestratorRequest::List(ListRequest {});
        let error = send_ipc_request_over(client_end, &request)
            .await
            .unwrap_err();
        assert!(matches!(error, IpcClientError::Parse(_)));
    }

    // --- end-to-end through the in-memory transport + server ----------------

    #[tokio::test]
    async fn round_trips_a_request_through_the_server() {
        // client sends a request -> server receives + responds -> client parses.
        let (connector, mut listener) = in_memory_transport();
        let handler = Arc::new(EchoHandler::new());
        {
            let handler = handler.clone();
            tokio::spawn(async move {
                use crate::ipc::transport::IpcListener;
                let stream = listener.accept().await.unwrap();
                handle_connection(stream, handler).await;
            });
        }

        let request = OrchestratorRequest::Status(crate::ipc::protocol::StatusRequest {
            instance_id: "i-42".to_string(),
        });
        let response = send_ipc_request_via(&connector, &request).await.unwrap();
        match response {
            OrchestratorResponse::Status(status) => {
                assert!(status.base.ok);
                let instance = status.instance.expect("instance summary");
                assert_eq!(instance.id, "i-42");
                assert_eq!(instance.status, InstanceStatus::Online);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn surfaces_an_error_response_from_the_server() {
        // A handler that refuses rpc_stream returns an error response, which the
        // client parses and returns like any other response.
        let (connector, mut listener) = in_memory_transport();
        let handler = Arc::new(RejectingStreamHandler);
        tokio::spawn(async move {
            use crate::ipc::transport::IpcListener;
            let stream = listener.accept().await.unwrap();
            handle_connection(stream, handler).await;
        });

        let request = OrchestratorRequest::RpcStream(RpcStreamRequest {
            instance_id: "missing".to_string(),
        });
        let response = send_ipc_request_via(&connector, &request).await.unwrap();
        match response {
            OrchestratorResponse::Error(error) => {
                assert!(!error.ok);
                assert!(error.error.contains("missing"));
            }
            other => panic!("expected error response, got {other:?}"),
        }
    }
}
