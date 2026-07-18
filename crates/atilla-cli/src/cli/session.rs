//! Minimal session-file layer for the CLI shell.
//!
//! Faithfully mirrors the subset of pi's
//! `packages/coding-agent/src/core/session-manager.ts` that the black-box CLI
//! contract exercises: session-id validation, opening/validating a session
//! file (without mutating an invalid one), listing local sessions by cwd, and
//! appending a `session_info` (display name) entry.
//!
//! NOTE ON WHY THIS IS NOT `atilla-agent`'s session storage: the ported
//! `atilla-agent::harness::session` is pi's *agent-core* v3 schema
//! (`packages/agent`), which persists an explicit `{"type":"leaf",...}` line,
//! hard-rejects non-version-3 headers, and reports invalid files as
//! "first line is not a valid session header". The coding-agent `SessionManager`
//! that the CLI drives has a different persistence model and a different
//! diagnostic ("Session file is not a valid pi session: <path>"), and it must
//! leave an invalid file byte-identical. coding-agent's `SessionManager` is not
//! ported to Rust on main, so the CLI carries this focused mirror instead.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::cli::config::get_agent_dir;

const CURRENT_SESSION_VERSION: i64 = 3;

/// Validate a session id. Mirrors `assertValidSessionId`. The `Err` string is
/// the exact message pi throws (the caller prefixes it with `Error: `).
pub fn assert_valid_session_id(id: &str) -> Result<(), String> {
    // Regex: ^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$
    let is_alnum = |c: char| c.is_ascii_alphanumeric();
    let is_body = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
    let chars: Vec<char> = id.chars().collect();
    let valid = match chars.as_slice() {
        [] => false,
        [only] => is_alnum(*only),
        [first, middle @ .., last] => {
            is_alnum(*first) && is_alnum(*last) && middle.iter().all(|c| is_body(*c))
        }
    };
    if valid {
        Ok(())
    } else {
        Err("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_string())
    }
}

/// Expand a leading `~` and lexically normalize `.`/`..`/duplicate separators
/// without resolving symlinks. Mirrors pi's `normalizePath` for our inputs.
fn normalize_path(input: &str) -> String {
    let expanded = if input == "~" {
        home_string()
    } else if let Some(rest) = input.strip_prefix("~/") {
        format!("{}/{}", home_string(), rest)
    } else {
        input.to_string()
    };
    lexical_normalize(&expanded)
}

fn home_string() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string())
}

/// Lexically collapse `.` / `..` and duplicate slashes, mirroring Node's
/// `path.resolve` normalization (no symlink resolution).
fn lexical_normalize(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if let Some(last) = stack.last() {
                    if *last != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !is_absolute {
                    stack.push("..");
                }
            }
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    if is_absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Resolve `input` against `base`, mirroring pi's `resolvePath`.
fn resolve_path(input: &str, base: &str) -> String {
    let normalized = normalize_path(input);
    if Path::new(&normalized).is_absolute() {
        lexical_normalize(&normalized)
    } else {
        let base_norm = normalize_path(base);
        lexical_normalize(&format!("{base_norm}/{normalized}"))
    }
}

fn process_cwd() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| ".".to_string())
}

/// Parse a JSONL session file into entries. Returns `[]` if the file does not
/// exist, is empty, or does not start with a valid `{"type":"session",...}`
/// header. Mirrors `loadEntriesFromFile`.
fn load_entries_from_file(path: &str) -> Vec<Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut entries: Vec<Value> = Vec::new();
    for line in content.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            entries.push(v);
        }
    }
    if entries.is_empty() {
        return entries;
    }
    let header = &entries[0];
    let type_ok = header.get("type").and_then(Value::as_str) == Some("session");
    let id_ok = header.get("id").and_then(Value::as_str).is_some();
    if !type_ok || !id_ok {
        return Vec::new();
    }
    entries
}

/// Read only the header of a session file (`{id, cwd}`), if valid.
fn read_session_header(path: &Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let first_line = content.split('\n').next()?;
    let header: Value = serde_json::from_str(first_line).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = header.get("id").and_then(Value::as_str)?.to_string();
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some((id, cwd))
}

fn session_cwd_matches(cwd: &str, resolved_cwd: &str) -> bool {
    !cwd.is_empty() && resolve_path(cwd, &process_cwd()) == resolved_cwd
}

/// The default per-cwd session directory path. Mirrors `getDefaultSessionDirPath`.
fn default_session_dir_path(cwd: &str) -> String {
    let resolved_cwd = resolve_path(cwd, &process_cwd());
    let agent_dir = get_agent_dir();
    let resolved_agent = resolve_path(agent_dir.to_str().unwrap_or("."), &process_cwd());
    let trimmed = resolved_cwd.trim_start_matches(['/', '\\']);
    let safe: String = trimmed
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let safe_path = format!("--{safe}--");
    lexical_normalize(&format!("{resolved_agent}/sessions/{safe_path}"))
}

