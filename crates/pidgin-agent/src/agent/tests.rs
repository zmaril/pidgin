//! Tests for the [`Agent`].
//!
//! Ports of `packages/agent/test/agent.test.ts`, driven by an eager mock stream
//! that mirrors the TS `MockAssistantStream` (a single terminal `done`/`error`
//! event carrying the final message). pi's suite leans on real async
//! interleaving — a `prompt()` promise that stays pending while `steer()` /
//! `abort()` / a second `prompt()` race against it. The eager/synchronous model
//! has no such timing: a `prompt()` call runs the whole loop to completion before
//! returning. Those cases are adapted to the deterministic sync order — the
//! observable invariants pi's tests actually lock in (guard errors, shared abort
//! signal, listener completion, queue-drain order) are asserted directly, with
//! the re-entrancy driven from a subscriber (which runs *inside* the synchronous
//! run). Each adaptation is called out inline with `ADAPTED:`.

// straitjacket-allow-file:duplication — each `#[test]` builds near-identical
// mock streams, contexts, and config from the shared helpers and asserts on the
// same event/message shapes by design; the clone detector reads these parallel
// ported cases as duplicates. Collapsing them would obscure which pi test each
// case mirrors.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantOptions};
use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Model, ModelCost, StopReason,
    StreamOptions,
};

use super::*;
use crate::types::{
    AfterToolCallContext, AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext,
    PrepareNextTurnContext,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// A plain "text" assistant response (pi's `createAssistantMessage(text)`).
fn assistant_text(text: &str) -> AssistantMessage {
    assistant_message(vec![text_block(text)], StopReason::Stop)
}

/// An assistant tool-use message (pi's `createAssistantToolUseMessage`).
fn assistant_tool_use(content: Vec<ContentBlock>) -> AssistantMessage {
    assistant_message(content, StopReason::ToolUse)
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

/// A user [`AgentMessage`] value.
fn user_message(text: &str) -> AgentMessage {
    json!({ "role": "user", "content": [{ "type": "text", "text": text }], "timestamp": 0 })
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

/// A [`StreamFn`] that always replays the same single response.
fn stream_fn_once(message: AssistantMessage) -> StreamFn {
    Arc::new(move |_model, _ctx, _opts, _signal| mock_stream(message.clone()))
}

/// A subscriber that records every event into a shared vec.
fn recording_subscriber() -> (Arc<Mutex<Vec<AgentEvent>>>, Listener) {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let listener: Listener = Arc::new(move |event: &AgentEvent, _signal| {
        sink.lock().unwrap().push(event.clone());
    });
    (events, listener)
}

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

fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(event_type).collect()
}

fn role_of(message: &AgentMessage) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// A tool whose `execute` is `body`.
fn tool_with(
    name: &str,
    body: impl Fn(&str, &Value, Option<&AbortSignal>, Option<&AgentToolUpdateCallback>) -> AgentToolResult
        + Send
        + Sync
        + 'static,
) -> AgentTool {
    AgentTool {
        name: name.into(),
        description: "test tool".into(),
        parameters: json!({ "type": "object" }),
        label: name.into(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(body),
    }
}

fn ok_result(text: &str, terminate: Option<bool>) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(text)],
        details: json!({}),
        added_tool_names: None,
        terminate,
    }
}

/// A distinct model, for the custom-initial-state / mutator tests.
fn model_with_id(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "faux".into(),
        provider: "faux".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: Vec::new(),
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

// ---------------------------------------------------------------------------
// Construction & state
// ---------------------------------------------------------------------------

#[test]
fn creates_agent_with_default_state() {
    let agent = Agent::default();

    assert_eq!(agent.system_prompt(), "");
    assert_eq!(agent.model().id, "unknown");
    assert_eq!(agent.thinking_level(), ThinkingLevel::Off);
    assert!(agent.tools().is_empty());
    assert!(agent.messages().is_empty());
    assert!(!agent.is_streaming());
    assert!(agent.streaming_message().is_none());
    assert!(agent.pending_tool_calls().is_empty());
    assert!(agent.error_message().is_none());
}

#[test]
fn creates_agent_with_custom_initial_state() {
    let custom_model = model_with_id("gpt-4o-mini");
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(custom_model.clone()),
            thinking_level: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    });

    assert_eq!(agent.system_prompt(), "You are a helpful assistant.");
    assert_eq!(agent.model(), custom_model);
    assert_eq!(agent.thinking_level(), ThinkingLevel::Low);
}

