//! The Unix-socket transport seam for the IPC client and server.
//!
//! # What this abstracts in pi
//!
//! pi's [`client.ts`] and [`server.ts`] talk to `node:net` directly:
//! `createConnection(socketPath)` on the client, `createServer(...)` +
//! `server.listen(socketPath)` on the server, and a probe
//! `createConnection(socketPath)` to detect a stale socket. There is exactly one
//! transport — the OS Unix-domain socket — and no test ever exercises it (pi
//! ships zero orchestrator tests).
//!
//! [`client.ts`]: https://github.com/earendil-works/pi
//! [`server.ts`]: https://github.com/earendil-works/pi
//!
//! # The seam
//!
//! Per the coordinator-approved seam decision, the socket is enabled natively
//! with tokio's `net` feature (a real [`tokio::net::UnixStream`] /
//! [`tokio::net::UnixListener`]), but kept behind a **small trait** so the client
//! and server logic can be driven **in-memory** in tests without binding a real
//! filesystem socket. The two directions are modelled after the pidgin-ai
//! transport seams (an injectable trait with a production impl and a test impl):
//!
//! - [`IpcConnector`] — the client side: "dial the orchestrator and yield a
//!   connected byte stream" (pi's `createConnection`).
//! - [`IpcListener`] — the server side: "accept the next inbound connection,
//!   yielding one byte stream" (pi's `createServer` connection callback).
//!
//! Both yield any `AsyncRead + AsyncWrite` stream, so the framing logic in
//! [`super::client`] and [`super::server`] is written once, generically, over
//! the stream. The production implementations ([`UnixSocketConnector`],
//! [`UnixSocketListener`]) wrap the tokio Unix types; the test transport
//! ([`in_memory_transport`]) wires a connector to a listener through
//! [`tokio::io::duplex`] pipes so a client and a server run in one process with
//! no socket file at all.
//!
//! The stale-socket liveness probe is likewise behind [`SocketProbe`] so the
//! `remove_stale_socket_if_needed` decision (pi's `removeStaleSocketIfNeeded` +
//! `isSocketLive`) can be unit-tested with an injected probe instead of a real
//! `connect`.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{UnixListener, UnixStream};

/// The client side of the socket seam: dial the orchestrator socket and yield a
/// connected bidirectional byte stream.
///
/// Mirrors pi's `createConnection(socketPath)` in `client.ts`. The associated
/// [`IpcConnector::Stream`] lets the framing logic stay generic — production
/// yields a [`UnixStream`], tests yield a [`tokio::io::DuplexStream`].
pub trait IpcConnector: Send + Sync {
    /// The connected stream this connector produces.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Connect to the orchestrator, resolving to a fresh stream.
    fn connect(&self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

/// The server side of the socket seam: accept the next inbound connection,
/// yielding one bidirectional byte stream per client.
///
/// Mirrors the connection callback pi registers with `createServer(...)` in
/// `server.ts`: each accepted connection is handled independently.
pub trait IpcListener: Send {
    /// The accepted stream this listener produces.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Accept the next inbound connection.
    fn accept(&mut self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

/// Probes whether a socket path has a live listener, mirroring pi's
/// `isSocketLive`.
///
/// Behind a trait so [`super::server::remove_stale_socket_if_needed`] can be
/// unit-tested with an injected decision instead of a real `connect`.
pub trait SocketProbe: Send + Sync {
    /// Resolve to `true` if a live server is listening on `path`, `false` if the
    /// socket is stale (connection refused / not found / reset), or an error for
    /// any other failure — mirroring pi's `isSocketLive` promise.
    fn is_socket_live(&self, path: &Path) -> impl Future<Output = io::Result<bool>> + Send;
}

/// Production [`IpcConnector`] over a real Unix-domain socket.
///
/// Wraps pi's `createConnection(socketPath)`.
#[derive(Debug, Clone)]
pub struct UnixSocketConnector {
    path: PathBuf,
}

impl UnixSocketConnector {
    /// A connector that dials the socket at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl IpcConnector for UnixSocketConnector {
    type Stream = UnixStream;

    async fn connect(&self) -> io::Result<Self::Stream> {
        UnixStream::connect(&self.path).await
    }
}

/// Production [`IpcListener`] over a real Unix-domain socket.
///
/// Wraps `createServer(...)` + `server.listen(socketPath)`: [`bind`] performs the
/// listen (surfacing bind errors synchronously, like pi's `server.listen`
/// `error` handler), and [`accept`] yields each connection.
///
/// [`bind`]: UnixSocketListener::bind
/// [`accept`]: IpcListener::accept
#[derive(Debug)]
pub struct UnixSocketListener {
    inner: UnixListener,
}

impl UnixSocketListener {
    /// Bind and start listening on the socket at `path`.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            inner: UnixListener::bind(path)?,
        })
    }
}

impl IpcListener for UnixSocketListener {
    type Stream = UnixStream;

