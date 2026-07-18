//! JSONL file storage mirroring
//! `packages/agent/src/harness/session/jsonl-storage.ts`.
//!
//! The header is written once at create; every mutation appends
//! `${JSON.stringify(line)}\n` (no indentation, one object per line, LF).
//! `set_leaf_id` appends a real `leaf` line. On load the current leaf is the
//! `targetId` of the last line when it is a leaf, otherwise the last entry id.
//! Non-version-3 headers are hard-rejected (no migration), which is why pi's
//! legacy v1 fixtures do not parse through this surface.

// straitjacket-allow-file:duplication — the SessionStorage impl mirrors the
// in-memory impl in storage.rs (pi keeps these as two parallel storage classes);
// the trivial trait-method bodies are identical by design.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use super::storage::{
    build_labels_by_id, generate_entry_id, make_leaf_entry, now_iso, path_to_root,
    update_label_cache, SessionStorage,
};
use crate::harness::types::{SessionError, SessionErrorCode, SessionMetadata, SessionTreeEntry};

/// Options for [`JsonlSessionStorage::create`].
pub struct JsonlCreateOptions {
    pub cwd: String,
    pub session_id: String,
    pub parent_session_path: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeaderWire<'a> {
    #[serde(rename = "type")]
    typ: &'a str,
    version: u8,
    id: &'a str,
    timestamp: &'a str,
    cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_session: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Map<String, Value>>,
}

fn invalid_session(file_path: &str, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidSession,
        format!("Invalid JSONL session file {file_path}: {message}"),
    )
}

fn invalid_entry(file_path: &str, line_number: usize, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidEntry,
        format!("Invalid JSONL session file {file_path}: line {line_number} {message}"),
    )
}

/// Serialize one session-tree line as pi does: compact JSON plus a trailing LF.
pub fn serialize_entry_line(entry: &SessionTreeEntry) -> String {
    format!(
        "{}\n",
        serde_json::to_string(entry).expect("session entry serializes")
    )
}

/// Serialize a header line from session metadata, in pi's field order
/// (`type, version, id, timestamp, cwd, parentSession?, metadata?`), plus a
/// trailing LF. The counterpart of [`load_jsonl_session_metadata`].
pub fn serialize_header_line(metadata: &SessionMetadata) -> String {
    let header = HeaderWire {
        typ: "session",
        version: 3,
        id: &metadata.id,
        timestamp: &metadata.created_at,
        cwd: metadata.cwd.as_deref().unwrap_or(""),
        parent_session: metadata.parent_session_path.as_deref(),
        metadata: metadata.metadata.as_ref(),
    };
    format!(
        "{}\n",
        serde_json::to_string(&header).expect("session header serializes")
    )
}

fn parse_header_line(line: &str, file_path: &str) -> Result<SessionMetadata, SessionError> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|_| invalid_session(file_path, "first line is not a valid session header"))?;
    let Value::Object(header) = parsed else {
        return Err(invalid_session(
            file_path,
            "first line is not a valid session header",
        ));
    };
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(invalid_session(
            file_path,
            "first line is not a valid session header",
        ));
    }
    if header.get("version").and_then(Value::as_i64) != Some(3) {
        return Err(invalid_session(file_path, "unsupported session version"));
    }
    let id = header
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(id) = id else {
        return Err(invalid_session(file_path, "session header is missing id"));
    };
    let timestamp = header
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(timestamp) = timestamp else {
        return Err(invalid_session(
            file_path,
            "session header is missing timestamp",
        ));
    };
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(cwd) = cwd else {
        return Err(invalid_session(file_path, "session header is missing cwd"));
    };
    let parent_session = match header.get("parentSession") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(invalid_session(
                file_path,
                "session header parentSession must be a string",
            ))
        }
    };
    let metadata = match header.get("metadata") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map.clone()),
        Some(_) => {
            return Err(invalid_session(
                file_path,
                "session header metadata must be an object",
            ))
        }
    };
    Ok(SessionMetadata {
        id: id.to_string(),
        created_at: timestamp.to_string(),
        cwd: Some(cwd.to_string()),
        path: Some(file_path.to_string()),
        parent_session_path: parent_session,
        metadata,
    })
}

