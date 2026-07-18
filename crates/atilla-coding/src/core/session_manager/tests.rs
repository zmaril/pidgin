//! Unit tests for the coding-agent `SessionManager`, translated from pi's
//! `packages/coding-agent/test/session-manager/*` suites.

use super::*;

const TS: &str = "2025-01-01T00:00:00Z";

// --- fixtures -----------------------------------------------------------

fn user_message(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": 1 })
}

fn assistant_message(text: &str, model: &str) -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": model,
        "usage": {
            "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 2
    })
}

fn base_custom_entry() -> CustomEntry {
    CustomEntry {
        custom_type: "t".to_string(),
        data: None,
        id: "id".to_string(),
        parent_id: None,
        timestamp: TS.to_string(),
    }
}

fn base_custom_message_entry() -> CustomMessageEntry {
    CustomMessageEntry {
        custom_type: "t".to_string(),
        content: Value::Null,
        display: false,
        details: None,
        id: "id".to_string(),
        parent_id: None,
        timestamp: TS.to_string(),
    }
}

/// A `message` entry with an explicit id/parent, for the free-function
/// context tests (which build trees directly rather than via appends).
fn message_entry(id: &str, parent: Option<&str>, message: Value) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
        message,
    })
}

fn user_entry(id: &str, parent: Option<&str>, text: &str) -> SessionEntry {
    message_entry(id, parent, user_message(text))
}

fn assistant_entry(id: &str, parent: Option<&str>, text: &str) -> SessionEntry {
    message_entry(id, parent, assistant_message(text, "claude-test"))
}

fn compaction_entry(
    id: &str,
    parent: Option<&str>,
    summary: &str,
    first_kept: &str,
) -> SessionEntry {
    SessionEntry::Compaction(CompactionEntry {
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
        summary: summary.to_string(),
        first_kept_entry_id: first_kept.to_string(),
        tokens_before: 1000,
        details: None,
        from_hook: None,
    })
}

fn branch_summary_entry(id: &str, parent: Option<&str>, summary: &str, from: &str) -> SessionEntry {
    SessionEntry::BranchSummary(BranchSummaryEntry {
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
        from_id: from.to_string(),
        summary: summary.to_string(),
        details: None,
        from_hook: None,
    })
}

fn custom_tree_entry(id: &str, parent: Option<&str>, custom_type: &str) -> SessionEntry {
    SessionEntry::Custom(CustomEntry {
        custom_type: custom_type.to_string(),
        data: Some(json!({ "hidden": true })),
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
    })
}

fn thinking_entry(id: &str, parent: Option<&str>, level: &str) -> SessionEntry {
    SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
        thinking_level: level.to_string(),
    })
}

fn model_entry(id: &str, parent: Option<&str>, provider: &str, model_id: &str) -> SessionEntry {
    SessionEntry::ModelChange(ModelChangeEntry {
        id: id.to_string(),
        parent_id: parent.map(String::from),
        timestamp: TS.to_string(),
        provider: provider.to_string(),
        model_id: model_id.to_string(),
    })
}

fn role(message: &Value) -> &str {
    message.get("role").and_then(Value::as_str).unwrap_or("")
}

fn roles(messages: &[Value]) -> Vec<&str> {
    messages.iter().map(role).collect()
}

// --- assert_valid_session_id -------------------------------------------

#[test]
fn assert_valid_session_id_accepts_and_rejects() {
    let valid = ["a", "1", "ab", "a-b", "a.b", "a_b", "A1.b-c_d", "abc12345"];
    for id in valid {
        assert!(assert_valid_session_id(id).is_ok(), "expected {id:?} valid");
    }
    let invalid = ["", "-bad", "bad ", "bad id", "-", "a b", ".x", "x.", "_x"];
    for id in invalid {
        let err = assert_valid_session_id(id).unwrap_err();
        assert!(
            err.starts_with("Session id must be non-empty"),
            "for {id:?}"
        );
    }
}

// --- migration ----------------------------------------------------------