/// A monotonic-ish short id generator (8 hex chars), collision-checked against
/// existing entry ids. Mirrors `generateId`'s contract (uniqueness within the
/// session); the exact bytes are not part of any black-box assertion.
fn generate_id(existing: &HashSet<String>) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mixed = nanos ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let id = format!("{:08x}", mixed & 0xFFFF_FFFF);
        if !existing.contains(&id) {
            return id;
        }
    }
}

/// ISO-8601 UTC timestamp with millisecond precision (`YYYY-MM-DDTHH:MM:SS.mmmZ`),
/// mirroring `new Date().toISOString()`. Value is not black-box asserted.
fn iso_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Convert days since the Unix epoch (`>= 0`) to (year, month, day) by walking
/// the Gregorian calendar year- then month-at-a-time. `SystemTime` since the
/// epoch is always non-negative, so the forward walk is sufficient here.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    fn is_leap(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
    }

    let mut year = 1970;
    let mut remaining = days.max(0);
    loop {
        let year_len = if is_leap(year) { 366 } else { 365 };
        if remaining < year_len {
            break;
        }
        remaining -= year_len;
        year += 1;
    }

    let feb = if is_leap(year) { 29 } else { 28 };
    let month_lengths = [31, feb, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0;
    while remaining >= month_lengths[month] {
        remaining -= month_lengths[month];
        month += 1;
    }
    (year, month as u32 + 1, remaining as u32 + 1)
}

/// The subset of pi's `SessionManager` that the CLI shell needs.
pub struct SessionManager {
    cwd: String,
    session_file: Option<String>,
    persist: bool,
    flushed: bool,
    file_entries: Vec<Value>,
    ids: HashSet<String>,
    leaf_id: Option<String>,
    /// Target directory used by `new_session` when persisting a fresh session.
    session_dir_for_new: Option<String>,
}

