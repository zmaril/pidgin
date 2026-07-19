//! Shared in-memory test harness for the `AgentSession` turn-runner and queue
//! suites, ported from pi's `test/suite/harness.ts` (+ the concurrent-suite
//! setup in `test/agent-session-concurrent.test.ts`).
//!
//! The harness wires a faux [`StreamFn`](atilla_agent::types::StreamFn) driven by
//! a scripted response list to an in-memory session/settings/model runtime and
//! records every emitted [`AgentSessionEvent`].
//!
//! ## Sync/eager + `!Send` model note
//!
//! `atilla_agent`'s agent loop is synchronous and eager: a `prompt` runs the
//! whole turn to completion on the calling thread. `AgentSession` is also **not**
//! `Send`/`Sync` (its resource loader / settings manager hold single-threaded
//! state), so it cannot cross a thread boundary and every mid-run hook (tool
//! `execute`, event listeners, the stream fn — all `Send + Sync`-bounded) is
//! structurally unable to capture the live session. There is therefore no way to
//! call `session.steer`/`follow_up`/`prompt` *while a turn is in flight*.
//!
//! The queue tests instead enqueue steering / follow-up messages while **idle**
//! and let the agent loop drain them (the loop polls its steering queue at the
//! start of each turn and its follow-up queue when it would otherwise stop), which
//! exercises the same `AgentSession` mirror-push / `queue_update` / splice-on-drain
//! code paths. Cases whose premise strictly requires genuine in-flight streaming
//! are `#[ignore]`d in the suites with a precise reason.

// straitjacket-allow-file:duplication

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atilla_agent::agent::{Agent, AgentOptions, InitialAgentState};
use atilla_agent::types::{AgentMessage, AgentTool, AgentToolResult, AgentToolUpdateCallback};
use atilla_ai::providers::faux::{faux_assistant_message, FauxAssistantOptions};
use atilla_ai::seams::{AbortSignal, StreamResult};
use atilla_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Model, ModelCost, StopReason,
    StreamOptions,
};

use crate::core::extensions::command::{CommandContext, RegisteredCommand, ResolvedCommand};
use crate::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use crate::core::extensions::events::common::{BuildSystemPromptOptions, ImageContent};
use crate::core::extensions::events::selection::{
    InputEvent, InputEventResult, InputSource, StreamingBehavior,
};
use crate::core::extensions::events::session::{
    ResourcesDiscoverReason, ResourcesDiscoverResult, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionShutdownEvent,
};
use crate::core::extensions::events::tool::{
    ToolCallEvent, ToolCallEventResult, ToolResultEvent, ToolResultEventResult,
};
use crate::core::extensions::events::turn::MessageEndEvent;
use crate::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionEmitOutcome,
    ExtensionErrorListener, ExtensionMode, ExtensionRunner, ExtensionUIContext, FlagValue,
    ProviderRegistrationHost, RegisteredTool, SessionContextHost, SessionControlHost,
    StubExtensionRunner, UnsubscribeFn,
};
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPath};
use crate::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions,
};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::{Settings, SettingsManager};
use crate::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

use super::events::AgentSessionEvent;
use super::session::{AgentSession, AgentSessionConfig};

/// A scripted provider response: a canned message, or a function of the request
/// context (pi's `FauxResponseStep`).
pub(crate) enum FauxResponse {
    /// A fixed message.
    Message(Box<AssistantMessage>),
    /// A message computed from the streaming context.
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
pub(crate) fn faux_model() -> Model {
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

/// A text content block.
pub(crate) fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        text_signature: None,
    }
}

/// A plain-text assistant response (pi's `fauxAssistantMessage("text")`).
pub(crate) fn assistant_text(text: &str) -> AssistantMessage {
    faux_assistant_message(vec![text_block(text)], FauxAssistantOptions::default(), 0)
}

/// An error assistant response (pi's `fauxAssistantMessage("", { stopReason:
/// "error", errorMessage })`).
pub(crate) fn assistant_error(error_message: &str) -> AssistantMessage {
    faux_assistant_message(
        Vec::new(),
        FauxAssistantOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(error_message.to_string()),
            ..Default::default()
        },
        0,
    )
}