#[test]
fn migration_adds_id_and_parent_to_v1_entries() {
    let mut entries = vec![
        json!({ "type": "session", "id": "sess-1", "timestamp": TS, "cwd": "/tmp" }),
        json!({ "type": "message", "timestamp": TS, "message": user_message("hi") }),
        json!({ "type": "message", "timestamp": TS, "message": assistant_message("hello", "test") }),
    ];

    migrate_session_entries(&mut entries);

    assert_eq!(entries[0]["version"], json!(3));
    let id1 = entries[1]["id"].as_str().unwrap();
    assert_eq!(id1.len(), 8);
    assert_eq!(entries[1]["parentId"], Value::Null);
    let id2 = entries[2]["id"].as_str().unwrap();
    assert_eq!(id2.len(), 8);
    assert_eq!(entries[2]["parentId"], json!(id1));
}

#[test]
fn migration_is_idempotent_for_migrated_entries() {
    let mut entries = vec![
        json!({ "type": "session", "id": "sess-1", "version": 2, "timestamp": TS, "cwd": "/tmp" }),
        json!({ "type": "message", "id": "abc12345", "parentId": null, "timestamp": TS,
                "message": user_message("hi") }),
        json!({ "type": "message", "id": "def67890", "parentId": "abc12345", "timestamp": TS,
                "message": assistant_message("hello", "test") }),
    ];

    migrate_session_entries(&mut entries);

    assert_eq!(entries[1]["id"], json!("abc12345"));
    assert_eq!(entries[2]["id"], json!("def67890"));
    assert_eq!(entries[2]["parentId"], json!("abc12345"));
}

#[test]
fn migration_v1_compaction_index_becomes_id() {
    let mut entries = vec![
        json!({ "type": "session", "id": "s", "timestamp": TS, "cwd": "/tmp" }),
        json!({ "type": "message", "timestamp": TS, "message": user_message("first") }),
        json!({ "type": "compaction", "timestamp": TS, "summary": "s",
                "firstKeptEntryIndex": 1, "tokensBefore": 10 }),
    ];

    migrate_session_entries(&mut entries);

    let first_id = entries[1]["id"].as_str().unwrap();
    assert_eq!(entries[2]["firstKeptEntryId"], json!(first_id));
    assert!(entries[2].get("firstKeptEntryIndex").is_none());
}

// --- parse / compaction / projection unit cases -------------------------

#[test]
fn parse_session_entries_skips_blank_and_malformed_lines() {
    let content = "{\"type\":\"session\",\"id\":\"s\"}\n\nnot json\n{\"type\":\"message\"}\n";
    let entries = parse_session_entries(content);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], json!("s"));
    assert_eq!(entries[1]["type"], json!("message"));
}

#[test]
fn get_latest_compaction_entry_returns_last() {
    let entries = vec![
        compaction_entry("1", None, "first", "0"),
        user_entry("2", Some("1"), "hi"),
        compaction_entry("3", Some("2"), "second", "1"),
    ];
    let latest = get_latest_compaction_entry(&entries).unwrap();
    assert_eq!(latest.summary, "second");
    assert!(get_latest_compaction_entry(&[]).is_none());
}

#[test]
fn session_entry_projection_covers_each_variant() {
    assert!(session_entry_to_context_messages(&custom_tree_entry("1", None, "state")).is_empty());
    assert!(session_entry_to_context_messages(&thinking_entry("1", None, "high")).is_empty());

    let custom_message = SessionEntry::CustomMessage(CustomMessageEntry {
        content: json!("hi"),
        display: true,
        ..base_custom_message_entry()
    });
    let projected = session_entry_to_context_messages(&custom_message);
    assert_eq!(role(&projected[0]), "custom");

    let compaction = session_entry_to_context_messages(&compaction_entry("1", None, "sum", "0"));
    assert_eq!(role(&compaction[0]), "compactionSummary");

    let branch = session_entry_to_context_messages(&branch_summary_entry("1", None, "sum", "root"));
    assert_eq!(role(&branch[0]), "branchSummary");
}

