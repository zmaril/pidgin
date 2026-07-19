//! Turn-runner tests, ported from pi's `test/suite/agent-session-prompt.test.ts`.
//!
//! Each `#[test]` mirrors a pi `AgentSession prompt characterization` case: same
//! setup (a faux stream fn + in-memory session/settings/model runtime, from
//! [`super::super::test_support`]), same assertions on the emitted events and
//! persisted / in-state messages. The pi cases that depend on subsystems deferred
//! to a later PR of the AgentSession port are `#[ignore]`d with the PR that
//! enables them.

// straitjacket-allow-file:duplication

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_ai::providers::faux::faux_tool_call;
use atilla_ai::Context;

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::{
    assistant_text, assistant_tool_use, create_harness, echo_tool, events_of_type, message_text,
    recording_tool, FauxResponse, HarnessOptions, TestExtensionRunner,
};
use crate::core::extensions::events::selection::StreamingBehavior;

use super::{PromptError, PromptOptions};

// ---------------------------------------------------------------------------
// Ported prompt-suite cases
// ---------------------------------------------------------------------------

#[test]
fn prompts_while_idle_and_records_a_single_text_response() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "hello",
    )))]);

    harness.session.prompt("hi", None, None).unwrap();

    assert_eq!(harness.message_roles(), vec!["user", "assistant"]);
    assert_eq!(message_text(&harness.session.messages()[0]), "hi");
    assert_eq!(harness.pending_response_count(), 0);
}

#[test]
fn handles_a_tool_call_turn_and_waits_for_the_follow_up_response() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Message(Box::new(assistant_text("done"))),
    ]);

    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(*tool_runs.lock().unwrap(), vec!["hello".to_string()]);
    assert_eq!(
        harness.message_roles(),
        vec!["user", "assistant", "toolResult", "assistant"]
    );
}

#[test]
fn executes_multiple_tool_calls_and_continues_with_a_single_follow_up() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![
            recording_tool("slow", Arc::clone(&tool_runs)),
            recording_tool("fast", Arc::clone(&tool_runs)),
        ],
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![
            faux_tool_call("slow", json!({ "value": "a" }), Some("call-1".to_string())),
            faux_tool_call("fast", json!({ "value": "b" }), Some("call-2".to_string())),
        ]))),
        FauxResponse::Fn(Box::new(|context: &Context| {
            let tool_results = context
                .messages
                .iter()
                .filter(|m| matches!(m, atilla_ai::Message::ToolResult(_)))
                .count();
            assistant_text(&format!("tool results: {tool_results}"))
        })),
    ]);

    harness.session.prompt("run tools", None, None).unwrap();

    let mut runs = tool_runs.lock().unwrap().clone();
    runs.sort();
    assert_eq!(runs, vec!["fast:b".to_string(), "slow:a".to_string()]);
    let tool_result_count = harness
        .session
        .messages()
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("toolResult"))
        .count();
    assert_eq!(tool_result_count, 2);
    assert_eq!(harness.message_roles().last().unwrap(), "assistant");
}

#[test]
fn preserves_image_attachments_in_the_provider_context() {
    let harness = create_harness(HarnessOptions::default());
    let saw_image = Arc::new(AtomicUsize::new(0));
    let saw_image_stream = Arc::clone(&saw_image);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            let context_json = serde_json::to_value(&context.messages).unwrap_or(Value::Null);
            let has_image = context_json
                .as_array()
                .map(|messages| {
                    messages.iter().any(|message| {
                        message
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|blocks| {
                                blocks
                                    .iter()
                                    .any(|b| b.get("type").and_then(Value::as_str) == Some("image"))
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if has_image {
                saw_image_stream.fetch_add(1, Ordering::SeqCst);
            }
            assistant_text("ok")
        },
    ))]);

    let images = vec![json!({
        "type": "image",
        "mimeType": "image/png",
        "data": "ZmFrZQ=="
    })];
    harness
        .session
        .prompt("describe", Some(images), None)
        .unwrap();

    assert_eq!(saw_image.load(Ordering::SeqCst), 1);
}

#[test]
fn throws_when_prompting_without_a_model() {
    let harness = create_harness(HarnessOptions {
        with_model: false,
        ..Default::default()
    });

    let error = harness.session.prompt("hi", None, None).unwrap_err();
    assert!(
        matches!(&error, PromptError::Preflight(message) if message.starts_with("No model selected.")),
        "expected a no-model preflight error, got {error:?}"
    );
}

