//! IPC layer mirroring `packages/orchestrator/src/ipc/`.
//!
//! pi's `ipc/` directory holds the newline-framed JSON wire protocol
//! (`protocol.ts`) plus the Unix-socket client (`client.ts`) and server
//! (`server.ts`) that speak it. This stage ports all three: the protocol module,
//! the [`client`] ([`client::send_ipc_request`]), and the [`server`]
//! ([`server::start_ipc_server`]).
//!
//! The socket itself sits behind the [`transport`] seam — a Rust-native
//! abstraction (not present in pi, which calls `node:net` inline) that lets the
//! client and server framing be driven in-memory in tests. Production uses the
//! real [`tokio::net`] Unix types; tests use in-memory duplex pipes.

pub mod client;
pub mod protocol;
pub mod server;
pub mod transport;