#[test]
fn subscribe_emits_no_event_and_unsubscribe_works() {
    let agent = Agent::default();
    let count = Arc::new(AtomicUsize::new(0));

    let sink = count.clone();
    let subscription = agent.subscribe(Arc::new(move |_event, _signal| {
        sink.fetch_add(1, Ordering::SeqCst);
    }));

    // No event fires on subscribe.
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // State mutators don't emit events.
    agent.set_system_prompt("Test prompt");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.system_prompt(), "Test prompt");

    // Unsubscribe stops future delivery.
    subscription.unsubscribe();
    agent.set_system_prompt("Another prompt");
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn updates_state_with_mutators() {
    let agent = Agent::default();

    agent.set_system_prompt("Custom prompt");
    assert_eq!(agent.system_prompt(), "Custom prompt");

    let new_model = model_with_id("gemini-2.5-flash");
    agent.set_model(new_model.clone());
    assert_eq!(agent.model(), new_model);

    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(agent.thinking_level(), ThinkingLevel::High);

    // set tools copies; a returned vec is independent of internal state.
    agent.set_tools(vec![tool_with("test", |_, _, _, _| ok_result("ok", None))]);
    let mut tools = agent.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "test");
    tools.clear();
    assert_eq!(agent.tools().len(), 1); // internal unaffected by the copy

    // set messages copies.
    let messages = vec![user_message("Hello")];
    agent.set_messages(messages.clone());
    assert_eq!(agent.messages(), messages);

    // append.
    let new_message = json!({ "role": "assistant", "content": [{ "type": "text", "text": "Hi" }] });
    agent.push_message(new_message.clone());
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(agent.messages()[1], new_message);

    // clear.
    agent.set_messages(Vec::new());
    assert!(agent.messages().is_empty());
}

// ---------------------------------------------------------------------------
// Lifecycle & subscribers
// ---------------------------------------------------------------------------

#[test]
fn emits_full_lifecycle_events_for_run_failures() {
    // ADAPTED: pi triggers this by throwing from `streamFn`, which hits
    // `handleRunFailure`. The eager `StreamFn` cannot throw; a provider failure
    // is encoded as an `error` terminal event flowing through the loop's normal
    // error path, which produces the identical observable lifecycle and state.
    let failure = faux_assistant_message(
        Vec::new(),
        FauxAssistantOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("provider exploded".into()),
            ..Default::default()
        },
        0,
    );
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(failure)),
        ..Default::default()
    });
    let (events, listener) = recording_subscriber();
    agent.subscribe(listener);

    agent.prompt_text("hello", Vec::new()).unwrap();

    assert_eq!(
        event_types(&events.lock().unwrap()),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );

    let last = agent.messages().last().cloned().unwrap();
    assert_eq!(role_of(&last), Some("assistant"));
    assert_eq!(
        last.get("stopReason").and_then(Value::as_str),
        Some("error")
    );
    assert_eq!(
        last.get("errorMessage").and_then(Value::as_str),
        Some("provider exploded")
    );
    assert_eq!(agent.error_message().as_deref(), Some("provider exploded"));
}

