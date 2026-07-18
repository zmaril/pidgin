//! Deferred port of pi's `core/tools/bash.ts`.
//!
//! The bash tool spawns a subprocess (or PTY) via an `ExecutionEnv`, streams
//! stdout/stderr through an `OutputAccumulator`, and enforces timeouts and
//! abort signals. All of that is process-execution machinery with no pure
//! algorithm to isolate, so it is Not yet ported: it waits on the child-process
//! and execution-environment layer.
