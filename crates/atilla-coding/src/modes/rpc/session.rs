//! In-memory RPC session runtime.
//!
//! pi's RPC mode is a thin dispatch shell over a live `AgentSession` runtime
//! object. That runtime (the LLM streaming/agent loop, model catalog, extension
//! system, compaction pipeline) is not yet ported to Rust. What *does* exist is
//! the storage-level session tree (`atilla_agent::harness::session::Session`),
//! plus this crate's `export_html` renderer.
//!
//! [`RpcSession`] backs the "implementable-now" command subset against those
//! pieces: it wraps a storage `Session` for the session-tree reads/writes and
//! holds the runtime-only settings (thinking level, queue modes, auto-compaction
//! / auto-retry toggles) as plain in-memory state. Everything requiring the
//! missing agent runtime is routed to an honest error by the dispatcher, never
//! faked and never a panic.

use std::rc::Rc;

use atilla_agent::harness::session::{InMemorySessionStorage, Session};
use atilla_agent::harness::types::SessionTreeEntry;
use serde_json::{json, Value};

use super::types::{QueueMode, RpcSessionState, ThinkingLevel};
use crate::core::export_html;

/// The in-memory session runtime for RPC mode.
pub struct RpcSession {
    session: Session,
    session_id: String,
    thinking_level: ThinkingLevel,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
    auto_compaction_enabled: bool,
    auto_retry_enabled: bool,
}

impl RpcSession {
    /// Create a fresh in-memory session.
    pub fn new() -> Self {
        let storage = InMemorySessionStorage::new();
        let session = Session::new(Rc::new(storage));
        let session_id = session.get_metadata().id;
        Self {
            session,
            session_id,
            // Defaults mirror pi: DEFAULT_THINKING_LEVEL = "medium", queue modes
            // default to "one-at-a-time", auto-compaction defaults on.
            thinking_level: ThinkingLevel::Medium,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction_enabled: true,
            auto_retry_enabled: false,
        }
    }

    /// The stored entries that RPC exposes, in append order.
    ///
    /// Mirrors pi's `SessionManager.getEntries()`, whose `SessionEntry` union
    /// excludes the leaf pointer and `active_tools_change` lines. The storage
    /// layer keeps those in its entry list, so they are filtered here to match
    /// pi's wire shape.
    fn exposed_entries(&self) -> Vec<SessionTreeEntry> {
        self.session
            .get_entries()
            .into_iter()
            .filter(|e| {
                !matches!(
                    e,
                    SessionTreeEntry::Leaf(_) | SessionTreeEntry::ActiveToolsChange(_)
                )
            })
            .collect()
    }

    fn leaf_id(&self) -> Option<String> {
        self.session.get_leaf_id().ok().flatten()
    }

    // ------------------------------------------------------------------
    // State
    // ------------------------------------------------------------------

    /// Build the `get_state` payload.
    pub fn state(&self) -> RpcSessionState {
        let message_count = self
            .exposed_entries()
            .iter()
            .filter(|e| matches!(e, SessionTreeEntry::Message(_)))
            .count() as u64;

        RpcSessionState {
            // No model runtime is ported yet; pi omits `model` when unset.
            model: None,
            thinking_level: self.thinking_level,
            is_streaming: false,
            is_compacting: false,
            steering_mode: self.steering_mode,
            follow_up_mode: self.follow_up_mode,
            // In-memory sessions have no backing file.
            session_file: None,
            session_id: self.session_id.clone(),
            session_name: self.session.get_session_name(),
            auto_compaction_enabled: self.auto_compaction_enabled,
            message_count,
            // No pending queue without the agent runtime.
            pending_message_count: 0,
        }
    }

    // ------------------------------------------------------------------
    // In-memory setting toggles
    // ------------------------------------------------------------------

    pub fn set_thinking_level(&mut self, level: ThinkingLevel) {
        self.thinking_level = level;
    }

