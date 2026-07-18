//! Ports `test/harness/session.test.ts`. The `runSessionSuite` structure is
//! reproduced by [`run_suite`], which takes a storage factory and runs every
//! behavior against it; it is invoked once for in-memory storage and once for
//! JSONL storage, exactly as pi runs the shared suite twice.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use serde_json::{json, Value};

use atilla_agent::harness::session::{
    ContextEntryTransform, CustomEntryProjector, InMemorySessionStorage, JsonlCreateOptions,
    JsonlSessionStorage, MoveSummary, Session, SessionContextBuildOptions, SessionStorage,
};
use atilla_agent::harness::types::SessionTreeEntry;
use common::{create_assistant_message, create_user_message, TempDir};

fn roles(context_messages: &[Value]) -> Vec<String> {
    context_messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("").to_string())
        .collect()
}

fn text_data(data: Option<&Value>) -> String {
    data.and_then(|d| d.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn run_suite(make: impl Fn() -> Rc<dyn SessionStorage>) {
    // appends messages and builds context in order
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session
            .append_message(create_assistant_message("two"))
            .unwrap();
        assert_eq!(
            roles(&session.build_context().unwrap().messages),
            vec!["user", "assistant"]
        );
    }

    // tracks model and thinking level changes
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session.append_model_change("openai", "gpt-4.1").unwrap();
        session.append_thinking_level_change("high").unwrap();
        let context = session.build_context().unwrap();
        assert_eq!(context.thinking_level, "high");
        let model = context.model.unwrap();
        assert_eq!(model.provider, "openai");
        assert_eq!(model.model_id, "gpt-4.1");
    }

    // supports branching by moving the leaf and appending a new branch
    {
        let session = Session::new(make());
        let user1 = session.append_message(create_user_message("one")).unwrap();
        let assistant1 = session
            .append_message(create_assistant_message("two"))
            .unwrap();
        session
            .append_message(create_user_message("three"))
            .unwrap();
        session.move_to(Some(&user1), None).unwrap();
        session
            .append_message(create_assistant_message("branched"))
            .unwrap();
        let branch: Vec<String> = session
            .get_branch(None)
            .unwrap()
            .iter()
            .map(|e| e.id().to_string())
            .collect();
        assert!(branch.contains(&user1));
        assert!(!branch.contains(&assistant1));
        assert_eq!(
            roles(&session.build_context().unwrap().messages),
            vec!["user", "assistant"]
        );
    }

    // supports moving the leaf to root
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session.move_to(None, None).unwrap();
        assert_eq!(session.get_leaf_id().unwrap(), None);
        assert!(session.build_context().unwrap().messages.is_empty());
    }

    // reconstructs compaction summaries in context
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session
            .append_message(create_assistant_message("two"))
            .unwrap();
        let user2 = session
            .append_message(create_user_message("three"))
            .unwrap();
        session
            .append_message(create_assistant_message("four"))
            .unwrap();
        session
            .append_compaction("summary", &user2, 1234, None, None)
            .unwrap();
        session.append_message(create_user_message("five")).unwrap();
        let context = session.build_context().unwrap();
        assert_eq!(context.messages[0]["role"], "compactionSummary");
        assert_eq!(context.messages.len(), 4);
    }

    // supports moving with branch summary entries in context
    {
        let session = Session::new(make());
        let user1 = session.append_message(create_user_message("one")).unwrap();
        let summary_id = session
            .move_to(
                Some(&user1),
                Some(MoveSummary {
                    summary: "summary text".into(),
                    details: None,
                    from_hook: None,
                }),
            )
            .unwrap()
            .expect("summary id");
        match session.get_entry(&summary_id).unwrap() {
            SessionTreeEntry::BranchSummary(e) => {
                assert_eq!(e.parent_id.as_deref(), Some(user1.as_str()));
                assert_eq!(e.from_id, user1);
            }
            other => panic!("expected branch_summary, got {}", other.type_str()),
        }
        let context = session.build_context().unwrap();
        assert_eq!(context.messages[1]["role"], "branchSummary");
    }

    // supports custom message entries in context
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session
            .append_custom_message_entry("custom", json!("hello"), true, Some(json!({"ok": true})))
            .unwrap();
        let context = session.build_context().unwrap();
        assert_eq!(context.messages[1]["role"], "custom");
    }

    // keeps custom entries in context entries but omits them from messages by default
    {
        let session = Session::new(make());
        session.append_message(create_user_message("one")).unwrap();
        session
            .append_custom_entry("chat_message", Some(json!({"text": "hello"})))
            .unwrap();
        let entry_types: Vec<&str> = session
            .build_context_entries()
            .unwrap()
            .iter()
            .map(|e| e.type_str())
            .collect();
        assert_eq!(entry_types, vec!["message", "custom"]);
        assert_eq!(session.build_context().unwrap().messages.len(), 1);
    }

    // projects custom entries with configured custom-entry projectors
    {
        let projector: CustomEntryProjector = Box::new(|entry, _index, _entries| {
            vec![create_user_message(&format!(
                "chat: {}",
                text_data(entry.data.as_ref())
            ))]
        });
        let mut projectors = std::collections::HashMap::new();
        projectors.insert("chat_message".to_string(), projector);
        let options = SessionContextBuildOptions {
            entry_transforms: Vec::new(),
            entry_projectors: projectors,
        };
        let session = Session::with_options(make(), options);
        session.append_message(create_user_message("one")).unwrap();
        session
            .append_custom_entry("chat_message", Some(json!({"text": "hello"})))
            .unwrap();
        let context = session.build_context().unwrap();
        assert_eq!(roles(&context.messages), vec!["user", "user"]);
        assert_eq!(
            context.messages[1]["content"],
            json!([{"type": "text", "text": "chat: hello"}])
        );
    }

    // applies context entry transforms after default compaction selection
    {
        let observed: Rc<Cell<Option<&'static str>>> = Rc::new(Cell::new(None));
        let observed_clone = observed.clone();
        let drop_compaction: ContextEntryTransform = Box::new(move |entries| {
            observed_clone.set(entries.first().map(|e| e.type_str()));
            entries
                .into_iter()
                .filter(|e| e.type_str() != "compaction")
                .collect()
        });
        let options = SessionContextBuildOptions {
            entry_transforms: vec![drop_compaction],
            entry_projectors: std::collections::HashMap::new(),
        };
        let session = Session::with_options(make(), options);
        session.append_message(create_user_message("one")).unwrap();
        let kept = session.append_message(create_user_message("two")).unwrap();
        session
            .append_compaction("summary", &kept, 1234, None, None)
            .unwrap();
        session
            .append_message(create_user_message("three"))
            .unwrap();
        let context = session.build_context().unwrap();
        assert_eq!(observed.get(), Some("compaction"));
        assert_eq!(roles(&context.messages), vec!["user", "user"]);
    }

    // normalizes session names
    {
        let session = Session::new(make());
        session
            .append_session_name(" hello\nworld\r\nagain ")
            .unwrap();
        assert_eq!(
            session.get_session_name().as_deref(),
            Some("hello world again")
        );
    }

    // supports labels and session info entries without affecting context
    {
        let session = Session::new(make());
        let user1 = session.append_message(create_user_message("one")).unwrap();
        session.append_label(&user1, Some("checkpoint")).unwrap();
        session.append_session_name("name").unwrap();
        let entries = session.get_entries();
        assert!(entries.iter().any(|e| e.type_str() == "label"));
        assert!(entries.iter().any(|e| e.type_str() == "session_info"));
        assert_eq!(session.get_label(&user1).as_deref(), Some("checkpoint"));
        assert_eq!(session.get_session_name().as_deref(), Some("name"));
        assert_eq!(session.build_context().unwrap().messages.len(), 1);
    }

    // rejects labels for missing entries
    {
        let session = Session::new(make());
        let error = session
            .append_label("missing", Some("checkpoint"))
            .unwrap_err();
        assert_eq!(error.message, "Entry missing not found");
    }
}

