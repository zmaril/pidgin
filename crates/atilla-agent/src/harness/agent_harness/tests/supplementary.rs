//! Supplementary tests for branches upstream leaves uncovered: phase-busy guards
//! on every entry point, idle-state guards, stream-option key deletes/collapse,
//! the compaction hook-override / cancel / nothing-to-compact paths, tree
//! navigation (same-target, unknown target, editor-text projection), and
//! abort-while-idle. All flagged SUPPLEMENTARY.

// straitjacket-allow-file:duplication — faithful parallel-structure test
// bodies repeat near-identical faux/session/harness scaffolding per scenario,
// mirroring pi's one-`it`-per-shape suite; not extractable duplication.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::json;

use super::*;
use crate::harness::agent_harness::{
    apply_stream_options_patch, AgentHarness, AgentHarnessEvent, NavigateTreeOptions,
};
use crate::harness::events::{
    AgentHarnessEventResult, AgentHarnessOwnEvent, AgentHarnessStreamOptions,
    AgentHarnessStreamOptionsPatch, CompactResult, SessionBeforeCompactResult,
};
use crate::harness::options::AgentHarnessErrorCode;
use crate::harness::session::Session;
use crate::types::AgentEvent;

/// SUPPLEMENTARY: `prompt()` re-entered mid-run rejects with a `busy` error.
#[test]
fn prompt_is_busy_during_a_run() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let busy = Rc::new(Cell::new(false));
    let b = busy.clone();
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::MessageStart { .. }) = event {
            if let Err(error) = h.prompt("re-entrant", None) {
                if error.code == AgentHarnessErrorCode::Busy {
                    b.set(true);
                }
            }
        }
    }));

    harness.prompt("hello", None).unwrap();
    assert!(busy.get());
}

/// SUPPLEMENTARY: `compact()` re-entered mid-run rejects with a `busy` error.
#[test]
fn compact_is_busy_during_a_run() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let busy = Rc::new(Cell::new(false));
    let b = busy.clone();
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::MessageStart { .. }) = event {
            if let Err(error) = h.compact(None) {
                if error.code == AgentHarnessErrorCode::Busy {
                    b.set(true);
                }
            }
        }
    }));

    harness.prompt("hello", None).unwrap();
    assert!(busy.get());
}

/// SUPPLEMENTARY: `steer`/`followUp` reject while idle.
#[test]
fn steer_and_follow_up_reject_while_idle() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    assert_eq!(
        harness.steer("x", None).unwrap_err().code,
        AgentHarnessErrorCode::InvalidState
    );
    assert_eq!(
        harness.follow_up("x", None).unwrap_err().code,
        AgentHarnessErrorCode::InvalidState
    );
}

/// SUPPLEMENTARY: `abort()` while idle returns empty cleared queues and still
/// emits the `abort` event.
#[test]
fn abort_while_idle_returns_empty_queues() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let events = Rc::new(RefCell::new(Vec::new()));
    let _sub = harness.subscribe(recording_subscriber(events.clone()));

    let result = harness.abort().unwrap();
    assert!(result.cleared_steer.is_empty());
    assert!(result.cleared_follow_up.is_empty());
    assert!(events.borrow().iter().any(|e| e == "abort"));
}

/// SUPPLEMENTARY: header/metadata key deletes collapse an emptied map to `None`.
#[test]
fn stream_option_key_deletes_collapse_to_none() {
    let base = AgentHarnessStreamOptions {
        headers: Some(
            [("only".to_string(), "x".to_string())]
                .into_iter()
                .collect(),
        ),
        metadata: Some([("only".to_string(), json!("x"))].into_iter().collect()),
        ..AgentHarnessStreamOptions::default()
    };
    let patch = AgentHarnessStreamOptionsPatch {
        headers: Some([("only".to_string(), None)].into_iter().collect()),
        metadata: Some([("only".to_string(), json!(null))].into_iter().collect()),
        ..AgentHarnessStreamOptionsPatch::default()
    };
    let merged = apply_stream_options_patch(&base, Some(&patch));
    assert_eq!(merged.headers, None);
    assert_eq!(merged.metadata, None);

    // A `None` patch is a no-op clone.
    let unchanged = apply_stream_options_patch(&base, None);
    assert_eq!(unchanged.headers.unwrap().len(), 1);
}

