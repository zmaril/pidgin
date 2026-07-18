//! Rust mirror of `@earendil-works/pi-ai` (`packages/ai`).
//!
//! This crate scaffolds the provider and model surface of pi's AI package.
//! Modules mirror pi's `src/` top-level layout; port order runs roughly
//! `types` and `utils` first, then `auth`, `providers`, `api`, and finally
//! `compat`. Every module here is an empty stub — no logic is ported yet.

pub mod api;
pub mod auth;
pub mod compat;
pub mod providers;
pub mod utils;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-ai";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_ai() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-ai");
    }
}