#[test]
fn session_entry_projection_normalizes_null_message_content() {
    let entry = message_entry("1", None, json!({ "role": "assistant", "content": null }));
    let messages = session_entry_to_context_messages(&entry);
    assert_eq!(messages[0]["content"], json!([]));

    let missing = message_entry("2", None, json!({ "role": "user" }));
    assert_eq!(
        session_entry_to_context_messages(&missing)[0]["content"],
        json!([])
    );
}

// --- build context (free functions) -------------------------------------

#[test]
fn build_context_trivial_cases() {
    let empty = build_session_context(&[], None);
    assert!(empty.messages.is_empty());
    assert_eq!(empty.thinking_level, "off");
    assert!(empty.model.is_none());

    let single = build_session_context(&[user_entry("1", None, "hello")], None);
    assert_eq!(single.messages.len(), 1);
    assert_eq!(role(&single.messages[0]), "user");

    let convo = vec![
        user_entry("1", None, "hello"),
        assistant_entry("2", Some("1"), "hi"),
        user_entry("3", Some("2"), "how"),
        assistant_entry("4", Some("3"), "great"),
    ];
    let ctx = build_session_context(&convo, None);
    assert_eq!(
        roles(&ctx.messages),
        ["user", "assistant", "user", "assistant"]
    );
}

#[test]
fn build_context_tracks_settings() {
    let with_thinking = vec![
        user_entry("1", None, "hello"),
        thinking_entry("2", Some("1"), "high"),
        assistant_entry("3", Some("2"), "thinking"),
    ];
    let ctx = build_session_context(&with_thinking, None);
    assert_eq!(ctx.thinking_level, "high");

    let from_assistant = vec![
        user_entry("1", None, "hi"),
        assistant_entry("2", Some("1"), "hey"),
    ];
    let model = build_session_context(&from_assistant, None).model.unwrap();
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.model_id, "claude-test");

    // A later assistant message overrides an earlier model_change.
    let overridden = vec![
        user_entry("1", None, "hi"),
        model_entry("2", Some("1"), "openai", "gpt-4"),
        assistant_entry("3", Some("2"), "hey"),
    ];
    let model = build_session_context(&overridden, None).model.unwrap();
    assert_eq!(model.model_id, "claude-test");
}

#[test]
fn build_context_handles_compaction() {
    let entries = vec![
        user_entry("1", None, "first"),
        assistant_entry("2", Some("1"), "response1"),
        user_entry("3", Some("2"), "second"),
        assistant_entry("4", Some("3"), "response2"),
        compaction_entry("5", Some("4"), "Summary of first two turns", "3"),
        user_entry("6", Some("5"), "third"),
        assistant_entry("7", Some("6"), "response3"),
    ];
    let ctx = build_session_context(&entries, None);
    assert_eq!(ctx.messages.len(), 5);
    assert_eq!(role(&ctx.messages[0]), "compactionSummary");
    assert_eq!(ctx.messages[1]["content"], json!("second"));
    assert_eq!(ctx.messages[3]["content"], json!("third"));
}

#[test]
fn build_context_uses_latest_compaction() {
    let entries = vec![
        user_entry("1", None, "a"),
        assistant_entry("2", Some("1"), "b"),
        compaction_entry("3", Some("2"), "First summary", "1"),
        user_entry("4", Some("3"), "c"),
        assistant_entry("5", Some("4"), "d"),
        compaction_entry("6", Some("5"), "Second summary", "4"),
        user_entry("7", Some("6"), "e"),
    ];
    let ctx = build_session_context(&entries, None);
    assert_eq!(ctx.messages.len(), 4);
    assert_eq!(ctx.messages[0]["summary"], json!("Second summary"));
}

#[test]
fn build_context_entries_includes_custom_but_not_in_messages() {
    let entries = vec![
        user_entry("1", None, "first"),
        custom_tree_entry("2", Some("1"), "old-state"),
        assistant_entry("3", Some("2"), "response1"),
        custom_tree_entry("4", Some("3"), "kept-card"),
        user_entry("5", Some("4"), "second"),
        compaction_entry("6", Some("5"), "Summary", "4"),
        custom_tree_entry("7", Some("6"), "after-card"),
        assistant_entry("8", Some("7"), "response2"),
    ];
    let selected: Vec<String> = build_context_entries(&entries, None)
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(selected, ["6", "4", "5", "7", "8"]);
    let ctx = build_session_context(&entries, None);
    assert_eq!(
        roles(&ctx.messages),
        ["compactionSummary", "user", "assistant"]
    );
}

