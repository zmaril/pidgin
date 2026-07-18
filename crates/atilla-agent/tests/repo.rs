//! Ports `test/harness/repo.test.ts` for both the in-memory and JSONL repos.

mod common;

use atilla_agent::harness::session::{
    ForkOptions, InMemorySessionRepo, JsonlCreate, JsonlSessionRepo,
};
use common::{create_assistant_message, create_user_message, TempDir};

#[test]
fn in_memory_opens_deletes_and_forks_by_metadata() {
    let repo = InMemorySessionRepo::new();
    let session = repo.create(Some("session-1"));
    let metadata = session.get_metadata();
    let user1 = session.append_message(create_user_message("one")).unwrap();
    let assistant1 = session
        .append_message(create_assistant_message("two"))
        .unwrap();
    let user2 = session
        .append_message(create_user_message("three"))
        .unwrap();

    assert_eq!(repo.open(&metadata).unwrap().get_metadata().id, "session-1");
    assert_eq!(
        repo.list().iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        vec!["session-1".to_string()]
    );

    let fork = repo
        .fork(
            &metadata,
            ForkOptions {
                entry_id: Some(user2.clone()),
                position: None,
                id: Some("session-2".into()),
            },
        )
        .unwrap();
    assert_eq!(
        fork.get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec![user1.clone(), assistant1.clone()]
    );

    let full_fork = repo
        .fork(
            &metadata,
            ForkOptions {
                entry_id: None,
                position: None,
                id: Some("session-3".into()),
            },
        )
        .unwrap();
    assert_eq!(
        full_fork
            .get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec![user1, assistant1, user2]
    );

    repo.delete(&metadata);
    let error = common::expect_err(repo.open(&metadata));
    assert_eq!(error.message, "Session not found: session-1");
}

#[test]
fn jsonl_stores_sessions_below_encoded_cwd_and_lists_by_cwd() {
    let root = TempDir::new();
    let repo = JsonlSessionRepo::new(root.path());
    let cwd = "/tmp/my-project";
    let other_cwd = "/tmp/other-project";
    let session = repo
        .create(JsonlCreate {
            cwd: cwd.into(),
            id: Some("019de8c2-de29-73e9-ae0c-e134db34c447".into()),
            ..Default::default()
        })
        .unwrap();
    let other = repo
        .create(JsonlCreate {
            cwd: other_cwd.into(),
            id: Some("other-session".into()),
            ..Default::default()
        })
        .unwrap();
    let metadata = session.get_metadata();
    let other_metadata = other.get_metadata();

    assert!(metadata
        .path
        .as_deref()
        .unwrap()
        .contains("--tmp-my-project--"));
    assert!(other_metadata
        .path
        .as_deref()
        .unwrap()
        .contains("--tmp-other-project--"));
    assert!(std::path::Path::new(metadata.path.as_deref().unwrap()).exists());

    assert_eq!(
        repo.list(Some(cwd))
            .unwrap()
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        vec![metadata.id.clone()]
    );
    let mut all: Vec<String> = repo
        .list(None)
        .unwrap()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    all.sort();
    let mut expected = vec![metadata.id.clone(), other_metadata.id.clone()];
    expected.sort();
    assert_eq!(all, expected);
}

#[test]
fn jsonl_opens_deletes_and_forks_by_metadata() {
    let root = TempDir::new();
    let repo = JsonlSessionRepo::new(root.path());
    let source = repo
        .create(JsonlCreate {
            cwd: "/tmp/source".into(),
            id: Some("source-session".into()),
            ..Default::default()
        })
        .unwrap();
    let source_metadata = source.get_metadata();
    let user1 = source.append_message(create_user_message("one")).unwrap();
    let assistant1 = source
        .append_message(create_assistant_message("two"))
        .unwrap();
    let user2 = source.append_message(create_user_message("three")).unwrap();

    assert_eq!(
        repo.open(&source_metadata).unwrap().get_metadata(),
        source_metadata
    );

    let fork = repo
        .fork(
            &source_metadata,
            JsonlCreate {
                cwd: "/tmp/target".into(),
                id: Some("fork-session".into()),
                ..Default::default()
            },
            ForkOptions {
                entry_id: Some(user2.clone()),
                position: None,
                id: None,
            },
        )
        .unwrap();
    let fork_metadata = fork.get_metadata();
    assert_eq!(fork_metadata.cwd.as_deref(), Some("/tmp/target"));
    assert_eq!(fork_metadata.parent_session_path, source_metadata.path);
    assert_eq!(
        fork.get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec![user1.clone(), assistant1.clone()]
    );

    let full_fork = repo
        .fork(
            &source_metadata,
            JsonlCreate {
                cwd: "/tmp/target".into(),
                id: Some("full-fork-session".into()),
                ..Default::default()
            },
            ForkOptions::default(),
        )
        .unwrap();
    assert_eq!(
        full_fork
            .get_entries()
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec![user1, assistant1, user2]
    );

    repo.delete(&source_metadata).unwrap();
    assert!(!std::path::Path::new(source_metadata.path.as_deref().unwrap()).exists());
    let error = common::expect_err(repo.open(&source_metadata));
    assert!(error.message.contains("Session not found"));
}

#[test]
fn jsonl_persists_header_metadata_through_create_list_and_fork() {
    let root = TempDir::new();
    let repo = JsonlSessionRepo::new(root.path());
    let mut meta = serde_json::Map::new();
    meta.insert("profile".into(), serde_json::json!("reviewer"));
    let source = repo
        .create(JsonlCreate {
            cwd: "/tmp/source".into(),
            id: Some("source-session".into()),
            parent_session_path: None,
            metadata: Some(meta.clone()),
        })
        .unwrap();
    let source_metadata = source.get_metadata();
    assert_eq!(source_metadata.metadata, Some(meta.clone()));

    let listed: Vec<_> = repo
        .list(Some("/tmp/source"))
        .unwrap()
        .into_iter()
        .map(|m| m.metadata)
        .collect();
    assert_eq!(listed, vec![Some(meta.clone())]);

    let fork = repo
        .fork(
            &source_metadata,
            JsonlCreate {
                cwd: "/tmp/target".into(),
                id: Some("fork-session".into()),
                ..Default::default()
            },
            ForkOptions::default(),
        )
        .unwrap();
    assert_eq!(fork.get_metadata().metadata, Some(meta));

    let mut writer_meta = serde_json::Map::new();
    writer_meta.insert("profile".into(), serde_json::json!("writer"));
    let overridden = repo
        .fork(
            &source_metadata,
            JsonlCreate {
                cwd: "/tmp/target".into(),
                id: Some("overridden-session".into()),
                parent_session_path: None,
                metadata: Some(writer_meta.clone()),
            },
            ForkOptions::default(),
        )
        .unwrap();
    assert_eq!(overridden.get_metadata().metadata, Some(writer_meta));
}
