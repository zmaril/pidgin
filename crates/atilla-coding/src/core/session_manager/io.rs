//! Slice B: the coding-agent `SessionManager` file-I/O fidelity layer.
//!
//! This child module reaches [`SessionManager`](super::SessionManager)'s private
//! fields to add the persisted factories, the deferred-flush write path, the
//! lenient streaming reader, and the discovery free functions — the subset the
//! CLI shell drives. Its public surface is a superset drop-in of the CLI's
//! stopgap `crates/atilla-cli/src/cli/session.rs`, so the CLI can later swap
//! onto it without changing a single call site.
//!
//! The load-bearing fidelity guarantees preserved here:
//!
//! - **Byte-identical on invalid.** [`SessionManager::open`] of a non-empty file
//!   that does not parse as a pi session returns
//!   `Session file is not a valid pi session: <path>` and leaves the file's
//!   bytes untouched.
//! - **No leaf line.** The active leaf is in-memory only; no `{"type":"leaf"}`
//!   line is ever written (the [`SessionEntry`](super::SessionEntry) union has no
//!   `Leaf` variant).
//! - **No rewrite of a valid v3 file.** Loading a current-version session never
//!   reserializes it; only a migration (v1/v2 → v3) or an empty-file
//!   initialization rewrites bytes.
//! - **Deferred flush.** A persisted session writes nothing to disk until the
//!   first assistant message exists; then the whole buffer flushes once with
//!   create-new (`wx`) semantics, and later entries append line-by-line.
//!
//! The lexical path helpers delegate to `crate::utils::paths` /
//! `crate::utils::bytes`, whose normalization is behaviorally identical to pi's
//! `normalizePath` / `resolvePath` for session inputs — so the encoded
//! session-directory names and resolved paths match `session.rs` byte-for-byte
//! while keeping this crate's single copy of the path logic.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{NewSessionOptions, SessionEntry, SessionHeader, SessionManager, SessionTag};

// ===========================================================================
// Clock / id seam
// ===========================================================================

/// The source of timestamps and ids threaded through [`SessionManager`].
///
/// Production always uses [`Seam::Real`] (the real `now_iso`/`uuidv7`/`generateId`
/// helpers). Byte-exact write tests inject a [`Seam::Fixed`] so the serialized
/// header and entry lines are deterministic and can be asserted verbatim.
#[derive(Clone)]
pub(crate) enum Seam {
    /// Real wall-clock timestamps and random ids.
    Real,
    /// Fixed timestamp + session id, with sequential entry ids derived from the
    /// current id count. Test-only.
    #[cfg(test)]
    Fixed {
        timestamp: String,
        session_id: String,
    },
}

impl Seam {
    /// A `Date.toISOString()`-shaped timestamp.
    pub(crate) fn timestamp(&self) -> String {
        match self {
            Seam::Real => super::now_iso(),
            #[cfg(test)]
            Seam::Fixed { timestamp, .. } => timestamp.clone(),
        }
    }

    /// A fresh session id (`uuidv7` in production).
    pub(crate) fn session_id(&self) -> String {
        match self {
            Seam::Real => super::create_session_id(),
            #[cfg(test)]
            Seam::Fixed { session_id, .. } => session_id.clone(),
        }
    }

    /// A unique short entry id, collision-checked against `existing`.
    pub(crate) fn generate_entry_id(&self, existing: &HashSet<String>) -> String {
        match self {
            Seam::Real => super::generate_id(existing),
            #[cfg(test)]
            Seam::Fixed { .. } => {
                let mut n = existing.len() as u64 + 1;
                loop {
                    let candidate = format!("{n:08x}");
                    if !existing.contains(&candidate) {
                        return candidate;
                    }
                    n += 1;
                }
            }
        }
    }
}

// ===========================================================================
// Lexical path helpers
// ===========================================================================
//
// These delegate to `crate::utils::paths` / `crate::utils::bytes`, whose
// lexical normalization is behaviorally identical to pi's `normalizePath` /
// `resolvePath` for session inputs. Reusing them (rather than re-porting the
// helpers a second time) keeps this crate's one copy of the path logic.

