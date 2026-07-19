// straitjacket-allow-file:duplication — faithful port of pi's agent
// `e2e.test.ts`. Each `#[test]` builds a near-identical agent + faux-provider
// setup and asserts on the same message/event shapes by design; the clone
// detector reads these parallel ported cases as duplicates. Collapsing them
// would obscure which pi `it(...)` each case mirrors.
//! End-to-end integration tests for the [`Agent`], ported from
//! `vendor/pi/packages/agent/test/e2e.test.ts` at pinned commit `3da591ab`.
//!
//! pi drives the real `Agent` over the faux provider (`registerFauxProvider` /
//! `faux.setResponses`), including factory-function responses that read the live
//! request context. The port mirrors that exactly: it wraps a real
//! [`FauxProvider`] in a [`StreamFn`] and lets the agent loop stream through it.
//!
//! ## Adaptations (each called out inline with `ADAPTED:`)
//!
//! - **Abort during streaming.** pi arms a `setTimeout` that calls `agent.abort()`
//!   ~30ms into a slow (`tokensPerSecond: 20`) stream. atilla's provider seam is
//!   eager — `stream_fn` builds the whole event sequence in one call — so there
//!   is no mid-stream wall clock to race. The faithful analog trips the run's
//!   abort signal from a subscriber on an event that fires *before* the stream
//!   call (`turn_start`); the faux provider then observes the aborted signal and
//!   returns its aborted terminal, producing the identical observable state
//!   (`stopReason: "aborted"`, an `errorMessage`, and `agent.state.errorMessage`
//!   mirroring it).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, faux_thinking, faux_tool_call, FauxAssistantOptions,
    FauxModelDefinition, FauxProvider, FauxResponseStep, RegisterFauxProviderOptions,
};
use atilla_ai::seams::{AbortSignal, Provider};
use atilla_ai::{ContentBlock, Message, Model, StopReason, UserContent};

use atilla_agent::agent::{Agent, AgentError, AgentOptions, InitialAgentState, Listener};
use atilla_agent::types::{
    AgentEvent, AgentToolResult, AgentToolUpdateCallback, StreamFn, ThinkingLevel,
};
use atilla_agent::{AgentTool, AgentToolExecute};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A faux provider queued with `responses`, wrapped in the [`StreamFn`] the agent
/// drives — the port of pi's `registerFauxProvider()` + `faux.setResponses()`
/// feeding `new Agent({ ... })`.
fn faux_with(
    options: RegisterFauxProviderOptions,
    responses: Vec<FauxResponseStep>,
) -> (Arc<FauxProvider>, Model, StreamFn) {
    let faux = Arc::new(FauxProvider::new(options));
    faux.set_responses(responses);
    let model = faux.get_model(None).expect("a faux model");
    let stream_faux = faux.clone();
    let stream_fn: StreamFn =
        Arc::new(move |model, ctx, opts, signal| stream_faux.stream(model, ctx, opts, signal));
    (faux, model, stream_fn)
}

/// pi's `fauxAssistantMessage("text")` string shorthand.
fn faux_text_message(text: &str) -> FauxResponseStep {
    faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0).into()
}

