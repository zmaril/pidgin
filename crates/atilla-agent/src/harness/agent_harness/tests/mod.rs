//! Tests for the [`AgentHarness`](crate::harness::agent_harness::AgentHarness)
//! port.
//!
//! [`harness_tests`] mirrors `test/harness/agent-harness.test.ts`,
//! [`stream_tests`] mirrors `test/harness/agent-harness-stream.test.ts`, and
//! [`supplementary`] covers branches upstream leaves untested (phase-busy guards
//! on every entry point, stream-option key deletes, compaction/branch-summary
//! paths, abort-mid-phase). Deterministic-sync adaptations of pi's
//! real-async-interleaving cases are flagged `ADAPTED` at each site.
//!
//! All scenarios are driven by a [`FauxProvider`] (turn streaming), a
//! [`FauxModels`] fake (compaction/branch-summary `completeSimple`), a
//! [`MemoryExecutionEnv`], and in-memory session storage.

// straitjacket-allow-file:duplication — the faithful parallel test bodies
// repeat near-identical faux/session/harness scaffolding per scenario, mirroring
// pi's one-`it`-per-shape suite.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, faux_tool_call, FauxAssistantOptions, FauxModelDefinition,
    FauxProvider, FauxResponseStep, FauxState, RegisterFauxProviderOptions,
};
use atilla_ai::seams::{AbortSignal, Provider};
use atilla_ai::{AssistantMessage, Context, Message, Model, StopReason, StreamOptions};

use crate::harness::agent_harness::AgentHarnessEvent;
use crate::harness::compaction::{CompletionOptions, Models};
use crate::harness::env::MemoryExecutionEnv;
use crate::harness::events::AgentHarnessStreamOptions;
use crate::harness::options::{AgentHarnessOptions, ProviderStream, ProviderStreamRequest};
use crate::harness::session::{InMemorySessionStorage, Session, SessionStorage};
use crate::harness::types::{SessionMetadata, SessionTreeEntry};
use crate::types::{AgentTool, AgentToolResult, ThinkingLevel};

mod harness_tests;
mod stream_tests;
mod supplementary;

// ---------------------------------------------------------------------------
// FauxModels — the `completeSimple` fake for compaction / branch summaries.
// ---------------------------------------------------------------------------

type CompleteFn = Box<dyn Fn(&Context, &CompletionOptions) -> AssistantMessage>;

pub(super) struct FauxModels {
    responses: RefCell<std::collections::VecDeque<CompleteFn>>,
}

impl FauxModels {
    pub(super) fn new() -> Self {
        Self {
            responses: RefCell::new(std::collections::VecDeque::new()),
        }
    }

    #[allow(dead_code)]
    pub(super) fn text(text: &str) -> CompleteFn {
        let text = text.to_string();
        Box::new(move |_ctx, _opts| {
            faux_assistant_message(
                vec![faux_text(text.clone())],
                FauxAssistantOptions::default(),
                0,
            )
        })
    }

    #[allow(dead_code)]
    pub(super) fn set_responses(&self, responses: Vec<CompleteFn>) {
        *self.responses.borrow_mut() = responses.into_iter().collect();
    }
}

impl Models for FauxModels {
    fn complete_simple(
        &self,
        _model: &Model,
        context: &Context,
        options: &CompletionOptions,
    ) -> AssistantMessage {
        let f = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("no faux completion queued");
        f(context, options)
    }
}

// ---------------------------------------------------------------------------
// Construction helpers.
// ---------------------------------------------------------------------------

pub(super) fn memory_env() -> Box<MemoryExecutionEnv> {
    Box::new(MemoryExecutionEnv::new("/work"))
}

pub(super) fn new_storage() -> Rc<dyn SessionStorage> {
    Rc::new(InMemorySessionStorage::new())
}

pub(super) fn storage_with_id(id: &str) -> Rc<dyn SessionStorage> {
    Rc::new(InMemorySessionStorage::with_options(
        None,
        Some(SessionMetadata::in_memory(id, "now")),
    ))
}

/// A fresh faux provider with a single default model.
pub(super) fn new_faux() -> Rc<FauxProvider> {
    Rc::new(FauxProvider::new(RegisterFauxProviderOptions::default()))
}