#[test]
fn subscribers_finish_before_prompt_returns() {
    // ADAPTED: pi asserts an async `agent_end` subscriber keeps the `prompt`
    // promise pending until a barrier resolves. The sync model has no pending
    // promise; the invariant preserved is that every subscriber runs to
    // completion before `prompt()` returns and before the agent is idle.
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });

    let listener_finished = Arc::new(AtomicBool::new(false));
    let flag = listener_finished.clone();
    agent.subscribe(Arc::new(move |event: &AgentEvent, _signal| {
        if matches!(event, AgentEvent::AgentEnd { .. }) {
            flag.store(true, Ordering::SeqCst);
        }
    }));

    agent.prompt_text("hello", Vec::new()).unwrap();

    assert!(listener_finished.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

#[test]
fn wait_for_idle_returns_after_run_completes() {
    // ADAPTED: pi's `waitForIdle` awaits async subscribers. In the eager model
    // `prompt()` already ran the loop and its subscribers to completion, so
    // `wait_for_idle()` is a no-op and the agent is already idle.
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });
    let seen = Arc::new(AtomicBool::new(false));
    let flag = seen.clone();
    agent.subscribe(Arc::new(move |event: &AgentEvent, _signal| {
        if matches!(event, AgentEvent::MessageEnd { .. }) {
            flag.store(true, Ordering::SeqCst);
        }
    }));

    agent.prompt_text("hello", Vec::new()).unwrap();
    agent.wait_for_idle();

    assert!(seen.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

#[test]
fn passes_active_abort_signal_to_subscribers() {
    // ADAPTED: pi records the signal on `agent_start`, then aborts externally
    // while the prompt promise is pending. The sync run never suspends, so the
    // abort is issued from a subscriber (which runs inside the run). The
    // invariant preserved: subscribers receive the run's shared signal, and
    // `abort()` trips the very signal they saw.
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });
    let received: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));

    let slot = received.clone();
    let handle = agent.clone();
    agent.subscribe(Arc::new(
        move |event: &AgentEvent, signal: &AbortSignal| match event {
            AgentEvent::AgentStart => {
                assert!(!signal.is_aborted());
                *slot.lock().unwrap() = Some(signal.clone());
            }
            AgentEvent::TurnStart => handle.abort(),
            _ => {}
        },
    ));

    agent.prompt_text("hello", Vec::new()).unwrap();

    let signal = received.lock().unwrap().clone().expect("recorded signal");
    assert!(signal.is_aborted());
    // Once the run finished there is no active signal.
    assert!(agent.signal().is_none());
}

// ---------------------------------------------------------------------------
// Tool-update settling
// ---------------------------------------------------------------------------

#[test]
fn ignores_tool_updates_after_execution_settles() {
    let captured: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let capture = captured.clone();
    let tool = tool_with("delayed_tool", move |_id, _args, _signal, on_update| {
        if let Some(on_update) = on_update {
            *capture.lock().unwrap() = Some(on_update.clone());
            on_update(&AgentToolResult {
                content: vec![text_block("running")],
                details: json!({ "status": "running" }),
                added_tool_names: None,
                terminate: None,
            });
        }
        AgentToolResult {
            content: vec![text_block("ok")],
            details: json!({ "status": "done" }),
            added_tool_names: None,
            terminate: Some(true),
        }
    });

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn_once(assistant_tool_use(vec![tool_call_block(
            "delayed_tool",
            "call-1",
            json!({}),
        )]))),
        ..Default::default()
    });
    let (events, listener) = recording_subscriber();
    agent.subscribe(listener);

    agent.prompt_text("run tool", Vec::new()).unwrap();
    let count_after_prompt = events.lock().unwrap().len();

    // A late update after the tool settled is ignored (gate closed).
    let late = captured.lock().unwrap().clone().expect("captured callback");
    late(&AgentToolResult {
        content: vec![text_block("late")],
        details: json!({ "status": "late" }),
        added_tool_names: None,
        terminate: None,
    });

    let events = events.lock().unwrap();
    let updates = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
        .count();
    assert_eq!(updates, 1);
    assert_eq!(events.len(), count_after_prompt);
}

