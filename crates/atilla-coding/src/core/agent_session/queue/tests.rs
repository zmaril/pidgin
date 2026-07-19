//! Queue-suite tests, ported from pi's `test/suite/agent-session-queue.test.ts`
//! and `test/agent-session-concurrent.test.ts`.
//!
//! ## Sync/eager + `!Send` adaptation
//!
//! pi drives most of these cases by blocking a `wait` tool on a promise and, from
//! a concurrent async context, calling `steer`/`followUp`/`prompt` while the run
//! is in flight. Under `atilla_agent`'s synchronous, eager loop that is
//! structurally impossible (see [`super::super::test_support`]): the turn runs to
//! completion on the calling thread, `AgentSession` is `!Send`/`!Sync`, and every
//! mid-run hook is `Send + Sync`-bounded and cannot capture the live session.
//!
//! The queue *semantics* are ported by enqueuing steering / follow-up messages
//! while **idle** and letting the loop drain them (it polls its steering queue at
//! each turn boundary and its follow-up queue when it would otherwise stop). This
//! exercises the same `AgentSession` mirror-push / `queue_update` / splice-on-drain
//! and agent-drain paths. Cases whose premise strictly requires genuine in-flight
//! streaming (or a PR7 subsystem) are `#[ignore]`d with a precise reason.

// straitjacket-allow-file:duplication

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_agent::types::QueueMode;
use atilla_ai::providers::faux::faux_tool_call;
use atilla_ai::{Context, Message};

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::{
    assistant_text, assistant_texts, assistant_tool_use, create_harness, message_text,
    recording_tool, user_message, user_texts, FauxResponse, Harness, HarnessOptions,
    TestExtensionRunner,
};
use crate::core::agent_session::turn::PromptError;

use super::{CustomMessageInput, DeliverAs};

/// The most recent `queue_update` payload recorded by the harness listener.
fn last_queue_update(harness: &Harness) -> Option<(Vec<String>, Vec<String>)> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentSessionEvent::QueueUpdate {
                steering,
                follow_up,
            } => Some((steering.clone(), follow_up.clone())),
            _ => None,
        })
}

/// The joined text of every `user` message in a provider request context.
fn context_user_texts(context: &Context) -> Vec<String> {
    context
        .messages
        .iter()
        .filter(|m| matches!(m, Message::User(_)))
        .map(|m| message_text(&serde_json::to_value(m).unwrap_or(Value::Null)))
        .collect()
}

// ---------------------------------------------------------------------------
// steer / followUp enqueue mechanics (idle)
// ---------------------------------------------------------------------------

#[test]
fn steer_and_follow_up_enqueue_update_the_mirrors_and_emit_queue_update() {
    let harness = create_harness(HarnessOptions::default());

    harness.session.steer("s1", None).unwrap();
    assert_eq!(harness.session.pending_message_count(), 1);
    assert_eq!(
        harness.session.get_steering_messages(),
        vec!["s1".to_string()]
    );

    harness.session.follow_up("f1", None).unwrap();
    assert_eq!(harness.session.pending_message_count(), 2);
    assert_eq!(
        harness.session.get_follow_up_messages(),
        vec!["f1".to_string()]
    );

    // A queue_update fired for each enqueue; the latest carries both mirrors.
    assert_eq!(
        last_queue_update(&harness),
        Some((vec!["s1".to_string()], vec!["f1".to_string()]))
    );
    // The messages were also enqueued on the agent's own queues.
    assert!(harness.agent.has_queued_messages());
}

#[test]
fn clear_queue_returns_and_clears_all_queues() {
    let harness = create_harness(HarnessOptions::default());
    harness.session.steer("s1", None).unwrap();
    harness.session.follow_up("f1", None).unwrap();

    let (steering, follow_up) = harness.session.clear_queue();
    assert_eq!(steering, vec!["s1".to_string()]);
    assert_eq!(follow_up, vec!["f1".to_string()]);

    assert_eq!(harness.session.pending_message_count(), 0);
    assert!(!harness.agent.has_queued_messages());
    assert_eq!(last_queue_update(&harness), Some((Vec::new(), Vec::new())));
}

#[test]
fn removes_queued_steering_text_before_the_queued_message_start_is_emitted() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "done",
    )))]);

    harness.session.steer("queued", None).unwrap();
    assert_eq!(harness.session.pending_message_count(), 1);

    harness.session.prompt("start", None, None).unwrap();

    // The mirror is spliced BEFORE the queued user `message_start` is emitted: the
    // last `queue_update` preceding that `message_start` no longer lists "queued"
    // (pi asserts `countsAtQueuedMessageStart == [0]`).
    let events = harness.events.lock().unwrap();
    let start_index = events
        .iter()
        .position(|event| {
            matches!(event, AgentSessionEvent::MessageStart { message } if message_text(message) == "queued")
        })
        .expect("a message_start for the queued text");
    let steering_before = events[..start_index]
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentSessionEvent::QueueUpdate { steering, .. } => Some(steering.clone()),
            _ => None,
        })
        .expect("a queue_update before the queued message_start");
    assert!(
        !steering_before.contains(&"queued".to_string()),
        "queued text should be spliced before its message_start, got {steering_before:?}"
    );
    drop(events);

    assert_eq!(harness.session.pending_message_count(), 0);
}

