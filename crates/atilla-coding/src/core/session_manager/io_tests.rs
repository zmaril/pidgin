//! File-I/O fidelity tests for the coding-agent `SessionManager`, translated
//! from pi's `packages/coding-agent/test/session-manager/file-operations.test.ts`
//! and `session-file-invalid.test.ts`, plus the byte-fidelity guards the CLI
//! contract pins (byte-identical-on-invalid, no-leaf, no-rewrite, deferred
//! flush, write-before-abort, exact-id discovery).

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use super::io::Seam;
use super::{
    default_session_dir_path, find_local_session_by_exact_id, find_most_recent_session,
    load_entries_from_file, read_session_header, SessionManager,
};

// --- fixtures ---------------------------------------------------------------

const FIXED_TS: &str = "2025-06-01T00:00:00.000Z";

fn user_message(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": 1 })
}

fn assistant_message() -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "ok" }],
        "provider": "anthropic",
        "model": "test-model",
        "timestamp": 2
    })
}

fn write_session(dir: &Path, name: &str, id: &str, cwd: &str) -> String {
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "{{\"type\":\"session\",\"id\":\"{id}\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"{cwd}\"}}\n"
        ),
    )
    .unwrap();
    path.to_str().unwrap().to_string()
}

fn path_str(dir: &Path, name: &str) -> String {
    dir.join(name).to_str().unwrap().to_string()
}

// --- loadEntriesFromFile ----------------------------------------------------

#[test]
fn load_entries_returns_empty_for_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    assert!(load_entries_from_file(&path_str(tmp.path(), "nope.jsonl")).is_empty());
}

#[test]
fn load_entries_returns_empty_for_empty_file() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "empty.jsonl");
    fs::write(&file, "").unwrap();
    assert!(load_entries_from_file(&file).is_empty());
}

#[test]
fn load_entries_returns_empty_without_valid_header() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "no-header.jsonl");
    fs::write(&file, "{\"type\":\"message\",\"id\":\"1\"}\n").unwrap();
    assert!(load_entries_from_file(&file).is_empty());
}

#[test]
fn load_entries_returns_empty_for_malformed_json() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "malformed.jsonl");
    fs::write(&file, "not json\n").unwrap();
    assert!(load_entries_from_file(&file).is_empty());
}

#[test]
fn load_entries_loads_valid_session_file() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "valid.jsonl");
    fs::write(
        &file,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
    )
    .unwrap();
    let entries = load_entries_from_file(&file);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["type"], "session");
    assert_eq!(entries[1]["type"], "message");
}

#[test]
fn load_entries_skips_malformed_but_keeps_valid() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "mixed.jsonl");
    fs::write(
        &file,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         not valid json\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
    )
    .unwrap();
    assert_eq!(load_entries_from_file(&file).len(), 2);
}

#[test]
fn open_streams_large_session_file() {
    // pi's ">Node max string length" test, translated as a large-but-bounded
    // streaming read: a header, a long run of blank lines, then one message.
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "large.jsonl");
    let mut content = String::from(
        "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
    );
    for _ in 0..200_000 {
        content.push('\n');
    }
    content.push_str(
        "{\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
    );
    fs::write(&file, content).unwrap();

    let sm = SessionManager::open(&file).unwrap();
    assert_eq!(sm.get_session_id(), "abc");
    assert_eq!(sm.get_entries().len(), 1);
    assert_eq!(
        sm.build_session_context().messages,
        vec![json!({ "role": "user", "content": "hi", "timestamp": 1 })]
    );
}

// --- findMostRecentSession --------------------------------------------------

#[test]
fn find_most_recent_returns_none_for_empty_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(find_most_recent_session(tmp.path().to_str().unwrap(), None).is_none());
}

#[test]
fn find_most_recent_returns_none_for_missing_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(find_most_recent_session(&path_str(tmp.path(), "nope"), None).is_none());
}

#[test]
fn find_most_recent_ignores_non_jsonl_and_headerless() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file.txt"), "hello").unwrap();
    fs::write(tmp.path().join("file.json"), "{}").unwrap();
    fs::write(tmp.path().join("invalid.jsonl"), "{\"type\":\"message\"}\n").unwrap();
    assert!(find_most_recent_session(tmp.path().to_str().unwrap(), None).is_none());
}