#[test]
fn ignores_settled_parallel_tool_update_while_another_runs() {
    // ADAPTED: pi uses two concurrently-scheduled tools (a settled tool whose
    // late update fires while a slow tool is still awaited). The eager parallel
    // executor runs prepared tools in source order with no real concurrency, so
    // the settled tool (call-1) finalizes first, then the sibling (call-2) fires
    // the *settled* tool's captured callback during its own execute. The
    // invariant preserved: the settled tool's late update is ignored (its gate is
    // closed) even though the batch is still in flight.
    let settled_cb: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));

    let capture = settled_cb.clone();
    let settled_tool = tool_with("settled_tool", move |_id, _args, _signal, on_update| {
        if let Some(on_update) = on_update {
            *capture.lock().unwrap() = Some(on_update.clone());
        }
        ok_result("done", Some(true))
    });

    let sibling = settled_cb.clone();
    let slow_tool = tool_with("slow_tool", move |_id, _args, _signal, _on_update| {
        // The settled tool already finalized; poke its late callback.
        if let Some(cb) = sibling.lock().unwrap().as_ref() {
            cb(&AgentToolResult {
                content: vec![text_block("late")],
                details: json!({ "status": "late" }),
                added_tool_names: None,
                terminate: None,
            });
        }
        ok_result("done", Some(true))
    });

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![settled_tool, slow_tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn_once(assistant_tool_use(vec![
            tool_call_block("settled_tool", "call-1", json!({})),
            tool_call_block("slow_tool", "call-2", json!({})),
        ]))),
        ..Default::default()
    });
    let (events, listener) = recording_subscriber();
    agent.subscribe(listener);

    agent.prompt_text("run tools", Vec::new()).unwrap();

    let updates = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
        .count();
    assert_eq!(updates, 0);
}

// ---------------------------------------------------------------------------
// Queues
// ---------------------------------------------------------------------------

#[test]
fn steer_queues_without_touching_messages() {
    let agent = Agent::default();
    let message = user_message("Steering message");
    agent.steer(message.clone());
    assert!(!agent.messages().contains(&message));
    assert!(agent.has_queued_messages());
}

#[test]
fn follow_up_queues_without_touching_messages() {
    let agent = Agent::default();
    let message = user_message("Follow-up message");
    agent.follow_up(message.clone());
    assert!(!agent.messages().contains(&message));
    assert!(agent.has_queued_messages());
}

#[test]
fn abort_is_a_noop_when_idle() {
    let agent = Agent::default();
    agent.abort(); // must not panic
    assert!(agent.signal().is_none());
}

// ---------------------------------------------------------------------------
// In-flight guards (re-entrant from a subscriber)
// ---------------------------------------------------------------------------

#[test]
fn prompt_rejects_while_streaming() {
    // ADAPTED: pi starts a blocking prompt then calls `prompt()` again while it
    // is pending. The sync analog re-enters `prompt()` from a subscriber, where
    // the run is active.
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });
    let result: Arc<Mutex<Option<Result<(), AgentError>>>> = Arc::new(Mutex::new(None));

    let slot = result.clone();
    let handle = agent.clone();
    agent.subscribe(Arc::new(move |event: &AgentEvent, _signal| {
        if matches!(event, AgentEvent::AgentStart) {
            *slot.lock().unwrap() = Some(handle.prompt_text("second", Vec::new()));
        }
    }));

    agent.prompt_text("first", Vec::new()).unwrap();

    assert_eq!(
        result.lock().unwrap().take().unwrap(),
        Err(AgentError::AlreadyProcessingPrompt)
    );
}

