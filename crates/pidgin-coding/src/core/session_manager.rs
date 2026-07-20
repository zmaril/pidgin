//! Coding-agent session tree, ported from
//! `packages/coding-agent/src/core/session-manager.ts`.
//!
//! # Why this is not `pidgin_agent`'s session layer
//!
//! `pidgin_agent::harness::session` mirrors pi's *agent-core* v3 schema
//! (`packages/agent`). The coding-agent `SessionManager` shares the tree model
//! but diverges in four load-bearing, byte-level ways, all preserved here:
//!
//! 1. **No persisted leaf line.** The leaf pointer is purely in-memory; on load
//!    it is the last non-header entry. agent-core persists an explicit
//!    `{"type":"leaf",…}` line, so [`SessionEntry`] has no `Leaf` variant.
//! 2. **`custom` / `custom_message` key order.** coding-agent emits the
//!    type-specific fields *before* `id`/`parentId`/`timestamp`
//!    (`session-manager.ts:1051` and `:1106`); agent-core emits them after.
//!    [`CustomEntry`] and [`CustomMessageEntry`] are defined here to match.
//! 3. **Metadata-less header.** [`SessionHeader`] is
//!    `{type,version,id,timestamp,cwd,parentSession?}` with no `metadata` key.
//! 4. **Context projection.** [`build_session_context`] normalizes null message
//!    content (`session-manager.ts:383-390`) and its [`SessionContext`] has no
//!    `activeToolNames` field (coding-agent has no `active_tools_change`).
//!
//! Everything else — the seven shared entry structs, `uuidv7`, the context
//! message creators, and [`SessionError`] — is reused from `pidgin_agent`.
//!
//! Slice A landed the pure, in-memory tree + context. Slice B (the
//! [`io`] submodule) adds the file-I/O fidelity layer: the persisted
//! [`SessionManager::create`] / [`SessionManager::open`] factories, the
//! deferred-flush write path ([`_persist`](SessionManager) buffers until the
//! first assistant message, then flushes the whole buffer once with create-new
//! `wx` semantics and appends line-by-line thereafter), the lenient streaming
//! reader, the invalid-session guarantees (byte-identical on failure, no
//! `{"type":"leaf"}` line, no rewrite of a valid v3 file), and the
//! discovery free functions. Its public surface is a superset drop-in of the
//! CLI's stopgap `crates/pidgin-cli/src/cli/session.rs`.

// straitjacket-allow-file[:duplication] — the `format_iso_millis` epoch-to-ISO
// helper here is one of several deliberate parallel implementations across the
// workspace (cf. the faithful port in `pidgin-orchestrator`'s `radius.rs`).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pidgin_agent::harness::session::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    parse_iso_millis,
};
use pidgin_agent::harness::session::uuidv7;
use pidgin_agent::harness::types::{ModelRef, SessionError};

// The seven entry structs whose coding-agent key order already matches
// agent-core are reused verbatim (and re-exported so callers get one
// namespace). `AgentMessage` is agent-core's opaque `serde_json::Value` alias.
pub use pidgin_agent::harness::types::{
    AgentMessage, BranchSummaryEntry, CompactionEntry, LabelEntry, MessageEntry, ModelChangeEntry,
    SessionInfoEntry, ThinkingLevelChangeEntry,
};

/// The current on-disk session format version (`CURRENT_SESSION_VERSION`).
pub const CURRENT_SESSION_VERSION: i64 = 3;

// ===========================================================================
// Header + option types
// ===========================================================================

/// The discriminant of a [`SessionHeader`]; serializes as the string
/// `"session"` and rejects any other value.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTag {
    #[serde(rename = "session")]
    Session,
}

/// Session header (first JSONL line). Mirrors coding-agent's `SessionHeader`:
/// `{type,version,id,timestamp,cwd,parentSession?}` — no `metadata` key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub tag: SessionTag,
    /// Absent on v1 sessions; always `3` on write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// Options for starting a new session. Mirrors `NewSessionOptions`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

// ===========================================================================
// Coding-agent-specific entry structs (diverging key order)
// ===========================================================================

/// `custom` entry — extension state, ignored by context building. The
/// type-specific fields precede `id`/`parentId`/`timestamp`, matching
/// `session-manager.ts:1051` (this is the byte-level divergence from
/// agent-core, whose `custom` entry orders them the other way).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// `custom_message` entry — extension-injected context. Type-specific fields
/// precede `id`/`parentId`/`timestamp`, matching `session-manager.ts:1106`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    pub custom_type: String,
    pub content: Value,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

// ===========================================================================
// The entry union
// ===========================================================================

/// A session tree entry. Serializes internally-tagged on `type`, with the tag
/// emitted first and then the variant's fields in declaration order — exactly
/// the bytes coding-agent's `JSON.stringify` produces. Unlike agent-core's
/// union there is no `Leaf` or `ActiveToolsChange` variant.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(MessageEntry),
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    ModelChange(ModelChangeEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    Label(LabelEntry),
    SessionInfo(SessionInfoEntry),
}