#[test]
fn find_most_recent_returns_single_valid_session() {
    let tmp = TempDir::new().unwrap();
    let file = write_session(tmp.path(), "session.jsonl", "abc", "/tmp");
    assert_eq!(
        find_most_recent_session(tmp.path().to_str().unwrap(), None),
        Some(file)
    );
}

#[test]
fn find_most_recent_returns_newest_by_mtime() {
    let tmp = TempDir::new().unwrap();
    write_session(tmp.path(), "older.jsonl", "old", "/tmp");
    sleep(Duration::from_millis(20));
    let newer = write_session(tmp.path(), "newer.jsonl", "new", "/tmp");
    assert_eq!(
        find_most_recent_session(tmp.path().to_str().unwrap(), None),
        Some(newer)
    );
}

#[test]
fn find_most_recent_skips_invalid_and_returns_valid() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("invalid.jsonl"),
        "{\"type\":\"not-session\"}\n",
    )
    .unwrap();
    sleep(Duration::from_millis(20));
    let valid = write_session(tmp.path(), "valid.jsonl", "abc", "/tmp");
    assert_eq!(
        find_most_recent_session(tmp.path().to_str().unwrap(), None),
        Some(valid)
    );
}

#[test]
fn find_most_recent_filters_by_cwd() {
    let tmp = TempDir::new().unwrap();
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    let file_a = write_session(tmp.path(), "a.jsonl", "a", project_a.to_str().unwrap());
    sleep(Duration::from_millis(20));
    let file_b = write_session(tmp.path(), "b.jsonl", "b", project_b.to_str().unwrap());

    let dir = tmp.path().to_str().unwrap();
    assert_eq!(
        find_most_recent_session(dir, Some(project_a.to_str().unwrap())),
        Some(file_a)
    );
    assert_eq!(
        find_most_recent_session(dir, Some(project_b.to_str().unwrap())),
        Some(file_b)
    );
}

// --- setSessionFile with corrupted files (via open) -------------------------

/// Assert that opening `file` (holding `original`) fails with the exact
/// invalid-session error and leaves the bytes untouched.
fn assert_open_rejects_and_preserves(file: &str, original: &str) {
    fs::write(file, original).unwrap();
    let err = match SessionManager::open(file) {
        Ok(_) => panic!("expected an invalid-session error"),
        Err(e) => e,
    };
    assert_eq!(
        err,
        format!("Session file is not a valid pi session: {file}")
    );
    // Byte-identical: the invalid file is never rewritten or truncated.
    assert_eq!(fs::read_to_string(file).unwrap(), original);
}

#[test]
fn open_throws_and_preserves_non_empty_headerless_file() {
    let tmp = TempDir::new().unwrap();
    assert_open_rejects_and_preserves(
        &path_str(tmp.path(), "no-header.jsonl"),
        "{\"type\":\"message\",\"id\":\"abc\",\"parentId\":\"orphaned\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"test\"}}\n",
    );
}

#[test]
fn open_throws_and_preserves_non_session_jsonl() {
    let tmp = TempDir::new().unwrap();
    assert_open_rejects_and_preserves(
        &path_str(tmp.path(), "not-a-session.log"),
        "{\"type\":\"event\",\"data\":\"not a session\"}\n",
    );
}

#[test]
fn open_initializes_empty_file_with_single_header() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "empty.jsonl");
    fs::write(&file, "").unwrap();

    let sm = SessionManager::open(&file).unwrap();
    assert!(!sm.get_session_id().is_empty());
    assert!(sm.get_header().is_some());
    assert_eq!(sm.get_session_file(), Some(file.as_str()));

    let content = fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1);
    let header: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["id"], sm.get_session_id());
}

#[test]
fn subsequent_open_of_initialized_empty_file_is_stable() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "empty.jsonl");
    fs::write(&file, "").unwrap();

    let first = SessionManager::open(&file).unwrap();
    let session_id = first.get_session_id().to_string();

    let second = SessionManager::open(&file).unwrap();
    assert_eq!(second.get_session_id(), session_id);
    assert_eq!(
        second.get_header().map(|h| h.tag),
        first.get_header().map(|h| h.tag)
    );
}