#[test]
fn session_suite_in_memory() {
    run_suite(|| Rc::new(InMemorySessionStorage::new()));
}

#[test]
fn session_suite_jsonl() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let counter = Cell::new(0u64);
    run_suite(|| {
        let n = counter.get();
        counter.set(n + 1);
        let path = dir.child(&format!("session-{n}.jsonl"));
        let storage = JsonlSessionStorage::create(
            &path,
            JsonlCreateOptions {
                cwd: cwd.clone(),
                session_id: "session-1".to_string(),
                parent_session_path: None,
                metadata: None,
            },
        )
        .unwrap();
        Rc::new(storage)
    });
}

#[test]
fn jsonl_persists_leaf_changes_and_inspects_file() {
    let dir = TempDir::new();
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = dir.child("session.jsonl");
    let storage: Rc<dyn SessionStorage> = Rc::new(
        JsonlSessionStorage::create(
            &path,
            JsonlCreateOptions {
                cwd,
                session_id: "session-1".to_string(),
                parent_session_path: None,
                metadata: None,
            },
        )
        .unwrap(),
    );

    let session = Session::new(storage.clone());
    let user1 = session.append_message(create_user_message("one")).unwrap();
    session
        .append_message(create_assistant_message("two"))
        .unwrap();
    session.append_label(&user1, Some("checkpoint")).unwrap();
    session.append_session_name("name").unwrap();
    session.move_to(Some(&user1), None).unwrap();
    session
        .append_message(create_assistant_message("branched"))
        .unwrap();

    let session2 = Session::new(storage);
    assert_eq!(
        roles(&session2.build_context().unwrap().messages),
        vec!["user", "assistant"]
    );
    assert_eq!(session2.get_label(&user1).as_deref(), Some("checkpoint"));
    assert_eq!(session2.get_session_name().as_deref(), Some("name"));

    // inspect hook: header + a leaf line, no "entry"-typed lines, string ids.
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.trim().split('\n').collect();
    assert!(lines.len() > 1);
    let header: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);
    let entries: Vec<Value> = lines[1..]
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(entries.iter().any(|e| e["type"] == "leaf"));
    for entry in &entries {
        assert_ne!(entry["type"], "entry");
        assert!(entry["id"].is_string());
    }
}