// ---------------------------------------------------------------------------
// steering / follow-up drain order (idle enqueue, drained by the loop)
// ---------------------------------------------------------------------------

#[test]
fn delivers_multiple_steering_messages_in_order_one_at_a_time() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("handled steer 1"))),
        FauxResponse::Message(Box::new(assistant_text("handled steer 2"))),
    ]);

    harness.session.steer("steer 1", None).unwrap();
    harness.session.steer("steer 2", None).unwrap();
    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(user_texts(&harness), vec!["start", "steer 1", "steer 2"]);
    assert_eq!(
        assistant_texts(&harness),
        vec!["handled steer 1", "handled steer 2"]
    );
    assert_eq!(harness.session.pending_message_count(), 0);
}

#[test]
fn delivers_all_steering_messages_in_one_batch_in_all_mode() {
    let harness = create_harness(HarnessOptions::default());
    harness.agent.set_steering_mode(QueueMode::All);

    let batched = Arc::new(Mutex::new(Vec::new()));
    let batched_stream = Arc::clone(&batched);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            *batched_stream.lock().unwrap() = context_user_texts(context);
            assistant_text("batched steer response")
        },
    ))]);

    harness.session.steer("steer 1", None).unwrap();
    harness.session.steer("steer 2", None).unwrap();
    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(
        *batched.lock().unwrap(),
        vec!["start", "steer 1", "steer 2"]
    );
    assert_eq!(assistant_texts(&harness), vec!["batched steer response"]);
}

#[test]
fn delivers_multiple_follow_up_messages_in_order_one_at_a_time() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("original turn complete"))),
        FauxResponse::Message(Box::new(assistant_text("handled follow-up 1"))),
        FauxResponse::Message(Box::new(assistant_text("handled follow-up 2"))),
    ]);

    harness.session.follow_up("follow-up 1", None).unwrap();
    harness.session.follow_up("follow-up 2", None).unwrap();
    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(
        user_texts(&harness),
        vec!["start", "follow-up 1", "follow-up 2"]
    );
    assert_eq!(
        assistant_texts(&harness),
        vec![
            "original turn complete",
            "handled follow-up 1",
            "handled follow-up 2"
        ]
    );
}

#[test]
fn delivers_all_follow_up_messages_in_one_batch_in_all_mode() {
    let harness = create_harness(HarnessOptions::default());
    harness.agent.set_follow_up_mode(QueueMode::All);

    let batched = Arc::new(Mutex::new(Vec::new()));
    let batched_stream = Arc::clone(&batched);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("original turn complete"))),
        FauxResponse::Fn(Box::new(move |context: &Context| {
            *batched_stream.lock().unwrap() = context_user_texts(context);
            assistant_text("batched follow-up response")
        })),
    ]);

    harness.session.follow_up("follow-up 1", None).unwrap();
    harness.session.follow_up("follow-up 2", None).unwrap();
    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(
        *batched.lock().unwrap(),
        vec!["start", "follow-up 1", "follow-up 2"]
    );
    assert_eq!(
        assistant_texts(&harness),
        vec!["original turn complete", "batched follow-up response"]
    );
}

#[test]
fn delivers_follow_up_only_after_the_current_run_finishes() {
    let harness = create_harness(HarnessOptions::default());
    let saw_assistant_before = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_assistant_before);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("original turn complete"))),
        FauxResponse::Fn(Box::new(move |context: &Context| {
            let has_assistant = context
                .messages
                .iter()
                .any(|m| matches!(m, Message::Assistant(_)));
            flag.store(has_assistant, Ordering::SeqCst);
            assistant_text("follow-up response")
        })),
    ]);

    harness
        .session
        .follow_up("after current run", None)
        .unwrap();
    harness.session.prompt("start", None, None).unwrap();

    assert_eq!(user_texts(&harness), vec!["start", "after current run"]);
    assert_eq!(
        assistant_texts(&harness),
        vec!["original turn complete", "follow-up response"]
    );
    assert!(
        saw_assistant_before.load(Ordering::SeqCst),
        "the current run's assistant reply precedes the follow-up turn"
    );
}

// ---------------------------------------------------------------------------
// custom-message delivery (nextTurn)
// ---------------------------------------------------------------------------

#[test]
fn injects_next_turn_custom_messages_into_the_next_prompt() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "done",
    )))]);

    harness
        .session
        .send_custom_message(
            CustomMessageInput {
                custom_type: "next-turn".to_string(),
                content: json!("carry this"),
                display: true,
                details: Some(json!({})),
            },
            false,
            Some(DeliverAs::NextTurn),
        )
        .unwrap();

    harness.session.prompt("normal prompt", None, None).unwrap();

    // The pending nextTurn message is injected into the turn alongside the user
    // message (pi asserts the roles `["user", "custom", "assistant"]`).
    assert_eq!(harness.message_roles(), vec!["user", "custom", "assistant"]);
    let carried = harness.session.messages().iter().any(|m| {
        m.get("role").and_then(Value::as_str) == Some("custom") && message_text(m) == "carry this"
    });
    assert!(
        carried,
        "the nextTurn custom message should carry into the turn"
    );
}

