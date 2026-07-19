//! Tests for the agent loop.
//!
//! Direct ports of `packages/agent/test/agent-loop.test.ts` (driven by an eager
//! mock stream that mirrors the TS `MockAssistantStream` — a single terminal
//! `done`/`error` event carrying the final message, with no intermediate
//! deltas), plus a handful of supplementary cases for the parallel
//! completion-ordering and abort branches the TS suite under-exercises.
//!
//! Where a TS case asserts on wall-clock parallelism (a tool "observing" that a
//! sibling has not yet resolved), the eager/synchronous model has no such timing;
//! those cases are ported by asserting on the deterministic **event ordering**
//! that distinguishes the sequential and parallel code paths, and on the
//! source-order result invariant — the load-bearing golden. Each such adaptation
//! is called out inline.

// straitjacket-allow-file:duplication — each `#[test]` rebuilds near-identical
// contexts, mock streams, and config from the shared helpers and asserts on the
// same event/message shapes by design; the clone detector reads these parallel
// ported cases as duplicates. Collapsing them would obscure which pi test each
// case mirrors.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantOptions};
use pidgin_ai::seams::provider::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Message, StopReason, StreamOptions,
    UserContent,
};

use super::*;
use crate::types::{
    AfterToolCallResult, AgentContext, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult,
    BeforeToolCallResult, ConvertToLlm, GetFollowUpMessages, GetSteeringMessages, PrepareArguments,
    ToolExecutionMode,
};

mod steering;
mod supplementary;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mock_model() -> pidgin_ai::Model {
    use pidgin_ai::providers::faux::{FauxProvider, RegisterFauxProviderOptions};
    FauxProvider::new(RegisterFauxProviderOptions::default())
        .get_model(None)
        .expect("faux has a default model")
}

/// Build an assistant message with the given content and stop reason (the port's
/// `createAssistantMessage`).
fn assistant_message(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    faux_assistant_message(
        content,
        FauxAssistantOptions {
            stop_reason: Some(stop_reason),
            ..Default::default()
        },
        0,
    )
}

/// Build a user [`AgentMessage`] value (the port's `createUserMessage`).
fn user_message(text: &str) -> AgentMessage {
    json!({ "role": "user", "content": text, "timestamp": 0 })
}

/// The eager port of the TS `MockAssistantStream`: a [`StreamResult`] whose only
/// event is the terminal `done`/`error` carrying the final message.
fn mock_stream(message: AssistantMessage) -> StreamResult {
    let reason = message.stop_reason;
    let event = if matches!(reason, StopReason::Error | StopReason::Aborted) {
        AssistantMessageEvent::Error {
            reason,
            error: message.clone(),
        }
    } else {
        AssistantMessageEvent::Done {
            reason,
            message: message.clone(),
        }
    };
    StreamResult {
        events: vec![event],
        message,
    }
}

/// A [`StreamFn`] that replays `responses` in order, one per call.
fn stream_fn_from(responses: Vec<AssistantMessage>) -> StreamFn {
    let responses = Arc::new(responses);
    let index = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_model, _ctx, _opts, _signal| {
        let i = index.fetch_add(1, Ordering::SeqCst);
        mock_stream(responses.get(i).cloned().expect("a queued response"))
    })
}

/// The identity converter used across the TS suite: passes through only
/// `user`/`assistant`/`toolResult` messages.
fn identity_converter() -> ConvertToLlm {
    Arc::new(|messages: &[AgentMessage]| {
        messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role").and_then(Value::as_str)?;
                if matches!(role, "user" | "assistant" | "toolResult") {
                    serde_json::from_value::<Message>(m.clone()).ok()
                } else {
                    None
                }
            })
            .collect()
    })
}