/// A `{ retry: { enabled, maxRetries, baseDelayMs } }` settings override (pi's
/// `createHarness({ settings: { retry: {...} } })`). Any component left `None`
/// falls back to the resolved default.
pub(crate) fn retry_settings(
    enabled: bool,
    max_retries: Option<i64>,
    base_delay_ms: Option<i64>,
) -> Settings {
    let mut retry = serde_json::Map::new();
    retry.insert("enabled".to_string(), json!(enabled));
    if let Some(max_retries) = max_retries {
        retry.insert("maxRetries".to_string(), json!(max_retries));
    }
    if let Some(base_delay_ms) = base_delay_ms {
        retry.insert("baseDelayMs".to_string(), json!(base_delay_ms));
    }
    let mut map = serde_json::Map::new();
    map.insert("retry".to_string(), Value::Object(retry));
    Settings::from_map(map)
}

/// A tool-use assistant response (pi's `fauxAssistantMessage(fauxToolCall(...),
/// { stopReason: "toolUse" })`).
pub(crate) fn assistant_tool_use(content: Vec<ContentBlock>) -> AssistantMessage {
    faux_assistant_message(
        content,
        FauxAssistantOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
        0,
    )
}

/// A `{ role: "user", content: [text] }` message value.
pub(crate) fn user_message(text: &str) -> AgentMessage {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": 0,
    })
}

/// The error message pi's faux provider streams when its response list is
/// exhausted (`packages/ai/src/providers/faux.ts`).
fn exhausted_response() -> AssistantMessage {
    faux_assistant_message(
        Vec::new(),
        FauxAssistantOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("No more faux responses queued".to_string()),
            ..Default::default()
        },
        0,
    )
}

/// A configured in-memory harness (pi's `createHarness`).
pub(crate) struct Harness {
    /// The session under test.
    pub session: AgentSession,
    /// A cheap handle to the same agent the session owns (pi's `session.agent`).
    pub agent: Agent,
    /// The scripted response list and its cursor.
    responses: Arc<Mutex<(Vec<FauxResponse>, usize)>>,
    /// Total provider stream invocations, including exhausted ones (pi's
    /// `faux.state.callCount`).
    call_count: Arc<AtomicUsize>,
    /// Every emitted session event, in order.
    pub events: Arc<Mutex<Vec<AgentSessionEvent>>>,
    _temp_dir: tempfile::TempDir,
}

/// Options for [`create_harness`].
pub(crate) struct HarnessOptions {
    /// Extra tools for the agent.
    pub tools: Vec<AgentTool>,
    /// Whether the agent starts with the `faux` model selected.
    pub with_model: bool,
    /// Whether the model runtime reports configured auth for `faux`.
    pub with_configured_auth: bool,
    /// A factory that builds the extension runner from a handle to the agent (so a
    /// runner can enqueue on the agent's queues). `None` uses the
    /// [`StubExtensionRunner`].
    #[allow(clippy::type_complexity)]
    pub make_runner: Option<Box<dyn FnOnce(&Agent) -> Box<dyn ExtensionRunner>>>,
    /// Settings overrides applied before the session is built (pi's
    /// `createHarness({ settings })` → `settingsManager.applyOverrides`). Used by
    /// the retry suite to set `retry.baseDelayMs`/`maxRetries`/`enabled`.
    pub settings: Option<Settings>,
    /// The compaction summarization provider seam (pi's per-test
    /// `session.agent.streamFn = ...` override that returns the summary). `Some`
    /// is the "custom `streamFn`" analog: it drives summarization and bypasses the
    /// configured-auth gate. `None` is the default `streamSimple` analog.
    pub summarization_models: Option<Box<dyn crate::core::compaction::Models>>,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            with_model: true,
            with_configured_auth: true,
            make_runner: None,
            settings: None,
            summarization_models: None,
        }
    }
}

impl Harness {
    /// Replace the scripted response list (pi's `setResponses`).
    pub fn set_responses(&self, responses: Vec<FauxResponse>) {
        let mut guard = self.responses.lock().unwrap();
        *guard = (responses, 0);
    }

    /// The number of unconsumed scripted responses (pi's `getPendingResponseCount`).
    pub fn pending_response_count(&self) -> usize {
        let guard = self.responses.lock().unwrap();
        guard.0.len().saturating_sub(guard.1)
    }