    async fn accept(&mut self) -> io::Result<Self::Stream> {
        let (stream, _addr) = self.inner.accept().await?;
        Ok(stream)
    }
}

/// Production [`SocketProbe`] that attempts a real connection, mirroring pi's
/// `isSocketLive`.
///
/// pi resolves `true` on `connect`, `false` for `ECONNREFUSED` / `ENOENT` /
/// `EPIPE` / `ECONNRESET`, and rejects otherwise. Those `errno`s map to the
/// [`io::ErrorKind`] variants matched below.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnixSocketProbe;

impl SocketProbe for UnixSocketProbe {
    async fn is_socket_live(&self, path: &Path) -> io::Result<bool> {
        match UnixStream::connect(path).await {
            Ok(_) => Ok(true),
            Err(error) => match error.kind() {
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::NotFound
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset => Ok(false),
                _ => Err(error),
            },
        }
    }
}

#[cfg(test)]
pub(crate) use testing::in_memory_transport;

/// In-memory transport used to drive the client and server together in tests,
/// with no real socket file.
///
/// A [`InMemoryConnector::connect`] creates a [`tokio::io::duplex`] pipe, hands
/// one end to the paired [`InMemoryListener`] over a channel, and returns the
/// other end — so a client and a server can talk end-to-end in one process.
#[cfg(test)]
mod testing {
    use super::*;
    use tokio::io::DuplexStream;
    use tokio::sync::mpsc;

    /// Buffer size for each in-memory duplex pipe. Generous relative to the tiny
    /// JSON frames the tests exchange.
    const DUPLEX_BUFFER: usize = 64 * 1024;

    /// Test [`IpcConnector`] that dials the paired [`InMemoryListener`].
    #[derive(Debug, Clone)]
    pub(crate) struct InMemoryConnector {
        tx: mpsc::UnboundedSender<DuplexStream>,
    }

    /// Test [`IpcListener`] fed by [`InMemoryConnector::connect`].
    #[derive(Debug)]
    pub(crate) struct InMemoryListener {
        rx: mpsc::UnboundedReceiver<DuplexStream>,
    }

    /// Build a connected connector/listener pair sharing an in-memory channel.
    pub(crate) fn in_memory_transport() -> (InMemoryConnector, InMemoryListener) {
        let (tx, rx) = mpsc::unbounded_channel();
        (InMemoryConnector { tx }, InMemoryListener { rx })
    }

    impl IpcConnector for InMemoryConnector {
        type Stream = DuplexStream;

        async fn connect(&self) -> io::Result<Self::Stream> {
            let (client_end, server_end) = tokio::io::duplex(DUPLEX_BUFFER);
            self.tx.send(server_end).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "in-memory listener was dropped",
                )
            })?;
            Ok(client_end)
        }
    }

    impl IpcListener for InMemoryListener {
        type Stream = DuplexStream;

        async fn accept(&mut self) -> io::Result<Self::Stream> {
            self.rx.recv().await.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "all in-memory connectors were dropped",
                )
            })
        }
    }
}