/// A config with the identity converter and no hooks (the TS default).
fn base_config() -> AgentLoopConfig {
    AgentLoopConfig {
        stream_options: StreamOptions::default(),
        reasoning: None,
        model: mock_model(),
        convert_to_llm: identity_converter(),
        transform_context: None,
        get_api_key: None,
        should_stop_after_turn: None,
        prepare_next_turn: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        tool_execution: None,
        before_tool_call: None,
        after_tool_call: None,
    }
}

/// A per-value predicate deciding whether an echo result sets `terminate`.
type TerminatePredicate = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The echo tool used across the TS suite; records each executed `value`.
fn echo_tool(executed: Arc<Mutex<Vec<String>>>) -> AgentTool {
    echo_tool_with(executed, None, None)
}

/// An echo tool with an optional execution mode and an optional per-value
/// `terminate` predicate.
fn echo_tool_with(
    executed: Arc<Mutex<Vec<String>>>,
    execution_mode: Option<ToolExecutionMode>,
    terminate_when: Option<TerminatePredicate>,
) -> AgentTool {
    AgentTool {
        name: "echo".into(),
        description: "Echo tool".into(),
        parameters: json!({ "type": "object" }),
        label: "Echo".into(),
        prepare_arguments: None,
        execution_mode,
        execute: Arc::new(move |_id, args, _signal, _cb| {
            let value = args
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            executed.lock().unwrap().push(value.clone());
            let terminate = terminate_when.as_ref().map(|p| p(&value));
            AgentToolResult {
                content: vec![ContentBlock::Text {
                    text: format!("echoed: {value}"),
                    text_signature: None,
                }],
                details: json!({ "value": value }),
                added_tool_names: None,
                terminate,
            }
        }),
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.into(),
        text_signature: None,
    }
}

fn tool_call_block(name: &str, id: &str, arguments: Value) -> ContentBlock {
    faux_tool_call(name, arguments, Some(id.into()))
}

/// A short label for an event, for sequence assertions.
fn event_type(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

fn role_of(message: &AgentMessage) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn tool_execution_end_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

fn tool_result_message_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } if role_of(message) == Some("toolResult") => message
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Direct ports of agent-loop.test.ts — describe("agentLoop with AgentMessage")
// ---------------------------------------------------------------------------

#[test]
fn should_emit_events_with_agent_message_types() {
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![assistant_message(
        vec![text_block("Hi there!")],
        StopReason::Stop,
    )]);

    let outcome = agent_loop(
        vec![user_message("Hello")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert_eq!(outcome.messages.len(), 2);
    assert_eq!(role_of(&outcome.messages[0]), Some("user"));
    assert_eq!(role_of(&outcome.messages[1]), Some("assistant"));

    let types: Vec<&str> = outcome.events.iter().map(event_type).collect();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(types.contains(&expected), "missing event {expected}");
    }
}

#[test]
fn should_handle_custom_message_types_via_convert_to_llm() {
    let notification = json!({
        "role": "notification",
        "text": "This is a notification",
        "timestamp": 0,
    });
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![notification],
        tools: Some(vec![]),
    };

    let converted: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
    let converted_capture = converted.clone();
    let mut config = base_config();
    config.convert_to_llm = Arc::new(move |messages: &[AgentMessage]| {
        let result: Vec<Message> = messages
            .iter()
            .filter(|m| role_of(m) != Some("notification"))
            .filter_map(|m| {
                let role = role_of(m)?;
                if matches!(role, "user" | "assistant" | "toolResult") {
                    serde_json::from_value::<Message>(m.clone()).ok()
                } else {
                    None
                }
            })
            .collect();
        *converted_capture.lock().unwrap() = result.clone();
        result
    });

    let stream_fn = stream_fn_from(vec![assistant_message(
        vec![text_block("Response")],
        StopReason::Stop,
    )]);

    agent_loop(
        vec![user_message("Hello")],
        context,
        config,
        None,
        &stream_fn,
    );

    let converted = converted.lock().unwrap();
    assert_eq!(converted.len(), 1);
    assert!(matches!(converted[0], Message::User(_)));
}