impl SessionManager {
    fn new_index(&mut self) {
        self.ids.clear();
        self.leaf_id = None;
        for entry in &self.file_entries {
            if entry.get("type").and_then(Value::as_str) == Some("session") {
                continue;
            }
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                self.ids.insert(id.to_string());
                self.leaf_id = Some(id.to_string());
            }
        }
    }

    /// Open a session file. Mirrors `SessionManager.open` + `setSessionFile`.
    ///
    /// Returns `Err(message)` (no `Error: ` prefix) when the file exists, is
    /// non-empty, but is not a valid pi session — leaving the file untouched.
    pub fn open(path: &str) -> Result<SessionManager, String> {
        let resolved = resolve_path(path, &process_cwd());
        let entries = load_entries_from_file(&resolved);
        let header_cwd = entries
            .first()
            .and_then(|h| h.get("cwd"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let cwd = header_cwd.unwrap_or_else(process_cwd);

        let mut mgr = SessionManager {
            cwd: resolve_path(&cwd, &process_cwd()),
            session_file: Some(resolved.clone()),
            persist: true,
            flushed: false,
            file_entries: Vec::new(),
            ids: HashSet::new(),
            leaf_id: None,
            session_dir_for_new: None,
        };

        if Path::new(&resolved).exists() {
            mgr.file_entries = entries;
            if mgr.file_entries.is_empty() {
                // Empty parse but the file has bytes => not a valid pi session.
                let size = std::fs::metadata(&resolved).map(|m| m.len()).unwrap_or(0);
                if size > 0 {
                    return Err(format!(
                        "Session file is not a valid pi session: {resolved}"
                    ));
                }
                // Genuinely empty file: initialize an in-memory header (pi would
                // rewrite it here; not exercised by the black-box contract).
                mgr.new_session(None);
            } else {
                mgr.new_index();
                mgr.flushed = true;
            }
        } else {
            mgr.new_session(None);
            mgr.session_file = Some(resolved);
        }
        Ok(mgr)
    }

    /// Create a new (persisted) session. Mirrors `SessionManager.create`.
    /// The file is not written until an assistant message is appended.
    pub fn create(cwd: &str, session_dir: Option<&str>, id: Option<&str>) -> SessionManager {
        let dir = match session_dir {
            Some(d) => normalize_path(d),
            None => default_session_dir_path(cwd),
        };
        let mut mgr = SessionManager {
            cwd: resolve_path(cwd, &process_cwd()),
            session_file: None,
            persist: true,
            flushed: false,
            file_entries: Vec::new(),
            ids: HashSet::new(),
            leaf_id: None,
            session_dir_for_new: Some(dir),
        };
        mgr.new_session(id);
        mgr
    }

    /// Create an in-memory (non-persisted) session. Mirrors `SessionManager.inMemory`.
    pub fn in_memory(cwd: &str) -> SessionManager {
        let mut mgr = SessionManager {
            cwd: resolve_path(cwd, &process_cwd()),
            session_file: None,
            persist: false,
            flushed: false,
            file_entries: Vec::new(),
            ids: HashSet::new(),
            leaf_id: None,
            session_dir_for_new: None,
        };
        mgr.new_session(None);
        mgr
    }

    fn new_session(&mut self, id: Option<&str>) {
        let timestamp = iso_timestamp();
        let session_id = id
            .map(|s| s.to_string())
            .unwrap_or_else(|| generate_id(&HashSet::new()));
        let header = serde_json::json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": session_id,
            "timestamp": timestamp,
            "cwd": self.cwd,
        });
        self.file_entries = vec![header];
        self.ids.clear();
        self.leaf_id = None;
        self.flushed = false;
        if self.persist {
            if let Some(dir) = &self.session_dir_for_new {
                let file_ts = timestamp.replace([':', '.'], "-");
                self.session_file = Some(lexical_normalize(&format!(
                    "{dir}/{file_ts}_{session_id}.jsonl"
                )));
            }
        }
    }

    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    pub fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    /// Append a `session_info` (display name) entry. Mirrors `appendSessionInfo`.
    pub fn append_session_info(&mut self, name: &str) {
        // Sanitize: collapse CR/LF runs to a single space, then trim.
        let mut sanitized = String::new();
        let mut in_break = false;
        for c in name.chars() {
            if c == '\r' || c == '\n' {
                if !in_break {
                    sanitized.push(' ');
                    in_break = true;
                }
            } else {
                sanitized.push(c);
                in_break = false;
            }
        }
        let sanitized = sanitized.trim().to_string();

        let id = generate_id(&self.ids);
        let entry = serde_json::json!({
            "type": "session_info",
            "id": id,
            "parentId": self.leaf_id,
            "timestamp": iso_timestamp(),
            "name": sanitized,
        });
        self.append_entry(entry);
    }

    fn append_entry(&mut self, entry: Value) {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.ids.insert(id.to_string());
            self.leaf_id = Some(id.to_string());
        }
        self.file_entries.push(entry.clone());
        self.persist_entry(&entry);
    }

    fn has_assistant(&self) -> bool {
        self.file_entries.iter().any(|e| {
            e.get("type").and_then(Value::as_str) == Some("message")
                && e.get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant")
        })
    }

    /// Mirrors `_persist`: defers writing until an assistant message exists,
    /// then writes the whole file, and appends subsequent entries.
    fn persist_entry(&mut self, entry: &Value) {
        if !self.persist {
            return;
        }
        let Some(file) = self.session_file.clone() else {
            return;
        };

        if !self.has_assistant() {
            // Before any assistant message the file is not yet flushed, so the
            // pre-flush entries stay buffered in memory until the first flush.
            if self.flushed {
                append_line(&file, entry);
            }
            return;
        }

        if !self.flushed {
            let mut buf = String::new();
            for e in &self.file_entries {
                buf.push_str(&serde_json::to_string(e).unwrap_or_default());
                buf.push('\n');
            }
            let _ = std::fs::write(&file, buf);
            self.flushed = true;
        } else {
            append_line(&file, entry);
        }
    }
}

fn append_line(file: &str, entry: &Value) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let line = serde_json::to_string(entry).unwrap_or_default();
        let _ = writeln!(f, "{line}");
    }
}

/// Find a local session whose id exactly matches `session_id`, applying pi's
/// cwd filtering. Mirrors `findLocalSessionByExactId` + `SessionManager.list`.
pub fn find_local_session_by_exact_id(
    session_id: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Option<String> {
    let dir = match session_dir {
        Some(d) => normalize_path(d),
        None => default_session_dir_path(cwd),
    };
    let filter_cwd = session_dir.is_some() && dir != default_session_dir_path(cwd);
    let resolved_cwd = resolve_path(cwd, &process_cwd());

    let read_dir = std::fs::read_dir(&dir).ok()?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some((id, header_cwd)) = read_session_header(&path) {
            if id != session_id {
                continue;
            }
            if !filter_cwd || session_cwd_matches(&header_cwd, &resolved_cwd) {
                return path.to_str().map(|s| s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18628), (2021, 1, 1));
        // 2020 is a leap year, so day 59 of the year is Feb 29.
        assert_eq!(civil_from_days(18321), (2020, 2, 29));
        assert_eq!(civil_from_days(18322), (2020, 3, 1));
    }
}
