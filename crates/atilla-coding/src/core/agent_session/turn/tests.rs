//! Turn-runner tests, ported from pi's `test/suite/agent-session-prompt.test.ts`.
//!
//! Each `#[test]` mirrors a pi `AgentSession prompt characterization` case: same
//! setup (a faux stream fn + in-memory session/settings/model runtime), same
//! assertions on the emitted events and persisted / in-state messages. The pi
//! cases that depend on subsystems deferred to a later PR of the AgentSession
//! port are `#[ignore]`d with the PR that enables them.

// straitjacket-allow-file:duplication

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_agent::agent::{Agent, AgentOptions, InitialAgentState};
use atilla_agent::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use atilla_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantOptions};
use atilla_ai::seams::{AbortSignal, StreamResult};
use atilla_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Model, ModelCost, StopReason,
    StreamOptions,
};

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::session::{AgentSession, AgentSessionConfig};
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPath};
use crate::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions,
};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;

use super::PromptError;

// ---------------------------------------------------------------------------
// Faux stream fn + harness (mirrors test/suite/harness.ts)
// ---------------------------------------------------------------------------

/// A scripted provider response: a canned message, or a function of the request
/// context (pi's `FauxResponseStep`).
enum FauxResponse {
    Message(Box<AssistantMessage>),
    #[allow(clippy::type_complexity)]
    Fn(Box<dyn Fn(&Context) -> AssistantMessage + Send + Sync>),
}

/// A [`StreamResult`] whose only event is the terminal `done`/`error` carrying
/// the final message (pi's `MockAssistantStream`).
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

/// The `faux` test model (pi's `registerFauxProvider().getModel()`).
fn faux_model() -> Model {
    Model {
        id: "faux-1".to_string(),
        name: "faux-1".to_string(),
        api: "openai-completions".to_string(),
        provider: "faux".to_string(),
        base_url: "https://faux.test/v1".to_string(),
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
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        text_signature: None,
    }
}

/// A plain-text assistant response (pi's `fauxAssistantMessage("text")`).
fn assistant_text(text: &str) -> AssistantMessage {
    faux_assistant_message(vec![text_block(text)], FauxAssistantOptions::default(), 0)
}

/// A tool-use assistant response (pi's `fauxAssistantMessage(fauxToolCall(...), { stopReason: "toolUse" })`).
fn assistant_tool_use(content: Vec<ContentBlock>) -> AssistantMessage {
    faux_assistant_message(
        content,
        FauxAssistantOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
        0,
    )
}

/// A configured in-memory harness (pi's `createHarness`).
struct Harness {
    session: AgentSession,
    responses: Arc<Mutex<(Vec<FauxResponse>, usize)>>,
    events: Arc<Mutex<Vec<AgentSessionEvent>>>,
    _temp_dir: tempfile::TempDir,
}

struct HarnessOptions {
    tools: Vec<AgentTool>,
    with_model: bool,
    with_configured_auth: bool,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            with_model: true,
            with_configured_auth: true,
        }
    }
}

impl Harness {
    fn set_responses(&self, responses: Vec<FauxResponse>) {
        let mut guard = self.responses.lock().unwrap();
        *guard = (responses, 0);
    }

    fn pending_response_count(&self) -> usize {
        let guard = self.responses.lock().unwrap();
        guard.0.len() - guard.1
    }

    fn message_roles(&self) -> Vec<String> {
        self.session
            .messages()
            .iter()
            .filter_map(|m| m.get("role").and_then(Value::as_str).map(String::from))
            .collect()
    }
}

/// The text content of an [`AgentMessage`] value (pi's `getMessageText`).
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn create_harness(options: HarnessOptions) -> Harness {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cwd = temp_dir.path().to_string_lossy().to_string();
    let agent_dir = temp_dir.path().join(".agent").to_string_lossy().to_string();

    // A model runtime that knows the `faux` provider, optionally with a runtime
    // api key so `has_configured_auth("faux")` is true.
    let model_runtime = build_model_runtime(&temp_dir, options.with_configured_auth);

    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        ..Default::default()
    });

    let responses: Arc<Mutex<(Vec<FauxResponse>, usize)>> = Arc::new(Mutex::new((Vec::new(), 0)));
    let stream_responses = Arc::clone(&responses);
    let stream_fn: atilla_agent::types::StreamFn = Arc::new(
        move |_model: &Model,
              context: &Context,
              _options: Option<&StreamOptions>,
              _signal: Option<&AbortSignal>| {
            let mut guard = stream_responses.lock().unwrap();
            let (list, index) = &mut *guard;
            let step = list.get(*index).unwrap_or_else(|| {
                panic!("no queued faux response for stream call #{index}");
            });
            let message = match step {
                FauxResponse::Message(message) => (**message).clone(),
                FauxResponse::Fn(builder) => builder(context),
            };
            *index += 1;
            mock_stream(message)
        },
    );

    let initial_state = InitialAgentState {
        system_prompt: Some("You are a test assistant.".to_string()),
        model: options.with_model.then(faux_model),
        thinking_level: None,
        tools: Some(options.tools),
        messages: None,
    };
    let agent = Agent::new(AgentOptions {
        initial_state: Some(initial_state),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: SessionManager::in_memory(&cwd),
        settings_manager: SettingsManager::create(&cwd, &agent_dir),
        cwd,
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime,
        initial_active_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner: None,
        session_start_event: None,
    });

    let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    // The unsubscribe handle is intentionally dropped; the listener stays
    // registered for the harness lifetime (dropping the handle does not remove it).
    let _unsubscribe = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
        sink.lock().unwrap().push(event.clone());
    }));

    Harness {
        session,
        responses,
        events,
        _temp_dir: temp_dir,
    }
}