fn parse_entry_line(
    line: &str,
    file_path: &str,
    line_number: usize,
) -> Result<SessionTreeEntry, SessionError> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|_| invalid_entry(file_path, line_number, "is not valid JSON"))?;
    let Value::Object(obj) = &parsed else {
        return Err(invalid_entry(
            file_path,
            line_number,
            "is not a valid session entry",
        ));
    };
    if !obj.get("type").is_some_and(Value::is_string) {
        return Err(invalid_entry(
            file_path,
            line_number,
            "is missing entry type",
        ));
    }
    if obj
        .get("id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(invalid_entry(file_path, line_number, "is missing entry id"));
    }
    match obj.get("parentId") {
        Some(Value::Null) | Some(Value::String(_)) => {}
        _ => {
            return Err(invalid_entry(
                file_path,
                line_number,
                "has invalid parentId",
            ))
        }
    }
    if obj
        .get("timestamp")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(invalid_entry(
            file_path,
            line_number,
            "is missing timestamp",
        ));
    }
    if obj.get("type").and_then(Value::as_str) == Some("leaf") {
        match obj.get("targetId") {
            Some(Value::Null) | Some(Value::String(_)) => {}
            _ => {
                return Err(invalid_entry(
                    file_path,
                    line_number,
                    "has invalid targetId",
                ))
            }
        }
    }
    serde_json::from_value(parsed)
        .map_err(|_| invalid_entry(file_path, line_number, "is not a valid session entry"))
}

struct JsonlInner {
    entries: Vec<SessionTreeEntry>,
    by_id: HashMap<String, SessionTreeEntry>,
    labels_by_id: HashMap<String, String>,
    current_leaf_id: Option<String>,
}

/// JSONL-backed session storage. Mirrors `JsonlSessionStorage`.
pub struct JsonlSessionStorage {
    file_path: String,
    metadata: SessionMetadata,
    inner: RefCell<JsonlInner>,
}

impl JsonlSessionStorage {
    /// Open an existing session file, rebuilding entries, labels, and leaf.
    pub fn open(file_path: &str) -> Result<Self, SessionError> {
        if !Path::new(file_path).exists() {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Failed to read session {file_path}: file not found"),
            ));
        }
        let content = std::fs::read_to_string(file_path).map_err(|e| {
            SessionError::storage(format!("Failed to read session {file_path}: {e}"))
        })?;
        let lines: Vec<&str> = content
            .split('\n')
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return Err(invalid_session(file_path, "missing session header"));
        }
        let metadata = parse_header_line(lines[0], file_path)?;
        let mut entries: Vec<SessionTreeEntry> = Vec::new();
        let mut current_leaf_id = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            let entry = parse_entry_line(line, file_path, i + 1)?;
            current_leaf_id = entry.leaf_id_after();
            entries.push(entry);
        }
        Ok(Self::from_parts(
            file_path.to_string(),
            metadata,
            entries,
            current_leaf_id,
        ))
    }

    /// Create a new session file, writing the header line.
    pub fn create(file_path: &str, options: JsonlCreateOptions) -> Result<Self, SessionError> {
        let metadata = SessionMetadata {
            id: options.session_id,
            created_at: now_iso(),
            cwd: Some(options.cwd),
            path: Some(file_path.to_string()),
            parent_session_path: options.parent_session_path,
            metadata: options.metadata,
        };
        std::fs::write(file_path, serialize_header_line(&metadata)).map_err(|e| {
            SessionError::storage(format!("Failed to create session {file_path}: {e}"))
        })?;
        Ok(Self::from_parts(
            file_path.to_string(),
            metadata,
            Vec::new(),
            None,
        ))
    }

    fn from_parts(
        file_path: String,
        metadata: SessionMetadata,
        entries: Vec<SessionTreeEntry>,
        current_leaf_id: Option<String>,
    ) -> Self {
        let by_id = entries
            .iter()
            .map(|entry| (entry.id().to_string(), entry.clone()))
            .collect();
        let labels_by_id = build_labels_by_id(&entries);
        Self {
            file_path,
            metadata,
            inner: RefCell::new(JsonlInner {
                entries,
                by_id,
                labels_by_id,
                current_leaf_id,
            }),
        }
    }

    fn append_line(&self, entry: &SessionTreeEntry, what: &str) -> Result<(), SessionError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .map_err(|e| SessionError::storage(format!("Failed to append session {what}: {e}")))?;
        file.write_all(serialize_entry_line(entry).as_bytes())
            .map_err(|e| SessionError::storage(format!("Failed to append session {what}: {e}")))?;
        Ok(())
    }
}

