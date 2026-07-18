//! The atilla engine. The CLI is a thin shell over this crate; the real work
//! lands here so it stays testable without going through argv.
//!
//! This crate is also the façade over the mirror crates: it re-exports
//! [`ai`], [`agent`], and [`coding`] so callers reach the whole ported
//! surface through `atilla_core`. Those crates are empty scaffolds today.

pub use atilla_agent as agent;
pub use atilla_ai as ai;
pub use atilla_coding as coding;

use anyhow::Result;

/// The atilla engine version. This is the workspace version, surfaced through
/// the façade so every language binding reports one authoritative number
/// instead of hardcoding its own.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Placeholder engine entry point. Replace with the real surface as it lands.
pub fn run() -> Result<String> {
    Ok("atilla: nothing to do yet".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_a_message() {
        assert!(run().unwrap().starts_with("atilla:"));
    }

    #[test]
    fn version_is_the_workspace_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }
}