    /// Advance to the next thinking level and return it.
    ///
    /// pi gates cycling on the active model's supported levels; with no model
    /// runtime this cycles deterministically over the full `ThinkingLevel`
    /// enum instead.
    pub fn cycle_thinking_level(&mut self) -> ThinkingLevel {
        self.thinking_level = self.thinking_level.next();
        self.thinking_level
    }

    pub fn set_steering_mode(&mut self, mode: QueueMode) {
        self.steering_mode = mode;
    }

    pub fn set_follow_up_mode(&mut self, mode: QueueMode) {
        self.follow_up_mode = mode;
    }

    pub fn set_auto_compaction(&mut self, enabled: bool) {
        self.auto_compaction_enabled = enabled;
    }

    pub fn set_auto_retry(&mut self, enabled: bool) {
        self.auto_retry_enabled = enabled;
    }

    // ------------------------------------------------------------------
    // Session tree reads
    // ------------------------------------------------------------------

    /// `get_entries`: exposed entries, optionally sliced after `since`.
    /// Returns the `{ entries, leafId }` data payload, or an error string when
    /// `since` names an unknown entry (`"Entry not found: <since>"`).
    pub fn get_entries(&self, since: Option<&str>) -> Result<Value, String> {
        let mut entries = self.exposed_entries();
        if let Some(since) = since {
            let idx = entries.iter().position(|e| e.id() == since);
            match idx {
                Some(i) => {
                    entries = entries.split_off(i + 1);
                }
                None => return Err(format!("Entry not found: {since}")),
            }
        }
        let entries_json: Vec<Value> = entries
            .iter()
            .map(|e| serde_json::to_value(e).expect("session entry serializes"))
            .collect();
        Ok(json!({ "entries": entries_json, "leafId": self.leaf_id() }))
    }

    /// `get_tree`: the exposed entries assembled into a forest, plus the leaf id.
    /// Mirrors pi's `SessionManager.getTree()` (roots = entries whose parent is
    /// absent/self/orphaned; children sorted oldest-first by timestamp).
    pub fn get_tree(&self) -> Value {
        let entries = self.exposed_entries();
        let tree = build_tree(&self.session, &entries);
        json!({ "tree": tree, "leafId": self.leaf_id() })
    }

    /// `get_last_assistant_text`: `{ text }` where `text` is the concatenated
    /// text of the most recent non-aborted assistant message, omitted entirely
    /// when there is none (matching pi's `{ text: undefined }` -> `{}`).
    pub fn get_last_assistant_text(&self) -> Value {
        let text = self
            .exposed_entries()
            .iter()
            .rev()
            .filter_map(|e| match e {
                SessionTreeEntry::Message(m) => Some(&m.message),
                _ => None,
            })
            .find_map(assistant_text);
        match text {
            Some(t) => json!({ "text": t }),
            None => json!({}),
        }
    }

    /// `get_messages`: `{ messages }` reconstructed from message entries.
    pub fn get_messages(&self) -> Value {
        let messages: Vec<Value> = self
            .exposed_entries()
            .iter()
            .filter_map(|e| match e {
                SessionTreeEntry::Message(m) => Some(m.message.clone()),
                _ => None,
            })
            .collect();
        json!({ "messages": messages })
    }

    /// `get_fork_messages`: `{ messages: [{ entryId, text }] }` for user
    /// messages carrying non-empty text.
    pub fn get_fork_messages(&self) -> Value {
        let mut result = Vec::new();
        for entry in self.exposed_entries() {
            let SessionTreeEntry::Message(m) = &entry else {
                continue;
            };
            if m.message.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let text = user_text(m.message.get("content"));
            if !text.is_empty() {
                result.push(json!({ "entryId": m.id, "text": text }));
            }
        }
        json!({ "messages": result })
    }