#[test]
fn continue_rejects_while_streaming() {
    // ADAPTED: re-entrant `continue_()` from a subscriber, same as above.
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });
    let result: Arc<Mutex<Option<Result<(), AgentError>>>> = Arc::new(Mutex::new(None));

    let slot = result.clone();
    let handle = agent.clone();
    agent.subscribe(Arc::new(move |event: &AgentEvent, _signal| {
        if matches!(event, AgentEvent::AgentStart) {
            *slot.lock().unwrap() = Some(handle.continue_());
        }
    }));

    agent.prompt_text("first", Vec::new()).unwrap();

    assert_eq!(
        result.lock().unwrap().take().unwrap(),
        Err(AgentError::AlreadyProcessingContinue)
    );
}

// ---------------------------------------------------------------------------
// continue()
// ---------------------------------------------------------------------------

#[test]
fn continue_processes_queued_follow_up_after_assistant_turn() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("Processed"))),
        ..Default::default()
    });

    agent.set_messages(vec![
        json!({ "role": "user", "content": [{ "type": "text", "text": "Initial" }], "timestamp": 0 }),
        serde_json::to_value(assistant_text("Initial response")).unwrap(),
    ]);

    agent.follow_up(
        json!({ "role": "user", "content": [{ "type": "text", "text": "Queued follow-up" }], "timestamp": 1 }),
    );

    agent.continue_().unwrap();

    let has_follow_up = agent.messages().iter().any(|message| {
        role_of(message) == Some("user")
            && message
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| {
                    content.iter().any(|part| {
                        part.get("text").and_then(Value::as_str) == Some("Queued follow-up")
                    })
                })
    });
    assert!(has_follow_up);
    assert_eq!(role_of(agent.messages().last().unwrap()), Some("assistant"));
}

#[test]
fn continue_keeps_one_at_a_time_steering_from_assistant_tail() {
    let counter = Arc::new(AtomicUsize::new(0));
    let count = counter.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        mock_stream(assistant_text(&format!("Processed {n}")))
    });
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.set_messages(vec![
        json!({ "role": "user", "content": [{ "type": "text", "text": "Initial" }], "timestamp": 0 }),
        serde_json::to_value(assistant_text("Initial response")).unwrap(),
    ]);

    agent.steer(
        json!({ "role": "user", "content": [{ "type": "text", "text": "Steering 1" }], "timestamp": 1 }),
    );
    agent.steer(
        json!({ "role": "user", "content": [{ "type": "text", "text": "Steering 2" }], "timestamp": 2 }),
    );

    agent.continue_().unwrap();

    let messages = agent.messages();
    let recent: Vec<Option<&str>> = messages.iter().rev().take(4).rev().map(role_of).collect();
    assert_eq!(
        recent,
        vec![
            Some("user"),
            Some("assistant"),
            Some("user"),
            Some("assistant")
        ]
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn continue_from_empty_transcript_errors() {
    let agent = Agent::default();
    assert_eq!(agent.continue_(), Err(AgentError::NoMessagesToContinue));
}

#[test]
fn continue_from_assistant_tail_without_queue_errors() {
    let agent = Agent::default();
    agent.set_messages(vec![serde_json::to_value(assistant_text("done")).unwrap()]);
    assert_eq!(agent.continue_(), Err(AgentError::ContinueFromAssistant));
}

// ---------------------------------------------------------------------------
// Hooks & options
// ---------------------------------------------------------------------------

#[test]
fn keeps_legacy_prepare_next_turn_signal_callback_behavior() {
    let tool = tool_with("noop", |_, _, _, _| ok_result("ok", None));

    let request_count = Arc::new(AtomicUsize::new(0));
    let saw_signal = Arc::new(AtomicBool::new(false));

    let saw = saw_signal.clone();
    let prepare_next_turn: PrepareNextTurnSignal = Arc::new(move |signal| {
        saw.store(signal.is_some(), Ordering::SeqCst);
        None
    });

    let count = request_count.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            mock_stream(assistant_tool_use(vec![tool_call_block(
                "noop",
                "tool-1",
                json!({}),
            )]))
        } else {
            mock_stream(assistant_text("done"))
        }
    });

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        prepare_next_turn: Some(prepare_next_turn),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt_text("start", Vec::new()).unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert!(saw_signal.load(Ordering::SeqCst));
}

