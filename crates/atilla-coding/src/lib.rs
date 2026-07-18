//! Rust mirror of `@earendil-works/pi-coding-agent` (`packages/coding-agent`).
//!
//! This crate is a light scaffold of pi's coding-agent shell. Modules mirror
//! pi's `src/` top-level directories; port order runs `utils` and `core`
//! first, then `modes`, `extensions`, and the `cli`/`bun` entry points. Every
//! module here is an empty stub — no logic is ported yet.

pub mod bun;
pub mod cli;
pub mod core;
pub mod extensions;
pub mod modes;
pub mod utils;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_coding_agent() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-coding-agent");
    }
}