    /// `set_session_name`: trim, reject empty, append a `session_info` entry.
    pub fn set_session_name(&self, name: &str) -> Result<(), String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("Session name cannot be empty".to_string());
        }
        self.session
            .append_session_name(trimmed)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Bash
    // ------------------------------------------------------------------

    /// `bash`: run `command` through `sh -c` and return a `BashResult`.
    ///
    /// pi's `session.executeBash` also appends a `bashExecution` message to the
    /// session context; that side effect needs the agent runtime and is
    /// deferred, so this drives the raw execution only. Output is stdout
    /// followed by stderr, matching pi's merged stream.
    pub fn run_bash(&self, command: &str) -> super::types::BashResult {
        use std::process::Command;
        let output = Command::new("sh").arg("-c").arg(command).output();
        match output {
            Ok(out) => {
                let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
                combined.push_str(&String::from_utf8_lossy(&out.stderr));
                super::types::BashResult {
                    output: combined,
                    exit_code: out.status.code(),
                    cancelled: false,
                    truncated: false,
                    full_output_path: None,
                }
            }
            Err(e) => super::types::BashResult {
                output: format!("failed to spawn command: {e}"),
                exit_code: None,
                cancelled: false,
                truncated: false,
                full_output_path: None,
            },
        }
    }

    // ------------------------------------------------------------------
    // Export
    // ------------------------------------------------------------------

    /// `export_html`: render the session tree to a standalone HTML file and
    /// return its path. When no output path is given a default is derived under
    /// the system temp directory (pi derives it from the session file).
    pub fn export_html(&self, output_path: Option<&str>) -> Result<Value, String> {
        let entries = self.exposed_entries();
        // Round-trip storage entries through their JSON wire form into the
        // export renderer's `SessionEntry` union (both mirror pi's shape).
        // Entries the export union does not model are skipped.
        let export_entries: Vec<export_html::SessionEntry> = entries
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();

        let session_data = export_html::assemble_session_data(
            export_html::SessionDataInputs {
                header: None,
                entries: export_entries,
                leaf_id: self.leaf_id(),
                system_prompt: None,
                tools: None,
            },
            None,
        );

        let path = match output_path {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::temp_dir().join(format!("atilla-session-{}.html", self.session_id)),
        };

        let options = export_html::ExportOptions {
            output_path: path,
            theme_inputs: export_html::ThemeInputs {
                resolved_colors: Vec::new(),
                export_colors: export_html::ThemeExportColors::default(),
            },
        };

        let written =
            export_html::export_session_data_to_html(&session_data, &options).map_err(|e| {
                format!("failed to write export: {e}")
            })?;
        Ok(json!({ "path": written.to_string_lossy() }))
    }

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    /// Append a raw message entry (used by unit tests to populate the tree).
    #[cfg(test)]
    pub fn append_message(&self, message: Value) {
        self.session
            .append_message(message)
            .expect("append message");
    }
}

