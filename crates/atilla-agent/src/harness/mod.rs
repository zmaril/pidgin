//! Rust mirror of `packages/agent/src/harness`.
//!
//! [`types`] mirrors `src/harness/types.ts` (the session-tree entry union and
//! supporting types) and [`session`] mirrors `src/harness/session/*` (uuidv7,
//! storage, session, repo), reproducing pi's agent-core version-3 JSONL
//! session-tree format byte-for-byte. [`env`] ports the harness execution
//! environment contract (`FileSystem`/`Shell`/`ExecutionEnv`, the `Result`
//! monad, and the `FileError`/`ExecutionError` types), [`utils`] ports the
//! `truncate`/`shell-output` leaves, and [`messages`] ports the synthesized
//! harness messages and LLM conversion.

pub mod env;
pub mod messages;
pub mod session;
pub mod types;
pub mod utils;
