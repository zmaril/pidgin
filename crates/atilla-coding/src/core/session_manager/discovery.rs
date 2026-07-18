//! Slice C: the coding-agent `SessionManager` discovery / list / fork surface.
//!
//! This child module adds the last of the `SessionManager` port: the
//! session-directory scanners that build [`SessionInfo`](super::SessionInfo)
//! rows ([`build_session_info`], [`SessionManager::list`],
//! [`SessionManager::list_all`]), resuming the most-recent session for a cwd
//! ([`SessionManager::continue_recent`]), and forking one session file into a
//! new one under a target cwd ([`SessionManager::fork_from`]).
//!
//! It reuses slice B's [`io`](super::io) helpers — the lenient reader, the
//! per-cwd/root directory computation, path resolution, and the deferred-flush
//! write path — rather than re-porting any of them. The load-bearing fidelity
//! points preserved from pi:
//!
//! - **`SessionInfo.modified`** is the newest user/assistant *message* activity
//!   time (a numeric `message.timestamp`, else the entry's ISO `timestamp`),
//!   falling back to the header time and then the file mtime — never blindly the
//!   mtime (`session-manager.ts:676-687`, pinned by
//!   `session-info-modified-timestamp.test.ts`).
//! - **`firstMessage`** is the first *user* message's text (`"(no messages)"`
//!   when none), **`messageCount`** counts every `message` entry, and the list
//!   is sorted by `modified` descending.
//! - **cwd scoping.** [`list`](SessionManager::list) /
//!   [`continue_recent`](SessionManager::continue_recent) filter to the cwd only
//!   when an explicit session directory that differs from the per-cwd default is
//!   passed, exactly like pi; [`list_all`](SessionManager::list_all) never
//!   filters.
//! - **Fork** copies every non-header entry verbatim into a freshly created file
//!   whose header carries a new id and a `parentSession` back-pointer to the
//!   source — no leaf line, coding-agent key order preserved by round-tripping
//!   through the typed [`SessionEntry`](super::SessionEntry).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use atilla_agent::harness::session::messages::parse_iso_millis;

use super::io;
use super::{
    assert_valid_session_id, format_iso_millis, NewSessionOptions, SessionEntry, SessionHeader,
    SessionInfo, SessionManager, SessionTag, CURRENT_SESSION_VERSION,
};

// ===========================================================================
// SessionInfo construction
// ===========================================================================

/// Whether `message` is a role-bearing message with a `content` key. Mirrors
/// pi's `isMessageWithContent`.
fn is_message_with_content(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str).is_some()
        && message
            .as_object()
            .is_some_and(|object| object.contains_key("content"))
}