use crate::utils::bytes::posix_normalize;
use crate::utils::paths::{self, PathInputOptions};

/// The current working directory (`process.cwd()`), or `.` when unavailable.
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| String::from("."))
}

/// Lexically collapse `.` / `..` and duplicate slashes (no symlink resolution).
fn lexical(path: &str) -> String {
    posix_normalize(path, false)
}

/// Expand a leading `~` (and `file://`) then lexically normalize. Mirrors pi's
/// `normalizePath` for our inputs.
fn normalize(input: &str) -> String {
    match paths::normalize_path(input, &PathInputOptions::default()) {
        Ok(expanded) => lexical(&expanded),
        Err(_) => lexical(input),
    }
}

/// Resolve `input` against `base`. Mirrors pi's `resolvePath`.
fn resolve(input: &str, base: &str) -> String {
    paths::resolve_path(input, base, &PathInputOptions::default())
        .unwrap_or_else(|_| normalize(input))
}

// ===========================================================================
// Session-directory computation
// ===========================================================================

/// The agent config directory (`$PI_CODING_AGENT_DIR` non-empty, else
/// `~/.pi/agent`). Mirrors `getAgentDir()`; `~` is expanded downstream by
/// [`resolve`].
fn agent_dir() -> PathBuf {
    match std::env::var_os("PI_CODING_AGENT_DIR") {
        Some(env_dir) if !env_dir.is_empty() => PathBuf::from(env_dir),
        _ => {
            let home = paths::normalize_path("~", &PathInputOptions::default())
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".pi").join("agent")
        }
    }
}

/// Encode a resolved cwd into the safe per-cwd directory segment `--<safe>--`:
/// leading separators dropped, and `/`, `\`, `:` mapped to `-`.
fn encode_cwd_segment(resolved_cwd: &str) -> String {
    let body: String = resolved_cwd
        .trim_start_matches(['/', '\\'])
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{body}--")
}

/// The default per-cwd session directory path. Mirrors
/// `getDefaultSessionDirPath`: `<agentDir>/sessions/--<safeCwd>--`.
pub fn default_session_dir_path(cwd_input: &str) -> String {
    let base = process_cwd();
    let resolved_agent = resolve(agent_dir().to_str().unwrap_or("."), &base);
    let segment = encode_cwd_segment(&resolve(cwd_input, &base));
    lexical(&format!("{resolved_agent}/sessions/{segment}"))
}

/// The default per-cwd session directory, creating it if absent. Mirrors
/// `getDefaultSessionDir`.
pub fn get_default_session_dir(cwd: &str) -> String {
    let dir = default_session_dir_path(cwd);
    if !Path::new(&dir).exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

/// Compose the session file path `<dir>/<file-ts>_<id>.jsonl`, where the
/// timestamp's `:` and `.` are replaced with `-`. Mirrors pi's `newSession`
/// filename construction.
pub(crate) fn compose_session_file(dir: &str, timestamp: &str, session_id: &str) -> String {
    let file_ts = timestamp.replace([':', '.'], "-");
    lexical(&format!("{dir}/{file_ts}_{session_id}.jsonl"))
}

// ===========================================================================
// Lenient reader + discovery
// ===========================================================================

/// Whether `entries` begins with a valid session header (`type == "session"`
/// and a string `id`) — the gate `loadEntriesFromFile` applies after parsing.
fn starts_with_session_header(entries: &[Value]) -> bool {
    entries.first().is_some_and(|header| {
        header.get("type").and_then(Value::as_str) == Some("session")
            && header.get("id").and_then(Value::as_str).is_some()
    })
}

/// Parse a JSONL session file into raw entries, streaming line-by-line.
///
/// Returns `[]` when the file does not exist, is empty, or does not start with a
/// valid `{"type":"session",...}` header (string `id`). Blank and malformed
/// lines are silently skipped. Mirrors `loadEntriesFromFile`, including its
/// streaming read so files larger than a single in-memory string still load.
pub fn load_entries_from_file(path: &str) -> Vec<Value> {
    let Ok(file) = File::open(normalize(path)) else {
        return Vec::new();
    };

    let mut entries: Vec<Value> = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            entries.push(value);
        }
    }

    if starts_with_session_header(&entries) {
        entries
    } else {
        Vec::new()
    }
}

