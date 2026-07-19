//! IPC layer mirroring `packages/orchestrator/src/ipc/`.
//!
//! pi's `ipc/` directory holds the newline-framed JSON wire protocol
//! (`protocol.ts`) plus the Unix-socket client (`client.ts`) and server
//! (`server.ts`) that speak it. This stage ports the protocol module; the
//! socket transport is ported in a later stage.

pub mod protocol;