/// A faux provider exposing two reasoning-capable models (`first`, `second`).
pub(super) fn new_faux_two_models() -> Rc<FauxProvider> {
    Rc::new(FauxProvider::new(RegisterFauxProviderOptions {
        models: Some(vec![model_def("first"), model_def("second")]),
        ..RegisterFauxProviderOptions::default()
    }))
}

fn model_def(id: &str) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.to_string(),
        name: None,
        reasoning: Some(true),
        input: None,
        cost: None,
        context_window: None,
        max_tokens: None,
    }
}

/// Default construction options wiring the faux provider's streaming into the
/// harness's provider seam (no recording).
pub(super) fn base_options(
    session: Session,
    faux: Rc<FauxProvider>,
    model: Model,
) -> AgentHarnessOptions {
    AgentHarnessOptions {
        env: memory_env(),
        session,
        models: Box::new(FauxModels::new()),
        stream: passthrough_stream(faux),
        tools: None,
        resources: None,
        system_prompt: None,
        stream_options: None,
        model,
        thinking_level: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    }
}

/// A provider seam that forwards straight to `faux` (no capture).
pub(super) fn passthrough_stream(faux: Rc<FauxProvider>) -> ProviderStream {
    Rc::new(move |req: ProviderStreamRequest| faux.stream(req.model, req.context, None, req.signal))
}

/// The per-call captures a recording provider seam collects.
#[derive(Default)]
pub(super) struct Recorder {
    pub options: Vec<AgentHarnessStreamOptions>,
    pub model_ids: Vec<String>,
    pub reasonings: Vec<Option<ThinkingLevel>>,
    pub system_prompts: Vec<String>,
    pub tool_names: Vec<Vec<String>>,
}

/// A provider seam that records each turn's request before forwarding to `faux`.
pub(super) fn recording_stream(
    faux: Rc<FauxProvider>,
    recorder: Rc<RefCell<Recorder>>,
) -> ProviderStream {
    Rc::new(move |req: ProviderStreamRequest| {
        {
            let mut rec = recorder.borrow_mut();
            rec.options.push(req.options.clone());
            rec.model_ids.push(req.model.id.clone());
            rec.reasonings.push(req.reasoning);
            rec.system_prompts
                .push(req.context.system_prompt.clone().unwrap_or_default());
            let names = req
                .context
                .tools
                .as_ref()
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            rec.tool_names.push(names);
        }
        faux.stream(req.model, req.context, None, req.signal)
    })
}

// ---------------------------------------------------------------------------
// Faux response builders.
// ---------------------------------------------------------------------------

/// A fixed assistant text response.
pub(super) fn text_response(text: &str) -> FauxResponseStep {
    faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0).into()
}

/// A factory response counting the user messages in the request context, used to
/// verify steering/follow-up injection ordering. Recorders captured by a faux
/// factory must be `Send + Sync` (pi's `FauxResponseFactory` bound), so they use
/// `Arc<Mutex<_>>`.
pub(super) fn counting_response(text: &str, counts: Arc<Mutex<Vec<usize>>>) -> FauxResponseStep {
    let text = text.to_string();
    FauxResponseStep::Factory(Box::new(
        move |ctx: &Context, _opts: Option<&StreamOptions>, _state: &FauxState, _model: &Model| {
            let users = ctx
                .messages
                .iter()
                .filter(|m| matches!(m, Message::User(_)))
                .count();
            counts.lock().unwrap().push(users);
            faux_assistant_message(
                vec![faux_text(text.clone())],
                FauxAssistantOptions::default(),
                0,
            )
        },
    ))
}

/// A factory recording the text of every user message in the request context.
pub(super) fn capturing_response(sink: Arc<Mutex<Vec<String>>>) -> FauxResponseStep {
    FauxResponseStep::Factory(Box::new(
        move |ctx: &Context, _opts: Option<&StreamOptions>, _state: &FauxState, _model: &Model| {
            for message in &ctx.messages {
                if let Message::User(user) = message {
                    for text in user_texts(user) {
                        sink.lock().unwrap().push(text);
                    }
                }
            }
            faux_assistant_message(vec![faux_text("ok")], FauxAssistantOptions::default(), 0)
        },
    ))
}

