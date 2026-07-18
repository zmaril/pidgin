//! Ports `test/harness/storage.test.ts` for both the in-memory and JSONL
//! storages. Shared behaviors (label lifecycle, find-by-type, path-to-root) run
//! through helpers over `&dyn SessionStorage` so the two backends are exercised
//! by the same assertions without duplicated bodies.

mod common;

use atilla_agent::harness::session::{
    load_jsonl_session_metadata, InMemorySessionStorage, JsonlCreateOptions, JsonlSessionStorage,
    SessionStorage,
};
use atilla_agent::harness::types::{
    LabelEntry, MessageEntry, SessionErrorCode, SessionMetadata, SessionTreeEntry,
};
use common::{create_assistant_message, create_user_message, TempDir};

fn message_entry(id: &str, parent: Option<&str>, message: serde_json::Value) -> SessionTreeEntry {
    SessionTreeEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        message,
    })
}

fn assert_label_lifecycle(storage: &dyn SessionStorage) {
    storage
        .append_entry(message_entry("entry-1", None, create_user_message("one")))
        .unwrap();
    assert_eq!(storage.get_label("entry-1"), None);
    storage
        .append_entry(SessionTreeEntry::Label(LabelEntry {
            id: "label-1".into(),
            parent_id: Some("entry-1".into()),
            timestamp: "2026-01-01T00:00:01.000Z".into(),
            target_id: "entry-1".into(),
            label: Some("checkpoint".into()),
        }))
        .unwrap();
    assert_eq!(storage.get_label("entry-1").as_deref(), Some("checkpoint"));
    storage
        .append_entry(SessionTreeEntry::Label(LabelEntry {
            id: "label-2".into(),
            parent_id: Some("label-1".into()),
            timestamp: "2026-01-01T00:00:02.000Z".into(),
            target_id: "entry-1".into(),
            label: None,
        }))
        .unwrap();
    assert_eq!(storage.get_label("entry-1"), None);
}

fn assert_find_by_type(storage: &dyn SessionStorage) {
    storage
        .append_entry(message_entry("entry-1", None, create_user_message("one")))
        .unwrap();
    let found: Vec<String> = storage
        .find_entries("message")
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(found, vec!["entry-1".to_string()]);
    assert!(storage.find_entries("session_info").is_empty());
}

#[test]
fn in_memory_returns_configured_metadata() {
    let metadata = SessionMetadata::in_memory("session-1", "2026-01-01T00:00:00.000Z");
    let storage = InMemorySessionStorage::with_options(None, Some(metadata.clone()));
    assert_eq!(storage.get_metadata(), metadata);
}

#[test]
fn in_memory_copies_initial_entries_and_persists_leaf() {
    let entry = message_entry("entry-1", None, create_user_message("one"));
    let initial = vec![entry.clone()];
    let storage = InMemorySessionStorage::with_options(Some(initial.clone()), None);
    // Mutating our local copy after construction does not affect the storage.
    let mut mutated = initial;
    mutated.push(message_entry("entry-2", None, create_user_message("two")));
    assert_eq!(
        storage
            .get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec!["entry-1".to_string()]
    );
    assert_eq!(storage.get_leaf_id().unwrap().as_deref(), Some("entry-1"));
    storage.set_leaf_id(None).unwrap();
    assert_eq!(storage.get_leaf_id().unwrap(), None);
    let last = storage.get_entries().pop().unwrap();
    match last {
        SessionTreeEntry::Leaf(leaf) => assert_eq!(leaf.target_id, None),
        other => panic!("expected leaf, got {}", other.type_str()),
    }
}

#[test]
fn in_memory_rejects_invalid_leaf_ids() {
    let storage = InMemorySessionStorage::new();
    let error = storage.set_leaf_id(Some("missing")).unwrap_err();
    assert_eq!(error.code, SessionErrorCode::NotFound);
    assert_eq!(error.message, "Entry missing not found");
}

#[test]
fn in_memory_finds_entries_by_type() {
    assert_find_by_type(&InMemorySessionStorage::new());
}

