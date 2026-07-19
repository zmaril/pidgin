//! Auto-retry tests, ported from pi's
//! `test/suite/agent-session-retry-events.test.ts` (14 cases) and
//! `test/agent-session-retry.test.ts` (5 cases).
//!
//! Each `#[test]` mirrors a pi retry / event-characterization case: the same faux
//! stream fn (a scripted list that injects a retryable error, then a success on
//! retry) over the in-memory session/settings/model runtime from
//! [`super::super::test_support`], with `retry.baseDelayMs = 1` so the exponential
//! backoff is negligible. Assertions cover the `auto_retry_start`/`auto_retry_end`
//! sequence, the attempt counter, `agent_end.will_retry`, give-up-after-max, and
//! the success reset.
//!
//! Cases whose premise needs genuine mid-run concurrency the sync/eager `!Send`
//! model cannot provide (cancelling the backoff mid-sleep, aborting mid-stream), or
//! streaming-delta characterization the terminal-only mock stream does not emit,
//! are `#[ignore]`d with a precise reason rather than weakened.

// straitjacket-allow-file:duplication

use std::sync::{Arc, Mutex};

use serde_json::json;

use atilla_ai::providers::faux::faux_tool_call;

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::{
    assistant_error, assistant_text, assistant_tool_use, create_harness, echo_tool, retry_settings,
    FauxResponse, Harness, HarnessOptions, TestExtensionRunner,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The `start:<attempt>` / `end:<success>` retry-event log a pi test builds by
/// subscribing and pushing on `auto_retry_start` / `auto_retry_end`.
fn retry_event_log(harness: &Harness) -> Vec<String> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::AutoRetryStart { attempt, .. } => Some(format!("start:{attempt}")),
            AgentSessionEvent::AutoRetryEnd { success, .. } => Some(format!("end:{success}")),
            _ => None,
        })
        .collect()
}

/// The `will_retry` flag of each `agent_end` event, in order (pi's
/// `harness.eventsOfType("agent_end").map((e) => e.willRetry)`).
fn agent_end_will_retry(harness: &Harness) -> Vec<bool> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::AgentEnd { will_retry, .. } => Some(*will_retry),
            _ => None,
        })
        .collect()
}

/// The `final_error` payloads of every `auto_retry_end` event.
fn auto_retry_end_final_errors(harness: &Harness) -> Vec<Option<String>> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::AutoRetryEnd { final_error, .. } => Some(final_error.clone()),
            _ => None,
        })
        .collect()
}

/// Count the `auto_retry_start` events.
fn auto_retry_start_count(harness: &Harness) -> usize {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, AgentSessionEvent::AutoRetryStart { .. }))
        .count()
}

/// Count the `agent_end` events.
fn agent_end_count(harness: &Harness) -> usize {
    agent_end_will_retry(harness).len()
}