/// The plain-text projection of a message. Mirrors `extractTextContent`: a
/// string content passes through; an array joins its `text` blocks with spaces.
fn extract_text_content(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The activity time of a user/assistant message, if any. Mirrors
/// `getMessageActivityTime`: a numeric `message.timestamp` wins, else the
/// entry's ISO `timestamp`, else `None`.
fn message_activity_time(message: &Value, entry_timestamp: &str) -> Option<i64> {
    if !is_message_with_content(message) {
        return None;
    }
    let role = message.get("role").and_then(Value::as_str);
    if role != Some("user") && role != Some("assistant") {
        return None;
    }
    if let Some(number) = message.get("timestamp") {
        if let Some(millis) = number.as_i64() {
            return Some(millis);
        }
        if let Some(millis) = number.as_f64() {
            return Some(millis as i64);
        }
    }
    iso_millis(entry_timestamp)
}

/// Parse an ISO timestamp to epoch millis, or `None` when it is not a
/// parseable date — the distinction pi draws with
/// `Number.isNaN(new Date(...).getTime())`.
///
/// [`parse_iso_millis`] already gates on the `YYYY-MM-DDThh:mm:ss` shape and
/// yields `0` for anything it cannot parse, so a `0` result stands in for
/// "invalid" here. The one true-epoch instant (`1970-01-01T00:00:00.000Z`) is
/// indistinguishable from invalid, but it is never a real session/message time,
/// so the collision is inert.
fn iso_millis(timestamp: &str) -> Option<i64> {
    match parse_iso_millis(timestamp) {
        0 => None,
        millis => Some(millis),
    }
}

fn system_time_millis(time: Option<SystemTime>) -> i64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build the listing metadata for one session file by streaming it once.
/// Mirrors `buildSessionInfo`; returns `None` when the file cannot be read or
/// does not begin with a `{"type":"session",...}` header.
pub fn build_session_info(path: &str) -> Option<SessionInfo> {
    let metadata = std::fs::metadata(path).ok()?;
    let file = File::open(path).ok()?;

    let mut header: Option<Value> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity: Option<i64> = None;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let entry_type = value.get("type").and_then(Value::as_str);

        if header.is_none() {
            if entry_type != Some("session") {
                return None;
            }
            header = Some(value);
            continue;
        }

        // Latest `session_info` wins, including explicit clears (empty name).
        if entry_type == Some("session_info") {
            name = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|trimmed| !trimmed.is_empty())
                .map(str::to_string);
        }

        if entry_type != Some("message") {
            continue;
        }
        message_count += 1;

        let entry_timestamp = value.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let message = value.get("message").cloned().unwrap_or(Value::Null);
        if let Some(activity) = message_activity_time(&message, entry_timestamp) {
            last_activity = Some(last_activity.unwrap_or(0).max(activity));
        }

        if !is_message_with_content(&message) {
            continue;
        }
        let role = message.get("role").and_then(Value::as_str);
        if role != Some("user") && role != Some("assistant") {
            continue;
        }
        let text = extract_text_content(&message);
        if text.is_empty() {
            continue;
        }
        if first_message.is_empty() && role == Some("user") {
            first_message = text.clone();
        }
        all_messages.push(text);
    }

    let header = header?;
    let header_timestamp = header
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("");
    let header_millis = iso_millis(header_timestamp);
    let modified_millis = match last_activity {
        Some(activity) if activity > 0 => activity,
        _ => header_millis.unwrap_or_else(|| system_time_millis(metadata.modified().ok())),
    };

    Some(SessionInfo {
        path: path.to_string(),
        id: header
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        cwd: header
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name,
        parent_session_path: header
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_string),
        created: header_timestamp.to_string(),
        modified: format_iso_millis(modified_millis),
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_string()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    })
}

// ===========================================================================
// Directory scanning
// ===========================================================================

/// The `*.jsonl` session infos in `dir`, unsorted. Mirrors `listSessionsFromDir`
/// (the sequential equivalent of pi's bounded-concurrency streaming — the port
/// matches the result set and final ordering, not the concurrency mechanism).
fn list_sessions_from_dir(dir: &str) -> Vec<SessionInfo> {
    io::jsonl_files_in_dir(dir)
        .iter()
        .filter_map(|path| path.to_str().and_then(build_session_info))
        .collect()
}

/// Sort a listing by `modified` descending (newest first), stably. Mirrors pi's
/// `sessions.sort((a, b) => b.modified.getTime() - a.modified.getTime())`.
fn sort_by_modified_desc(sessions: &mut [SessionInfo]) {
    sessions.sort_by_key(|b| std::cmp::Reverse(parse_iso_millis(&b.modified)));
}

/// Resolve the directory to scan and whether cwd filtering applies for the
/// current-folder APIs. cwd filtering kicks in only when an explicit
/// `session_dir` differing from the per-cwd default is passed — the shared
/// preamble of pi's `list` and `continueRecent`.
fn resolve_scan_dir(cwd: &str, session_dir: Option<&str>) -> (String, bool) {
    let dir = match session_dir {
        Some(dir) => io::normalize_input(dir),
        None => io::get_default_session_dir(cwd),
    };
    let filter_cwd = session_dir.is_some() && dir != io::default_session_dir_path(cwd);
    (dir, filter_cwd)
}

impl SessionManager {
    /// List the sessions for `cwd`. Mirrors `SessionManager.list`.
    ///
    /// `session_dir` defaults to the encoded per-cwd directory. When an explicit
    /// directory differing from that default is passed, results are filtered to
    /// sessions whose header cwd matches `cwd`; otherwise every session in the
    /// directory is returned. The list is sorted by `modified` descending.
    pub fn list(cwd: &str, session_dir: Option<&str>) -> Vec<SessionInfo> {
        let (dir, filter_cwd) = resolve_scan_dir(cwd, session_dir);
        let resolved_cwd = io::resolve_input(cwd);

        let mut sessions = list_sessions_from_dir(&dir);
        if filter_cwd {
            sessions.retain(|session| io::session_cwd_matches(&session.cwd, &resolved_cwd));
        }
        sort_by_modified_desc(&mut sessions);
        sessions
    }

