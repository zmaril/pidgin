//! `modes/rpc` — headless JSONL RPC entrypoint.
//!
//! Mirrors pi's `packages/coding-agent/src/modes/rpc/rpc-mode.ts`
//! (`runRpcMode(runtimeHost: AgentSessionRuntime): Promise<never>`).
//!
//! This is a clean function boundary left for the RPC worker. pi's real entry
//! point drives a JSONL command/response loop over stdin/stdout against a live
//! agent-session runtime. That runtime (`AgentSessionRuntime`/`AgentSession`)
//! is not yet ported to Rust, so the parameter is intentionally omitted for
//! now; a later worker will add it and fill in the protocol loop.

/// Entry point for `--mode rpc`.
///
/// Mirrors pi's `runRpcMode`. The return type is [`std::convert::Infallible`]
/// wrapped in a `Result` to mirror pi's `Promise<never>`: on success the loop
/// runs until the process exits, so the `Ok` variant can never be produced.
///
/// Currently a placeholder that reports an honest error (never a panic). The
/// caller is responsible for printing the diagnostic and choosing an exit code.
pub fn run_rpc_mode() -> anyhow::Result<std::convert::Infallible> {
    anyhow::bail!("RPC mode is not yet implemented in atilla")
}