// ---------------------------------------------------------------------------
// extension-command rejection
// ---------------------------------------------------------------------------

#[test]
fn throws_when_queueing_an_extension_command_with_steer() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(|_agent| {
            Box::new(TestExtensionRunner::new().with_command("testcmd"))
        })),
        ..Default::default()
    });

    let error = harness.session.steer("/testcmd queued", None).unwrap_err();
    assert!(
        matches!(&error, PromptError::Preflight(message) if message == "Extension command \"/testcmd\" cannot be queued. Use prompt() or execute the command when not streaming."),
        "got {error:?}"
    );
    assert_eq!(harness.session.pending_message_count(), 0);
}

#[test]
fn throws_when_queueing_an_extension_command_with_follow_up() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(|_agent| {
            Box::new(TestExtensionRunner::new().with_command("testcmd"))
        })),
        ..Default::default()
    });

    let error = harness
        .session
        .follow_up("/testcmd queued", None)
        .unwrap_err();
    assert!(
        matches!(&error, PromptError::Preflight(message) if message == "Extension command \"/testcmd\" cannot be queued. Use prompt() or execute the command when not streaming."),
        "got {error:?}"
    );
    assert_eq!(harness.session.pending_message_count(), 0);
}

// ---------------------------------------------------------------------------
// follow-ups queued during agent_end
// ---------------------------------------------------------------------------

#[test]
fn delivers_follow_ups_queued_during_agent_end() {
    // pi queues the follow-up via `pi.sendUserMessage(..., { deliverAs: "followUp" })`
    // inside an `agent_end` handler. The `!Send` session cannot be reached from the
    // runner, so the callback enqueues on the (shared) agent directly; the observable
    // continuation — the follow-up turn — is identical.
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(|agent| {
            let agent = agent.clone();
            Box::new(TestExtensionRunner::new().with_agent_end(Arc::new(move || {
                agent.follow_up(user_message("conflict report"));
            })))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("reply"))),
        FauxResponse::Message(Box::new(assistant_text("follow-up reply"))),
    ]);

    harness.session.prompt("hello", None, None).unwrap();

    assert_eq!(user_texts(&harness), vec!["hello", "conflict report"]);
    assert_eq!(assistant_texts(&harness), vec!["reply", "follow-up reply"]);
}

// ---------------------------------------------------------------------------
// concurrent-prompt guard: portable (idle) cases
// ---------------------------------------------------------------------------

#[test]
fn allows_a_prompt_after_the_previous_run_completes() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_text("first done"))),
        FauxResponse::Message(Box::new(assistant_text("second done"))),
    ]);

    harness.session.prompt("first message", None, None).unwrap();
    assert!(harness.session.is_idle());

    harness
        .session
        .prompt("second message", None, None)
        .unwrap();
    assert_eq!(
        user_texts(&harness),
        vec!["first message", "second message"]
    );
}

#[test]
fn persists_message_end_events_in_order_for_a_tool_call_turn() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![recording_tool("dummy", Arc::clone(&tool_runs))],
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "dummy",
            json!({ "value": "x" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Message(Box::new(assistant_text("done"))),
    ]);

    harness.session.prompt("hi", None, None).unwrap();

    let entries = harness.session.session_manager().get_entries();
    let roles: Vec<String> = entries
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
    assert_eq!(roles, vec!["user", "assistant", "toolResult", "assistant"]);
}

// ---------------------------------------------------------------------------
// Cases requiring genuine in-flight streaming (structurally impossible) or a
// PR7 subsystem — ignored with a precise reason.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unit5: enabled by PR7 (`_tryExecuteExtensionCommand` / idle command dispatch)"]
fn dispatches_extension_commands_immediately_when_prompted_while_idle() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — extension `sendUserMessage` steer must run mid-turn; unreachable from any Send+Sync hook. Drain order covered by the idle steering tests"]
fn delivers_extension_origin_steering_messages_before_the_next_llm_call() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — `sendCustomMessage(deliverAs: steer)` routes to the agent queue only while streaming, which cannot be entered from a mid-run hook"]
fn queues_custom_messages_with_deliver_as_steer_while_streaming() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — `sendCustomMessage(deliverAs: followUp)` routes to the agent queue only while streaming, which cannot be entered from a mid-run hook"]
fn queues_custom_messages_with_deliver_as_follow_up_while_streaming() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — the streaming guard needs a run in flight; `prompt` cannot be called from any mid-run hook. Guard logic is unit-covered by the idle enqueue paths"]
fn throws_when_prompt_called_while_streaming() {}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — requires mid-run extension `sendUserMessage` + PR7 input-source reporting; the session cannot be reached mid-turn"]
fn queues_extension_origin_steering_messages_while_streaming() {}

#[test]
#[ignore = "unit5: PR7 (`tool_call` extension handler + emit ordering) and mid-run persistence snapshot; not a queue behavior"]
fn waits_for_queued_agent_events_before_emitting_tool_call() {}
