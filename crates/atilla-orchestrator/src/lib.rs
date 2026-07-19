//! Rust mirror of `@earendil-works/pi-orchestrator` (`packages/orchestrator`).
//!
//! pi's orchestrator manages multiple coding-agent instances behind a Unix
//! socket, persisting machine/instance records and registering presence with
//! radius. This crate ports that package faithfully, leaf-first. The ported
//! modules so far cover the foundational layer — record types, path/env/version
//! config, and JSON-file persistence — plus the IPC wire protocol, the RPC child
//! process (spawn, JSONL framing, and request/response correlation), radius
//! presence (registration and heartbeat), and the IPC Unix-socket transport (the
//! [`ipc::client`] and [`ipc::server`], behind an in-memory-testable
//! [`ipc::transport`] seam). The supervisor and entry points are ported in
//! subsequent stages.
//!
//! The re-export barrel below mirrors pi's `index.ts`, limited to the modules
//! ported so far. Like pi's `index.ts`, it does **not** re-export [`radius`]:
//! that module is imported directly by the [`supervisor`]. The
//! [`credential_store`] module is a Rust-native seam supporting radius (pi reads
//! the file through `@earendil-works/pi-coding-agent`), so it is likewise not
//! part of pi's barrel.

pub mod config;
pub mod credential_store;
pub mod handler;
pub mod ipc;
pub mod radius;
pub mod rpc_process;
pub mod storage;
pub mod supervisor;
pub mod types;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-orchestrator";

/// A process-wide lock serializing every test that mutates a global environment
/// variable (`PI_ORCHESTRATOR_DIR`, `RADIUS_API_KEY`, `PI_RADIUS_*`). These vars
/// steer the storage/config/radius helpers, so tests across modules must not race
/// on them; a single shared lock serializes them all.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_orchestrator() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-orchestrator");
    }
}