impl Default for RpcSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the concatenated text of an assistant message, skipping aborted
/// messages that carry no content. Returns `None` when there is no text.
fn assistant_text(message: &Value) -> Option<String> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let content = message.get("content");
    let blocks = content.and_then(Value::as_array);
    let stop_reason = message.get("stopReason").and_then(Value::as_str);
    if stop_reason == Some("aborted") && blocks.map(|b| b.is_empty()).unwrap_or(true) {
        return None;
    }
    let mut text = String::new();
    if let Some(blocks) = blocks {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Extract user-message text: a plain string, or the joined `text` blocks.
fn user_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Assemble a forest of `{ entry, children, label? }` nodes from a flat entry
/// list, mirroring pi's `getTree()` algorithm.
fn build_tree(session: &Session, entries: &[SessionTreeEntry]) -> Vec<Value> {
    use std::collections::HashMap;

    // node id -> (entry json, children ids). Preserve append order for roots.
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();
    let ids: std::collections::HashSet<&str> = entries.iter().map(|e| e.id()).collect();

    for entry in entries {
        let id = entry.id().to_string();
        children_of.entry(id.clone()).or_default();
        match entry.parent_id() {
            None => roots.push(id),
            Some(p) if p == entry.id() => roots.push(id),
            Some(p) if ids.contains(p) => {
                children_of.entry(p.to_string()).or_default().push(id);
            }
            // Orphan (broken parent chain) -> treated as a root.
            Some(_) => roots.push(id),
        }
    }

    let entry_by_id: std::collections::HashMap<&str, &SessionTreeEntry> =
        entries.iter().map(|e| (e.id(), e)).collect();

    fn timestamp(entry: &SessionTreeEntry) -> String {
        // All entry structs share a `timestamp` field; read it via JSON.
        serde_json::to_value(entry)
            .ok()
            .and_then(|v| {
                v.get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    fn build_node(
        id: &str,
        entry_by_id: &std::collections::HashMap<&str, &SessionTreeEntry>,
        children_of: &std::collections::HashMap<String, Vec<String>>,
        session: &Session,
    ) -> Value {
        let entry = entry_by_id[id];
        let mut child_ids = children_of.get(id).cloned().unwrap_or_default();
        // Sort children oldest-first by timestamp.
        child_ids.sort_by(|a, b| {
            let ta = timestamp(entry_by_id[a.as_str()]);
            let tb = timestamp(entry_by_id[b.as_str()]);
            ta.cmp(&tb)
        });
        let children: Vec<Value> = child_ids
            .iter()
            .map(|c| build_node(c, entry_by_id, children_of, session))
            .collect();
        let mut node = serde_json::Map::new();
        node.insert(
            "entry".to_string(),
            serde_json::to_value(entry).expect("entry serializes"),
        );
        node.insert("children".to_string(), Value::Array(children));
        if let Some(label) = session.get_label(id) {
            node.insert("label".to_string(), Value::String(label));
        }
        Value::Object(node)
    }

    roots
        .iter()
        .map(|id| build_node(id, &entry_by_id, &children_of, session))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_match_pi() {
        let s = RpcSession::new();
        let st = s.state();
        assert_eq!(st.thinking_level, ThinkingLevel::Medium);
        assert_eq!(st.steering_mode, QueueMode::OneAtATime);
        assert_eq!(st.follow_up_mode, QueueMode::OneAtATime);
        assert!(st.auto_compaction_enabled);
        assert!(!st.is_streaming);
        assert_eq!(st.message_count, 0);
        assert!(st.session_name.is_none());
        assert!(st.model.is_none());
    }

    #[test]
    fn get_entries_since_unknown_errors() {
        let s = RpcSession::new();
        let err = s.get_entries(Some("nope")).unwrap_err();
        assert_eq!(err, "Entry not found: nope");
    }

    #[test]
    fn set_session_name_rejects_blank() {
        let s = RpcSession::new();
        assert_eq!(
            s.set_session_name("   ").unwrap_err(),
            "Session name cannot be empty"
        );
        s.set_session_name("hello").unwrap();
        assert_eq!(s.state().session_name.as_deref(), Some("hello"));
    }

    #[test]
    fn cycle_thinking_level_wraps() {
        let mut s = RpcSession::new();
        s.set_thinking_level(ThinkingLevel::Max);
        assert_eq!(s.cycle_thinking_level(), ThinkingLevel::Minimal);
    }

    #[test]
    fn bash_runs_echo() {
        let s = RpcSession::new();
        let r = s.run_bash("echo hello");
        assert_eq!(r.output.trim(), "hello");
        assert_eq!(r.exit_code, Some(0));
        assert!(!r.cancelled);
    }

    #[test]
    fn last_assistant_text_and_messages() {
        let s = RpcSession::new();
        s.append_message(json!({"role": "user", "content": "hi"}));
        s.append_message(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hello there"}]
        }));
        let last = s.get_last_assistant_text();
        assert_eq!(last["text"], json!("hello there"));
        let msgs = s.get_messages();
        assert_eq!(msgs["messages"].as_array().unwrap().len(), 2);
        let fork = s.get_fork_messages();
        assert_eq!(fork["messages"].as_array().unwrap().len(), 1);
        assert_eq!(fork["messages"][0]["text"], json!("hi"));
    }
}
