//! Rust mirror of `@earendil-works/pi-agent-core` (`packages/agent`).
//!
//! pi's agent package splits into two entry points: the portable `.` export
//! (`index.ts`, aggregating `agent`, `agent-loop`, `harness`, `proxy`, and
//! `types`) and a platform-specific `./node` export (`node.ts`). The modules
//! below mirror that split: everything is portable except [`node`], which
//! carries the Node-only surface. Port order runs `types` first, then the
//! `agent`/`agent_loop`/`harness` core, then `proxy`, then `node`. Every
//! module here is an empty stub — no logic is ported yet.

pub mod agent;
pub mod agent_loop;
pub mod harness;
pub mod node;
pub mod proxy;
pub mod types;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-agent-core";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_agent_core() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-agent-core");
    }
}