#[test]
fn should_apply_transform_context_before_convert_to_llm() {
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![
            user_message("old message 1"),
            to_agent_message(&assistant_message(
                vec![text_block("old response 1")],
                StopReason::Stop,
            )),
            user_message("old message 2"),
            to_agent_message(&assistant_message(
                vec![text_block("old response 2")],
                StopReason::Stop,
            )),
        ],
        tools: Some(vec![]),
    };

    let transformed_len: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let converted_len: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let mut config = base_config();
    let tl = transformed_len.clone();
    config.transform_context = Some(Arc::new(move |messages: &[AgentMessage], _signal| {
        let kept: Vec<AgentMessage> = messages.iter().rev().take(2).rev().cloned().collect();
        *tl.lock().unwrap() = kept.len();
        kept
    }));
    let cl = converted_len.clone();
    config.convert_to_llm = Arc::new(move |messages: &[AgentMessage]| {
        let result: Vec<Message> = messages
            .iter()
            .filter_map(|m| {
                let role = role_of(m)?;
                if matches!(role, "user" | "assistant" | "toolResult") {
                    serde_json::from_value::<Message>(m.clone()).ok()
                } else {
                    None
                }
            })
            .collect();
        *cl.lock().unwrap() = result.len();
        result
    });

    let stream_fn = stream_fn_from(vec![assistant_message(
        vec![text_block("Response")],
        StopReason::Stop,
    )]);

    agent_loop(
        vec![user_message("new message")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(*transformed_len.lock().unwrap(), 2);
    assert_eq!(*converted_len.lock().unwrap(), 2);
}

#[test]
fn should_handle_tool_calls_and_results() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert_eq!(*executed.lock().unwrap(), vec!["hello".to_string()]);

    let start = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }));
    let end = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    assert!(start.is_some());
    match end {
        Some(AgentEvent::ToolExecutionEnd { is_error, .. }) => assert!(!is_error),
        _ => panic!("expected tool_execution_end"),
    }
}

#[test]
fn should_not_execute_tool_calls_from_a_length_truncated_message() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = call_count.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        let i = cc.fetch_add(1, Ordering::SeqCst);
        let message = if i == 0 {
            assistant_message(
                vec![tool_call_block("echo", "tool-1", json!({ "value": "abc" }))],
                StopReason::Length,
            )
        } else {
            assistant_message(vec![text_block("done")], StopReason::Stop)
        };
        mock_stream(message)
    });

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    // The tool must never execute with potentially truncated arguments.
    assert!(executed.lock().unwrap().is_empty());

    let end = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    match end {
        Some(AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        }) => {
            assert!(is_error);
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            assert!(text.contains("output token limit"), "got: {text}");
        }
        _ => panic!("expected tool_execution_end"),
    }

    // The loop continues so the model can re-issue the tool call.
    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert_eq!(role_of(outcome.messages.last().unwrap()), Some("assistant"));
}

#[test]
fn before_tool_call_receives_validated_args() {
    // `beforeToolCall` is invoked with the validated args, and (with a no-op hook
    // that does not mutate them) those same args flow to `execute` — validation is
    // not re-run after the hook. See `before_tool_call_mutated_args_are_executed`
    // for the in-place-mutation adoption that mirrors pi's "should execute mutated
    // beforeToolCall args without revalidation".
    let executed = Arc::new(Mutex::new(Vec::new()));
    let seen_args: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let mut config = base_config();
    let seen = seen_args.clone();
    config.before_tool_call = Some(Arc::new(move |ctx: &mut BeforeToolCallContext, _signal| {
        *seen.lock().unwrap() = Some(ctx.args.clone());
        None
    }));

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(
        seen_args.lock().unwrap().clone(),
        Some(json!({ "value": "hello" }))
    );
    assert_eq!(*executed.lock().unwrap(), vec!["hello".to_string()]);
}