impl SessionEntry {
    /// The `type` discriminant string.
    pub fn type_str(&self) -> &'static str {
        match self {
            SessionEntry::Message(_) => "message",
            SessionEntry::ThinkingLevelChange(_) => "thinking_level_change",
            SessionEntry::ModelChange(_) => "model_change",
            SessionEntry::Compaction(_) => "compaction",
            SessionEntry::BranchSummary(_) => "branch_summary",
            SessionEntry::Custom(_) => "custom",
            SessionEntry::CustomMessage(_) => "custom_message",
            SessionEntry::Label(_) => "label",
            SessionEntry::SessionInfo(_) => "session_info",
        }
    }

    /// The entry `id`.
    pub fn id(&self) -> &str {
        match self {
            SessionEntry::Message(e) => &e.id,
            SessionEntry::ThinkingLevelChange(e) => &e.id,
            SessionEntry::ModelChange(e) => &e.id,
            SessionEntry::Compaction(e) => &e.id,
            SessionEntry::BranchSummary(e) => &e.id,
            SessionEntry::Custom(e) => &e.id,
            SessionEntry::CustomMessage(e) => &e.id,
            SessionEntry::Label(e) => &e.id,
            SessionEntry::SessionInfo(e) => &e.id,
        }
    }

    /// The entry `parentId` (`None` serializes as JSON `null`).
    pub fn parent_id(&self) -> Option<&str> {
        let parent = match self {
            SessionEntry::Message(e) => &e.parent_id,
            SessionEntry::ThinkingLevelChange(e) => &e.parent_id,
            SessionEntry::ModelChange(e) => &e.parent_id,
            SessionEntry::Compaction(e) => &e.parent_id,
            SessionEntry::BranchSummary(e) => &e.parent_id,
            SessionEntry::Custom(e) => &e.parent_id,
            SessionEntry::CustomMessage(e) => &e.parent_id,
            SessionEntry::Label(e) => &e.parent_id,
            SessionEntry::SessionInfo(e) => &e.parent_id,
        };
        parent.as_deref()
    }

    /// The entry `timestamp`.
    pub fn timestamp(&self) -> &str {
        match self {
            SessionEntry::Message(e) => &e.timestamp,
            SessionEntry::ThinkingLevelChange(e) => &e.timestamp,
            SessionEntry::ModelChange(e) => &e.timestamp,
            SessionEntry::Compaction(e) => &e.timestamp,
            SessionEntry::BranchSummary(e) => &e.timestamp,
            SessionEntry::Custom(e) => &e.timestamp,
            SessionEntry::CustomMessage(e) => &e.timestamp,
            SessionEntry::Label(e) => &e.timestamp,
            SessionEntry::SessionInfo(e) => &e.timestamp,
        }
    }

    /// Overwrite the entry `parentId`. Used when re-chaining a branched path.
    pub fn set_parent_id(&mut self, parent: Option<String>) {
        match self {
            SessionEntry::Message(e) => e.parent_id = parent,
            SessionEntry::ThinkingLevelChange(e) => e.parent_id = parent,
            SessionEntry::ModelChange(e) => e.parent_id = parent,
            SessionEntry::Compaction(e) => e.parent_id = parent,
            SessionEntry::BranchSummary(e) => e.parent_id = parent,
            SessionEntry::Custom(e) => e.parent_id = parent,
            SessionEntry::CustomMessage(e) => e.parent_id = parent,
            SessionEntry::Label(e) => e.parent_id = parent,
            SessionEntry::SessionInfo(e) => e.parent_id = parent,
        }
    }
}

/// A raw file entry: the header or a tree entry. Mirrors coding-agent's
/// `FileEntry = SessionHeader | SessionEntry`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum FileEntry {
    Header(SessionHeader),
    Entry(Box<SessionEntry>),
}

/// A defensive-copy tree node returned by [`SessionManager::get_tree`].
#[derive(Clone, Debug, PartialEq)]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    /// Resolved label for this entry, if any.
    pub label: Option<String>,
    /// Timestamp of the latest label change for this entry, if any.
    pub label_timestamp: Option<String>,
}

/// Rebuilt conversation context. Mirrors coding-agent's `SessionContext`:
/// `{messages, thinkingLevel, model}` — no `activeToolNames`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<ModelRef>,
}

/// Session listing metadata. Mirrors `SessionInfo`; populated by the
/// discovery/listing surface in a later slice.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub parent_session_path: Option<String>,
    pub created: String,
    pub modified: String,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

/// The read-only projection of a [`SessionManager`]. Mirrors coding-agent's
/// `ReadonlySessionManager` (a `Pick<>` of the read methods) — the handle the
/// RPC / agent-runtime layers consume when they must not mutate the tree.
pub trait ReadonlySessionManager {
    fn get_cwd(&self) -> &str;
    fn get_session_dir(&self) -> &str;
    fn get_session_id(&self) -> &str;
    fn get_session_file(&self) -> Option<&str>;
    fn get_leaf_id(&self) -> Option<&str>;
    fn get_leaf_entry(&self) -> Option<SessionEntry>;
    fn get_entry(&self, id: &str) -> Option<SessionEntry>;
    fn get_label(&self, id: &str) -> Option<String>;
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry>;
    fn build_context_entries(&self) -> Vec<SessionEntry>;
    fn get_header(&self) -> Option<&SessionHeader>;
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_tree(&self) -> Vec<SessionTreeNode>;
    fn get_session_name(&self) -> Option<String>;
}