/// pi's `getTextContent`: join every `text` block of a message.
fn text_content(message: &Value) -> String {
    message["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn role_of(message: &Value) -> Option<&str> {
    message["role"].as_str()
}

/// A shared log of `(event kind, pending tool-call ids)` pairs recorded from a
/// subscriber — the port of pi's `pendingToolCallsDuringEvents` array.
type PendingLog = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// A minimal arithmetic evaluator standing in for pi's `calculate` (which uses
/// `new Function("return " + expression)`); it covers the integer expressions the
/// suite exercises (`+ - * /` with `*`/`/` binding tighter than `+`/`-`).
fn eval_expression(expression: &str) -> f64 {
    let s: String = expression.chars().filter(|c| !c.is_whitespace()).collect();

    // Split into additive terms, tracking the operator preceding each.
    let mut total = 0.0;
    let mut term = String::new();
    let mut add = true; // sign applied to the current term
    let mut pending_sign = true;
    for ch in s.chars() {
        if ch == '+' || ch == '-' {
            let value = eval_mul_div(&term);
            total += if add { value } else { -value };
            term.clear();
            add = pending_sign;
            pending_sign = ch == '+';
        } else {
            term.push(ch);
        }
    }
    let value = eval_mul_div(&term);
    total += if add { value } else { -value };
    total
}

fn eval_mul_div(term: &str) -> f64 {
    let mut result = 1.0;
    let mut factor = String::new();
    let mut mul = true;
    for ch in term.chars() {
        if ch == '*' || ch == '/' {
            let value: f64 = factor.parse().unwrap_or(0.0);
            result = if mul { result * value } else { result / value };
            factor.clear();
            mul = ch == '*';
        } else {
            factor.push(ch);
        }
    }
    let value: f64 = factor.parse().unwrap_or(0.0);
    if mul {
        result * value
    } else {
        result / value
    }
}

/// pi's `calculateTool` (`test/utils/calculate.ts`): evaluate `expression` and
/// return `"{expression} = {result}"`.
fn calculate_tool() -> AgentTool {
    let execute: AgentToolExecute = Arc::new(
        |_id: &str,
         args: &Value,
         _signal: Option<&AbortSignal>,
         _update: Option<&AgentToolUpdateCallback>|
         -> AgentToolResult {
            let expression = args["expression"].as_str().unwrap_or("");
            let value = eval_expression(expression);
            // Integer results render without a trailing ".0" (JS number formatting).
            let rendered = if value.fract() == 0.0 {
                format!("{}", value as i64)
            } else {
                format!("{value}")
            };
            AgentToolResult {
                content: vec![ContentBlock::Text {
                    text: format!("{expression} = {rendered}"),
                    text_signature: None,
                }],
                details: Value::Null,
                added_tool_names: None,
                terminate: None,
            }
        },
    );
    AgentTool {
        name: "calculate".into(),
        description: "Evaluate mathematical expressions".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "The mathematical expression to evaluate"
                }
            },
            "required": ["expression"]
        }),
        label: "Calculator".into(),
        prepare_arguments: None,
        execute,
        execution_mode: None,
    }
}

// ---------------------------------------------------------------------------
// describe("Agent integration with faux provider")
// ---------------------------------------------------------------------------

#[test]
fn handles_a_basic_text_prompt() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions::default(),
        vec![faux_text_message("4")],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant. Keep your responses concise.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent
        .prompt("What is 2+2? Answer with just the number.".into())
        .unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(role_of(&messages[0]), Some("user"));
    assert_eq!(role_of(&messages[1]), Some("assistant"));
    assert!(text_content(&messages[1]).contains("4"));
}

#[test]
fn executes_tools_and_tracks_pending_tool_calls() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions::default(),
        vec![
            faux_assistant_message(
                vec![
                    faux_text("Let me calculate that."),
                    faux_tool_call(
                        "calculate",
                        json!({ "expression": "123 * 456" }),
                        Some("calc-1".into()),
                    ),
                ],
                FauxAssistantOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
                0,
            )
            .into(),
            faux_text_message("The result is 56088."),
        ],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some(
                "You are a helpful assistant. Always use the calculator tool for math.".into(),
            ),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(vec![calculate_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    // Record the pending-tool-call set observed during each execution event.
    let pending_during: PendingLog = Arc::new(Mutex::new(Vec::new()));
    let sink = pending_during.clone();
    let handle = agent.clone();
    let listener: Listener = Arc::new(move |event: &AgentEvent, _signal: &AbortSignal| {
        let kind = match event {
            AgentEvent::ToolExecutionStart { .. } => Some("tool_execution_start"),
            AgentEvent::ToolExecutionEnd { .. } => Some("tool_execution_end"),
            _ => None,
        };
        if let Some(kind) = kind {
            let ids: Vec<String> = handle.pending_tool_calls().into_iter().collect();
            sink.lock().unwrap().push((kind.to_string(), ids));
        }
    });
    agent.subscribe(listener);

    agent
        .prompt("Calculate 123 * 456 using the calculator tool.".into())
        .unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);

    let tool_result = messages
        .iter()
        .find(|message| role_of(message) == Some("toolResult"))
        .expect("a tool result message");
    assert!(text_content(tool_result).contains("123 * 456 = 56088"));

    let final_message = messages.last().unwrap();
    assert_eq!(role_of(final_message), Some("assistant"));
    assert!(text_content(final_message).contains("56088"));

    assert!(agent.pending_tool_calls().is_empty());
    let recorded = pending_during.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            (
                "tool_execution_start".to_string(),
                vec!["calc-1".to_string()]
            ),
            ("tool_execution_end".to_string(), Vec::<String>::new()),
        ]
    );
}