impl SessionStorage for JsonlSessionStorage {
    fn get_metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let inner = self.inner.borrow();
        if let Some(id) = &inner.current_leaf_id {
            if !inner.by_id.contains_key(id) {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {id} not found"),
                ));
            }
        }
        Ok(inner.current_leaf_id.clone())
    }

    fn set_leaf_id(&self, leaf_id: Option<&str>) -> Result<(), SessionError> {
        let entry = {
            let inner = self.inner.borrow();
            if let Some(id) = leaf_id {
                if !inner.by_id.contains_key(id) {
                    return Err(SessionError::entry_not_found(id));
                }
            }
            make_leaf_entry(&inner.by_id, inner.current_leaf_id.clone(), leaf_id)
        };
        self.append_line(&entry, entry.id())?;
        let mut inner = self.inner.borrow_mut();
        inner.by_id.insert(entry.id().to_string(), entry.clone());
        inner.entries.push(entry);
        inner.current_leaf_id = leaf_id.map(str::to_string);
        Ok(())
    }

    fn create_entry_id(&self) -> String {
        generate_entry_id(&self.inner.borrow().by_id)
    }

    fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError> {
        self.append_line(&entry, entry.id())?;
        let mut inner = self.inner.borrow_mut();
        inner.by_id.insert(entry.id().to_string(), entry.clone());
        update_label_cache(&mut inner.labels_by_id, &entry);
        inner.current_leaf_id = entry.leaf_id_after();
        inner.entries.push(entry);
        Ok(())
    }

    fn get_entry(&self, id: &str) -> Option<SessionTreeEntry> {
        self.inner.borrow().by_id.get(id).cloned()
    }

    fn find_entries(&self, entry_type: &str) -> Vec<SessionTreeEntry> {
        self.inner
            .borrow()
            .entries
            .iter()
            .filter(|entry| entry.type_str() == entry_type)
            .cloned()
            .collect()
    }

    fn get_label(&self, id: &str) -> Option<String> {
        self.inner.borrow().labels_by_id.get(id).cloned()
    }

    fn get_path_to_root(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError> {
        path_to_root(&self.inner.borrow().by_id, leaf_id)
    }

    fn get_entries(&self) -> Vec<SessionTreeEntry> {
        self.inner.borrow().entries.clone()
    }
}

/// Read only the header line and return its metadata. Mirrors
/// `loadJsonlSessionMetadata`, which reads a single line rather than the whole
/// file.
pub fn load_jsonl_session_metadata(file_path: &str) -> Result<SessionMetadata, SessionError> {
    if !Path::new(file_path).exists() {
        return Err(SessionError::new(
            SessionErrorCode::NotFound,
            format!("Failed to read session header {file_path}: file not found"),
        ));
    }
    let file = std::fs::File::open(file_path).map_err(|e| {
        SessionError::storage(format!("Failed to read session header {file_path}: {e}"))
    })?;
    let mut first_line = String::new();
    BufReader::new(file)
        .read_line(&mut first_line)
        .map_err(|e| {
            SessionError::storage(format!("Failed to read session header {file_path}: {e}"))
        })?;
    if first_line.trim().is_empty() {
        return Err(invalid_session(file_path, "missing session header"));
    }
    parse_header_line(first_line.trim_end_matches(['\n', '\r']), file_path)
}