/// Build a [`SessionHeader`] leniently from a header value: missing
/// `timestamp`/`cwd` become empty strings, matching the loose on-disk shape.
fn header_from_value(value: &Value, id: String) -> SessionHeader {
    let string_field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    SessionHeader {
        tag: SessionTag::Session,
        version: value.get("version").and_then(Value::as_i64),
        id,
        timestamp: string_field("timestamp"),
        cwd: string_field("cwd"),
        parent_session: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Read only the header of a session file, if valid. Mirrors
/// `readSessionHeader` (reads just the first line). Returns `None` unless the
/// first line is JSON with `type == "session"` and a string `id`.
pub fn read_session_header(path: &Path) -> Option<SessionHeader> {
    let file = File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    let value: Value = serde_json::from_str(first.trim()).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = value.get("id").and_then(Value::as_str)?.to_string();
    Some(header_from_value(&value, id))
}

fn session_cwd_matches(cwd: &str, resolved_cwd: &str) -> bool {
    !cwd.is_empty() && resolve(cwd, &process_cwd()) == resolved_cwd
}

/// Collect the `*.jsonl` files in `dir` paired with their parsed session
/// headers, skipping non-jsonl files and any without a valid header. Shared by
/// both discovery functions.
fn session_files_with_headers(dir: &str) -> Vec<(PathBuf, SessionHeader)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            read_session_header(&path).map(|header| (path, header))
        })
        .collect()
}

/// Return the most recently modified valid session file in `session_dir`,
/// optionally filtered to a `cwd`. Mirrors `findMostRecentSession`.
pub fn find_most_recent_session(session_dir: &str, cwd: Option<&str>) -> Option<String> {
    let resolved_cwd = cwd.map(|c| resolve(c, &process_cwd()));
    let mut best: Option<(String, std::time::SystemTime)> = None;
    for (path, header) in session_files_with_headers(&normalize(session_dir)) {
        if let Some(resolved_cwd) = &resolved_cwd {
            if !session_cwd_matches(&header.cwd, resolved_cwd) {
                continue;
            }
        }
        let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        let Some(path) = path.to_str().map(str::to_string) else {
            continue;
        };
        if best.as_ref().is_none_or(|(_, prev)| mtime > *prev) {
            best = Some((path, mtime));
        }
    }
    best.map(|(path, _)| path)
}

/// Find a local session whose header `id` exactly matches `session_id`,
/// returning its path. cwd filtering applies only when an explicit `session_dir`
/// is passed *and* it differs from the default. Mirrors
/// `findLocalSessionByExactId` + `SessionManager.list`.
pub fn find_local_session_by_exact_id(
    session_id: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Option<String> {
    let dir = match session_dir {
        Some(d) => normalize(d),
        None => default_session_dir_path(cwd),
    };
    let filter_cwd = session_dir.is_some() && dir != default_session_dir_path(cwd);
    let resolved_cwd = resolve(cwd, &process_cwd());

    for (path, header) in session_files_with_headers(&dir) {
        if header.id != session_id {
            continue;
        }
        if filter_cwd && !session_cwd_matches(&header.cwd, &resolved_cwd) {
            continue;
        }
        if let Some(path) = path.to_str() {
            return Some(path.to_string());
        }
    }
    None
}

// ===========================================================================
// Persisted factories + write path
// ===========================================================================

/// Serialize a value to a single JSONL line (no trailing newline).
fn json_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Append one already-serialized line (plus newline) to `file`, creating it if
/// necessary. Mirrors `appendFileSync`.
fn append_line(file: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let _ = writeln!(f, "{line}");
    }
}