/// SUPPLEMENTARY: `compact()` on an empty session reports "Nothing to compact".
#[test]
fn compact_nothing_to_compact() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let error = harness.compact(None).unwrap_err();
    assert_eq!(error.code, AgentHarnessErrorCode::Compaction);
    assert!(error.message.contains("Nothing to compact"));
}

/// SUPPLEMENTARY: a `session_before_compact` hook can supply the compaction
/// result; it is persisted with `fromHook` and drives `session_compact`.
#[test]
fn compact_uses_hook_provided_result() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let session = Session::new(storage.clone());
    // Seed some history so `prepareCompaction` returns a preparation.
    let user = session
        .append_message(json!({ "role": "user", "content": "hi", "timestamp": 0 }))
        .unwrap();
    let _ = session
        .append_message(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "there" }],
            "timestamp": 0,
            "stopReason": "stop",
            "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15 },
        }))
        .unwrap();

    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, model)).unwrap();

    let provided = CompactResult {
        summary: "hooked summary".into(),
        first_kept_entry_id: user.clone(),
        tokens_before: 42,
        details: Some(json!({ "note": "from hook" })),
    };
    let provided_for_hook = provided.clone();
    let _sub = harness.on(
        "session_before_compact",
        Rc::new(move |_event| {
            Ok(Some(AgentHarnessEventResult::SessionBeforeCompact(Some(
                SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(provided_for_hook.clone()),
                },
            ))))
        }),
    );

    let compact_events = Rc::new(RefCell::new(Vec::new()));
    let ce = compact_events.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Own(own) = event {
            if let AgentHarnessOwnEvent::SessionCompact(ev) = own.as_ref() {
                ce.borrow_mut()
                    .push((ev.compaction_entry.summary.clone(), ev.from_hook));
            }
        }
    }));

    let result = harness.compact(None).unwrap();
    assert_eq!(result.summary, "hooked summary");
    assert_eq!(result.first_kept_entry_id, user);

    assert_eq!(
        *compact_events.borrow(),
        vec![("hooked summary".to_string(), true)]
    );
    // The compaction entry is persisted.
    let inspect = Session::new(storage);
    let has_compaction = inspect
        .get_entries()
        .iter()
        .any(|e| matches!(e, SessionTreeEntry::Compaction(_)));
    assert!(has_compaction);
}

/// SUPPLEMENTARY: a `session_before_compact` hook can cancel compaction.
#[test]
fn compact_hook_can_cancel() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let session = Session::new(storage.clone());
    session
        .append_message(json!({ "role": "user", "content": "hi", "timestamp": 0 }))
        .unwrap();
    session
        .append_message(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "there" }],
            "timestamp": 0,
            "stopReason": "stop",
            "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15 },
        }))
        .unwrap();

    let harness = AgentHarness::new(base_options(Session::new(storage), faux, model)).unwrap();
    let _sub = harness.on(
        "session_before_compact",
        Rc::new(|_event| {
            Ok(Some(AgentHarnessEventResult::SessionBeforeCompact(Some(
                SessionBeforeCompactResult {
                    cancel: Some(true),
                    compaction: None,
                },
            ))))
        }),
    );

    let error = harness.compact(None).unwrap_err();
    assert_eq!(error.code, AgentHarnessErrorCode::Compaction);
    assert!(error.message.contains("cancelled"));
}