    /// List every session, unscoped by cwd. Mirrors `SessionManager.listAll`.
    ///
    /// With an explicit `session_dir` the flat directory is scanned; with `None`
    /// every per-cwd subdirectory under the sessions root is scanned. Always
    /// sorted by `modified` descending.
    pub fn list_all(session_dir: Option<&str>) -> Vec<SessionInfo> {
        let mut sessions = match session_dir {
            Some(dir) => list_sessions_from_dir(&io::normalize_input(dir)),
            None => {
                let mut all = Vec::new();
                if let Ok(read_dir) = std::fs::read_dir(io::sessions_dir_path()) {
                    for entry in read_dir.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            all.extend(list_sessions_from_dir(&path.to_string_lossy()));
                        }
                    }
                }
                all
            }
        };
        sort_by_modified_desc(&mut sessions);
        sessions
    }

    /// Resume the most recent session for `cwd`, or start a fresh one when none
    /// exists. Mirrors `SessionManager.continueRecent`.
    ///
    /// cwd filtering of the most-recent search applies only when an explicit
    /// `session_dir` differing from the per-cwd default is passed. The resumed
    /// session keeps `cwd` (it is *not* re-derived from the file header, unlike
    /// [`open`](SessionManager::open)).
    pub fn continue_recent(cwd: &str, session_dir: Option<&str>) -> Self {
        let (dir, filter_cwd) = resolve_scan_dir(cwd, session_dir);
        let mut manager = Self::empty(cwd, &dir, true, io::Seam::Real);

        let recent = io::find_most_recent_session(&dir, filter_cwd.then_some(cwd));
        if let Some(recent) = recent {
            if manager.set_session_file(&recent).is_ok() {
                return manager;
            }
        }
        // No recent session (or a defensive load failure): start a fresh one.
        let _ = manager.new_session(NewSessionOptions::default());
        manager
    }

    /// Fork the session at `source_path` into a new session under `target_cwd`.
    /// Mirrors `SessionManager.forkFrom`.
    ///
    /// Every non-header entry from the source is copied verbatim into a freshly
    /// created file whose header carries a new id (validated when supplied via
    /// `options.id`, else a fresh `uuidv7`) and a `parentSession` back-pointer to
    /// the resolved source path. Returns `Err(message)` (no `Error: ` prefix)
    /// when the source is empty/invalid, headerless, or the requested id is
    /// invalid — the cases pi throws on.
    pub fn fork_from(
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: NewSessionOptions,
    ) -> Result<Self, String> {
        let resolved_source = io::resolve_input(source_path);
        let resolved_target = io::resolve_input(target_cwd);

        let source_entries = io::load_entries_from_file(&resolved_source);
        if source_entries.is_empty() {
            return Err(format!(
                "Cannot fork: source session file is empty or invalid: {resolved_source}"
            ));
        }
        let has_header = source_entries
            .iter()
            .any(|entry| entry.get("type").and_then(Value::as_str) == Some("session"));
        if !has_header {
            return Err(format!(
                "Cannot fork: source session has no header: {resolved_source}"
            ));
        }
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }

        let dir = match session_dir {
            Some(dir) => io::normalize_input(dir),
            None => io::get_default_session_dir(&resolved_target),
        };
        // `empty` creates the directory when it is missing.
        let mut manager = Self::empty(&resolved_target, &dir, true, io::Seam::Real);

        let new_session_id = match options.id {
            Some(id) => id,
            None => manager.gen_session_id(),
        };
        let timestamp = manager.gen_timestamp();
        let new_session_file = io::compose_session_file(&dir, &timestamp, &new_session_id);

        let entries: Vec<SessionEntry> = source_entries
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("session"))
            .filter_map(|entry| serde_json::from_value::<SessionEntry>(entry.clone()).ok())
            .collect();

        manager.header = SessionHeader {
            tag: SessionTag::Session,
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp,
            cwd: resolved_target,
            parent_session: Some(resolved_source),
        };
        manager.session_id = new_session_id;
        manager.entries = entries;
        manager.rebuild_index();
        manager.session_file = Some(new_session_file);
        // Fork writes eagerly (pi writes the header with `wx`, then appends every
        // entry); the file exists on disk before the manager is returned.
        manager.rewrite_file();
        manager.flushed = true;
        Ok(manager)
    }
}

#[cfg(test)]
mod tests;