/// A harness with retry enabled, `maxRetries` = `max_retries`, `baseDelayMs` = 1.
fn retry_harness(max_retries: i64) -> Harness {
    create_harness(HarnessOptions {
        settings: Some(retry_settings(true, Some(max_retries), Some(1))),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// agent-session-retry-events.test.ts
// ---------------------------------------------------------------------------

#[test]
fn retries_after_a_transient_error_and_succeeds() {
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("recovered"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(retry_event_log(&harness), vec!["start:1", "end:true"]);
    assert_eq!(agent_end_will_retry(&harness), vec![true, false]);
    assert_eq!(harness.call_count(), 2);
    assert!(!harness.session.is_retrying());
}

#[test]
fn retries_multiple_transient_failures_and_succeeds_on_the_final_attempt() {
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("success"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(
        retry_event_log(&harness),
        vec!["start:1", "start:2", "end:true"]
    );
    assert_eq!(harness.call_count(), 3);
}

#[test]
fn exhausts_max_retries_and_emits_a_failure_event() {
    let harness = retry_harness(2);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(
        retry_event_log(&harness),
        vec!["start:1", "start:2", "end:false"]
    );
    assert_eq!(agent_end_will_retry(&harness), vec![true, true, false]);
    assert_eq!(harness.call_count(), 3);
    assert!(!harness.session.is_retrying());
}

#[test]
fn prompt_waits_for_retry_completion_and_returns_only_when_the_loop_is_done() {
    // pi injects a 40ms delay into the assistant `message_end` handler to prove
    // `prompt()` still waits for the retry loop. Under the eager/sync model
    // `prompt()` always blocks to loop completion, so the async delay is N/A; the
    // load-bearing assertions (the retry ran and no retry is in progress) hold.
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("recovered"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(harness.call_count(), 2);
    assert!(!harness.session.is_retrying());
}

#[test]
fn does_not_retry_when_retry_is_disabled() {
    let harness = create_harness(HarnessOptions {
        settings: Some(retry_settings(false, None, None)),
        ..Default::default()
    });
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_error(
        "overloaded_error",
    )))]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(harness.call_count(), 1);
    assert_eq!(auto_retry_start_count(&harness), 0);
}

#[test]
fn does_not_retry_non_retryable_errors() {
    let harness = retry_harness(3);
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_error(
        "invalid_api_key",
    )))]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(harness.call_count(), 1);
    assert_eq!(auto_retry_start_count(&harness), 0);
}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — cancelling the backoff mid-sleep \
            needs a second thread to trip the retry abort signal while prompt() blocks \
            the drive thread; the actor-pattern abort-handle wiring lands with the RPC \
            turn commands. abort_retry/is_retrying/abortable_sleep are implemented and \
            unit-covered idle in retry::tests"]
fn cancels_retry_sleep_when_abort_retry_is_called() {}

#[test]
fn waits_for_the_full_loop_when_retry_recovery_produces_tool_calls() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        settings: Some(retry_settings(true, Some(3), Some(1))),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Message(Box::new(assistant_text("final answer"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    assert_eq!(harness.call_count(), 3);
    assert_eq!(*tool_runs.lock().unwrap(), vec!["hello".to_string()]);
    assert!(!harness.session.is_streaming());

    // A follow-up prompt must work (no "already processing" error).
    harness.session.prompt("follow-up", None, None).unwrap();
    assert_eq!(harness.call_count(), 4);
}

#[test]
fn emits_extension_events_before_public_event_subscribers() {
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let order_for_runner = Arc::clone(&order);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_event_order(order_for_runner))
        })),
        ..Default::default()
    });

    let order_for_listener = Arc::clone(&order);
    let _unsubscribe = harness
        .session
        .subscribe(Arc::new(move |event: &AgentSessionEvent| match event {
            AgentSessionEvent::MessageStart { message } => {
                let role = message
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                order_for_listener
                    .lock()
                    .unwrap()
                    .push(format!("public:message_start:{role}"));
            }
            AgentSessionEvent::MessageEnd { message } => {
                let role = message
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                order_for_listener
                    .lock()
                    .unwrap()
                    .push(format!("public:message_end:{role}"));
            }
            _ => {}
        }));

    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "done",
    )))]);
    harness.session.prompt("hi", None, None).unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        vec![
            "extension:message_start:user",
            "public:message_start:user",
            "extension:message_end:user",
            "public:message_end:user",
            "extension:message_start:assistant",
            "public:message_start:assistant",
            "extension:message_end:assistant",
            "public:message_end:assistant",
        ]
    );
}

#[test]
#[ignore = "unit5: the mock stream emits only the terminal done/error event (the \
            eager single-message form), so no message_update deltas are produced; the \
            full single-prompt event order incl. message_update needs a delta-emitting \
            faux provider. Message-role ordering is covered in turn::tests"]
fn emits_the_expected_event_order_for_a_single_prompt() {}

#[test]
#[ignore = "unit5: the mock stream emits only the terminal done/error event, so no \
            message_update deltas are produced; the full tool-call event order incl. \
            message_update needs a delta-emitting faux provider. The tool-call turn \
            shape is covered in turn::tests"]
fn emits_the_expected_event_order_for_a_tool_call_turn() {}

#[test]
#[ignore = "unit5: the mock stream emits only the terminal done/error event and never \
            the thinking/text/toolcall deltas; message_update delta characterization \
            needs a delta-emitting faux provider"]
fn emits_streaming_deltas_for_text_thinking_and_tool_calls() {}