/// SUPPLEMENTARY: `navigateTree` to the current leaf is a no-op.
#[test]
fn navigate_tree_same_target_is_noop() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let session = Session::new(storage.clone());
    session
        .append_message(json!({ "role": "user", "content": "hello", "timestamp": 0 }))
        .unwrap();
    let leaf = session.get_leaf_id().unwrap().unwrap();

    let harness = AgentHarness::new(base_options(Session::new(storage), faux, model)).unwrap();
    let result = harness
        .navigate_tree(&leaf, NavigateTreeOptions::default())
        .unwrap();
    assert!(!result.cancelled);
    assert!(result.editor_text.is_none());
}

/// SUPPLEMENTARY: `navigateTree` to an unknown entry is rejected.
#[test]
fn navigate_tree_unknown_target_errors() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let session = Session::new(storage.clone());
    session
        .append_message(json!({ "role": "user", "content": "hello", "timestamp": 0 }))
        .unwrap();

    let harness = AgentHarness::new(base_options(Session::new(storage), faux, model)).unwrap();
    let error = harness
        .navigate_tree("does-not-exist", NavigateTreeOptions::default())
        .unwrap_err();
    assert_eq!(error.code, AgentHarnessErrorCode::InvalidArgument);
}

/// SUPPLEMENTARY: navigating to an earlier user entry returns its editor text and
/// emits `session_tree`.
#[test]
fn navigate_tree_returns_editor_text_and_emits_session_tree() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let session = Session::new(storage.clone());
    let user = session
        .append_message(json!({
            "role": "user",
            "content": [{ "type": "text", "text": "first prompt" }],
            "timestamp": 0,
        }))
        .unwrap();
    session
        .append_message(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "reply" }],
            "timestamp": 0,
        }))
        .unwrap();
    let old_leaf = session.get_leaf_id().unwrap();

    let harness = AgentHarness::new(base_options(Session::new(storage), faux, model)).unwrap();
    let tree_events = Rc::new(RefCell::new(Vec::new()));
    let tree_ev = tree_events.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Own(own) = event {
            if let AgentHarnessOwnEvent::SessionTree(ev) = own.as_ref() {
                tree_ev.borrow_mut().push(ev.old_leaf_id.clone());
            }
        }
    }));

    let result = harness
        .navigate_tree(&user, NavigateTreeOptions::default())
        .unwrap();
    assert!(!result.cancelled);
    assert_eq!(result.editor_text.as_deref(), Some("first prompt"));
    assert_eq!(*tree_events.borrow(), vec![old_leaf]);
}

/// SUPPLEMENTARY: idle `setModel`/`setThinkingLevel` persist change entries.
#[test]
fn idle_setters_persist_change_entries() {
    let faux = new_faux_two_models();
    let first = faux.get_model(Some("first")).unwrap();
    let second = faux.get_model(Some("second")).unwrap();
    let storage = new_storage();
    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, first)).unwrap();

    harness.set_model(second).unwrap();
    harness.set_thinking_level(ThinkingLevel::High).unwrap();

    let inspect = Session::new(storage);
    let entries = inspect.get_entries();
    assert!(entries
        .iter()
        .any(|e| matches!(e, SessionTreeEntry::ModelChange(_))));
    assert!(entries
        .iter()
        .any(|e| matches!(e, SessionTreeEntry::ThinkingLevelChange(_))));
}

/// SUPPLEMENTARY: `prompt` is busy while a compaction is in progress is covered
/// by the phase guard; here we confirm a fresh harness starts idle and a plain
/// `prompt` succeeds after a completed one (phase resets to idle).
#[test]
fn phase_resets_to_idle_between_prompts() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("one"), text_response("two")]);
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let first = harness.prompt("a", None).unwrap();
    assert_eq!(role(&first), Some("assistant"));
    // Second prompt only succeeds if the phase reset to idle.
    let second = harness.prompt("b", None).unwrap();
    assert_eq!(role(&second), Some("assistant"));
}