#[test]
fn handles_abort_during_streaming() {
    // ADAPTED: pi aborts ~30ms into a slow stream; the eager provider has no
    // mid-stream window, so the abort is tripped from a subscriber on `turn_start`
    // — before the stream call — and the faux provider returns its aborted
    // terminal (see the module doc).
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions {
            tokens_per_second: Some(20.0),
            token_size_min: Some(2),
            token_size_max: Some(2),
            ..Default::default()
        },
        vec![faux_text_message(
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen",
        )],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let handle = agent.clone();
    let listener: Listener = Arc::new(move |event: &AgentEvent, _signal: &AbortSignal| {
        if matches!(event, AgentEvent::TurnStart) {
            handle.abort();
        }
    });
    agent.subscribe(listener);

    agent.prompt("Count slowly from 1 to 20.".into()).unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 2);

    let last_message = messages.last().unwrap();
    assert_eq!(role_of(last_message), Some("assistant"));
    assert_eq!(last_message["stopReason"], "aborted");
    let error_message = last_message["errorMessage"].as_str();
    assert!(error_message.is_some());
    assert_eq!(agent.error_message().as_deref(), error_message);
}

#[test]
fn emits_lifecycle_updates_while_streaming() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions {
            token_size_min: Some(1),
            token_size_max: Some(1),
            ..Default::default()
        },
        vec![faux_text_message("1 2 3 4 5")],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let listener: Listener = Arc::new(move |event: &AgentEvent, _signal: &AbortSignal| {
        sink.lock().unwrap().push(event_type(event).to_string());
    });
    agent.subscribe(listener);

    agent.prompt("Count from 1 to 5.".into()).unwrap();

    let events = events.lock().unwrap().clone();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(
            events.iter().any(|e| e == expected),
            "missing event {expected}"
        );
    }
    let index = |name: &str| events.iter().position(|e| e == name).unwrap();
    let last_index = |name: &str| events.iter().rposition(|e| e == name).unwrap();
    assert!(index("agent_start") < index("message_start"));
    assert!(index("message_start") < index("message_end"));
    assert!(index("message_end") < last_index("agent_end"));

    assert!(!agent.is_streaming());
    assert_eq!(agent.messages().len(), 2);
}

#[test]
fn maintains_context_across_multiple_turns() {
    // The second response is a factory (pi's `(context) => ...`) that inspects the
    // live request messages for "Alice".
    let factory: FauxResponseStep =
        FauxResponseStep::Factory(Box::new(|context, _opts, _state, _model| {
            let has_alice = context.messages.iter().any(|message| {
                match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => text.contains("Alice"),
                UserContent::Blocks(blocks) => blocks.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text, .. } if text.contains("Alice"))
                }),
            },
            _ => false,
        }
            });
            faux_assistant_message(
                vec![faux_text(if has_alice {
                    "Your name is Alice."
                } else {
                    "I do not know your name."
                })],
                FauxAssistantOptions::default(),
                0,
            )
        }));

    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions::default(),
        vec![faux_text_message("Nice to meet you, Alice."), factory],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt("My name is Alice.".into()).unwrap();
    assert_eq!(agent.messages().len(), 2);

    agent.prompt("What is my name?".into()).unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 4);

    let last_message = &messages[3];
    assert_eq!(role_of(last_message), Some("assistant"));
    assert!(text_content(last_message).to_lowercase().contains("alice"));
}