// ===========================================================================
// Module free functions
// ===========================================================================

fn session_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$").unwrap())
}

/// Validate a session id. Mirrors `assertValidSessionId`; the `Err` text is the
/// exact message pi throws (callers prefix it with `Error: `).
pub fn assert_valid_session_id(id: &str) -> Result<(), String> {
    if session_id_regex().is_match(id) {
        Ok(())
    } else {
        Err("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_string())
    }
}

fn crlf_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\r\n]+").unwrap())
}

/// Collapse CR/LF runs to a single space, then trim. Mirrors
/// `name.replace(/[\r\n]+/g, " ").trim()`.
fn sanitize_session_name(name: &str) -> String {
    crlf_regex().replace_all(name, " ").trim().to_string()
}

fn create_session_id() -> String {
    uuidv7()
}

/// Generate a unique short id (8 hex chars), collision-checked against
/// `existing`. Mirrors `generateId` (pi slices a fresh random UUID). A uuidv7
/// is monotonic — distinct on every call, even within one millisecond — but its
/// random tail can repeat within a millisecond, so the whole uuid is folded to
/// 8 hex rather than sliced. The exact bytes are not part of any assertion.
fn generate_id(existing: &HashSet<String>) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    for _ in 0..100 {
        let mut hasher = DefaultHasher::new();
        uuidv7().hash(&mut hasher);
        let candidate = format!("{:08x}", hasher.finish() & 0xFFFF_FFFF);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    // Astronomically unlikely; fall back to a raw uuid tail.
    uuidv7().chars().filter(|c| *c != '-').take(8).collect()
}

/// Parse JSONL content into raw values, skipping blank and malformed lines.
/// Mirrors `parseSessionEntries` (returns loosely-typed rows because callers
/// feed it cross-version and hand-edited files).
pub fn parse_session_entries(content: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for line in content.trim().split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            entries.push(value);
        }
    }
    entries
}

/// The latest `compaction` entry in tree order, if any. Mirrors
/// `getLatestCompactionEntry`.
pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&CompactionEntry> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionEntry::Compaction(compaction) => Some(compaction),
        _ => None,
    })
}

// --- migration ---------------------------------------------------------------

fn is_session_row(row: &Value) -> bool {
    row.get("type").and_then(Value::as_str) == Some("session")
}

/// Bring raw entries up to [`CURRENT_SESSION_VERSION`], mutating in place.
/// Mirrors `migrateSessionEntries`.
pub fn migrate_session_entries(entries: &mut [Value]) {
    migrate_to_current_version(entries);
}

fn migrate_to_current_version(entries: &mut [Value]) -> bool {
    let version = entries
        .iter()
        .find(|row| is_session_row(row))
        .and_then(|header| header.get("version"))
        .and_then(Value::as_i64)
        .unwrap_or(1);

    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2 {
        migrate_v1_to_v2(entries);
    }
    if version < 3 {
        migrate_v2_to_v3(entries);
    }
    true
}

/// v1 -> v2: assign `id`/`parentId` tree structure and convert compaction's
/// `firstKeptEntryIndex` to `firstKeptEntryId`.
fn migrate_v1_to_v2(entries: &mut [Value]) {
    let mut ids: HashSet<String> = HashSet::new();
    let mut assigned: Vec<Option<String>> = vec![None; entries.len()];
    let mut prev: Option<String> = None;

    for (index, row) in entries.iter_mut().enumerate() {
        if is_session_row(row) {
            row["version"] = json!(2);
            continue;
        }
        let id = generate_id(&ids);
        ids.insert(id.clone());
        row["id"] = json!(id);
        row["parentId"] = match &prev {
            Some(parent) => json!(parent),
            None => Value::Null,
        };
        assigned[index] = Some(id.clone());
        prev = Some(id);
    }

    // Resolve compaction index references now that every row has an id.
    for row in entries.iter_mut() {
        if row.get("type").and_then(Value::as_str) != Some("compaction") {
            continue;
        }
        let Some(target) = row
            .get("firstKeptEntryIndex")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
        else {
            continue;
        };
        if let Some(Some(target_id)) = assigned.get(target) {
            row["firstKeptEntryId"] = json!(target_id);
        }
        if let Some(object) = row.as_object_mut() {
            object.remove("firstKeptEntryIndex");
        }
    }
}

/// v2 -> v3: rename the legacy `hookMessage` message role to `custom`.
fn migrate_v2_to_v3(entries: &mut [Value]) {
    for row in entries.iter_mut() {
        if is_session_row(row) {
            row["version"] = json!(3);
            continue;
        }
        if row.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        if row.pointer("/message/role").and_then(Value::as_str) == Some("hookMessage") {
            row["message"]["role"] = json!("custom");
        }
    }
}

// --- context projection ------------------------------------------------------

