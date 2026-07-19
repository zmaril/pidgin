//! The `atilla-orchestrator` binary: a thin shell mirroring pi's `cli.ts`
//! executable (`#!/usr/bin/env node`).
//!
//! Following the atilla CLI convention (the main crate `atilla-cli` declares a
//! `[[bin]]` with an explicit `name`/`path = "src/main.rs"`), this crate declares
//! its binary as `[[bin]] name = "atilla-orchestrator", path = "src/main.rs"`.
//! All parsing and dispatch lives in [`cli`]; `main` only parses argv and runs
//! the selected subcommand, returning its exit code.

mod cli;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    cli::run().await
}