#[test]
fn throws_when_prompting_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: false,
        ..Default::default()
    });

    let error = harness.session.prompt("hi", None, None).unwrap_err();
    assert!(
        matches!(&error, PromptError::Preflight(message) if message.starts_with("No API key found for faux.")),
        "expected a no-auth preflight error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Turn-lifecycle events + persistence
// ---------------------------------------------------------------------------

#[test]
fn emits_agent_settled_and_forwards_lifecycle_events_to_listeners() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "hello",
    )))]);

    harness.session.prompt("hi", None, None).unwrap();

    // A run start, an assistant message, an agent end, and a final settle are all
    // forwarded to listeners.
    assert_eq!(
        events_of_type(&harness, |e| matches!(e, AgentSessionEvent::AgentStart)),
        1
    );
    assert_eq!(
        events_of_type(&harness, |e| matches!(
            e,
            AgentSessionEvent::AgentEnd { .. }
        )),
        1
    );
    assert_eq!(
        events_of_type(&harness, |e| matches!(e, AgentSessionEvent::AgentSettled)),
        1
    );
    // agent_end is emitted before agent_settled.
    let events = harness.events.lock().unwrap();
    let end_index = events
        .iter()
        .position(|e| matches!(e, AgentSessionEvent::AgentEnd { .. }))
        .unwrap();
    let settled_index = events
        .iter()
        .position(|e| matches!(e, AgentSessionEvent::AgentSettled))
        .unwrap();
    assert!(end_index < settled_index);
    // The session is idle again after the run settles.
    assert!(harness.session.is_idle());
}

#[test]
fn persists_finalized_messages_to_the_session_manager() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "hello",
    )))]);

    harness.session.prompt("hi", None, None).unwrap();

    let entries = harness.session.session_manager().get_entries();
    let persisted_roles: Vec<String> = entries
        .iter()
        .filter_map(|entry| serde_json::to_value(entry).ok())
        .filter_map(|value| {
            value
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    assert_eq!(persisted_roles, vec!["user", "assistant"]);
}

// ---------------------------------------------------------------------------
// Queue-slice prompt cases (enabled by PR4)
// ---------------------------------------------------------------------------

#[test]
fn send_user_message_while_idle_triggers_a_turn() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "response",
    )))]);

    harness
        .session
        .send_user_message("from extension", None)
        .unwrap();

    assert_eq!(harness.message_roles(), vec!["user", "assistant"]);
    assert_eq!(
        message_text(&harness.session.messages()[0]),
        "from extension"
    );
}

#[test]
fn does_not_report_streaming_behavior_to_input_handlers_while_idle() {
    let input_events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&input_events);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_input_recording(sink))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text("ok")))]);

    // A streaming behavior is supplied, but the session is idle, so it is NOT
    // reported to the input handler (pi `this.isStreaming ? behavior : undefined`).
    harness
        .session
        .prompt_with(
            "idle",
            PromptOptions {
                streaming_behavior: Some(StreamingBehavior::FollowUp),
                ..PromptOptions::defaults()
            },
        )
        .unwrap();

    let events = input_events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].streaming_behavior, None);
}

// ---------------------------------------------------------------------------
// Deferred pi cases (enabled by later PRs of the AgentSession port)
// ---------------------------------------------------------------------------

// The streaming-guard queue routing IS ported (see `prompt_with`), but these two
// pi cases can only be observed by acting on the session *while a turn is in
// flight*. Under the sync/eager agent that is structurally impossible: the loop
// runs to completion on the calling thread, `AgentSession` is not `Send`/`Sync`,
// and every mid-run hook (tool `execute`, listeners, the stream fn) is
// `Send + Sync`-bounded and so cannot capture the live session. The idle-driven
// queue tests (`queue::tests`) cover the routing / mirror mechanics instead.
#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — no mid-run hook can call session.prompt while streaming; routing covered idle in queue::tests"]
fn reports_streaming_behavior_to_input_handlers_while_streaming() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — the streaming guard needs an in-flight run, unreachable from any mid-run hook"]
fn throws_when_prompted_during_streaming_without_a_streaming_behavior() {}

#[test]
#[ignore = "unit5: enabled by PR7 (skill-command expansion)"]
fn expands_skill_commands_before_sending_the_prompt() {}

#[test]
#[ignore = "unit5: enabled by PR7 (prompt-template expansion)"]
fn expands_prompt_templates_before_sending_the_prompt() {}

#[test]
#[ignore = "unit5: enabled by PR7 (extension-command dispatch)"]
fn dispatches_extension_commands_without_consuming_a_provider_response() {}
