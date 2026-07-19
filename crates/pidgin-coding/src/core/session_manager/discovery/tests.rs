//! Discovery / list / fork tests for the coding-agent `SessionManager`,
//! translated from pi's `session-manager/file-operations.test.ts` (the list /
//! `listAll` / `continueRecent` cwd-scoping cases slice B deferred),
//! `session-info-modified-timestamp.test.ts`, and the `forkFrom` cases in
//! `session-manager/custom-session-id.test.ts`.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use tempfile::TempDir;

use pidgin_agent::harness::session::messages::parse_iso_millis;

use super::super::SessionManager;
use super::build_session_info;
use crate::core::session_manager::NewSessionOptions;

// --- fixtures ---------------------------------------------------------------

fn user_message(text: &str, timestamp: i64) -> Value {
    json!({ "role": "user", "content": text, "timestamp": timestamp })
}

fn assistant_message(text: &str, timestamp: i64) -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "provider": "anthropic",
        "model": "test-model",
        "timestamp": timestamp
    })
}

/// Create a persisted session under `dir` for `cwd`, seeding a user + assistant
/// message so the deferred flush lands a file on disk. Returns its path. Mirrors
/// the `createPersistedSession` helper in pi's file-operations test.
fn persisted_session(cwd: &str, dir: &str, label: &str, base_time: i64) -> String {
    let mut session = SessionManager::create(cwd, Some(dir), None);
    session.append_message(user_message(label, base_time));
    session.append_message(assistant_message(
        &format!("reply to {label}"),
        base_time + 1,
    ));
    session
        .get_session_file()
        .expect("persisted session file")
        .to_string()
}

fn write_session_file(path: &Path, header: &str, entries: &[&str]) {
    let mut body = String::from(header);
    body.push('\n');
    for entry in entries {
        body.push_str(entry);
        body.push('\n');
    }
    fs::write(path, body).unwrap();
}

// --- list / listAll / continueRecent cwd scoping ----------------------------

#[test]
fn scopes_current_folder_apis_by_cwd_while_listing_all_flat_sessions() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    let (cwd_a, cwd_b) = (project_a.to_str().unwrap(), project_b.to_str().unwrap());

    let session_a = persisted_session(cwd_a, dir, "from A", 1_000);
    let session_b = persisted_session(cwd_b, dir, "from B", 2_000);

    // `list` is scoped to the requested cwd.
    let current_a = SessionManager::list(cwd_a, Some(dir));
    assert_eq!(
        current_a.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
        vec![session_a.clone()]
    );

    // `list_all` returns every flat session regardless of cwd.
    let all = SessionManager::list_all(Some(dir));
    let mut paths: Vec<String> = all.iter().map(|s| s.path.clone()).collect();
    paths.sort();
    let mut expected = vec![session_a.clone(), session_b.clone()];
    expected.sort();
    assert_eq!(paths, expected);

    // `continue_recent` resumes the cwd-scoped most-recent session.
    let continued_a = SessionManager::continue_recent(cwd_a, Some(dir));
    assert_eq!(continued_a.get_session_file(), Some(session_a.as_str()));
}

#[test]
fn list_sorts_by_modified_descending() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let older = persisted_session(cwd, dir, "older", 1_000);
    let newer = persisted_session(cwd, dir, "newer", 5_000);

    let listed = SessionManager::list(cwd, Some(dir));
    let paths: Vec<String> = listed.iter().map(|s| s.path.clone()).collect();
    // Newest message activity first.
    assert_eq!(paths, vec![newer, older]);
}

#[test]
fn continue_recent_starts_fresh_when_no_session_exists() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("empty");
    fs::create_dir_all(&dir).unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let session = SessionManager::continue_recent(cwd, Some(dir.to_str().unwrap()));
    assert!(!session.get_session_id().is_empty());
    // A brand-new session defers its write, so nothing is on disk yet.
    let file = session.get_session_file().unwrap();
    assert!(!Path::new(file).exists());
}

#[test]
fn list_all_returns_empty_for_missing_directory() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("nope");
    assert!(SessionManager::list_all(Some(missing.to_str().unwrap())).is_empty());
}

// --- SessionInfo.modified ---------------------------------------------------

#[test]
fn modified_uses_last_message_timestamp_not_file_mtime() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let file = tmp.path().join("session.jsonl");

    // Header + a first assistant message so the session persists on open.
    write_session_file(
        &file,
        "{\"type\":\"session\",\"id\":\"test-session\",\"version\":3,\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}",
        &[],
    );
    let path = file.to_str().unwrap();

    let mut mgr = SessionManager::open(path).unwrap();
    mgr.append_message(assistant_message("hi", 1_500_000_000_000));

    // A later assistant message carries the activity time we expect to surface.
    let msg_time: i64 = 1_600_000_000_000;
    let mut mgr2 = SessionManager::open(path).unwrap();
    mgr2.append_message(assistant_message("later", msg_time));

    let sessions = SessionManager::list("/tmp", Some(dir));
    let session = sessions
        .iter()
        .find(|s| s.path == path)
        .expect("session listed");

    assert_eq!(parse_iso_millis(&session.modified), msg_time);

    let mtime_millis = fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert_ne!(parse_iso_millis(&session.modified), mtime_millis);
}