impl SessionManager {
    /// Construct a bare manager with no session started yet. Resolves `cwd`,
    /// normalizes `session_dir`, and (when persisting) creates the directory.
    /// Mirrors the shared body of pi's private constructor.
    pub(crate) fn empty(cwd: &str, session_dir: &str, persist: bool, seam: Seam) -> Self {
        let resolved_cwd = resolve(cwd, &process_cwd());
        // In-memory sessions carry an empty dir; `normalize("")` would
        // yield ".", so preserve the empty sentinel explicitly.
        let normalized_dir = if session_dir.is_empty() {
            String::new()
        } else {
            normalize(session_dir)
        };
        if persist && !normalized_dir.is_empty() && !Path::new(&normalized_dir).exists() {
            let _ = std::fs::create_dir_all(&normalized_dir);
        }
        SessionManager {
            session_id: String::new(),
            session_file: None,
            session_dir: normalized_dir,
            cwd: resolved_cwd,
            persist,
            flushed: false,
            seam,
            header: super::placeholder_header(),
            entries: Vec::new(),
            by_id: std::collections::HashMap::new(),
            ids: HashSet::new(),
            labels_by_id: std::collections::HashMap::new(),
            label_timestamps_by_id: std::collections::HashMap::new(),
            leaf_id: None,
        }
    }

    /// Create a new persisted session. The file is not written until the first
    /// assistant message is appended. Mirrors `SessionManager.create`.
    ///
    /// `session_dir` defaults to the encoded per-cwd directory. `id`, when
    /// present, is expected to already be valid (the CLI validates it before
    /// calling); an invalid id leaves the session un-started rather than
    /// throwing, matching the infallible drop-in signature.
    pub fn create(cwd: &str, session_dir: Option<&str>, id: Option<&str>) -> Self {
        Self::create_with_seam(cwd, session_dir, id, Seam::Real)
    }

    pub(crate) fn create_with_seam(
        cwd: &str,
        session_dir: Option<&str>,
        id: Option<&str>,
        seam: Seam,
    ) -> Self {
        let dir = match session_dir {
            Some(d) => normalize(d),
            None => get_default_session_dir(cwd),
        };
        let mut manager = Self::empty(cwd, &dir, true, seam);
        let options = NewSessionOptions {
            id: id.map(|s| s.to_string()),
            parent_session: None,
        };
        let _ = manager.new_session(options);
        manager
    }