/// An assistant message requesting a single tool call.
pub(super) fn tool_call_response(name: &str, args: Value, id: &str) -> FauxResponseStep {
    faux_assistant_message(
        vec![faux_tool_call(name, args, Some(id.to_string()))],
        FauxAssistantOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..FauxAssistantOptions::default()
        },
        0,
    )
    .into()
}

fn user_texts(user: &atilla_ai::UserMessage) -> Vec<String> {
    match &user.content {
        atilla_ai::UserContent::Text(text) => vec![text.clone()],
        atilla_ai::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                atilla_ai::ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Tool fixtures (ports of `test/utils/calculate.ts`, `get-current-time.ts`).
// ---------------------------------------------------------------------------

pub(super) fn calculate_tool() -> AgentTool {
    AgentTool {
        name: "calculate".into(),
        description: "Evaluate mathematical expressions".into(),
        parameters: json!({ "type": "object" }),
        label: "Calculator".into(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(|_id, args, _signal, _cb| {
            let expression = args
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or_default();
            AgentToolResult {
                content: vec![atilla_ai::ContentBlock::Text {
                    text: format!("{expression} = ok"),
                    text_signature: None,
                }],
                details: Value::Null,
                added_tool_names: None,
                terminate: None,
            }
        }),
    }
}

pub(super) fn get_current_time_tool() -> AgentTool {
    AgentTool {
        name: "get_current_time".into(),
        description: "Get the current date and time".into(),
        parameters: json!({ "type": "object" }),
        label: "Current Time".into(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(|_id, _args, _signal, _cb| AgentToolResult {
            content: vec![atilla_ai::ContentBlock::Text {
                text: "now".into(),
                text_signature: None,
            }],
            details: json!({ "utcTimestamp": 0 }),
            added_tool_names: None,
            terminate: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// Shared assertion helpers.
// ---------------------------------------------------------------------------

/// The `role` of an [`AgentMessage`](crate::types::AgentMessage) value.
pub(super) fn role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// User-message text parts persisted in the session, in order.
pub(super) fn persisted_user_texts(session: &Session) -> Vec<String> {
    let mut out = Vec::new();
    for entry in session.get_entries() {
        if let SessionTreeEntry::Message(entry) = entry {
            if role(&entry.message) == Some("user") {
                if let Some(content) = entry.message.get("content") {
                    collect_text(content, &mut out);
                }
            }
        }
    }
    out
}

/// Roles of message entries persisted in the session, in order.
pub(super) fn persisted_roles(session: &Session) -> Vec<String> {
    session
        .get_entries()
        .into_iter()
        .filter_map(|entry| match entry {
            SessionTreeEntry::Message(entry) => role(&entry.message).map(str::to_string),
            _ => None,
        })
        .collect()
}

fn collect_text(content: &Value, out: &mut Vec<String>) {
    match content {
        Value::String(text) => out.push(text.clone()),
        Value::Array(parts) => {
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        out.push(text.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// A subscriber that records the `type` discriminant of every event.
pub(super) fn recording_subscriber(
    sink: Rc<RefCell<Vec<String>>>,
) -> crate::harness::agent_harness::Subscriber {
    Rc::new(
        move |event: &AgentHarnessEvent, _signal: Option<&AbortSignal>| {
            let ty = match event {
                AgentHarnessEvent::Own(own) => own.type_str().to_string(),
                AgentHarnessEvent::Loop(loop_event) => loop_event_type(loop_event).to_string(),
            };
            sink.borrow_mut().push(ty);
        },
    )
}

pub(super) fn loop_event_type(event: &crate::types::AgentEvent) -> &'static str {
    use crate::types::AgentEvent::*;
    match event {
        AgentStart => "agent_start",
        AgentEnd { .. } => "agent_end",
        TurnStart => "turn_start",
        TurnEnd { .. } => "turn_end",
        MessageStart { .. } => "message_start",
        MessageUpdate { .. } => "message_update",
        MessageEnd { .. } => "message_end",
        ToolExecutionStart { .. } => "tool_execution_start",
        ToolExecutionUpdate { .. } => "tool_execution_update",
        ToolExecutionEnd { .. } => "tool_execution_end",
    }
}