#[test]
fn build_context_follows_branches() {
    let entries = vec![
        user_entry("1", None, "start"),
        assistant_entry("2", Some("1"), "response"),
        user_entry("3", Some("2"), "branch A"),
        user_entry("4", Some("2"), "branch B"),
    ];
    let ctx_a = build_session_context(&entries, Some("3"));
    assert_eq!(ctx_a.messages[2]["content"], json!("branch A"));
    let ctx_b = build_session_context(&entries, Some("4"));
    assert_eq!(ctx_b.messages[2]["content"], json!("branch B"));
}

#[test]
fn build_context_includes_branch_summary() {
    let entries = vec![
        user_entry("1", None, "start"),
        assistant_entry("2", Some("1"), "response"),
        user_entry("3", Some("2"), "abandoned path"),
        branch_summary_entry("4", Some("2"), "Summary of abandoned work", "3"),
        user_entry("5", Some("4"), "new direction"),
    ];
    let ctx = build_session_context(&entries, Some("5"));
    assert_eq!(ctx.messages.len(), 4);
    assert_eq!(role(&ctx.messages[2]), "branchSummary");
    assert_eq!(ctx.messages[3]["content"], json!("new direction"));
}

#[test]
fn build_context_edge_cases() {
    let entries = vec![
        user_entry("1", None, "hello"),
        assistant_entry("2", Some("1"), "hi"),
    ];
    assert_eq!(
        build_session_context(&entries, Some("nonexistent"))
            .messages
            .len(),
        2
    );

    let orphan = vec![
        user_entry("1", None, "hello"),
        assistant_entry("2", Some("missing"), "orphan"),
    ];
    assert_eq!(build_session_context(&orphan, Some("2")).messages.len(), 1);
}

// --- append + tree traversal (SessionManager) ---------------------------

#[test]
fn append_builds_parent_chain_and_advances_leaf() {
    let mut session = SessionManager::in_memory("/w");
    assert!(session.get_leaf_id().is_none());

    let id1 = session.append_message(user_message("first"));
    assert_eq!(session.get_leaf_id(), Some(id1.as_str()));
    let id2 = session.append_message(assistant_message("second", "test"));
    let id3 = session.append_thinking_level_change("high");
    assert_eq!(session.get_leaf_id(), Some(id3.as_str()));

    let entries = session.get_entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].parent_id(), None);
    assert_eq!(entries[1].parent_id(), Some(id1.as_str()));
    assert_eq!(entries[2].parent_id(), Some(id2.as_str()));
}

#[test]
fn get_branch_returns_root_to_leaf() {
    let mut session = SessionManager::in_memory("/w");
    assert!(session.get_branch(None).is_empty());

    let id1 = session.append_message(user_message("1"));
    let id2 = session.append_message(assistant_message("2", "test"));
    let id3 = session.append_message(user_message("3"));

    let full: Vec<String> = session
        .get_branch(None)
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(full, [id1.clone(), id2.clone(), id3]);

    let partial: Vec<String> = session
        .get_branch(Some(&id2))
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(partial, [id1, id2]);
}

#[test]
fn get_tree_builds_branches() {
    let mut session = SessionManager::in_memory("/w");
    let id1 = session.append_message(user_message("1"));
    let id2 = session.append_message(assistant_message("2", "test"));
    let id3 = session.append_message(user_message("3"));

    session.branch(&id2).unwrap();
    let id4 = session.append_message(user_message("4-branch"));

    let tree = session.get_tree();
    assert_eq!(tree.len(), 1);
    let root = &tree[0];
    assert_eq!(root.entry.id(), id1);
    let node2 = &root.children[0];
    assert_eq!(node2.entry.id(), id2);
    let mut child_ids: Vec<&str> = node2.children.iter().map(|c| c.entry.id()).collect();
    child_ids.sort_unstable();
    let mut expected = [id3.as_str(), id4.as_str()];
    expected.sort_unstable();
    assert_eq!(child_ids, expected);
}

