//! Deferred port of pi's `utils/child-process.ts`.
//!
//! The original spawns and supervises OS child processes (streaming stdio,
//! cross-spawn Windows argument handling, sync + async lifecycles). A faithful
//! Rust port needs `std::process`/`tokio::process` plumbing that is out of
//! scope for the pure-utilities PR. Not yet ported.