    /// Total provider stream invocations so far (pi's `faux.state.callCount`).
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    /// The roles of every message currently in agent state.
    pub fn message_roles(&self) -> Vec<String> {
        self.session
            .messages()
            .iter()
            .filter_map(|m| m.get("role").and_then(Value::as_str).map(String::from))
            .collect()
    }
}

/// A compaction summarization [`Models`](crate::core::compaction::Models) seam
/// that returns a fixed `summary` and counts its calls (pi's per-test
/// `useSummaryStreamFn`, which overrides `session.agent.streamFn` to emit the
/// summary and returns a call counter).
pub(crate) struct SummaryModels {
    summary: String,
    calls: Arc<AtomicUsize>,
}

impl SummaryModels {
    /// Build a summarizer that always answers with `summary`; the returned counter
    /// tracks how many times it was invoked (pi returns `getStreamCallCount`).
    pub fn build(summary: &str) -> (Box<dyn crate::core::compaction::Models>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let models = Box::new(Self {
            summary: summary.to_string(),
            calls: Arc::clone(&calls),
        });
        (models, calls)
    }
}

impl crate::core::compaction::Models for SummaryModels {
    fn complete_simple(
        &self,
        model: &Model,
        _context: &Context,
        _options: &crate::core::compaction::CompletionOptions,
    ) -> AssistantMessage {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut message = faux_assistant_message(
            vec![text_block(&self.summary)],
            FauxAssistantOptions::default(),
            0,
        );
        message.api = model.api.clone();
        message.provider = model.provider.clone();
        message.model = model.id.clone();
        message
    }
}

/// The text content of an [`AgentMessage`] value (pi's `getMessageText`).
pub(crate) fn message_text(message: &Value) -> String {
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

/// The texts of every `user` message in agent state (pi's `getUserTexts`).
pub(crate) fn user_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(message_text)
        .collect()
}

/// The texts of every `assistant` message in agent state (pi's `getAssistantTexts`).
pub(crate) fn assistant_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .map(message_text)
        .collect()
}

/// Count the events matching `matcher` (pi's ad-hoc event filters).
pub(crate) fn events_of_type(
    harness: &Harness,
    matcher: impl Fn(&AgentSessionEvent) -> bool,
) -> usize {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matcher(e))
        .count()
}