/// Walk from the selected leaf to the root, returning entries root-first.
/// `leaf_id` mirrors pi's `leafId?: string`: `None` walks from the last entry
/// (pi's `undefined`); an unknown id also falls back to the last entry.
fn build_session_path(entries: &[SessionEntry], leaf_id: Option<&str>) -> Vec<SessionEntry> {
    let index: HashMap<&str, &SessionEntry> = entries.iter().map(|e| (e.id(), e)).collect();
    let leaf = match leaf_id {
        Some(id) => index.get(id).copied().or_else(|| entries.last()),
        None => entries.last(),
    };
    let Some(leaf) = leaf else {
        return Vec::new();
    };

    let mut path = Vec::new();
    let mut current = Some(leaf);
    while let Some(entry) = current {
        path.push(entry.clone());
        current = entry
            .parent_id()
            .and_then(|parent| index.get(parent).copied());
    }
    path.reverse();
    path
}

fn context_settings(path: &[SessionEntry]) -> (String, Option<ModelRef>) {
    let mut thinking_level = "off".to_string();
    let mut model: Option<ModelRef> = None;
    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange(e) => thinking_level = e.thinking_level.clone(),
            SessionEntry::ModelChange(e) => {
                model = Some(ModelRef {
                    provider: e.provider.clone(),
                    model_id: e.model_id.clone(),
                });
            }
            SessionEntry::Message(e)
                if e.message.get("role").and_then(Value::as_str) == Some("assistant") =>
            {
                model = Some(ModelRef {
                    provider: string_field(&e.message, "provider"),
                    model_id: string_field(&e.message, "model"),
                });
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

fn string_field(message: &Value, key: &str) -> String {
    message
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Project one entry into context messages. Mirrors
/// `sessionEntryToContextMessages`, including the null-content normalization
/// for `user`/`assistant`/`toolResult` messages (`session-manager.ts:383-390`).
pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match entry {
        SessionEntry::Message(e) => vec![normalize_message_content(&e.message)],
        SessionEntry::CustomMessage(e) => {
            let content = if e.content.is_null() {
                Value::Array(Vec::new())
            } else {
                e.content.clone()
            };
            vec![create_custom_message(
                &e.custom_type,
                &content,
                e.display,
                e.details.as_ref(),
                &e.timestamp,
            )]
        }
        SessionEntry::BranchSummary(e) if !e.summary.is_empty() => {
            vec![create_branch_summary_message(
                &e.summary,
                &e.from_id,
                &e.timestamp,
            )]
        }
        SessionEntry::Compaction(e) => vec![create_compaction_summary_message(
            &e.summary,
            e.tokens_before,
            &e.timestamp,
        )],
        _ => Vec::new(),
    }
}

fn normalize_message_content(message: &Value) -> Value {
    let role = message.get("role").and_then(Value::as_str);
    let normalizable = matches!(role, Some("user") | Some("assistant") | Some("toolResult"));
    let content_is_null = message.get("content").is_none_or(Value::is_null);
    if normalizable && content_is_null {
        if let Some(object) = message.as_object() {
            let mut clone = object.clone();
            clone.insert("content".to_string(), Value::Array(Vec::new()));
            return Value::Object(clone);
        }
    }
    message.clone()
}

/// The compaction-aware entry list for the leaf path. Mirrors coding-agent's
/// exported `buildContextEntries`.
pub fn build_context_entries(entries: &[SessionEntry], leaf_id: Option<&str>) -> Vec<SessionEntry> {
    select_compaction_aware(&build_session_path(entries, leaf_id))
}

fn select_compaction_aware(path: &[SessionEntry]) -> Vec<SessionEntry> {
    let mut compaction_index: Option<usize> = None;
    for (index, entry) in path.iter().enumerate() {
        if matches!(entry, SessionEntry::Compaction(_)) {
            compaction_index = Some(index);
        }
    }
    let Some(compaction_index) = compaction_index else {
        return path.to_vec();
    };
    let SessionEntry::Compaction(compaction) = &path[compaction_index] else {
        return path.to_vec();
    };

    let mut selected = vec![path[compaction_index].clone()];
    let mut found_first_kept = false;
    for entry in &path[..compaction_index] {
        if entry.id() == compaction.first_kept_entry_id {
            found_first_kept = true;
        }
        if found_first_kept {
            selected.push(entry.clone());
        }
    }
    selected.extend_from_slice(&path[compaction_index + 1..]);
    selected
}

/// Build the full session context (messages + settings) for the leaf path.
/// Mirrors coding-agent's exported `buildSessionContext`.
pub fn build_session_context(entries: &[SessionEntry], leaf_id: Option<&str>) -> SessionContext {
    let path = build_session_path(entries, leaf_id);
    let (thinking_level, model) = context_settings(&path);
    let messages = build_context_entries(entries, leaf_id)
        .iter()
        .flat_map(session_entry_to_context_messages)
        .collect();
    SessionContext {
        messages,
        thinking_level,
        model,
    }
}

// --- timestamps --------------------------------------------------------------

/// A best-effort `Date.toISOString()`-shaped timestamp for entries created at
/// runtime. Only the shape matters to this slice; the exact value is not
/// asserted anywhere.
pub(crate) fn now_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format_iso_millis(millis)
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ`. The calendar is
/// derived by settling into 400-year blocks and then walking the remaining
/// years and months — a decomposition distinct from the closed-form and
/// single-year-scan helpers elsewhere in the workspace.
fn format_iso_millis(millis: i64) -> String {
    let total_seconds = millis.div_euclid(1000);
    let sub_ms = millis.rem_euclid(1000);
    let mut day_count = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let clock = (
        seconds_of_day / 3600,
        seconds_of_day % 3600 / 60,
        seconds_of_day % 60,
    );

    let mut year = 1970 + 400 * day_count.div_euclid(146_097);
    day_count = day_count.rem_euclid(146_097);
    while day_count >= year_length(year) {
        day_count -= year_length(year);
        year += 1;
    }

    let months = month_lengths(year);
    let mut month_pos = 0;
    while day_count >= months[month_pos] {
        day_count -= months[month_pos];
        month_pos += 1;
    }

    format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{sub_ms:03}Z",
        month_pos + 1,
        day_count + 1,
        clock.0,
        clock.1,
        clock.2,
    )
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn year_length(year: i64) -> i64 {
    if is_leap_year(year) {
        366
    } else {
        365
    }
}

fn month_lengths(year: i64) -> [i64; 12] {
    let february = if is_leap_year(year) { 29 } else { 28 };
    [31, february, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
}

// ===========================================================================
// SessionManager
// ===========================================================================

/// Manages a conversation as an append-only entry tree.
///
/// The in-memory surface — construction via [`SessionManager::in_memory`], the
/// `append_*` mutators, tree navigation, and context building — lives here. The
/// persisted factories ([`SessionManager::create`], [`SessionManager::open`]),
/// the deferred-flush write path, and the discovery free functions live in the
/// [`io`] submodule (slice B).
pub struct SessionManager {
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    /// Whether the buffered pre-flush entries have been written to disk yet.
    /// Mirrors pi's `flushed`; see [`io`] for the deferred-flush protocol.
    flushed: bool,
    /// The clock/id source. Real in production; a fixed seam in byte-exact
    /// write tests. See [`io::Seam`].
    seam: io::Seam,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
    ids: HashSet<String>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

impl SessionManager {
    /// Create an in-memory session (never persisted). Mirrors
    /// `SessionManager.inMemory` (`new SessionManager(cwd, "", undefined,
    /// false)`).
    pub fn in_memory(cwd: &str) -> Self {
        let mut manager = SessionManager::empty(cwd, "", false, io::Seam::Real);
        // A bare `inMemory()` never carries an id, so this cannot fail.
        let _ = manager.new_session(NewSessionOptions::default());
        manager
    }

    /// Reset in-memory state and start a fresh session. Mirrors `newSession`:
    /// when persisting it computes the (would-be) session file path
    /// (`<dir>/<file-ts>_<id>.jsonl`) but writes nothing until the first
    /// assistant message. Returns the session file path, if any.
    pub fn new_session(&mut self, options: NewSessionOptions) -> Result<Option<String>, String> {
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        let session_id = match options.id.clone() {
            Some(id) => id,
            None => self.gen_session_id(),
        };
        let timestamp = self.gen_timestamp();
        self.header = SessionHeader {
            tag: SessionTag::Session,
            version: Some(CURRENT_SESSION_VERSION),
            id: session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: options.parent_session,
        };
        self.session_id = session_id.clone();
        self.entries.clear();
        self.by_id.clear();
        self.ids.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;
        if self.persist && !self.session_dir.is_empty() {
            self.session_file = Some(io::compose_session_file(
                &self.session_dir,
                &timestamp,
                &session_id,
            ));
        }
        Ok(self.session_file.clone())
    }

    // --- accessors ----------------------------------------------------------

    /// The working directory stored in the header. Mirrors `getCwd`.
    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    /// Override the effective working directory, leaving the loaded header's own
    /// `cwd` untouched. Mirrors the `cwdOverride` argument of
    /// `SessionManager.open` (`session-manager.ts`), where the opened manager's
    /// effective cwd is `cwdOverride ?? header.cwd ?? process.cwd()`; the port's
    /// [`open`](SessionManager::open) resolves the header/`process.cwd()` arms and
    /// this applies the override arm.
    pub fn set_cwd(&mut self, cwd: &str) {
        self.cwd = cwd.to_string();
    }

    /// The session directory (empty for in-memory). Mirrors `getSessionDir`.
    pub fn get_session_dir(&self) -> &str {
        &self.session_dir
    }

    /// The session id. Mirrors `getSessionId`.
    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    /// The session file path, if persisting. Mirrors `getSessionFile`.
    pub fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    /// The current leaf id, or `None` before any entry. Mirrors `getLeafId`.
    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    /// The session header. Mirrors `getHeader`.
    pub fn get_header(&self) -> Option<&SessionHeader> {
        Some(&self.header)
    }

    /// Whether the session persists to disk. Mirrors `isPersisted`.
    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    /// Whether the session directory is the encoded per-cwd default. Mirrors
    /// `usesDefaultSessionDir`: `sessionDir === getDefaultSessionDirPath(cwd)`.
    /// In-memory sessions have an empty `session_dir`, so this is `false` for
    /// them.
    pub fn uses_default_session_dir(&self) -> bool {
        self.session_dir == io::default_session_dir_path(&self.cwd)
    }

    // --- append operations --------------------------------------------------

    /// Append a message as a child of the current leaf. Returns the entry id.
    pub fn append_message(&mut self, message: AgentMessage) -> String {
        let entry = SessionEntry::Message(MessageEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
            message,
        });
        self.push_entry(entry)
    }

    /// Append a thinking-level change. Returns the entry id.
    pub fn append_thinking_level_change(&mut self, thinking_level: &str) -> String {
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
            thinking_level: thinking_level.to_string(),
        });
        self.push_entry(entry)
    }

    /// Append a model change. Returns the entry id.
    pub fn append_model_change(&mut self, provider: &str, model_id: &str) -> String {
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        });
        self.push_entry(entry)
    }

    /// Append a compaction summary. Returns the entry id.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> String {
        let entry = SessionEntry::Compaction(CompactionEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.to_string(),
            tokens_before,
            details,
            from_hook,
        });
        self.push_entry(entry)
    }

    /// Append an extension `custom` entry. Returns the entry id.
    pub fn append_custom_entry(&mut self, custom_type: &str, data: Option<Value>) -> String {
        let entry = SessionEntry::Custom(CustomEntry {
            custom_type: custom_type.to_string(),
            data,
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
        });
        self.push_entry(entry)
    }

    /// Append a `session_info` (display name) entry. The name is sanitized
    /// (CR/LF collapsed to a space, then trimmed). Returns the entry id.
    pub fn append_session_info(&mut self, name: &str) -> String {
        let entry = SessionEntry::SessionInfo(SessionInfoEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
            name: Some(sanitize_session_name(name)),
        });
        self.push_entry(entry)
    }

    /// Append an extension `custom_message` entry (participates in context).
    /// Returns the entry id.
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: Value,
        display: bool,
        details: Option<Value>,
    ) -> String {
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            custom_type: custom_type.to_string(),
            content,
            display,
            details,
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: self.gen_timestamp(),
        });
        self.push_entry(entry)
    }

    /// Set or clear a label on an entry. Passing `None` (or an empty string)
    /// clears it. Errors if `target_id` is unknown. Returns the entry id.
    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError> {
        if !self.by_id.contains_key(target_id) {
            return Err(SessionError::entry_not_found(target_id));
        }
        let timestamp = self.gen_timestamp();
        let entry = SessionEntry::Label(LabelEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: timestamp.clone(),
            target_id: target_id.to_string(),
            label: label.map(str::to_string),
        });
        let id = self.push_entry(entry);
        match label {
            Some(text) if !text.is_empty() => {
                self.labels_by_id
                    .insert(target_id.to_string(), text.to_string());
                self.label_timestamps_by_id
                    .insert(target_id.to_string(), timestamp);
            }
            _ => {
                self.labels_by_id.remove(target_id);
                self.label_timestamps_by_id.remove(target_id);
            }
        }
        Ok(id)
    }

    // --- tree navigation ----------------------------------------------------

    /// The current leaf entry, if any. Mirrors `getLeafEntry`.
    pub fn get_leaf_entry(&self) -> Option<SessionEntry> {
        self.leaf_id
            .as_deref()
            .and_then(|id| self.entry_ref(id))
            .cloned()
    }

    /// The entry with `id`, if present. Mirrors `getEntry`.
    pub fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        self.entry_ref(id).cloned()
    }

    /// The direct children of `parent_id`, in insertion order. Mirrors
    /// `getChildren`.
    pub fn get_children(&self, parent_id: &str) -> Vec<SessionEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.parent_id() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// The label for `id`, if any. Mirrors `getLabel`.
    pub fn get_label(&self, id: &str) -> Option<String> {
        self.labels_by_id.get(id).cloned()
    }

    /// Walk from `from_id` (or the current leaf) to the root, root-first.
    /// Mirrors `getBranch`.
    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        let mut current = from_id.map(str::to_string).or_else(|| self.leaf_id.clone());
        let mut path = Vec::new();
        while let Some(id) = current {
            match self.entry_ref(&id) {
                Some(entry) => {
                    current = entry.parent_id().map(str::to_string);
                    path.push(entry.clone());
                }
                None => break,
            }
        }
        path.reverse();
        path
    }

    /// The compaction-aware entry list for the current leaf. Mirrors the
    /// instance `buildContextEntries`.
    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        match self.leaf_id.as_deref() {
            Some(id) => build_context_entries(&self.get_entries(), Some(id)),
            None => Vec::new(),
        }
    }

    /// The session context for the current leaf. Mirrors the instance
    /// `buildSessionContext`.
    pub fn build_session_context(&self) -> SessionContext {
        match self.leaf_id.as_deref() {
            Some(id) => build_session_context(&self.get_entries(), Some(id)),
            None => SessionContext {
                messages: Vec::new(),
                thinking_level: "off".to_string(),
                model: None,
            },
        }
    }

    /// All entries, excluding the header, in insertion order. Mirrors
    /// `getEntries`.
    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }

    /// The current session name from the latest `session_info` entry. Mirrors
    /// `getSessionName`.
    pub fn get_session_name(&self) -> Option<String> {
        for entry in self.entries.iter().rev() {
            if let SessionEntry::SessionInfo(info) = entry {
                let trimmed = info.name.as_deref().unwrap_or("").trim();
                return if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
        }
        None
    }

    /// The session as a defensive-copy tree. Orphaned entries become roots and
    /// children are sorted by timestamp. Mirrors `getTree`.
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let index: HashMap<&str, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (entry.id(), i))
            .collect();

        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.entries.len()];
        let mut roots: Vec<usize> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            match entry.parent_id() {
                Some(parent) if parent != entry.id() => match index.get(parent) {
                    Some(&parent_index) => children[parent_index].push(i),
                    None => roots.push(i),
                },
                _ => roots.push(i),
            }
        }

        for child_list in &mut children {
            // Stable sort keeps insertion order among equal timestamps.
            child_list.sort_by_key(|&i| parse_iso_millis(self.entries[i].timestamp()));
        }

        roots
            .into_iter()
            .map(|i| self.build_tree_node(i, &children))
            .collect()
    }

    fn build_tree_node(&self, index: usize, children: &[Vec<usize>]) -> SessionTreeNode {
        let entry = self.entries[index].clone();
        let label = self.labels_by_id.get(entry.id()).cloned();
        let label_timestamp = self.label_timestamps_by_id.get(entry.id()).cloned();
        let child_nodes = children[index]
            .iter()
            .map(|&child| self.build_tree_node(child, children))
            .collect();
        SessionTreeNode {
            entry,
            children: child_nodes,
            label,
            label_timestamp,
        }
    }

    // --- branching ----------------------------------------------------------

    /// Move the leaf to an earlier entry, starting a new branch on the next
    /// append. Mirrors `branch`.
    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionError> {
        if !self.by_id.contains_key(branch_from_id) {
            return Err(SessionError::entry_not_found(branch_from_id));
        }
        self.leaf_id = Some(branch_from_id.to_string());
        Ok(())
    }

    /// Reset the leaf to `None`; the next append becomes a new root. Mirrors
    /// `resetLeaf`.
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// Move the leaf and record a `branch_summary` of the abandoned path.
    /// Mirrors `branchWithSummary`. Returns the summary entry id.
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionError> {
        if let Some(id) = branch_from_id {
            if !self.by_id.contains_key(id) {
                return Err(SessionError::entry_not_found(id));
            }
        }
        self.leaf_id = branch_from_id.map(str::to_string);
        let entry = SessionEntry::BranchSummary(BranchSummaryEntry {
            id: self.next_id(),
            parent_id: branch_from_id.map(str::to_string),
            timestamp: self.gen_timestamp(),
            from_id: branch_from_id.unwrap_or("root").to_string(),
            summary: summary.to_string(),
            details,
            from_hook,
        });
        Ok(self.push_entry(entry))
    }

    /// Replace the session with only the root-to-`leaf_id` path, stripping and
    /// re-chaining labels. Mirrors `createBranchedSession`.
    ///
    /// In-memory sessions replace their state and return `None`. Persisted
    /// sessions additionally mint a new session file
    /// (`<dir>/<file-ts>_<id>.jsonl`), point its header's `parentSession` at the
    /// previous file, and return the new path. The new file is written eagerly
    /// only when the retained path already contains an assistant message;
    /// otherwise the write is deferred to [`_persist`](Self::persist_last_entry)
    /// exactly like [`Self::create`], so a leaf-only fork never lands a
    /// duplicate header on disk.
    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<String>, SessionError> {
        let previous_session_file = self.session_file.clone();
        let path = self.get_branch(Some(leaf_id));
        if path.is_empty() {
            return Err(SessionError::entry_not_found(leaf_id));
        }

        // Drop label entries and re-chain the retained path so nothing is
        // orphaned when a label sat between two kept entries.
        let mut path_without_labels: Vec<SessionEntry> = Vec::new();
        let mut parent: Option<String> = None;
        for entry in &path {
            if matches!(entry, SessionEntry::Label(_)) {
                continue;
            }
            let mut rechained = entry.clone();
            rechained.set_parent_id(parent.clone());
            parent = Some(rechained.id().to_string());
            path_without_labels.push(rechained);
        }

        let new_session_id = self.gen_session_id();
        let timestamp = self.gen_timestamp();
        let header = SessionHeader {
            tag: SessionTag::Session,
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            // Persisted forks point back at the file they were cut from;
            // in-memory forks have no parent file.
            parent_session: if self.persist {
                previous_session_file
            } else {
                None
            },
        };

        // Recreate labels for retained entries, chained after the last one,
        // preserving each label's original timestamp.
        let mut seen: HashSet<String> = path_without_labels
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        let mut label_parent = path_without_labels.last().map(|e| e.id().to_string());
        let mut label_entries: Vec<SessionEntry> = Vec::new();
        for entry in &path_without_labels {
            let Some(label) = self.labels_by_id.get(entry.id()) else {
                continue;
            };
            let label_timestamp = self
                .label_timestamps_by_id
                .get(entry.id())
                .cloned()
                .unwrap_or_default();
            let id = generate_id(&seen);
            seen.insert(id.clone());
            label_entries.push(SessionEntry::Label(LabelEntry {
                id: id.clone(),
                parent_id: label_parent.clone(),
                timestamp: label_timestamp,
                target_id: entry.id().to_string(),
                label: Some(label.clone()),
            }));
            label_parent = Some(id);
        }

        self.header = header;
        self.session_id = new_session_id.clone();
        self.entries = path_without_labels;
        self.entries.extend(label_entries);
        self.rebuild_index();

        if self.persist {
            let new_file = io::compose_session_file(&self.session_dir, &timestamp, &new_session_id);
            self.session_file = Some(new_file.clone());
            // Only write eagerly if there is already an assistant message;
            // otherwise defer to `_persist` on the first assistant response.
            if self.has_assistant_message() {
                self.rewrite_file();
                self.flushed = true;
            } else {
                self.flushed = false;
            }
            return Ok(Some(new_file));
        }
        Ok(None)
    }

    // --- internals ----------------------------------------------------------

    fn entry_ref(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id).map(|&index| &self.entries[index])
    }

    fn next_id(&self) -> String {
        self.seam.generate_entry_id(&self.ids)
    }

    /// A `Date.toISOString()`-shaped timestamp from the active seam (real clock
    /// in production, a fixed value in byte-exact tests).
    fn gen_timestamp(&self) -> String {
        self.seam.timestamp()
    }

    /// A fresh session id from the active seam (`uuidv7` in production).
    fn gen_session_id(&self) -> String {
        self.seam.session_id()
    }

    fn push_entry(&mut self, entry: SessionEntry) -> String {
        let id = entry.id().to_string();
        self.by_id.insert(id.clone(), self.entries.len());
        self.ids.insert(id.clone());
        self.leaf_id = Some(id.clone());
        self.entries.push(entry);
        // Deferred-flush persistence (slice B): buffers until the first
        // assistant message, then flushes once and appends thereafter.
        self.persist_last_entry();
        id
    }

    fn rebuild_index(&mut self) {
        self.by_id.clear();
        self.ids.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        for (index, entry) in self.entries.iter().enumerate() {
            let id = entry.id().to_string();
            self.by_id.insert(id.clone(), index);
            self.ids.insert(id.clone());
            self.leaf_id = Some(id);
            if let SessionEntry::Label(label) = entry {
                match label.label.as_deref() {
                    Some(text) if !text.is_empty() => {
                        self.labels_by_id
                            .insert(label.target_id.clone(), text.to_string());
                        self.label_timestamps_by_id
                            .insert(label.target_id.clone(), label.timestamp.clone());
                    }
                    _ => {
                        self.labels_by_id.remove(&label.target_id);
                        self.label_timestamps_by_id.remove(&label.target_id);
                    }
                }
            }
        }
    }
}