#[test]
fn before_tool_call_mutated_args_are_executed() {
    // Port of pi's "should execute mutated beforeToolCall args without
    // revalidation" (agent-loop.test.ts): `beforeToolCall` rewrites `args.value`
    // in place and the loop executes the tool with the mutated args — the string
    // "hello" is replaced with the number 123 (validation is not re-run), and the
    // tool must observe 123.
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = executed.clone();
    let tool = AgentTool {
        name: "echo".into(),
        description: "Echo tool".into(),
        parameters: json!({ "type": "object" }),
        label: "Echo".into(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(move |_id, args, _signal, _cb| {
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            recorded.lock().unwrap().push(value.clone());
            AgentToolResult {
                content: vec![ContentBlock::Text {
                    text: format!("echoed: {value}"),
                    text_signature: None,
                }],
                details: json!({ "value": value }),
                added_tool_names: None,
                terminate: None,
            }
        }),
    };

    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![tool]),
    };

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(move |ctx: &mut BeforeToolCallContext, _signal| {
        // Mutate the validated args in place, exactly as pi's case does.
        ctx.args["value"] = json!(123);
        None
    }));

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(*executed.lock().unwrap(), vec![json!(123)]);
}

#[test]
fn should_prepare_tool_arguments_for_validation() {
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_capture = executed.clone();

    let prepare: PrepareArguments = Arc::new(|args: &Value| {
        if !args.is_object() {
            return args.clone();
        }
        let old = args.get("oldText").and_then(Value::as_str);
        let new = args.get("newText").and_then(Value::as_str);
        match (old, new) {
            (Some(o), Some(n)) => {
                let mut edits = args
                    .get("edits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                edits.push(json!({ "oldText": o, "newText": n }));
                json!({ "edits": edits })
            }
            _ => args.clone(),
        }
    });

    let tool = AgentTool {
        name: "edit".into(),
        description: "Edit tool".into(),
        parameters: json!({ "type": "object" }),
        label: "Edit".into(),
        prepare_arguments: Some(prepare),
        execution_mode: None,
        execute: Arc::new(move |_id, args, _signal, _cb| {
            let edits = args.get("edits").cloned().unwrap_or(json!([]));
            executed_capture.lock().unwrap().push(edits.clone());
            let count = edits.as_array().map(|a| a.len()).unwrap_or(0);
            AgentToolResult {
                content: vec![text_block(&format!("edited {count}"))],
                details: json!({ "count": count }),
                added_tool_names: None,
                terminate: None,
            }
        }),
    };

    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![tool]),
    };

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "edit",
                "tool-1",
                json!({ "oldText": "before", "newText": "after" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    agent_loop(
        vec![user_message("edit something")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert_eq!(
        *executed.lock().unwrap(),
        vec![json!([{ "oldText": "before", "newText": "after" }])]
    );
}

#[test]
fn parallel_persists_tool_results_in_source_order_despite_end_inversion() {
    // Port of "should emit tool_execution_end in completion order but persist tool
    // results in source order". pi drives the inversion with async timing (a slow
    // first tool releases after the second finishes). The eager model has no such
    // timing; the same end-order inversion is produced deterministically by mixing
    // a *prepared* call (tool-1, whose end fires in phase 2) with an *immediate*
    // call (tool-2 targets a missing tool, whose end fires in phase 1). The asserted
    // orderings are identical to pi's: ends invert to [tool-2, tool-1] while the
    // persisted result messages stay in source order [tool-1, tool-2].
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![
                tool_call_block("echo", "tool-1", json!({ "value": "first" })),
                tool_call_block("ghost", "tool-2", json!({ "value": "second" })),
            ],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("echo both")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(
        tool_execution_end_ids(&outcome.events),
        vec!["tool-2".to_string(), "tool-1".to_string()]
    );
    assert_eq!(
        tool_result_message_ids(&outcome.events),
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );

    let turn_tool_result_ids: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TurnEnd { tool_results, .. } if !tool_results.is_empty() => Some(
                tool_results
                    .iter()
                    .map(|tr| tr.tool_call_id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        turn_tool_result_ids,
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
}

#[test]
fn should_inject_queued_messages_after_all_tool_calls_complete() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let queued_delivered = Arc::new(Mutex::new(false));
    let saw_interrupt = Arc::new(Mutex::new(false));

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Sequential);
    let ex = executed.clone();
    let qd = queued_delivered.clone();
    let steering: GetSteeringMessages = Arc::new(move || {
        if !ex.lock().unwrap().is_empty() && !*qd.lock().unwrap() {
            *qd.lock().unwrap() = true;
            vec![user_message("interrupt")]
        } else {
            vec![]
        }
    });
    config.get_steering_messages = Some(steering);

    let call_index = Arc::new(AtomicUsize::new(0));
    let ci = call_index.clone();
    let saw = saw_interrupt.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, ctx, _opts, _signal| {
        let i = ci.fetch_add(1, Ordering::SeqCst);
        if i == 1 {
            let found = ctx.messages.iter().any(|m| {
                matches!(m, Message::User(u) if u.content == UserContent::Text("interrupt".into()))
            });
            *saw.lock().unwrap() = found;
        }
        let message = if i == 0 {
            assistant_message(
                vec![
                    tool_call_block("echo", "tool-1", json!({ "value": "first" })),
                    tool_call_block("echo", "tool-2", json!({ "value": "second" })),
                ],
                StopReason::ToolUse,
            )
        } else {
            assistant_message(vec![text_block("done")], StopReason::Stop)
        };
        mock_stream(message)
    });

    let outcome = agent_loop(
        vec![user_message("start")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(
        *executed.lock().unwrap(),
        vec!["first".to_string(), "second".to_string()]
    );

    let tool_ends: Vec<bool> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(tool_ends, vec![false, false]);

    let sequence: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart { message } => match role_of(message) {
                Some("toolResult") => Some(format!(
                    "tool:{}",
                    message["toolCallId"].as_str().unwrap_or_default()
                )),
                Some("user") => message
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let interrupt_idx = sequence.iter().position(|s| s == "interrupt");
    assert!(
        interrupt_idx.is_some(),
        "interrupt not injected: {sequence:?}"
    );
    let interrupt_idx = interrupt_idx.unwrap();
    let t1 = sequence.iter().position(|s| s == "tool:tool-1").unwrap();
    let t2 = sequence.iter().position(|s| s == "tool:tool-2").unwrap();
    assert!(t1 < interrupt_idx);
    assert!(t2 < interrupt_idx);

    assert!(*saw_interrupt.lock().unwrap());
}

#[test]
fn should_force_sequential_when_a_tool_declares_sequential_mode() {
    // Adapted port of "should force sequential execution when a tool has
    // executionMode=sequential even with default parallel config". pi asserts the
    // absence of wall-clock overlap; the eager port instead asserts the sequential
    // event interleaving (each tool's end precedes the next tool's start) that only
    // the sequential path produces, plus source-order result persistence.
    let executed = Arc::new(Mutex::new(Vec::new()));
    let slow = echo_tool_with(executed.clone(), Some(ToolExecutionMode::Sequential), None);
    // name the tool "slow" to mirror the TS fixture
    let slow = AgentTool {
        name: "slow".into(),
        ..slow
    };
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![slow]),
    };

    // config is parallel (default), but the tool forces sequential.
    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![
                tool_call_block("slow", "tool-1", json!({ "value": "first" })),
                tool_call_block("slow", "tool-2", json!({ "value": "second" })),
            ],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("run both")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert!(is_sequential_event_order(&outcome.events));
    assert_eq!(
        tool_result_message_ids(&outcome.events),
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
}

#[test]
fn should_force_sequential_when_one_of_multiple_tools_declares_sequential() {
    let execution_order = Arc::new(Mutex::new(Vec::new()));

    let eo1 = execution_order.clone();
    let slow = AgentTool {
        name: "slow".into(),
        description: "Slow tool".into(),
        parameters: json!({ "type": "object" }),
        label: "Slow".into(),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        execute: Arc::new(move |_id, args, _signal, _cb| {
            let v = args
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            eo1.lock().unwrap().push(format!("slow:{v}"));
            AgentToolResult {
                content: vec![text_block(&format!("slow: {v}"))],
                details: json!({ "value": v }),
                added_tool_names: None,
                terminate: None,
            }
        }),
    };
    let eo2 = execution_order.clone();
    let fast = AgentTool {
        name: "fast".into(),
        description: "Fast tool".into(),
        parameters: json!({ "type": "object" }),
        label: "Fast".into(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(move |_id, args, _signal, _cb| {
            let v = args
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            eo2.lock().unwrap().push(format!("fast:{v}"));
            AgentToolResult {
                content: vec![text_block(&format!("fast: {v}"))],
                details: json!({ "value": v }),
                added_tool_names: None,
                terminate: None,
            }
        }),
    };

    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![slow, fast]),
    };

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![
                tool_call_block("slow", "tool-1", json!({ "value": "a" })),
                tool_call_block("fast", "tool-2", json!({ "value": "b" })),
            ],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("run both")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    let order = execution_order.lock().unwrap().clone();
    assert_eq!(order.first().map(String::as_str), Some("slow:a"));
    assert!(order.contains(&"fast:b".to_string()));
    assert!(is_sequential_event_order(&outcome.events));
}

#[test]
fn should_allow_parallel_when_all_tools_declare_parallel_mode() {
    // Adapted port of "should allow parallel execution when all tools have
    // executionMode=parallel". pi asserts wall-clock overlap; the eager port asserts
    // the parallel event batching (every tool's start precedes the first tool's end)
    // that only the parallel path produces.
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool_with(executed, Some(ToolExecutionMode::Parallel), None);
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![tool]),
    };

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![
                tool_call_block("echo", "tool-1", json!({ "value": "first" })),
                tool_call_block("echo", "tool-2", json!({ "value": "second" })),
            ],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("echo both")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert!(is_parallel_event_order(&outcome.events));
}

#[test]
fn should_use_prepare_next_turn_snapshot_before_continuing() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "first prompt".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed)]),
    };

    let prepared = Arc::new(Mutex::new(false));
    let mut config = base_config();
    let prep_flag = prepared.clone();
    config.prepare_next_turn = Some(Arc::new(move |ctx: &PrepareNextTurnContext| {
        if *prep_flag.lock().unwrap() {
            return None;
        }
        *prep_flag.lock().unwrap() = true;
        Some(crate::types::AgentLoopTurnUpdate {
            context: Some(AgentContext {
                system_prompt: "second prompt".into(),
                messages: ctx.context.messages.clone(),
                tools: ctx.context.tools.clone(),
            }),
            model: None,
            thinking_level: None,
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let second_prompt: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let lc = llm_calls.clone();
    let sp = second_prompt.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, ctx, _opts, _signal| {
        let n = lc.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 2 {
            *sp.lock().unwrap() = ctx.system_prompt.clone().unwrap_or_default();
        }
        let message = if n == 1 {
            assistant_message(
                vec![tool_call_block(
                    "echo",
                    "tool-1",
                    json!({ "value": "hello" }),
                )],
                StopReason::ToolUse,
            )
        } else {
            assistant_message(vec![text_block("done")], StopReason::Stop)
        };
        mock_stream(message)
    });

    agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(second_prompt.lock().unwrap().as_str(), "second prompt");
}

// ---------------------------------------------------------------------------
// Direct ports — describe("agentLoopContinue with AgentMessage")
// ---------------------------------------------------------------------------

#[test]
fn continue_throws_when_context_has_no_messages() {
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![]);
    let result = agent_loop_continue(context, base_config(), None, &stream_fn);
    assert_eq!(result.unwrap_err(), AgentLoopError::NoMessages);
}

#[test]
fn continue_throws_when_last_message_is_assistant() {
    // Not in the TS file's explicit cases but exercised by the second guard the TS
    // relies on (`Cannot continue from message role: assistant`).
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![to_agent_message(&assistant_message(
            vec![text_block("hi")],
            StopReason::Stop,
        ))],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![]);
    let result = agent_loop_continue(context, base_config(), None, &stream_fn);
    assert_eq!(result.unwrap_err(), AgentLoopError::ContinueFromAssistant);
}

#[test]
fn continue_from_existing_context_without_emitting_user_events() {
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![user_message("Hello")],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![assistant_message(
        vec![text_block("Response")],
        StopReason::Stop,
    )]);

    let outcome = agent_loop_continue(context, base_config(), None, &stream_fn).unwrap();

    // Should only return the new assistant message (not the existing user message).
    assert_eq!(outcome.messages.len(), 1);
    assert_eq!(role_of(&outcome.messages[0]), Some("assistant"));

    let message_end_roles: Vec<&str> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => role_of(message),
            _ => None,
        })
        .collect();
    assert_eq!(message_end_roles, vec!["assistant"]);
}