#[test]
fn branch_moves_leaf_and_errors_on_unknown() {
    let mut session = SessionManager::in_memory("/w");
    let id1 = session.append_message(user_message("1"));
    session.append_message(assistant_message("2", "test"));

    session.branch(&id1).unwrap();
    assert_eq!(session.get_leaf_id(), Some(id1.as_str()));
    let id3 = session.append_message(user_message("branched"));
    assert_eq!(
        session.get_entry(&id3).unwrap().parent_id(),
        Some(id1.as_str())
    );

    let err = session.branch("nonexistent").unwrap_err();
    assert_eq!(err.to_string(), "Entry nonexistent not found");
}

#[test]
fn branch_with_summary_inserts_summary() {
    let mut session = SessionManager::in_memory("/w");
    let id1 = session.append_message(user_message("1"));
    session.append_message(assistant_message("2", "test"));
    session.append_message(user_message("3"));

    let summary_id = session
        .branch_with_summary(Some(&id1), "Summary of abandoned work", None, None)
        .unwrap();
    assert_eq!(session.get_leaf_id(), Some(summary_id.as_str()));
    let entry = session.get_entry(&summary_id).unwrap();
    assert_eq!(entry.parent_id(), Some(id1.as_str()));
    assert_eq!(entry.type_str(), "branch_summary");

    let err = session
        .branch_with_summary(Some("nope"), "s", None, None)
        .unwrap_err();
    assert_eq!(err.to_string(), "Entry nope not found");
}

#[test]
fn leaf_and_entry_lookups() {
    let mut session = SessionManager::in_memory("/w");
    assert!(session.get_leaf_entry().is_none());
    assert!(session.get_entry("nope").is_none());

    session.append_message(user_message("first"));
    let id2 = session.append_message(assistant_message("second", "test"));
    assert_eq!(session.get_leaf_entry().unwrap().id(), id2);
}

#[test]
fn build_session_context_follows_current_branch() {
    let mut session = SessionManager::in_memory("/w");
    session.append_message(user_message("msg1"));
    let id2 = session.append_message(assistant_message("msg2", "test"));
    session.append_message(user_message("msg3"));

    session.branch(&id2).unwrap();
    session.append_message(assistant_message("msg4-branch", "test"));

    let ctx = session.build_session_context();
    assert_eq!(ctx.messages.len(), 3);
    assert_eq!(ctx.messages[0]["content"], json!("msg1"));
}

// --- save custom entry --------------------------------------------------

#[test]
fn custom_entry_is_in_tree_but_not_in_messages() {
    let mut session = SessionManager::in_memory("/w");
    let msg_id = session.append_message(user_message("hello"));
    let custom_id = session.append_custom_entry("my_data", Some(json!({ "foo": "bar" })));
    let msg2_id = session.append_message(assistant_message("hi", "test"));

    let entries = session.get_entries();
    assert_eq!(entries.len(), 3);
    let SessionEntry::Custom(custom) = session.get_entry(&custom_id).unwrap() else {
        panic!("expected custom entry");
    };
    assert_eq!(custom.custom_type, "my_data");
    assert_eq!(custom.data, Some(json!({ "foo": "bar" })));
    assert_eq!(custom.parent_id.as_deref(), Some(msg_id.as_str()));

    let path: Vec<String> = session
        .get_branch(None)
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(path, [msg_id, custom_id, msg2_id]);
    assert_eq!(session.build_session_context().messages.len(), 2);
}

// --- labels -------------------------------------------------------------

