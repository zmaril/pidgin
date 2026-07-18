//! Mirrors pi-coding-agent's `modes` module (`packages/coding-agent/src/modes`).
//!
//! pi exposes interactive, print, and rpc run modes. Only the headless RPC
//! entrypoint boundary is scaffolded so far; the others remain unported.

pub mod interactive;
pub mod rpc;