/// A model runtime that knows the `faux` provider via a temp `models.json`,
/// optionally marked configured through a runtime api key.
fn build_model_runtime(temp_dir: &tempfile::TempDir, with_configured_auth: bool) -> ModelRuntime {
    let models_path = temp_dir.path().join("models.json");
    let providers = json!({
        "providers": {
            "faux": {
                "baseUrl": "https://faux.test/v1",
                "api": "openai-completions",
                "models": [{
                    "id": "faux-1",
                    "name": "faux-1",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 128000,
                    "maxTokens": 4096
                }]
            }
        }
    });
    std::fs::write(&models_path, providers.to_string()).expect("write models.json");

    let mut runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: ModelsPath::Path(models_path.to_string_lossy().to_string()),
        allow_model_network: Some(false),
        ..Default::default()
    });
    if with_configured_auth {
        runtime.set_runtime_api_key("faux", "faux-key");
    }
    runtime
}

/// An echo tool that records the `text` argument of each call.
fn echo_tool(runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: json!({ "type": "object" }),
        label: "Echo".to_string(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(
            move |_id: &str,
                  params: &Value,
                  _signal: Option<&AbortSignal>,
                  _on_update: Option<&AgentToolUpdateCallback>| {
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                runs.lock().unwrap().push(text.clone());
                AgentToolResult {
                    content: vec![text_block(&format!("echo:{text}"))],
                    details: json!({ "text": text }),
                    added_tool_names: None,
                    terminate: None,
                }
            },
        ),
    }
}

/// A tool that records `name:value` for each call.
fn recording_tool(name: &str, runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
    let tool_name = name.to_string();
    AgentTool {
        name: name.to_string(),
        description: format!("{name} tool"),
        parameters: json!({ "type": "object" }),
        label: name.to_string(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(
            move |_id: &str,
                  params: &Value,
                  _signal: Option<&AbortSignal>,
                  _on_update: Option<&AgentToolUpdateCallback>| {
                let value = params
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                runs.lock().unwrap().push(format!("{tool_name}:{value}"));
                AgentToolResult {
                    content: vec![text_block(&format!("{tool_name}:{value}"))],
                    details: json!({ "value": value }),
                    added_tool_names: None,
                    terminate: None,
                }
            },
        ),
    }
}

fn events_of_type(harness: &Harness, matcher: impl Fn(&AgentSessionEvent) -> bool) -> usize {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matcher(e))
        .count()
}

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
                saw_image_stream.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    assert_eq!(saw_image.load(std::sync::atomic::Ordering::SeqCst), 1);
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
// Deferred pi cases (enabled by later PRs of the AgentSession port)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unit5: enabled by PR7 (skill-command expansion)"]
fn expands_skill_commands_before_sending_the_prompt() {}

#[test]
#[ignore = "unit5: enabled by PR7 (prompt-template expansion)"]
fn expands_prompt_templates_before_sending_the_prompt() {}

#[test]
#[ignore = "unit5: enabled by PR7 (extension-command dispatch)"]
fn dispatches_extension_commands_without_consuming_a_provider_response() {}

#[test]
#[ignore = "unit5: enabled by PR4 (sendUserMessage delivery)"]
fn send_user_message_while_idle_triggers_a_turn() {}

#[test]
#[ignore = "unit5: enabled by PR7 (input-handler extension events)"]
fn does_not_report_streaming_behavior_to_input_handlers_while_idle() {}

#[test]
#[ignore = "unit5: enabled by PR4 (streaming queue routing) + PR7 (input handlers)"]
fn reports_streaming_behavior_to_input_handlers_while_streaming() {}

#[test]
#[ignore = "unit5: enabled by PR4 (streaming guard requires concurrent in-flight run)"]
fn throws_when_prompted_during_streaming_without_a_streaming_behavior() {}
