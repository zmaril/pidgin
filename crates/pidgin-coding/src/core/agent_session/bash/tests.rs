//! Bash + persistence tests, ported from pi's
//! `test/suite/agent-session-bash-persistence.test.ts`.
//!
//! Each case mirrors a pi `AgentSession bash and persistence characterization`
//! case: same in-memory harness ([`super::super::test_support`]), same assertions
//! on the recorded `bashExecution` message, the pending/flush behavior, and the
//! session-entry ordering. Cases whose premise strictly requires genuine
//! in-flight streaming (a bash result recorded from a live mid-run hook, or a
//! run aborted mid-stream) are structurally impossible under the sync/eager,
//! `!Send` session (see the module note in [`super::super`]) and are `#[ignore]`d
//! with that reason; the deferral + flush-into-next-turn behavior they cover is
//! exercised directly via the run-active flag.

// straitjacket-allow-file:duplication

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::providers::faux::faux_tool_call;

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::queue::CustomMessageInput;
use crate::core::agent_session::test_support::{
    assistant_text, assistant_tool_use, create_harness, echo_tool, events_of_type, FauxResponse,
    Harness, HarnessOptions,
};
use crate::core::tools::bash::{BashError, BashExecOptions, BashExecResult, BashOperations};

use super::{BashResult, ExecuteBashOptions};

/// A [`BashResult`] with the given output/exit code and no truncation/spill,
/// matching pi's inline `{ output, exitCode, cancelled: false, truncated: false }`
/// literals.
fn ok_result(output: &str, exit_code: i32) -> BashResult {
    BashResult {
        output: output.to_string(),
        exit_code: Some(exit_code),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    }
}

/// Whether any message currently in agent state has the `bashExecution` role.
fn has_bash_execution(harness: &Harness) -> bool {
    harness
        .session
        .messages()
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("bashExecution"))
}

/// A [`BashOperations`] that emits a single fixed output chunk then exits 0 (pi's
/// per-test `operations` that calls `options.onData(Buffer.from(...))`).
struct EmitOps {
    text: &'static str,
}

impl BashOperations for EmitOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        mut opts: BashExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<BashExecResult, BashError>> + 'a>> {
        Box::pin(async move {
            (opts.on_data)(self.text.as_bytes());
            Ok(BashExecResult { exit_code: Some(0) })
        })
    }
}

/// A [`BashOperations`] that never completes until its abort signal is tripped,
/// then fails with [`BashError::Aborted`] (pi's per-test `operations` whose
/// `exec` promise rejects on `signal` abort).
struct BlockOps;

impl BashOperations for BlockOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        opts: BashExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<BashExecResult, BashError>> + 'a>> {
        Box::pin(async move {
            match opts.signal {
                Some(mut rx) => loop {
                    if *rx.borrow() {
                        return Err(BashError::Aborted);
                    }
                    if rx.changed().await.is_err() {
                        return Err(BashError::Aborted);
                    }
                },
                None => Err(BashError::Aborted),
            }
        })
    }
}

#[test]
fn records_bash_results_immediately_while_idle() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .record_bash_result("echo hi", &ok_result("hi", 0), None);

    assert!(!harness.session.has_pending_bash_messages());
    let messages = harness.session.messages();
    assert_eq!(
        messages.last().unwrap().get("role").and_then(Value::as_str),
        Some("bashExecution")
    );
    let entry_types: Vec<&str> = harness
        .session
        .session_manager()
        .get_entries()
        .iter()
        .map(|e| e.type_str())
        .collect();
    assert!(entry_types.contains(&"message"));
}