/// An echo tool that records the `text` argument of each call.
pub(crate) fn echo_tool(runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
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
pub(crate) fn recording_tool(name: &str, runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
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

/// Build the harness (pi's `createHarness`).
pub(crate) fn create_harness(options: HarnessOptions) -> Harness {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cwd = temp_dir.path().to_string_lossy().to_string();
    let agent_dir = temp_dir.path().join(".agent").to_string_lossy().to_string();

    let model_runtime = build_model_runtime(&temp_dir, options.with_configured_auth);
    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        ..Default::default()
    });

    let responses: Arc<Mutex<(Vec<FauxResponse>, usize)>> = Arc::new(Mutex::new((Vec::new(), 0)));
    let call_count = Arc::new(AtomicUsize::new(0));
    let stream_responses = Arc::clone(&responses);
    let stream_call_count = Arc::clone(&call_count);
    let stream_fn: atilla_agent::types::StreamFn = Arc::new(
        move |_model: &Model,
              context: &Context,
              _options: Option<&StreamOptions>,
              _signal: Option<&AbortSignal>| {
            stream_call_count.fetch_add(1, Ordering::SeqCst);
            let message = {
                let mut guard = stream_responses.lock().unwrap();
                let (list, index) = &mut *guard;
                match list.get(*index) {
                    Some(step) => {
                        let message = match step {
                            FauxResponse::Message(message) => (**message).clone(),
                            FauxResponse::Fn(builder) => builder(context),
                        };
                        *index += 1;
                        message
                    }
                    // pi's faux provider streams an error message when exhausted,
                    // rather than throwing.
                    None => exhausted_response(),
                }
            };
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
    // A cheap handle to the same shared agent state, for the runner factory and
    // for `harness.agent`.
    let agent_handle = agent.clone();

    let extension_runner = options.make_runner.map(|factory| factory(&agent_handle));

    let mut settings_manager = SettingsManager::create(&cwd, &agent_dir);
    if let Some(overrides) = options.settings {
        settings_manager.apply_overrides(overrides);
    }

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: SessionManager::in_memory(&cwd),
        settings_manager,
        cwd,
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime,
        initial_active_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner,
        session_start_event: None,
        summarization_models: options.summarization_models,
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
        agent: agent_handle,
        responses,
        call_count,
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

// ---------------------------------------------------------------------------
// TestExtensionRunner: a configurable runner for the extension-touching cases
// ---------------------------------------------------------------------------

/// Callback invoked (once) on the first `agent_end` emit (pi's
/// `pi.on("agent_end", ...)`). Captures only `Send + Sync` handles.
pub(crate) type AgentEndCallback = Arc<dyn Fn() + Send + Sync>;

/// A configurable [`ExtensionRunner`] for the extension-touching queue/concurrent
/// cases. Every method delegates to an inner [`StubExtensionRunner`] except the
/// hooks a test opts into:
///
/// * `input_events` — when set, `has_handlers("input")` is `true` and `emit_input`
///   records each [`InputEvent`] (pi's `pi.on("input", ...)`).
/// * `commands` — names for which `get_command` returns a stub command (pi's
///   `pi.registerCommand(...)`).
/// * `on_agent_end` — invoked on the first `agent_end` emit (pi's
///   `pi.on("agent_end", ...)`).
pub(crate) struct TestExtensionRunner {
    inner: StubExtensionRunner,
    input_events: Option<Arc<Mutex<Vec<InputEvent>>>>,
    commands: Vec<String>,
    on_agent_end: Option<AgentEndCallback>,
    agent_end_fired: AtomicBool,
    event_order: Option<Arc<Mutex<Vec<String>>>>,
    #[allow(clippy::type_complexity)]
    before_compact:
        Option<Arc<dyn Fn(&SessionBeforeCompactEvent) -> SessionBeforeCompactResult + Send + Sync>>,
}

/// A `session_before_compact` handler for [`TestExtensionRunner`] (pi's
/// `pi.on("session_before_compact", ...)`).
pub(crate) type BeforeCompactHandler =
    Arc<dyn Fn(&SessionBeforeCompactEvent) -> SessionBeforeCompactResult + Send + Sync>;

impl TestExtensionRunner {
    /// Start from a runner that only forwards to the stub.
    pub fn new() -> Self {
        Self {
            inner: StubExtensionRunner,
            input_events: None,
            commands: Vec::new(),
            on_agent_end: None,
            agent_end_fired: AtomicBool::new(false),
            event_order: None,
            before_compact: None,
        }
    }

    /// Register a `session_before_compact` handler (pi's
    /// `pi.on("session_before_compact", ...)`); it may cancel or supply a
    /// replacement compaction. Also reports `has_handlers("session_before_compact")`.
    pub fn with_before_compact(mut self, handler: BeforeCompactHandler) -> Self {
        self.before_compact = Some(handler);
        self
    }

    /// Record `extension:message_start:<role>` / `extension:message_end:<role>`
    /// into `sink` as each message event is dispatched (pi's `pi.on("message_start"
    /// / "message_end", ...)` order-recording handlers), and report
    /// `has_handlers("message_start"/"message_end")`.
    pub fn with_event_order(mut self, sink: Arc<Mutex<Vec<String>>>) -> Self {
        self.event_order = Some(sink);
        self
    }

    /// Record every `input` event into `sink` and report `has_handlers("input")`.
    pub fn with_input_recording(mut self, sink: Arc<Mutex<Vec<InputEvent>>>) -> Self {
        self.input_events = Some(sink);
        self
    }

    /// Register `name` as an extension command (so it cannot be queued).
    pub fn with_command(mut self, name: &str) -> Self {
        self.commands.push(name.to_string());
        self
    }

    /// Invoke `callback` on the first `agent_end` emit.
    pub fn with_agent_end(mut self, callback: AgentEndCallback) -> Self {
        self.on_agent_end = Some(callback);
        self
    }
}

/// A stub [`ResolvedCommand`] used by [`TestExtensionRunner::get_command`].
fn stub_resolved_command(name: &str) -> ResolvedCommand {
    ResolvedCommand {
        command: RegisteredCommand {
            name: name.to_string(),
            source_info: SourceInfo {
                path: "/test/extension.ts".to_string(),
                source: "test".to_string(),
                scope: SourceScope::Project,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            },
            description: Some("Test command".to_string()),
            get_argument_completions: None,
            handler: Arc::new(|_args, _ctx| Ok(())),
        },
        invocation_name: name.to_string(),
    }
}

impl ExtensionRunner for TestExtensionRunner {
    fn emit_session_shutdown(&self, event: SessionShutdownEvent) {
        self.inner.emit_session_shutdown(event);
    }

    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        if let ExtensionDispatchEvent::AgentEnd(_) = event {
            if let Some(callback) = &self.on_agent_end {
                if !self.agent_end_fired.swap(true, Ordering::SeqCst) {
                    callback();
                }
            }
        }
        if let ExtensionDispatchEvent::SessionBeforeCompact(before) = event {
            if let Some(handler) = &self.before_compact {
                return ExtensionEmitOutcome::BeforeCompact(handler(before));
            }
        }
        if let (Some(sink), ExtensionDispatchEvent::MessageStart(start)) =
            (&self.event_order, event)
        {
            let role = start
                .message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            sink.lock()
                .unwrap()
                .push(format!("extension:message_start:{role}"));
        }
        self.inner.emit(event)
    }

    fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage> {
        if let Some(sink) = &self.event_order {
            let role = event
                .message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            sink.lock()
                .unwrap()
                .push(format!("extension:message_end:{role}"));
        }
        self.inner.emit_message_end(event)
    }

    fn emit_input(
        &self,
        text: &str,
        images: Option<&[ImageContent]>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult {
        if let Some(sink) = &self.input_events {
            sink.lock().unwrap().push(InputEvent {
                text: text.to_string(),
                images: images.map(<[ImageContent]>::to_vec),
                source,
                streaming_behavior,
            });
        }
        InputEventResult::Continue
    }

    fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<&[ImageContent]>,
        system_prompt: &str,
        system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult> {
        self.inner
            .emit_before_agent_start(prompt, images, system_prompt, system_prompt_options)
    }

    fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        self.inner.emit_resources_discover(cwd, reason)
    }

    fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        self.inner.emit_tool_call(event)
    }

    fn emit_tool_result(&self, event: &ToolResultEvent) -> Option<ToolResultEventResult> {
        self.inner.emit_tool_result(event)
    }

    fn has_handlers(&self, event_type: &str) -> bool {
        (event_type == "input" && self.input_events.is_some())
            || (matches!(event_type, "message_start" | "message_end") && self.event_order.is_some())
            || (event_type == "session_before_compact" && self.before_compact.is_some())
            || self.inner.has_handlers(event_type)
    }

    fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        if self.commands.iter().any(|c| c == name) {
            return Some(stub_resolved_command(name));
        }
        self.inner.get_command(name)
    }

    fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        self.inner.get_registered_commands()
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        self.inner.get_all_registered_tools()
    }

    fn get_flag_values(&self) -> std::collections::BTreeMap<String, FlagValue> {
        self.inner.get_flag_values()
    }

    fn create_command_context(&self) -> Box<dyn CommandContext> {
        self.inner.create_command_context()
    }

    fn bind_core(
        &self,
        actions: Arc<dyn SessionControlHost>,
        context_actions: Arc<dyn SessionContextHost>,
        provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    ) {
        self.inner
            .bind_core(actions, context_actions, provider_actions);
    }

    fn set_ui_context(&self, ui_context: Option<ExtensionUIContext>, mode: ExtensionMode) {
        self.inner.set_ui_context(ui_context, mode);
    }

    fn bind_command_context(&self, actions: Option<Arc<dyn ExtensionCommandContextHost>>) {
        self.inner.bind_command_context(actions);
    }

    fn on_error(&self, listener: ExtensionErrorListener) -> UnsubscribeFn {
        self.inner.on_error(listener)
    }

    fn emit_error(&self, error: ExtensionError) {
        self.inner.emit_error(error);
    }

    fn invalidate(&self, message: &str) {
        self.inner.invalidate(message);
    }
}