#[test]
fn continue_allows_custom_message_types_as_last_message() {
    let custom_message = json!({ "role": "custom", "text": "Hook content", "timestamp": 0 });
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![custom_message],
        tools: Some(vec![]),
    };

    let mut config = base_config();
    config.convert_to_llm = Arc::new(|messages: &[AgentMessage]| {
        messages
            .iter()
            .map(|m| {
                if role_of(m) == Some("custom") {
                    json!({
                        "role": "user",
                        "content": m.get("text").cloned().unwrap_or(json!("")),
                        "timestamp": m.get("timestamp").cloned().unwrap_or(json!(0)),
                    })
                } else {
                    m.clone()
                }
            })
            .filter_map(|m| {
                let role = m.get("role").and_then(Value::as_str)?;
                if matches!(role, "user" | "assistant" | "toolResult") {
                    serde_json::from_value::<Message>(m).ok()
                } else {
                    None
                }
            })
            .collect()
    });

    let stream_fn = stream_fn_from(vec![assistant_message(
        vec![text_block("Response to custom message")],
        StopReason::Stop,
    )]);

    let outcome = agent_loop_continue(context, config, None, &stream_fn).unwrap();
    assert_eq!(outcome.messages.len(), 1);
    assert_eq!(role_of(&outcome.messages[0]), Some("assistant"));
}