#[test]
fn defers_bash_results_while_streaming_and_flushes_them_before_the_next_prompt() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "after flush",
    )))]);

    // pi defers a bash result recorded mid-turn (while streaming) so it does not
    // break tool_use/tool_result ordering. The sync/eager, !Send session cannot
    // record from a live mid-run hook (see the module note), so the run-active
    // flag is flipped directly to exercise the same deferral path.
    harness.session.set_agent_run_active(true);
    harness
        .session
        .record_bash_result("echo hi", &ok_result("hi", 0), None);

    assert!(harness.session.has_pending_bash_messages());
    assert!(!has_bash_execution(&harness));

    // Clear the run flag and drive the next prompt: its preflight flushes the
    // pending bash message into agent state + the session before the new turn
    // (pi asserts the same after `await session.prompt("next turn")`).
    harness.session.set_agent_run_active(false);
    harness.session.prompt("next turn", None, None).unwrap();

    assert!(!harness.session.has_pending_bash_messages());
    assert!(has_bash_execution(&harness));
    let message_entries = harness
        .session
        .session_manager()
        .get_entries()
        .iter()
        .filter(|e| e.type_str() == "message")
        .count();
    assert!(message_entries > 0);
}

#[cfg(unix)]
#[tokio::test]
async fn executes_bash_commands_and_records_the_result() {
    let harness = create_harness(HarnessOptions::default());

    let result = harness
        .session
        .execute_bash("printf 'hello'", None, ExecuteBashOptions::default())
        .await
        .unwrap();

    assert!(result.output.contains("hello"));
    assert_eq!(
        harness
            .session
            .messages()
            .last()
            .unwrap()
            .get("role")
            .and_then(Value::as_str),
        Some("bashExecution")
    );
}

#[tokio::test]
async fn cancels_running_bash_commands_with_abort_bash() {
    let harness = create_harness(HarnessOptions::default());
    let ops: Arc<dyn BashOperations> = Arc::new(BlockOps);

    let fut = harness.session.execute_bash(
        "sleep",
        None,
        ExecuteBashOptions {
            operations: Some(ops),
            ..Default::default()
        },
    );
    tokio::pin!(fut);

    // Drive the future until the command is running (arming the abort signal),
    // then trip it and let the run resolve.
    let result = loop {
        tokio::select! {
            biased;
            r = &mut fut => break r,
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                if harness.session.is_bash_running() {
                    harness.session.abort_bash();
                }
            }
        }
    };

    let result = result.unwrap();
    assert!(result.cancelled);
    assert!(!harness.session.is_bash_running());
}

#[test]
fn persists_user_assistant_tool_result_and_custom_messages_in_order() {
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

    harness
        .session
        .send_custom_message(
            CustomMessageInput {
                custom_type: "note".to_string(),
                content: json!("hello"),
                display: true,
                details: Some(json!({ "a": 1 })),
            },
            false,
            None,
        )
        .unwrap();
    harness.session.prompt("start", None, None).unwrap();

    let entry_types: Vec<&str> = harness
        .session
        .session_manager()
        .get_entries()
        .iter()
        .map(|e| e.type_str())
        .collect();
    assert_eq!(
        entry_types,
        vec!["custom_message", "message", "message", "message", "message"]
    );
    assert_eq!(
        harness.message_roles(),
        vec!["custom", "user", "assistant", "toolResult", "assistant"]
    );
}

#[test]
fn does_not_emit_message_end_for_bash_execution_messages() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .record_bash_result("echo hi", &ok_result("hi", 0), None);

    let message_end_count = events_of_type(&harness, |e| {
        matches!(e, AgentSessionEvent::MessageEnd { .. })
    });
    assert_eq!(message_end_count, 0);
}

#[test]
#[ignore = "unit5: sync/eager + !Send AgentSession — aborting a run mid-stream needs a \
            second thread to trip the agent abort signal while prompt() blocks the drive \
            thread (the mock stream emits only the terminal event, so there is no in-flight \
            window); the actor-pattern abort-handle wiring lands with the RPC turn commands"]
fn persists_aborted_assistant_messages() {}

#[tokio::test]
async fn records_bash_output_through_custom_operations() {
    let harness = create_harness(HarnessOptions::default());
    let ops: Arc<dyn BashOperations> = Arc::new(EmitOps {
        text: "hello from custom ops",
    });

    let result = harness
        .session
        .execute_bash(
            "custom",
            None,
            ExecuteBashOptions {
                operations: Some(ops),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(result.output.contains("hello from custom ops"));
    assert_eq!(
        harness
            .session
            .messages()
            .last()
            .unwrap()
            .get("role")
            .and_then(Value::as_str),
        Some("bashExecution")
    );
}