#[test]
fn emits_agent_end_for_error_responses() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_error(
        "broken",
    )))]);

    harness.session.prompt("hi", None, None).unwrap();

    assert_eq!(agent_end_count(&harness), 1);
    assert!(matches!(
        harness.events.lock().unwrap().last(),
        Some(AgentSessionEvent::AgentSettled)
    ));
}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — aborting a run mid-stream needs a \
            second thread to trip the agent abort signal while prompt() blocks the \
            drive thread; the actor-pattern abort-handle wiring lands with the RPC \
            turn commands"]
fn emits_agent_end_for_aborted_runs_and_persists_the_aborted_assistant_message() {}

// ---------------------------------------------------------------------------
// agent-session-retry.test.ts
// ---------------------------------------------------------------------------

#[test]
fn retry_suite_retries_after_a_transient_error_and_succeeds() {
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("Success"))),
    ]);

    harness.session.prompt("Test", None, None).unwrap();

    assert_eq!(harness.call_count(), 2);
    assert_eq!(retry_event_log(&harness), vec!["start:1", "end:true"]);
    assert!(!harness.session.is_retrying());
}

#[test]
fn retry_suite_exhausts_max_retries_and_emits_failure() {
    let harness = retry_harness(2);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
    ]);

    harness.session.prompt("Test", None, None).unwrap();

    assert_eq!(harness.call_count(), 3);
    let log = retry_event_log(&harness);
    assert!(log.contains(&"start:1".to_string()));
    assert!(log.contains(&"start:2".to_string()));
    assert!(log.contains(&"end:false".to_string()));
    assert!(!harness.session.is_retrying());
}

#[test]
fn retry_suite_prompt_waits_for_retry_completion_when_message_end_is_delayed() {
    // pi delays the assistant `message_end` handler by 40ms; N/A under the eager
    // model where `prompt()` always blocks to loop completion.
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("Success"))),
    ]);

    harness.session.prompt("Test", None, None).unwrap();

    assert_eq!(harness.call_count(), 2);
    assert!(!harness.session.is_retrying());
}

#[test]
fn retry_suite_retries_provider_network_error_failures() {
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error(
            "Provider finish_reason: network_error",
        ))),
        FauxResponse::Message(Box::new(assistant_text("Recovered after retry"))),
    ]);

    harness.session.prompt("Test", None, None).unwrap();

    assert_eq!(harness.call_count(), 2);
    assert_eq!(retry_event_log(&harness), vec!["start:1", "end:true"]);
}

#[test]
fn retry_suite_prompt_waits_for_full_agent_loop_when_retry_produces_tool_calls() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        settings: Some(retry_settings(true, Some(3), Some(1))),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Message(Box::new(assistant_text("Final answer."))),
    ]);

    harness.session.prompt("Test", None, None).unwrap();

    assert_eq!(harness.call_count(), 3);
    assert_eq!(*tool_runs.lock().unwrap(), vec!["hello".to_string()]);
    assert!(!harness.session.is_streaming());

    harness.session.prompt("Follow-up", None, None).unwrap();
    assert_eq!(harness.call_count(), 4);
}

// ---------------------------------------------------------------------------
// Give-up / abort-branch unit coverage (idle, no concurrency required)
// ---------------------------------------------------------------------------

#[test]
fn is_not_retrying_between_turns() {
    let harness = retry_harness(3);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_text("recovered"))),
    ]);

    assert!(!harness.session.is_retrying());
    harness.session.prompt("test", None, None).unwrap();
    // The backoff signal is cleared once the retry completes.
    assert!(!harness.session.is_retrying());
}

#[test]
fn final_error_carries_the_terminal_error_message_on_give_up() {
    let harness = retry_harness(1);
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_error("overloaded_error"))),
        FauxResponse::Message(Box::new(assistant_error("service_unavailable 503"))),
    ]);

    harness.session.prompt("test", None, None).unwrap();

    // One start (attempt 1), then give up: the terminal failure carries the last
    // assistant error message, not the "Retry cancelled" abort string.
    assert_eq!(retry_event_log(&harness), vec!["start:1", "end:false"]);
    assert_eq!(
        auto_retry_end_final_errors(&harness),
        vec![Some("service_unavailable 503".to_string())]
    );
}