#[test]
fn prefers_prepare_next_turn_with_context() {
    // Supplementary: `prepareNextTurnWithContext` wins over `prepareNextTurn`
    // when both are set, and receives the after-turn context.
    let tool = tool_with("noop", |_, _, _, _| ok_result("ok", None));

    let with_ctx_calls = Arc::new(AtomicUsize::new(0));
    let legacy_calls = Arc::new(AtomicUsize::new(0));

    let with_ctx = with_ctx_calls.clone();
    let prepare_with_context: PrepareNextTurnWithContext =
        Arc::new(move |ctx: &PrepareNextTurnContext, signal| {
            assert!(signal.is_some());
            // The context carries the just-finished turn's messages.
            assert!(!ctx.new_messages.is_empty());
            with_ctx.fetch_add(1, Ordering::SeqCst);
            None
        });

    let legacy = legacy_calls.clone();
    let prepare_legacy: PrepareNextTurnSignal = Arc::new(move |_signal| {
        legacy.fetch_add(1, Ordering::SeqCst);
        None
    });

    let responses = vec![
        assistant_tool_use(vec![tool_call_block("noop", "tool-1", json!({}))]),
        assistant_text("done"),
    ];

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        prepare_next_turn: Some(prepare_legacy),
        prepare_next_turn_with_context: Some(prepare_with_context),
        stream_fn: Some(stream_fn_from(responses)),
        ..Default::default()
    });

    agent.prompt_text("start", Vec::new()).unwrap();

    assert!(with_ctx_calls.load(Ordering::SeqCst) >= 1);
    assert_eq!(legacy_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn forwards_session_id_to_stream_options() {
    let received: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let slot = received.clone();
    let stream_fn: StreamFn =
        Arc::new(move |_model, _ctx, opts: Option<&StreamOptions>, _signal| {
            *slot.lock().unwrap() = opts.and_then(|o| o.session_id.clone());
            mock_stream(assistant_text("ok"))
        });

    let agent = Agent::new(AgentOptions {
        session_id: Some("session-abc".into()),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt_text("hello", Vec::new()).unwrap();
    assert_eq!(received.lock().unwrap().as_deref(), Some("session-abc"));

    agent.set_session_id(Some("session-def".into()));
    assert_eq!(agent.session_id().as_deref(), Some("session-def"));

    agent.prompt_text("hello again", Vec::new()).unwrap();
    assert_eq!(received.lock().unwrap().as_deref(), Some("session-def"));
}

#[test]
fn forwards_max_retry_delay_ms_to_stream_options() {
    // pi spreads `AgentLoopConfig.maxRetryDelayMs` into the stream options
    // (`agent.ts:441`); the port forwards the `AgentOptions` field onto
    // `StreamOptions.max_retry_delay_ms`.
    let received: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let slot = received.clone();
    let stream_fn: StreamFn =
        Arc::new(move |_model, _ctx, opts: Option<&StreamOptions>, _signal| {
            *slot.lock().unwrap() = opts.and_then(|o| o.max_retry_delay_ms);
            mock_stream(assistant_text("ok"))
        });

    let agent = Agent::new(AgentOptions {
        max_retry_delay_ms: Some(12_000),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt_text("hello", Vec::new()).unwrap();
    assert_eq!(*received.lock().unwrap(), Some(12_000));
}

#[test]
fn max_retry_delay_ms_defaults_to_none_on_stream_options() {
    // When unset on `AgentOptions`, the stream sees `None` — the provider-side
    // default (60000) stays in effect rather than being overridden.
    let received: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(Some(1)));
    let slot = received.clone();
    let stream_fn: StreamFn =
        Arc::new(move |_model, _ctx, opts: Option<&StreamOptions>, _signal| {
            *slot.lock().unwrap() = opts.and_then(|o| o.max_retry_delay_ms);
            mock_stream(assistant_text("ok"))
        });

    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt_text("hello", Vec::new()).unwrap();
    assert_eq!(*received.lock().unwrap(), None);
}

// ---------------------------------------------------------------------------
// Supplementary: queue-drain modes, reset, abort edge branches
// ---------------------------------------------------------------------------

#[test]
fn steering_all_mode_injects_every_queued_message() {
    // Supplementary: `steeringMode = all` drains the whole steering batch at the
    // next poll, so both steering messages precede a single assistant response.
    let agent = Agent::new(AgentOptions {
        steering_mode: Some(QueueMode::All),
        stream_fn: Some(stream_fn_once(assistant_text("Processed"))),
        ..Default::default()
    });
    assert_eq!(agent.steering_mode(), QueueMode::All);

    agent.set_messages(vec![
        json!({ "role": "user", "content": [{ "type": "text", "text": "Initial" }], "timestamp": 0 }),
        serde_json::to_value(assistant_text("Initial response")).unwrap(),
    ]);
    agent.steer(
        json!({ "role": "user", "content": [{ "type": "text", "text": "Steering 1" }], "timestamp": 1 }),
    );
    agent.steer(
        json!({ "role": "user", "content": [{ "type": "text", "text": "Steering 2" }], "timestamp": 2 }),
    );

    agent.continue_().unwrap();

    let messages = agent.messages();
    let recent: Vec<Option<&str>> = messages.iter().rev().take(3).rev().map(role_of).collect();
    // Both steering messages, then one assistant response.
    assert_eq!(recent, vec![Some("user"), Some("user"), Some("assistant")]);
}

#[test]
fn clear_all_queues_and_has_queued_messages() {
    let agent = Agent::default();
    agent.steer(user_message("s"));
    agent.follow_up(user_message("f"));
    assert!(agent.has_queued_messages());

    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

#[test]
fn reset_clears_state_and_queues() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_once(assistant_text("ok"))),
        ..Default::default()
    });
    agent.prompt_text("hello", Vec::new()).unwrap();
    assert!(!agent.messages().is_empty());
    agent.steer(user_message("s"));

    agent.reset();

    assert!(agent.messages().is_empty());
    assert!(!agent.is_streaming());
    assert!(agent.streaming_message().is_none());
    assert!(agent.pending_tool_calls().is_empty());
    assert!(agent.error_message().is_none());
    assert!(!agent.has_queued_messages());
}

#[test]
fn abort_from_subscriber_yields_aborted_message() {
    // Supplementary: aborting on `agent_start` (before the first stream call)
    // trips the run's signal; a signal-aware stream_fn observes it and yields an
    // aborted assistant message, which sets `error_message` via `turn_end`.
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, signal| {
        if signal.is_some_and(AbortSignal::is_aborted) {
            mock_stream(faux_assistant_message(
                Vec::new(),
                FauxAssistantOptions {
                    stop_reason: Some(StopReason::Aborted),
                    error_message: Some("Request was aborted".into()),
                    ..Default::default()
                },
                0,
            ))
        } else {
            mock_stream(assistant_text("ok"))
        }
    });

    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let handle = agent.clone();
    agent.subscribe(Arc::new(move |event: &AgentEvent, _signal| {
        if matches!(event, AgentEvent::AgentStart) {
            handle.abort();
        }
    }));

    agent.prompt_text("hello", Vec::new()).unwrap();

    let last = agent.messages().last().cloned().unwrap();
    assert_eq!(
        last.get("stopReason").and_then(Value::as_str),
        Some("aborted")
    );
    assert_eq!(
        agent.error_message().as_deref(),
        Some("Request was aborted")
    );
}

