//! Rust mirror of `@earendil-works/pi-orchestrator` (`packages/orchestrator`).
//!
//! pi's orchestrator manages multiple coding-agent instances behind a Unix
//! socket, persisting machine/instance records and registering presence with
//! radius. This crate ports that package faithfully, leaf-first. This first
//! stage covers the foundational modules: record types, path/env/version
//! config, and JSON-file persistence. The IPC protocol, RPC child process,
//! radius presence, socket transport, supervisor, and entry points are ported
//! in subsequent stages.
//!
//! The re-export barrel below mirrors pi's `index.ts`, limited to the modules
//! ported so far.

pub mod config;
pub mod storage;
pub mod types;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-orchestrator";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_orchestrator() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-orchestrator");
    }
}