#[test]
fn open_does_not_rewrite_a_valid_v3_file() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "valid.jsonl");
    let original = "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n";
    fs::write(&file, original).unwrap();

    let sm = SessionManager::open(&file).unwrap();
    assert_eq!(sm.get_session_id(), "abc");
    // No rewrite of a current-version file: bytes are untouched.
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
}

#[test]
fn open_migrates_and_rewrites_older_version_file() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "v2.jsonl");
    let original = "{\"type\":\"session\",\"version\":2,\"id\":\"v2sess\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"hookMessage\",\"content\":\"x\"}}\n";
    fs::write(&file, original).unwrap();

    let sm = SessionManager::open(&file).unwrap();
    assert_eq!(sm.get_session_id(), "v2sess");
    // A migration rewrites the file: version bumped to 3, hookMessage → custom.
    let rewritten = fs::read_to_string(&file).unwrap();
    assert_ne!(rewritten, original);
    let lines: Vec<&str> = rewritten.lines().filter(|l| !l.is_empty()).collect();
    let header: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["version"], 3);
    let message: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(message["message"]["role"], "custom");
}

// --- deferred flush + byte-exact write (fixed seam) -------------------------

fn byte_exact_manager(dir: &Path) -> SessionManager {
    let seam = Seam::Fixed {
        timestamp: FIXED_TS.to_string(),
        session_id: "testsess".to_string(),
    };
    SessionManager::create_with_seam("/work/project", Some(dir.to_str().unwrap()), None, seam)
}

#[test]
fn create_defers_write_until_first_assistant_message() {
    let tmp = TempDir::new().unwrap();
    let mut sm = byte_exact_manager(tmp.path());
    let file = sm.get_session_file().unwrap().to_string();

    // Nothing on disk before any entry, nor after a user-only message.
    assert!(!Path::new(&file).exists());
    sm.append_message(user_message("hi"));
    assert!(!Path::new(&file).exists());

    // The first assistant message flushes the whole buffer once.
    sm.append_message(assistant_message());
    assert!(Path::new(&file).exists());
    let lines: Vec<String> = fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(lines.len(), 3);
}

/// A byte-exact manager that has already flushed (user + assistant appended),
/// returned with its session file path.
fn flushed_byte_exact_manager(dir: &Path) -> (SessionManager, String) {
    let mut sm = byte_exact_manager(dir);
    let file = sm.get_session_file().unwrap().to_string();
    sm.append_message(user_message("hi"));
    sm.append_message(assistant_message());
    (sm, file)
}

#[test]
fn deferred_flush_writes_byte_exact_lines() {
    let tmp = TempDir::new().unwrap();
    let (sm, file) = flushed_byte_exact_manager(tmp.path());

    let content = fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);

    // Header byte-exact: key order + version, no metadata/leaf keys.
    let expected_header = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"testsess\",\"timestamp\":\"{FIXED_TS}\",\"cwd\":\"{}\"}}",
        sm.get_cwd()
    );
    assert_eq!(lines[0], expected_header);

    // Entry lines equal their typed serialization (deterministic ids).
    assert_eq!(sm.get_entries()[0].id(), "00000001");
    assert_eq!(sm.get_entries()[1].id(), "00000002");
    assert_eq!(
        lines[1],
        serde_json::to_string(&sm.get_entries()[0]).unwrap()
    );
    assert_eq!(
        lines[2],
        serde_json::to_string(&sm.get_entries()[1]).unwrap()
    );

    // No persisted leaf line ever.
    assert!(!content.contains("\"type\":\"leaf\""));
}

#[test]
fn session_info_appends_byte_exact_line_after_flush() {
    let tmp = TempDir::new().unwrap();
    let (mut sm, file) = flushed_byte_exact_manager(tmp.path());
    // Written even though the surrounding CLI run may go on to abort.
    sm.append_session_info("  Hello  ");

    let content = fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(
        lines[3],
        "{\"type\":\"session_info\",\"id\":\"00000003\",\"parentId\":\"00000002\",\"timestamp\":\"2025-06-01T00:00:00.000Z\",\"name\":\"Hello\"}"
    );
    assert!(!content.contains("\"type\":\"leaf\""));
}

