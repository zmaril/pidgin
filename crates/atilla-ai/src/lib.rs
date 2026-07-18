//! Rust mirror of `@earendil-works/pi-ai` (`packages/ai`).
//!
//! This crate mirrors the provider and model surface of pi's AI package.
//! Modules mirror pi's `src/` top-level layout; port order runs roughly
//! `types` and `utils` first, then `auth`, `providers`, `api`, and finally
//! `compat`. Stage 1 ports the boundary types (`types.ts`) and cost math
//! (`models.ts`'s `calculateCost`); Stage 2 ports the Anthropic Messages SSE
//! streaming parser (`api/anthropic.rs`) and its JSON-repair helpers
//! (`utils/json_parse.rs`). The remaining modules are still stubs.

pub mod api;
pub mod auth;
pub mod compat;
pub mod cost;
pub mod providers;
pub mod types;
pub mod utils;

pub use cost::{calculate_cost, calculate_cost_with};
pub use types::*;

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