#[test]
fn in_memory_maintains_label_lookup() {
    assert_label_lifecycle(&InMemorySessionStorage::new());
}

#[test]
fn in_memory_walks_paths_to_root() {
    let root = message_entry("root", None, create_user_message("root"));
    let child = message_entry("child", Some("root"), create_assistant_message("child"));
    let storage = InMemorySessionStorage::with_options(Some(vec![root, child]), None);
    let path: Vec<String> = storage
        .get_path_to_root(Some("child"))
        .unwrap()
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(path, vec!["root".to_string(), "child".to_string()]);
    assert!(storage.get_path_to_root(None).unwrap().is_empty());
}

#[test]
fn jsonl_throws_for_missing_files_when_opening() {
    let dir = TempDir::new();
    let error = common::expect_err(JsonlSessionStorage::open(&dir.child("session.jsonl")));
    assert_eq!(error.code, SessionErrorCode::NotFound);
}

fn create_jsonl(dir: &TempDir, cwd: &str) -> (String, JsonlSessionStorage) {
    let path = dir.child("session.jsonl");
    let storage = JsonlSessionStorage::create(
        &path,
        JsonlCreateOptions {
            cwd: cwd.to_string(),
            session_id: "session-1".to_string(),
            parent_session_path: None,
            metadata: None,
        },
    )
    .unwrap();
    (path, storage)
}

#[test]
fn jsonl_writes_the_header_on_create() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (path, storage) = create_jsonl(&dir, &cwd);
    assert!(std::path::Path::new(&path).exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.trim().split('\n').count(), 1);
    assert_eq!(storage.get_leaf_id().unwrap(), None);
    assert!(storage.get_entries().is_empty());
    storage
        .append_entry(message_entry("user-1", None, create_user_message("one")))
        .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.trim().split('\n').collect();
    let line0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let line1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(line0["type"], "session");
    assert_eq!(line1["id"], "user-1");
    assert_eq!(lines.len(), 2);
}

#[test]
fn jsonl_throws_for_malformed_headers() {
    let dir = TempDir::new();
    let path = dir.child("session.jsonl");
    std::fs::write(&path, "not json\n").unwrap();
    let error = common::expect_err(JsonlSessionStorage::open(&path));
    assert!(error
        .message
        .contains("first line is not a valid session header"));
}

#[test]
fn jsonl_throws_for_malformed_entry_lines() {
    let dir = TempDir::new();
    let path = dir.child("session.jsonl");
    let cwd = dir.path().to_string_lossy().into_owned();
    let header = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"session-1\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"{cwd}\"}}"
    );
    let entry = atilla_agent::harness::session::serialize_entry_line(&message_entry(
        "entry-1",
        None,
        create_user_message("one"),
    ));
    std::fs::write(&path, format!("{header}\nnot json\n{entry}")).unwrap();
    let error = common::expect_err(JsonlSessionStorage::open(&path));
    assert_eq!(error.code, SessionErrorCode::InvalidEntry);
}

#[test]
fn jsonl_creates_and_reads_metadata_from_header() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.child("session.jsonl");
    let storage = JsonlSessionStorage::create(
        &path,
        JsonlCreateOptions {
            cwd: cwd.clone(),
            session_id: "session-1".to_string(),
            parent_session_path: Some("/tmp/parent.jsonl".to_string()),
            metadata: None,
        },
    )
    .unwrap();
    let metadata = storage.get_metadata();
    assert_eq!(metadata.id, "session-1");
    assert_eq!(metadata.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(metadata.path.as_deref(), Some(path.as_str()));
    assert_eq!(
        metadata.parent_session_path.as_deref(),
        Some("/tmp/parent.jsonl")
    );
    storage
        .append_entry(message_entry("user-1", None, create_user_message("one")))
        .unwrap();
    assert_eq!(load_jsonl_session_metadata(&path).unwrap(), metadata);
}