    /// Open a persisted session file. Mirrors `SessionManager.open` +
    /// `setSessionFile`.
    ///
    /// Returns `Err(message)` (no `Error: ` prefix) when the file exists, is
    /// non-empty, but does not parse as a pi session — leaving the file's bytes
    /// untouched. The session directory is derived from the file's parent.
    pub fn open(path: &str) -> Result<Self, String> {
        let resolved = resolve(path, &process_cwd());
        let raw = load_entries_from_file(&resolved);
        let header_cwd = raw
            .first()
            .and_then(|h| h.get("cwd"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let cwd = header_cwd.unwrap_or_else(process_cwd);
        let dir = resolve("..", &resolved);

        let mut manager = Self::empty(&cwd, &dir, true, Seam::Real);
        manager.set_session_file(&resolved)?;
        Ok(manager)
    }

    /// Switch to a different session file (resume/branching). Loads and
    /// validates it, throwing (without mutating the file) when it is non-empty
    /// but invalid, initializing an empty file with a header, and migrating an
    /// older-version file to v3 (rewriting only then). Mirrors `setSessionFile`.
    pub(crate) fn set_session_file(&mut self, session_file: &str) -> Result<(), String> {
        let resolved = resolve(session_file, &process_cwd());
        self.session_file = Some(resolved.clone());

        if !Path::new(&resolved).exists() {
            // No file yet: start a fresh session but keep the explicit path.
            self.new_session(NewSessionOptions::default())?;
            self.session_file = Some(resolved);
            return Ok(());
        }

        let raw = load_entries_from_file(&resolved);
        if raw.is_empty() {
            let size = std::fs::metadata(&resolved).map(|m| m.len()).unwrap_or(0);
            if size > 0 {
                // Non-empty but unparseable: reject without touching the file.
                return Err(format!(
                    "Session file is not a valid pi session: {resolved}"
                ));
            }
            // Genuinely empty (0-byte) file: initialize a valid header, keep the
            // explicit path, and write the header out.
            self.new_session(NewSessionOptions::default())?;
            self.session_file = Some(resolved);
            self.rewrite_file();
            self.flushed = true;
            return Ok(());
        }

        let mut migrated = raw;
        let did_migrate = super::migrate_to_current_version(&mut migrated);
        self.load_typed(&migrated);
        if did_migrate {
            self.rewrite_file();
        }
        self.flushed = true;
        Ok(())
    }

    /// Populate the typed header/entries/index from a (possibly migrated) list
    /// of raw file values. The first value is the validated header.
    fn load_typed(&mut self, values: &[Value]) {
        let header = values
            .first()
            .and_then(|v| {
                serde_json::from_value::<SessionHeader>(v.clone())
                    .ok()
                    .or_else(|| {
                        v.get("id")
                            .and_then(Value::as_str)
                            .map(|id| header_from_value(v, id.to_string()))
                    })
            })
            .unwrap_or_else(super::placeholder_header);
        self.session_id = header.id.clone();
        self.header = header;
        self.entries = values
            .iter()
            .skip(1)
            .filter(|v| v.get("type").and_then(Value::as_str) != Some("session"))
            .filter_map(|v| serde_json::from_value::<SessionEntry>(v.clone()).ok())
            .collect();
        self.rebuild_index();
    }

    /// Whether the buffered entries contain an assistant message. Mirrors pi's
    /// `hasAssistant` check that gates the deferred flush.
    pub(crate) fn has_assistant_message(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(entry, SessionEntry::Message(m)
                if m.message.get("role").and_then(Value::as_str) == Some("assistant"))
        })
    }

    /// The deferred-flush write step, run after every append. Mirrors
    /// `_persist`: buffer in memory until an assistant message exists, then
    /// flush the whole buffer once with create-new (`wx`) semantics, and append
    /// line-by-line thereafter.
    pub(crate) fn persist_last_entry(&mut self) {
        if !self.persist {
            return;
        }
        let Some(file) = self.session_file.clone() else {
            return;
        };

        if !self.has_assistant_message() {
            // Before the first assistant message nothing is on disk, so keep the
            // pre-flush entries buffered (unless a prior load already flushed).
            if self.flushed {
                if let Some(entry) = self.entries.last() {
                    append_line(&file, &json_line(entry));
                }
            }
            return;
        }

        if !self.flushed {
            self.write_all_create_new(&file);
            self.flushed = true;
        } else if let Some(entry) = self.entries.last() {
            append_line(&file, &json_line(entry));
        }
    }

    /// Write the header plus every buffered entry to a freshly created file
    /// (`wx`: fails if the file already exists). The first deferred flush.
    fn write_all_create_new(&self, file: &str) {
        let Ok(mut handle) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)
        else {
            return;
        };
        let _ = handle.write_all(self.serialize_all().as_bytes());
    }

    /// Truncate-and-rewrite the whole file (header + entries). Mirrors
    /// `_rewriteFile`; used only for empty-file initialization, migration, and
    /// the persisted `createBranchedSession`-with-assistant arm.
    pub(crate) fn rewrite_file(&self) {
        if !self.persist {
            return;
        }
        let Some(file) = &self.session_file else {
            return;
        };
        let _ = std::fs::write(file, self.serialize_all());
    }

    /// The full on-disk body: the header line followed by one line per entry.
    fn serialize_all(&self) -> String {
        let mut buffer = String::new();
        buffer.push_str(&json_line(&self.header));
        buffer.push('\n');
        for entry in &self.entries {
            buffer.push_str(&json_line(entry));
            buffer.push('\n');
        }
        buffer
    }
}
