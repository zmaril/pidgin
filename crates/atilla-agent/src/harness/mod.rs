//! Rust mirror of `packages/agent/src/harness`.
//!
//! Only the session subsystem is ported so far. It mirrors
//! `src/harness/types.ts` (the session-tree entry union and supporting types)
//! and `src/harness/session/*` (uuidv7, storage, session, repo), reproducing
//! pi's agent-core version-3 JSONL session-tree format byte-for-byte.

pub mod compaction;
pub mod session;
pub mod types;