#[test]
fn jsonl_round_trips_custom_header_metadata() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.child("session.jsonl");
    let mut meta = serde_json::Map::new();
    meta.insert("profile".into(), serde_json::json!("reviewer"));
    let storage = JsonlSessionStorage::create(
        &path,
        JsonlCreateOptions {
            cwd,
            session_id: "session-1".to_string(),
            parent_session_path: None,
            metadata: Some(meta.clone()),
        },
    )
    .unwrap();
    assert_eq!(storage.get_metadata().metadata, Some(meta.clone()));
    let loaded = JsonlSessionStorage::open(&path).unwrap();
    assert_eq!(loaded.get_metadata().metadata, Some(meta.clone()));
    assert_eq!(
        load_jsonl_session_metadata(&path).unwrap().metadata,
        Some(meta)
    );
}

#[test]
fn jsonl_omits_header_metadata_when_not_provided() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (path, _storage) = create_jsonl(&dir, &cwd);
    let content = std::fs::read_to_string(&path).unwrap();
    let header: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert!(header.get("metadata").is_none());
    assert_eq!(load_jsonl_session_metadata(&path).unwrap().metadata, None);
}

#[test]
fn jsonl_throws_for_non_object_header_metadata() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.child("session.jsonl");
    let header = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"session-1\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"{cwd}\",\"metadata\":\"profile\"}}"
    );
    std::fs::write(&path, format!("{header}\n")).unwrap();
    let error = common::expect_err(JsonlSessionStorage::open(&path));
    assert!(error
        .message
        .contains("session header metadata must be an object"));
}

#[test]
fn jsonl_loads_existing_entries_and_reconstructs_leaf() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (path, storage) = create_jsonl(&dir, &cwd);
    storage
        .append_entry(message_entry("root", None, create_user_message("root")))
        .unwrap();
    storage
        .append_entry(message_entry(
            "child",
            Some("root"),
            create_assistant_message("child"),
        ))
        .unwrap();
    let loaded = JsonlSessionStorage::open(&path).unwrap();
    assert_eq!(loaded.get_leaf_id().unwrap().as_deref(), Some("child"));
    assert_eq!(
        loaded
            .get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec!["root".to_string(), "child".to_string()]
    );
    loaded.set_leaf_id(Some("root")).unwrap();
    let reloaded = JsonlSessionStorage::open(&path).unwrap();
    assert_eq!(reloaded.get_leaf_id().unwrap().as_deref(), Some("root"));
    match reloaded.get_entries().pop().unwrap() {
        SessionTreeEntry::Leaf(leaf) => assert_eq!(leaf.target_id.as_deref(), Some("root")),
        other => panic!("expected leaf, got {}", other.type_str()),
    }
    let path_ids: Vec<String> = loaded
        .get_path_to_root(Some("child"))
        .unwrap()
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(path_ids, vec!["root".to_string(), "child".to_string()]);
}

#[test]
fn jsonl_finds_entries_by_type() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (_path, storage) = create_jsonl(&dir, &cwd);
    assert_find_by_type(&storage);
}

#[test]
fn jsonl_maintains_label_lookup() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (path, storage) = create_jsonl(&dir, &cwd);
    assert_label_lifecycle(&storage);
    let loaded = JsonlSessionStorage::open(&path).unwrap();
    assert_eq!(loaded.get_label("entry-1"), None);
}

#[test]
fn jsonl_reads_metadata_through_first_line_only() {
    // Mirrors the "readTextFile is not called" test: load_jsonl_session_metadata
    // reads only the first line, so a file whose body is unparseable still
    // yields header metadata.
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.child("session.jsonl");
    let header = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"session-1\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"{cwd}\"}}"
    );
    std::fs::write(&path, format!("{header}\nnot json at all\n")).unwrap();
    let metadata = load_jsonl_session_metadata(&path).unwrap();
    assert_eq!(metadata.id, "session-1");
    assert_eq!(metadata.created_at, "2026-01-01T00:00:00.000Z");
    assert_eq!(metadata.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(metadata.parent_session_path, None);
}
