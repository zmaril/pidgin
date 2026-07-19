//! Mirrors pi-coding-agent's `modes` module (`packages/coding-agent/src/modes`).
//!
//! pi exposes interactive, print, and rpc run modes. The headless RPC
//! entrypoint and single-shot print mode are ported; interactive remains
//! unported.

pub mod interactive;
pub mod print;
pub mod rpc;
