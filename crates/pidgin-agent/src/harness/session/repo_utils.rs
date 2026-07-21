//! Shared session repository leaf helpers, mirroring
//! `packages/agent/src/harness/session/repo-utils.ts`.

use serde_json::Value;

use super::storage::SessionStorage;
use super::uuid::uuidv7;
use crate::harness::types::{SessionError, SessionErrorCode, SessionTreeEntry};

/// Where to fork relative to a target entry. Mirrors pi's `position` option.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ForkPosition {
    /// Path up to (and excluding) the target, which must be a user message.
    Before,
    /// Path up to and including the target.
    At,
}

/// Options for forking a session. Mirrors the fork option subset of
/// `SessionForkOptions`.
#[derive(Default)]
pub struct ForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
    pub id: Option<String>,
}

/// Generate a full uuidv7 session id. Mirrors `createSessionId`.
pub fn create_session_id() -> String {
    uuidv7()
}

/// Compute the entries a fork should copy. Mirrors `getEntriesToFork`.
pub fn get_entries_to_fork(
    storage: &dyn SessionStorage,
    entry_id: Option<&str>,
    position: Option<ForkPosition>,
) -> Result<Vec<SessionTreeEntry>, SessionError> {
    let Some(entry_id) = entry_id else {
        return Ok(storage.get_entries());
    };
    let target = storage.get_entry(entry_id).ok_or_else(|| {
        SessionError::new(
            SessionErrorCode::InvalidForkTarget,
            format!("Entry {entry_id} not found"),
        )
    })?;
    let effective_leaf_id: Option<String> = if position.unwrap_or(ForkPosition::Before)
        == ForkPosition::At
    {
        Some(target.id().to_string())
    } else {
        let is_user_message = matches!(
            &target,
            SessionTreeEntry::Message(e) if e.message.get("role").and_then(Value::as_str) == Some("user")
        );
        if !is_user_message {
            return Err(SessionError::new(
                SessionErrorCode::InvalidForkTarget,
                format!("Entry {entry_id} is not a user message"),
            ));
        }
        target.parent_id().map(str::to_string)
    };
    storage.get_path_to_root(effective_leaf_id.as_deref())
}