#[test]
fn preserves_thinking_content_blocks() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions {
            models: Some(vec![FauxModelDefinition {
                id: "faux-reasoning".into(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            }]),
            ..Default::default()
        },
        vec![faux_assistant_message(
            vec![faux_thinking("step by step"), faux_text("4")],
            FauxAssistantOptions::default(),
            0,
        )
        .into()],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Low),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    agent.prompt("What is 2+2?".into()).unwrap();

    let messages = agent.messages();
    let assistant_message = &messages[1];
    assert_eq!(role_of(assistant_message), Some("assistant"));
    assert_eq!(
        assistant_message["content"],
        json!([
            { "type": "thinking", "thinking": "step by step" },
            { "type": "text", "text": "4" }
        ])
    );
}

// ---------------------------------------------------------------------------
// describe("Agent.continue() with faux provider")
// ---------------------------------------------------------------------------

#[test]
fn throws_when_no_messages_in_context() {
    let (_faux, model, stream_fn) = faux_with(RegisterFauxProviderOptions::default(), Vec::new());

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("Test".into()),
            model: Some(model),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let error = agent.continue_().unwrap_err();
    assert!(matches!(error, AgentError::NoMessagesToContinue));
    assert!(error.to_string().contains("No messages to continue from"));
}

#[test]
fn throws_when_last_message_is_assistant() {
    let (_faux, model, stream_fn) = faux_with(RegisterFauxProviderOptions::default(), Vec::new());

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("Test".into()),
            model: Some(model.clone()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let assistant_message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "Hello" }],
        "api": model.api,
        "provider": model.provider,
        "model": model.id,
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 0,
    });
    agent.set_messages(vec![assistant_message]);

    let error = agent.continue_().unwrap_err();
    assert!(matches!(error, AgentError::ContinueFromAssistant));
    assert!(error
        .to_string()
        .contains("Cannot continue from message role: assistant"));
}

#[test]
fn continues_and_gets_a_response_when_last_message_is_user() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions::default(),
        vec![faux_text_message("HELLO WORLD")],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant. Follow instructions exactly.".into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let user_message = json!({
        "role": "user",
        "content": [{ "type": "text", "text": "Say exactly: HELLO WORLD" }],
        "timestamp": 0,
    });
    agent.set_messages(vec![user_message]);

    agent.continue_().unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(role_of(&messages[0]), Some("user"));
    assert_eq!(role_of(&messages[1]), Some("assistant"));
    assert!(text_content(&messages[1])
        .to_uppercase()
        .contains("HELLO WORLD"));
}

#[test]
fn continues_and_processes_tool_results() {
    let (_faux, model, stream_fn) = faux_with(
        RegisterFauxProviderOptions::default(),
        vec![faux_text_message("The answer is 8.")],
    );

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some(
                "You are a helpful assistant. After getting a calculation result, state the answer clearly.".into(),
            ),
            model: Some(model.clone()),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(vec![calculate_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let user_message = json!({
        "role": "user",
        "content": [{ "type": "text", "text": "What is 5 + 3?" }],
        "timestamp": 0,
    });
    let assistant_message = json!({
        "role": "assistant",
        "content": [
            { "type": "text", "text": "Let me calculate that." },
            { "type": "toolCall", "id": "calc-1", "name": "calculate", "arguments": { "expression": "5 + 3" } }
        ],
        "api": model.api,
        "provider": model.provider,
        "model": model.id,
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "toolUse",
        "timestamp": 0,
    });
    let tool_result = json!({
        "role": "toolResult",
        "toolCallId": "calc-1",
        "toolName": "calculate",
        "content": [{ "type": "text", "text": "5 + 3 = 8" }],
        "isError": false,
        "timestamp": 0,
    });
    agent.set_messages(vec![user_message, assistant_message, tool_result]);

    agent.continue_().unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);

    let last_message = messages.last().unwrap();
    assert_eq!(role_of(last_message), Some("assistant"));
    assert!(text_content(last_message).contains("8"));
}

// ---------------------------------------------------------------------------
// Shared: event-type name mapping (pi compares `event.type` strings).
// ---------------------------------------------------------------------------

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