fn placeholder_header() -> SessionHeader {
    SessionHeader {
        tag: SessionTag::Session,
        version: Some(CURRENT_SESSION_VERSION),
        id: String::new(),
        timestamp: String::new(),
        cwd: String::new(),
        parent_session: None,
    }
}

impl ReadonlySessionManager for SessionManager {
    fn get_cwd(&self) -> &str {
        SessionManager::get_cwd(self)
    }
    fn get_session_dir(&self) -> &str {
        SessionManager::get_session_dir(self)
    }
    fn get_session_id(&self) -> &str {
        SessionManager::get_session_id(self)
    }
    fn get_session_file(&self) -> Option<&str> {
        SessionManager::get_session_file(self)
    }
    fn get_leaf_id(&self) -> Option<&str> {
        SessionManager::get_leaf_id(self)
    }
    fn get_leaf_entry(&self) -> Option<SessionEntry> {
        SessionManager::get_leaf_entry(self)
    }
    fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        SessionManager::get_entry(self, id)
    }
    fn get_label(&self, id: &str) -> Option<String> {
        SessionManager::get_label(self, id)
    }
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, from_id)
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        SessionManager::build_context_entries(self)
    }
    fn get_header(&self) -> Option<&SessionHeader> {
        SessionManager::get_header(self)
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        SessionManager::get_entries(self)
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        SessionManager::get_tree(self)
    }
    fn get_session_name(&self) -> Option<String> {
        SessionManager::get_session_name(self)
    }
}

// Slice B: the file-I/O fidelity layer (factories, deferred-flush write path,
// lenient reader, discovery). Kept in a child module so it can reach the
// `SessionManager` private fields while keeping each source file well under the
// 1500-line limit.
mod io;

pub use io::{
    default_session_dir_path, find_local_session_by_exact_id, find_most_recent_session,
    get_default_session_dir, load_entries_from_file, read_session_header,
};

// Slice C: the discovery / list / fork surface (`build_session_info`, the `list`
// / `list_all` / `continue_recent` / `fork_from` factories). Kept in its own
// child module so it can reach the private fields + io helpers while every source
// file stays well under the 1500-line limit.
mod discovery;

pub use discovery::build_session_info;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod io_tests;