// ---------------------------------------------------------------------------
// Event-order classifiers used by the sequential/parallel adaptation tests.
// ---------------------------------------------------------------------------

/// True when, for a two-tool batch, each tool's `tool_execution_end` precedes the
/// next tool's `tool_execution_start` — the interleaving only the sequential path
/// produces.
fn is_sequential_event_order(events: &[AgentEvent]) -> bool {
    let starts: Vec<usize> = event_indices(events, |e| {
        matches!(e, AgentEvent::ToolExecutionStart { .. })
    });
    let ends: Vec<usize> =
        event_indices(events, |e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    if starts.len() < 2 || ends.len() < 2 {
        return false;
    }
    // end of tool #1 comes before start of tool #2.
    ends[0] < starts[1]
}

/// True when, for a two-tool batch, every `tool_execution_start` precedes the
/// first `tool_execution_end` — the batching only the parallel path produces.
fn is_parallel_event_order(events: &[AgentEvent]) -> bool {
    let starts: Vec<usize> = event_indices(events, |e| {
        matches!(e, AgentEvent::ToolExecutionStart { .. })
    });
    let ends: Vec<usize> =
        event_indices(events, |e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    if starts.len() < 2 || ends.is_empty() {
        return false;
    }
    starts[1] < ends[0]
}

fn event_indices(events: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| if pred(e) { Some(i) } else { None })
        .collect()
}