#[test]
fn labels_set_get_and_clear() {
    let mut session = SessionManager::in_memory("/w");
    let msg_id = session.append_message(user_message("hello"));
    assert!(session.get_label(&msg_id).is_none());

    let label_id = session
        .append_label_change(&msg_id, Some("checkpoint"))
        .unwrap();
    assert_eq!(session.get_label(&msg_id).as_deref(), Some("checkpoint"));
    let SessionEntry::Label(label) = session.get_entry(&label_id).unwrap() else {
        panic!("expected label");
    };
    assert_eq!(label.target_id, msg_id);
    assert_eq!(label.label.as_deref(), Some("checkpoint"));

    session.append_label_change(&msg_id, None).unwrap();
    assert!(session.get_label(&msg_id).is_none());
}

#[test]
fn labels_last_wins_and_appear_in_tree() {
    let mut session = SessionManager::in_memory("/w");
    let msg_id = session.append_message(user_message("hello"));
    session.append_label_change(&msg_id, Some("first")).unwrap();
    session
        .append_label_change(&msg_id, Some("second"))
        .unwrap();
    let last = session.append_label_change(&msg_id, Some("third")).unwrap();
    assert_eq!(session.get_label(&msg_id).as_deref(), Some("third"));

    let last_ts = session.get_entry(&last).unwrap().timestamp().to_string();
    let node = session
        .get_tree()
        .into_iter()
        .find(|n| n.entry.id() == msg_id)
        .unwrap();
    assert_eq!(node.label.as_deref(), Some("third"));
    assert_eq!(node.label_timestamp.as_deref(), Some(last_ts.as_str()));
}

#[test]
fn labels_error_when_target_missing() {
    let mut session = SessionManager::in_memory("/w");
    let err = session
        .append_label_change("non-existent", Some("label"))
        .unwrap_err();
    assert_eq!(err.to_string(), "Entry non-existent not found");
}

#[test]
fn labels_not_in_session_context() {
    let mut session = SessionManager::in_memory("/w");
    let msg_id = session.append_message(user_message("hello"));
    session
        .append_label_change(&msg_id, Some("checkpoint"))
        .unwrap();
    let ctx = session.build_session_context();
    assert_eq!(ctx.messages.len(), 1);
    assert_eq!(role(&ctx.messages[0]), "user");
}

// --- createBranchedSession (in-memory) ----------------------------------

#[test]
fn create_branched_session_errors_on_unknown() {
    let mut session = SessionManager::in_memory("/w");
    session.append_message(user_message("hello"));
    let err = session.create_branched_session("nonexistent").unwrap_err();
    assert_eq!(err.to_string(), "Entry nonexistent not found");
}

#[test]
fn create_branched_session_extracts_path() {
    let mut session = SessionManager::in_memory("/w");
    let id1 = session.append_message(user_message("1"));
    let id2 = session.append_message(assistant_message("2", "test"));
    session.append_message(user_message("3"));

    session.branch(&id2).unwrap();
    let id4 = session.append_message(user_message("4"));
    let id5 = session.append_message(assistant_message("5", "test"));

    assert_eq!(session.create_branched_session(&id5).unwrap(), None);
    let ids: Vec<String> = session
        .get_entries()
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    assert_eq!(ids, [id1, id2, id4, id5]);
}

#[test]
fn create_branched_session_preserves_and_rewires_labels() {
    let mut session = SessionManager::in_memory("/w");
    let msg1 = session.append_message(user_message("hello"));
    session
        .append_label_change(&msg1, Some("checkpoint"))
        .unwrap();
    let model_change = session.append_model_change("anthropic", "claude-test");
    let msg2 = session.append_message(user_message("followup"));

    session.create_branched_session(&msg2).unwrap();

    // The stripped label between msg1 and the model change is re-chained.
    assert_eq!(
        session.get_entry(&model_change).unwrap().parent_id(),
        Some(msg1.as_str())
    );
    assert_eq!(session.get_label(&msg1).as_deref(), Some("checkpoint"));
}