// ---------------------------------------------------------------------------
// Post-construction hook installation
// ---------------------------------------------------------------------------

#[test]
fn installs_tool_call_hooks_after_construction() {
    // A caller holding a pre-built agent installs the tool-call hooks after the
    // fact (pi's `AgentSession._installAgentToolHooks` reassigns
    // `agent.beforeToolCall` / `agent.afterToolCall`, `agent-session.ts:449-490`).
    let tool = tool_with("noop", |_, _, _, _| ok_result("ok", Some(true)));

    let before_calls = Arc::new(AtomicUsize::new(0));
    let after_calls = Arc::new(AtomicUsize::new(0));

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn_once(assistant_tool_use(vec![tool_call_block(
            "noop",
            "tool-1",
            json!({}),
        )]))),
        ..Default::default()
    });

    // Hooks are absent at construction time, then installed post-hoc.
    let before = before_calls.clone();
    agent.set_before_tool_call(Some(Arc::new(
        move |_ctx: &mut BeforeToolCallContext, _signal| {
            before.fetch_add(1, Ordering::SeqCst);
            None
        },
    )));
    let after = after_calls.clone();
    agent.set_after_tool_call(Some(Arc::new(
        move |_ctx: &AfterToolCallContext, _signal| {
            after.fetch_add(1, Ordering::SeqCst);
            None
        },
    )));

    agent.prompt_text("start", Vec::new()).unwrap();

    assert_eq!(before_calls.load(Ordering::SeqCst), 1);
    assert_eq!(after_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn replaces_and_clears_tool_call_hook_after_construction() {
    // Setting a new closure replaces the previous one; `None` clears it. Each run
    // reads the current value (the field is re-read per run, matching pi).
    let tool = tool_with("noop", |_, _, _, _| ok_result("ok", Some(true)));

    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));

    let first = first_calls.clone();
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        before_tool_call: Some(Arc::new(
            move |_ctx: &mut BeforeToolCallContext, _signal| {
                first.fetch_add(1, Ordering::SeqCst);
                None
            },
        )),
        stream_fn: Some(stream_fn_from(vec![
            assistant_tool_use(vec![tool_call_block("noop", "tool-1", json!({}))]),
            assistant_tool_use(vec![tool_call_block("noop", "tool-2", json!({}))]),
            assistant_tool_use(vec![tool_call_block("noop", "tool-3", json!({}))]),
        ])),
        ..Default::default()
    });

    // Constructor-provided hook fires on the first run (constructor path faithful).
    agent.prompt_text("first", Vec::new()).unwrap();
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);

    // Replace with a different closure; the new one fires, the old one does not.
    let second = second_calls.clone();
    agent.set_before_tool_call(Some(Arc::new(
        move |_ctx: &mut BeforeToolCallContext, _signal| {
            second.fetch_add(1, Ordering::SeqCst);
            None
        },
    )));
    agent.prompt_text("second", Vec::new()).unwrap();
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);

    // Clearing it leaves no hook installed; a subsequent run fires neither.
    agent.set_before_tool_call(None);
    agent.prompt_text("third", Vec::new()).unwrap();
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn installs_prepare_next_turn_with_context_after_construction() {
    // The context-aware next-turn hook is likewise installable post-construction.
    let tool = tool_with("noop", |_, _, _, _| ok_result("ok", None));

    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn_from(vec![
            assistant_tool_use(vec![tool_call_block("noop", "tool-1", json!({}))]),
            assistant_text("done"),
        ])),
        ..Default::default()
    });

    agent.set_prepare_next_turn_with_context(Some(Arc::new(
        move |ctx: &PrepareNextTurnContext, signal| {
            assert!(signal.is_some());
            assert!(!ctx.new_messages.is_empty());
            seen.fetch_add(1, Ordering::SeqCst);
            None
        },
    )));

    agent.prompt_text("start", Vec::new()).unwrap();

    assert!(calls.load(Ordering::SeqCst) >= 1);
}
