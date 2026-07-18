//! The `./node` entrypoint, mirroring `packages/agent/src/node.ts`.
//!
//! pi's `node.ts` is the platform-specific superset entrypoint:
//!
//! ```ts
//! export { NodeExecutionEnv } from "./harness/env/nodejs.ts";
//! export * from "./index.ts";
//! ```
//!
//! This module reproduces that surface: it re-exports the host-backed
//! [`NodeExecutionEnv`] alongside the crate's portable public surface (the
//! `.`/`index.ts` export, which in this crate is the library root).

pub use crate::harness::env::NodeExecutionEnv;

#[doc(no_inline)]
pub use crate::*;
