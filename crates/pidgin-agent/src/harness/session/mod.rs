//! Version-3 JSONL session tree, ported from
//! `packages/agent/src/harness/session`.
//!
//! agent-core is the canonical v3 schema (the `.` entrypoint and the
//! leaf-persisting tree model), so this is what the port mirrors. The leaf
//! pointer is a persisted `{"type":"leaf",...}` line, the header carries
//! optional `metadata`, and non-version-3 headers are hard-rejected.

pub mod jsonl_repo;
pub mod jsonl_storage;
pub mod memory_repo;
pub mod messages;
pub mod repo_utils;
// The `session` module mirrors pi's `session/session.ts` inside `session/`.
#[allow(clippy::module_inception)]
pub mod session;
pub mod storage;
pub mod uuid;

pub use jsonl_repo::{JsonlCreate, JsonlSessionRepo};
pub use jsonl_storage::{
    load_jsonl_session_metadata, serialize_entry_line, serialize_header_line, JsonlCreateOptions,
    JsonlSessionStorage,
};
pub use memory_repo::InMemorySessionRepo;
pub use messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};
pub use repo_utils::{create_session_id, get_entries_to_fork, ForkOptions, ForkPosition};
pub use session::{
    build_context_entries, build_session_context, default_context_entry_transform,
    session_entry_to_context_messages, ContextEntryTransform, CustomEntryProjector, MoveSummary,
    Session, SessionContextBuildOptions,
};
pub use storage::{InMemorySessionStorage, SessionStorage};
pub use uuid::{uuidv7, Uuidv7Generator};