// --- build_session_info -----------------------------------------------------

#[test]
fn build_session_info_derives_first_message_and_count() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("s.jsonl");
    write_session_file(
        &file,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"cwd\":\"/w\"}",
        &[
            "{\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hello there\"}}",
            "{\"type\":\"message\",\"id\":\"2\",\"parentId\":\"1\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi back\"}]}}",
        ],
    );

    let info = build_session_info(file.to_str().unwrap()).unwrap();
    assert_eq!(info.id, "abc");
    assert_eq!(info.cwd, "/w");
    assert_eq!(info.message_count, 2);
    assert_eq!(info.first_message, "hello there");
    assert_eq!(info.all_messages_text, "hello there hi back");
    assert!(info.name.is_none());
    assert!(info.parent_session_path.is_none());
}

#[test]
fn build_session_info_reports_no_messages_placeholder() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("s.jsonl");
    write_session_file(
        &file,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"cwd\":\"/w\"}",
        &[],
    );
    let info = build_session_info(file.to_str().unwrap()).unwrap();
    assert_eq!(info.message_count, 0);
    assert_eq!(info.first_message, "(no messages)");
}

#[test]
fn build_session_info_uses_latest_session_name() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("s.jsonl");
    write_session_file(
        &file,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"cwd\":\"/w\"}",
        &[
            "{\"type\":\"session_info\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"name\":\"first\"}",
            "{\"type\":\"session_info\",\"id\":\"2\",\"parentId\":\"1\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"name\":\"second\"}",
        ],
    );
    let info = build_session_info(file.to_str().unwrap()).unwrap();
    assert_eq!(info.name.as_deref(), Some("second"));
}

#[test]
fn build_session_info_returns_none_without_header() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("s.jsonl");
    fs::write(&file, "{\"type\":\"message\",\"id\":\"1\"}\n").unwrap();
    assert!(build_session_info(file.to_str().unwrap()).is_none());
}

// --- forkFrom ---------------------------------------------------------------

fn write_fork_source(dir: &Path, with_entry: bool) -> String {
    let source = dir.join("source.jsonl");
    let header =
        "{\"type\":\"session\",\"version\":3,\"id\":\"legacy-session-id\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"cwd\":\"/other\"}";
    if with_entry {
        write_session_file(
            &source,
            header,
            &["{\"type\":\"message\",\"id\":\"entry-1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}],\"provider\":\"openai\",\"model\":\"gpt\",\"timestamp\":1}}"],
        );
    } else {
        write_session_file(&source, header, &[]);
    }
    source.to_str().unwrap().to_string()
}

#[test]
fn fork_from_creates_new_session_with_uuid_and_parent_pointer() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let source = write_fork_source(tmp.path(), true);

    let forked =
        SessionManager::fork_from(&source, dir, Some(dir), NewSessionOptions::default()).unwrap();
    let header = forked.get_header().unwrap();

    // A fresh uuidv7 id (36 chars, version nibble `7`).
    assert_eq!(header.id.len(), 36);
    assert_eq!(header.id.as_bytes()[14], b'7');
    assert_eq!(header.parent_session.as_deref(), Some(source.as_str()));

    // The forked file exists on disk and copied the source's one entry.
    let file = forked.get_session_file().unwrap();
    assert!(Path::new(file).exists());
    assert_eq!(forked.get_entries().len(), 1);
}

#[test]
fn fork_from_uses_provided_id_and_filename() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let source = write_fork_source(tmp.path(), false);

    let options = NewSessionOptions {
        id: Some("forked-session-id".to_string()),
        parent_session: None,
    };
    let forked = SessionManager::fork_from(&source, dir, Some(dir), options).unwrap();
    let header = forked.get_header().unwrap();
    assert_eq!(header.id, "forked-session-id");
    assert_eq!(header.parent_session.as_deref(), Some(source.as_str()));

    let file = forked.get_session_file().unwrap();
    assert!(file.contains("forked-session-id"));
    let name = Path::new(file).file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with("_forked-session-id.jsonl"));
    // `YYYY-MM-DDThh-mm-ss-sssZ_<id>.jsonl` shape (colons/dots replaced by `-`).
    assert!(name.as_bytes()[4] == b'-' && name.contains('T'));
}

#[test]
fn fork_from_rejects_empty_source() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let empty = tmp.path().join("empty.jsonl");
    fs::write(&empty, "").unwrap();

    let result = SessionManager::fork_from(
        empty.to_str().unwrap(),
        dir,
        Some(dir),
        NewSessionOptions::default(),
    );
    let Err(err) = result else {
        panic!("expected fork to reject an empty source");
    };
    assert!(err.starts_with("Cannot fork: source session file is empty or invalid:"));
}

#[test]
fn fork_from_rejects_invalid_id() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let source = write_fork_source(tmp.path(), true);

    let options = NewSessionOptions {
        id: Some("-bad".to_string()),
        parent_session: None,
    };
    let Err(err) = SessionManager::fork_from(&source, dir, Some(dir), options) else {
        panic!("expected fork to reject an invalid id");
    };
    assert!(err.contains("Session id must be non-empty"));
}
