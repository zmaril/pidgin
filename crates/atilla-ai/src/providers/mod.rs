//! Provider implementations, mirroring pi-ai's `providers` module
//! (`packages/ai/src/providers`).
//!
//! Providers implement the [`crate::seams::provider::Provider`] seam. Stage 3
//! ports pi's faux provider ([`faux`]) — the scripted, deterministic provider
//! pi's agent and coding-agent tests drive via `registerFauxProvider`. The real
//! wire providers implement the same seam as their HTTP/streaming paths land.

pub mod faux;
