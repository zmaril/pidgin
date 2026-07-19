//! Hand-written core implementation behind the generated `PidginCore` trait.
//!
//! The generated binding (`src/generated.rs`) routes every PHP-visible op
//! through this trait impl, so the engine wiring lives here — hand-written and
//! stable — while the PHP surface is regenerated from the fluessig api schema
//! (`schema/api.json`). See README.md for the regeneration pipeline.

/// The engine-backed implementation of the generated `Pidgin` contract.
///
/// Stateless for M0: the single `version` op reaches straight through the
/// `pidgin-core` façade, so PHP sees the same authoritative version number as
/// the Rust core rather than a value baked into this binding.
pub struct PidginImpl;

impl crate::generated::PidginCore for PidginImpl {
    fn version() -> anyhow::Result<String> {
        Ok(pidgin_core::version().to_string())
    }
}