#[test]
fn create_branched_session_drops_labels_off_path() {
    let mut session = SessionManager::in_memory("/w");
    let msg1 = session.append_message(user_message("hello"));
    let msg2 = session.append_message(assistant_message("hi", "test"));
    let msg3 = session.append_message(user_message("followup"));
    session.append_label_change(&msg1, Some("first")).unwrap();
    session.append_label_change(&msg2, Some("second")).unwrap();
    session.append_label_change(&msg3, Some("third")).unwrap();

    session.create_branched_session(&msg2).unwrap();

    assert_eq!(session.get_label(&msg1).as_deref(), Some("first"));
    assert_eq!(session.get_label(&msg2).as_deref(), Some("second"));
    assert!(session.get_label(&msg3).is_none());
}

// --- session name + reset -----------------------------------------------

#[test]
fn session_info_sanitizes_and_reads_back() {
    let mut session = SessionManager::in_memory("/w");
    session.append_message(user_message("hi"));
    session.append_session_info("  line one\r\nline two  ");
    assert_eq!(
        session.get_session_name().as_deref(),
        Some("line one line two")
    );
}

#[test]
fn reset_leaf_empties_context() {
    let mut session = SessionManager::in_memory("/w");
    session.append_message(user_message("hi"));
    session.reset_leaf();
    assert!(session.get_leaf_id().is_none());
    assert!(session.build_session_context().messages.is_empty());
    assert!(session.build_context_entries().is_empty());
}

#[test]
fn in_memory_session_is_not_persisted() {
    let session = SessionManager::in_memory("/some/dir");
    assert!(!session.is_persisted());
    assert!(session.get_session_file().is_none());
    assert_eq!(session.get_cwd(), "/some/dir");
    assert!(!session.uses_default_session_dir());
    assert_eq!(session.get_header().unwrap().version, Some(3));
}

// --- byte-exact serialization (guards the divergences) ------------------

#[test]
fn custom_entry_serializes_in_coding_agent_key_order() {
    let entry = SessionEntry::Custom(CustomEntry {
        custom_type: "my_data".to_string(),
        data: Some(json!({ "foo": "bar" })),
        id: "abc12345".to_string(),
        parent_id: Some("parent01".to_string()),
        ..base_custom_entry()
    });
    assert_eq!(
        serde_json::to_string(&entry).unwrap(),
        r#"{"type":"custom","customType":"my_data","data":{"foo":"bar"},"id":"abc12345","parentId":"parent01","timestamp":"2025-01-01T00:00:00Z"}"#
    );
}

#[test]
fn custom_message_entry_serializes_in_coding_agent_key_order() {
    let entry = SessionEntry::CustomMessage(CustomMessageEntry {
        custom_type: "note".to_string(),
        content: json!("hi"),
        display: true,
        details: Some(json!({ "k": 1 })),
        id: "abc12345".to_string(),
        parent_id: None,
        ..base_custom_message_entry()
    });
    assert_eq!(
        serde_json::to_string(&entry).unwrap(),
        r#"{"type":"custom_message","customType":"note","content":"hi","display":true,"details":{"k":1},"id":"abc12345","parentId":null,"timestamp":"2025-01-01T00:00:00Z"}"#
    );
}

#[test]
fn header_serializes_without_metadata_key() {
    let mut header = SessionHeader {
        tag: SessionTag::Session,
        version: Some(CURRENT_SESSION_VERSION),
        id: "s1".to_string(),
        timestamp: TS.to_string(),
        cwd: "/tmp".to_string(),
        parent_session: None,
    };
    assert_eq!(
        serde_json::to_string(&header).unwrap(),
        r#"{"type":"session","version":3,"id":"s1","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}"#
    );
    header.parent_session = Some("/parent".to_string());
    assert!(serde_json::to_string(&header)
        .unwrap()
        .ends_with(r#""parentSession":"/parent"}"#));
}

#[test]
fn message_value_preserves_insertion_order() {
    // Guards that `serde_json`'s `preserve_order` is unified into this crate:
    // without it the message object would serialize alphabetically.
    let entry = message_entry(
        "i",
        None,
        json!({ "role": "user", "content": "hi", "timestamp": 1 }),
    );
    assert_eq!(
        serde_json::to_string(&entry).unwrap(),
        r#"{"type":"message","id":"i","parentId":null,"timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hi","timestamp":1}}"#
    );
}