#[test]
fn append_session_info_on_flushed_session_writes_immediately() {
    // Mirrors the CLI's `--name` path: an already-flushed session appends the
    // session_info line to disk before the run may abort.
    let tmp = TempDir::new().unwrap();
    let (mut sm, file) = flushed_byte_exact_manager(tmp.path());
    let before = fs::read_to_string(&file).unwrap().lines().count();

    sm.append_session_info("named");
    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(after.lines().count(), before + 1);
    let last: Value = serde_json::from_str(after.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "session_info");
    assert_eq!(last["name"], "named");
}

#[test]
fn no_leaf_line_after_branching_and_forking() {
    let tmp = TempDir::new().unwrap();
    let mut sm = byte_exact_manager(tmp.path());
    let first = sm.append_message(user_message("hi"));
    sm.append_message(assistant_message());
    sm.append_thinking_level_change("high");
    sm.branch(&first).unwrap();
    let branched = sm.create_branched_session(&first).unwrap();

    // The branched session mints a new persisted file...
    let branched_file = branched.expect("persisted fork returns a file path");
    // ...whose header points back at the source file.
    assert_eq!(sm.get_session_file(), Some(branched_file.as_str()));
    let header = sm.get_header().unwrap();
    assert!(header.parent_session.is_some());

    // The branched session's entries never include a leaf entry.
    for entry in sm.get_entries() {
        assert_ne!(entry.type_str(), "leaf");
    }
    // Nor does the branched file's serialized form.
    assert!(!fs::read_to_string(&branched_file)
        .unwrap_or_default()
        .contains("\"type\":\"leaf\""));
}

// --- find_local_session_by_exact_id -----------------------------------------

#[test]
fn find_by_exact_id_returns_matching_path() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let file = write_session(
        tmp.path(),
        "s.jsonl",
        "target-id",
        project.to_str().unwrap(),
    );

    let found = find_local_session_by_exact_id(
        "target-id",
        project.to_str().unwrap(),
        Some(tmp.path().to_str().unwrap()),
    );
    assert_eq!(found, Some(file));
}

#[test]
fn find_by_exact_id_returns_none_when_missing() {
    let tmp = TempDir::new().unwrap();
    write_session(tmp.path(), "s.jsonl", "some-id", "/tmp");
    let found =
        find_local_session_by_exact_id("other-id", "/tmp", Some(tmp.path().to_str().unwrap()));
    assert!(found.is_none());
}

#[test]
fn find_by_exact_id_applies_cwd_filter_with_explicit_dir() {
    let tmp = TempDir::new().unwrap();
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    write_session(
        tmp.path(),
        "s.jsonl",
        "shared-id",
        project_a.to_str().unwrap(),
    );

    let dir = tmp.path().to_str().unwrap();
    // Matching cwd finds it; a different cwd is filtered out.
    assert!(
        find_local_session_by_exact_id("shared-id", project_a.to_str().unwrap(), Some(dir))
            .is_some()
    );
    assert!(
        find_local_session_by_exact_id("shared-id", project_b.to_str().unwrap(), Some(dir))
            .is_none()
    );
}

// --- read_session_header / default dir --------------------------------------

#[test]
fn read_session_header_parses_id_and_cwd() {
    let tmp = TempDir::new().unwrap();
    let file = write_session(tmp.path(), "s.jsonl", "hdr-id", "/work/dir");
    let header = read_session_header(Path::new(&file)).unwrap();
    assert_eq!(header.id, "hdr-id");
    assert_eq!(header.cwd, "/work/dir");
}

#[test]
fn read_session_header_rejects_non_session_first_line() {
    let tmp = TempDir::new().unwrap();
    let file = path_str(tmp.path(), "x.jsonl");
    fs::write(&file, "{\"type\":\"message\",\"id\":\"1\"}\n").unwrap();
    assert!(read_session_header(Path::new(&file)).is_none());
}

#[test]
fn default_session_dir_path_encodes_cwd() {
    let dir = default_session_dir_path("/work/project");
    assert!(dir.ends_with("/sessions/--work-project--"));
    assert!(dir.contains("/agent/"));
}
