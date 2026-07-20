//! `AgentSessionRuntime` characterization, ported from pi's
//! `test/suite/agent-session-runtime.test.ts` (~10 cases) and
//! `test/agent-session-runtime-events.test.ts` (~4 cases).
//!
//! ## Harness
//!
//! pi drives the runtime with a real faux-provider session factory and records the
//! `session_before_switch` / `session_before_fork` / `session_shutdown` /
//! `session_start` lifecycle via extension handlers registered through
//! `createRuntimeForTest`. This port mirrors that with an offline factory that
//! builds a faux-model session ([`build_runtime_session`]) around each prepared
//! [`SessionManager`], and a [`RecordingRunner`] extension runner (shared across
//! every session the factory builds) that records the same four lifecycle events
//! and honors switch/fork cancellation.
//!
//! ## Adaptations
//!
//! The offline factory constructs sessions directly (to inject the lifecycle
//! runner) rather than through the sdk `create_agent_session`, so it does not
//! reproduce that factory's model/thinking restore-on-resume — the one pi case that
//! turns on it is `#[ignore]`d with a precise reason. The extension-context
//! staleness pi asserts after `dispose` (`extensionRunner.createContext()` throws)
//! is not reproduced: the runner seam exposes no `createContext`, so that suite's
//! phase-ordering case is ported without the staleness probe.

// straitjacket-allow-file:duplication

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_agent::agent::{Agent, AgentOptions, InitialAgentState};
use pidgin_agent::types::{AgentMessage, StreamFn};
use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, Context, Model, StopReason, StreamOptions,
};

use crate::core::agent_session::test_support::{assistant_text, faux_model, FauxResponse};
use crate::core::extensions::command::{CommandContext, ResolvedCommand};
use crate::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use crate::core::extensions::events::common::{BuildSystemPromptOptions, ImageContent};
use crate::core::extensions::events::selection::{
    InputEventResult, InputSource, StreamingBehavior,
};
use crate::core::extensions::events::session::{
    ForkPosition, ResourcesDiscoverReason, ResourcesDiscoverResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchReason,
    SessionBeforeSwitchResult, SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
    SessionStartReason,
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
use crate::core::settings_manager::SettingsManager;

use super::super::session::{AgentSession, AgentSessionConfig};
use super::{
    create_agent_session_runtime, AgentSessionRuntime, AgentSessionRuntimeError,
    AgentSessionRuntimeFactoryOptions, AgentSessionRuntimeResult, CreateAgentSessionRuntimeFactory,
    ForkOptions, NewSessionOptions,
};

// ---------------------------------------------------------------------------
// Recorded lifecycle events
// ---------------------------------------------------------------------------

/// A recorded session-lifecycle event (pi's `RecordedSessionEvent`).
#[derive(Debug, Clone, PartialEq)]
enum Lifecycle {
    BeforeSwitch(SessionBeforeSwitchEvent),
    BeforeFork(SessionBeforeForkEvent),
    Shutdown(SessionShutdownEvent),
    Start(SessionStartEvent),
}

fn start(reason: SessionStartReason, previous: Option<&str>) -> Lifecycle {
    Lifecycle::Start(SessionStartEvent {
        reason,
        previous_session_file: previous.map(String::from),
    })
}

fn shutdown(reason: SessionShutdownReason, target: Option<&str>) -> Lifecycle {
    Lifecycle::Shutdown(SessionShutdownEvent {
        reason,
        target_session_file: target.map(String::from),
    })
}

fn before_switch(reason: SessionBeforeSwitchReason, target: Option<&str>) -> Lifecycle {
    Lifecycle::BeforeSwitch(SessionBeforeSwitchEvent {
        reason,
        target_session_file: target.map(String::from),
    })
}

fn before_fork(entry_id: &str, position: ForkPosition) -> Lifecycle {
    Lifecycle::BeforeFork(SessionBeforeForkEvent {
        entry_id: entry_id.to_string(),
        position,
    })
}

// ---------------------------------------------------------------------------
// RecordingRunner
// ---------------------------------------------------------------------------

/// A `message_end` replacement handler (pi's `pi.on("message_end", ...)`).
type MessageEndHandler = Arc<dyn Fn(&AgentMessage) -> Option<AgentMessage> + Send + Sync>;

/// The shared state every [`RecordingRunner`] the factory builds writes into, so a
/// `/new`, `/resume`, or `/fork` records into the same sink across the outgoing and
/// incoming sessions (pi records via one closure over the whole test).
#[derive(Clone)]
struct RunnerHandles {
    events: Arc<Mutex<Vec<Lifecycle>>>,
    /// Event names for which a handler is registered (`has_handlers` is true and the
    /// event is recorded), mirroring pi's per-test `pi.on(...)` set.
    recorded: Arc<HashSet<String>>,
    /// When it matches the emitted `session_before_switch` reason, the switch is
    /// cancelled (pi's `cancelReason` variable).
    switch_cancel: Arc<Mutex<Option<SessionBeforeSwitchReason>>>,
    /// When true, the next `session_before_fork` is cancelled and the flag resets
    /// (pi's `cancelNextFork`).
    fork_cancel: Arc<Mutex<bool>>,
    /// An ordered phase sink for the `before_session_invalidate` ordering case (pi's
    /// `phases` array); `session_shutdown` pushes `"session_shutdown"` into it.
    phase_sink: Option<Arc<Mutex<Vec<String>>>>,
    /// A `message_end` replacement handler (pi's `pi.on("message_end", ...)`).
    message_end: Option<MessageEndHandler>,
}

impl RunnerHandles {
    fn build(&self) -> Box<dyn ExtensionRunner> {
        Box::new(RecordingRunner {
            inner: StubExtensionRunner,
            handles: self.clone(),
        })
    }
}

/// A configurable [`ExtensionRunner`] that records the session-lifecycle events the
/// runtime emits and honors switch/fork cancellation. Every non-lifecycle method
/// delegates to an inner [`StubExtensionRunner`].
struct RecordingRunner {
    inner: StubExtensionRunner,
    handles: RunnerHandles,
}

impl RecordingRunner {
    fn records(&self, event_type: &str) -> bool {
        self.handles.recorded.contains(event_type)
    }
}

impl ExtensionRunner for RecordingRunner {
    fn emit_session_shutdown(&self, event: SessionShutdownEvent) {
        if self.records("session_shutdown") {
            if let Some(sink) = &self.handles.phase_sink {
                sink.lock().unwrap().push("session_shutdown".to_string());
            }
            self.handles
                .events
                .lock()
                .unwrap()
                .push(Lifecycle::Shutdown(event));
        }
    }

    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        match event {
            ExtensionDispatchEvent::SessionStart(start) if self.records("session_start") => {
                self.handles
                    .events
                    .lock()
                    .unwrap()
                    .push(Lifecycle::Start(start.clone()));
                ExtensionEmitOutcome::None
            }
            ExtensionDispatchEvent::SessionBeforeSwitch(before)
                if self.records("session_before_switch") =>
            {
                self.handles
                    .events
                    .lock()
                    .unwrap()
                    .push(Lifecycle::BeforeSwitch(before.clone()));
                let cancel = *self.handles.switch_cancel.lock().unwrap() == Some(before.reason);
                ExtensionEmitOutcome::BeforeSwitch(SessionBeforeSwitchResult {
                    cancel: cancel.then_some(true),
                })
            }
            ExtensionDispatchEvent::SessionBeforeFork(before)
                if self.records("session_before_fork") =>
            {
                self.handles
                    .events
                    .lock()
                    .unwrap()
                    .push(Lifecycle::BeforeFork(before.clone()));
                let mut fork_cancel = self.handles.fork_cancel.lock().unwrap();
                let cancel = *fork_cancel;
                if cancel {
                    *fork_cancel = false;
                }
                ExtensionEmitOutcome::BeforeFork(SessionBeforeForkResult {
                    cancel: cancel.then_some(true),
                    ..SessionBeforeForkResult::default()
                })
            }
            _ => self.inner.emit(event),
        }
    }

    fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage> {
        if let Some(handler) = &self.handles.message_end {
            return handler(&event.message);
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
        self.inner
            .emit_input(text, images, source, streaming_behavior)
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
        if self.records(event_type) {
            return true;
        }
        if event_type == "message_end" && self.handles.message_end.is_some() {
            return true;
        }
        self.inner.has_handlers(event_type)
    }

    fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
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

// ---------------------------------------------------------------------------
// Offline session factory
// ---------------------------------------------------------------------------

/// A [`StreamResult`] whose only event carries the final message (pi's
/// `MockAssistantStream`).
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

/// A faux stream fn over a shared, scripted response list.
fn faux_stream_fn(
    responses: Arc<Mutex<(Vec<FauxResponse>, usize)>>,
    call_count: Arc<AtomicUsize>,
) -> StreamFn {
    Arc::new(
        move |_model: &Model,
              context: &Context,
              _options: Option<&StreamOptions>,
              _signal: Option<&AbortSignal>| {
            call_count.fetch_add(1, Ordering::SeqCst);
            let message = {
                let mut guard = responses.lock().unwrap();
                let (list, index) = &mut *guard;
                match list.get(*index) {
                    Some(FauxResponse::Message(message)) => {
                        let message = (**message).clone();
                        *index += 1;
                        message
                    }
                    Some(FauxResponse::Fn(builder)) => {
                        let message = builder(context);
                        *index += 1;
                        message
                    }
                    None => assistant_text("(exhausted)"),
                }
            };
            mock_stream(message)
        },
    )
}

/// A model runtime that knows the `faux` provider (mirrors the harness helper).
fn faux_model_runtime(agent_dir: &str) -> ModelRuntime {
    std::fs::create_dir_all(agent_dir).expect("create agent dir");
    let models_path = std::path::Path::new(agent_dir).join("models.json");
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
        models_path: ModelsPath::Path(models_path.to_string_lossy().into_owned()),
        allow_model_network: Some(false),
        ..Default::default()
    });
    runtime.set_runtime_api_key("faux", "faux-key");
    runtime
}

/// Build a faux-model session for the prepared manager, restoring any persisted
/// messages and seeding a fresh session's model/thinking entries the way the sdk
/// factory does (so `getLeafId` is non-null on a brand-new session).
fn build_runtime_session(
    options: AgentSessionRuntimeFactoryOptions,
    responses: Arc<Mutex<(Vec<FauxResponse>, usize)>>,
    call_count: Arc<AtomicUsize>,
    handles: &RunnerHandles,
) -> AgentSessionRuntimeResult {
    let cwd = options.cwd.clone();
    let agent_dir = options.agent_dir.clone();
    let model_runtime = faux_model_runtime(&agent_dir);
    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        ..Default::default()
    });
    let settings_manager = SettingsManager::create(&cwd, &agent_dir);
    let stream_fn = faux_stream_fn(responses, call_count);

    let restored_messages = options.session_manager.build_session_context().messages;
    let has_messages = !restored_messages.is_empty();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a test assistant.".to_string()),
            model: Some(faux_model()),
            thinking_level: None,
            tools: Some(Vec::new()),
            messages: has_messages.then(|| restored_messages.clone()),
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: options.session_manager,
        settings_manager,
        cwd: cwd.clone(),
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime,
        initial_active_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner: Some(handles.build()),
        session_start_event: options.session_start_event,
        summarization_models: None,
    });

    // Seed a brand-new session's model/thinking entries (pi's sdk factory tail,
    // `sdk.ts:471-484`) so a fresh session has a leaf entry.
    if !has_messages {
        let mut manager = session.session_manager();
        manager.append_model_change("faux", "faux-1");
        manager.append_thinking_level_change("medium");
    }

    AgentSessionRuntimeResult {
        session,
        cwd,
        model_fallback_message: None,
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Options for [`build_harness`].
#[derive(Default)]
struct HarnessConfig {
    /// Lifecycle event names for which a handler is registered.
    recorded: &'static [&'static str],
    /// Whether the initial session is in-memory (pi's `SessionManager.inMemory`).
    in_memory: bool,
    /// A phase sink for the `before_session_invalidate` ordering case.
    phase_sink: Option<Arc<Mutex<Vec<String>>>>,
    /// A `message_end` replacement handler.
    message_end: Option<MessageEndHandler>,
}

/// A built runtime plus its shared recording state.
struct Harness {
    runtime: AgentSessionRuntime,
    events: Arc<Mutex<Vec<Lifecycle>>>,
    switch_cancel: Arc<Mutex<Option<SessionBeforeSwitchReason>>>,
    fork_cancel: Arc<Mutex<bool>>,
    _temp: tempfile::TempDir,
}

impl Harness {
    fn events(&self) -> Vec<Lifecycle> {
        self.events.lock().unwrap().clone()
    }

    fn clear_events(&self) {
        self.events.lock().unwrap().clear();
    }
}

fn build_harness(config: HarnessConfig) -> Harness {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().to_string_lossy().into_owned();
    let agent_dir = temp.path().join(".agent").to_string_lossy().into_owned();
    let sessions_dir = temp.path().join("sessions").to_string_lossy().into_owned();

    let responses = Arc::new(Mutex::new((seed_responses(6), 0usize)));
    let call_count = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let switch_cancel = Arc::new(Mutex::new(None));
    let fork_cancel = Arc::new(Mutex::new(false));
    let recorded: Arc<HashSet<String>> =
        Arc::new(config.recorded.iter().map(|s| s.to_string()).collect());

    let handles = RunnerHandles {
        events: Arc::clone(&events),
        recorded,
        switch_cancel: Arc::clone(&switch_cancel),
        fork_cancel: Arc::clone(&fork_cancel),
        phase_sink: config.phase_sink,
        message_end: config.message_end,
    };

    let factory_responses = Arc::clone(&responses);
    let factory_calls = Arc::clone(&call_count);
    let factory_handles = handles.clone();
    let create_runtime: CreateAgentSessionRuntimeFactory = Box::new(move |options| {
        build_runtime_session(
            options,
            Arc::clone(&factory_responses),
            Arc::clone(&factory_calls),
            &factory_handles,
        )
    });

    let initial_manager = if config.in_memory {
        SessionManager::in_memory(&cwd)
    } else {
        SessionManager::create(&cwd, Some(&sessions_dir), None)
    };

    let runtime = create_agent_session_runtime(
        create_runtime,
        AgentSessionRuntimeFactoryOptions {
            cwd,
            agent_dir,
            session_manager: initial_manager,
            session_start_event: None,
        },
    )
    .expect("initial runtime");
    // pi's `createRuntimeForTest` calls `runtime.session.bindExtensions({})` to emit
    // the initial `session_start`.
    runtime.session().bind_extensions();

    Harness {
        runtime,
        events,
        switch_cancel,
        fork_cancel,
        _temp: temp,
    }
}

fn seed_responses(count: usize) -> Vec<FauxResponse> {
    (0..count)
        .map(|i| FauxResponse::Message(Box::new(assistant_text(&format!("resp-{i}")))))
        .collect()
}

/// The text of a user message value (joins text parts; a bare string as-is).
fn user_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// `(role, user-text-or-None)` pairs for every message, for branch-duplication
/// equality checks.
fn message_shapes(session: &AgentSession) -> Vec<(String, Option<String>)> {
    session
        .messages()
        .iter()
        .map(|m| {
            let role = m
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let text = (role == "user").then(|| user_text(m));
            (role, text)
        })
        .collect()
}

/// Build a second, persisted session rooted in its own cwd, seeded with a `user`
/// "other" message (and, when `with_reply`, an `assistant` "reply" so the
/// deferred-flush write puts the file — and its cwd header — on disk). Returns the
/// temp dir (the caller must keep it alive), its cwd, and the session file path.
fn persisted_other_session(with_reply: bool) -> (tempfile::TempDir, String, Option<String>) {
    let other = tempfile::tempdir().expect("tempdir");
    let other_cwd = other.path().to_string_lossy().into_owned();
    let mut other_manager = SessionManager::create(&other_cwd, Some(&other_cwd), None);
    other_manager.append_message(json!({
        "role": "user",
        "content": [{ "type": "text", "text": "other" }],
        "timestamp": 0,
    }));
    if with_reply {
        other_manager.append_message(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "reply" }],
            "timestamp": 0,
        }));
    }
    let other_session_file = other_manager.get_session_file().map(String::from);
    (other, other_cwd, other_session_file)
}

/// Prompt the harness's session with "hello" then "again" (the two-turn setup the
/// fork-at-current-position cases share before capturing the branch shape).
fn prompt_hello_again(harness: &Harness) {
    harness
        .runtime
        .session()
        .prompt("hello", None, None)
        .expect("prompt hello");
    harness
        .runtime
        .session()
        .prompt("again", None, None)
        .expect("prompt again");
}

/// The current session's leaf entry id, asserted present.
fn require_leaf_id(harness: &Harness) -> String {
    let leaf_id = harness
        .runtime
        .session()
        .session_manager()
        .get_leaf_id()
        .map(String::from);
    assert!(leaf_id.is_some());
    leaf_id.unwrap()
}

/// Fork at `leaf_id` (position `At`), asserting the fork is kept (not cancelled)
/// and selects no text — the shared tail of the fork-at-current-position cases.
fn fork_at_leaf(harness: &mut Harness, leaf_id: &str) {
    let result = harness
        .runtime
        .fork(
            leaf_id,
            ForkOptions {
                position: Some(ForkPosition::At),
            },
        )
        .expect("fork");
    assert!(!result.cancelled);
    assert_eq!(result.selected_text, None);
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn persists_message_end_assistant_replacements_to_the_session_manager() {
    let handler: MessageEndHandler = Arc::new(|message: &AgentMessage| {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let mut replacement = message.clone();
        if let Some(Value::Object(cost)) = replacement.pointer_mut("/usage/cost") {
            cost.insert("total".to_string(), json!(0.123));
        }
        Some(replacement)
    });
    let harness = build_harness(HarnessConfig {
        message_end: Some(handler),
        ..HarnessConfig::default()
    });

    harness
        .runtime
        .session()
        .prompt("hello", None, None)
        .expect("prompt");

    let session = harness.runtime.session();
    let assistant = session
        .messages()
        .into_iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("assistant message in state");
    assert_eq!(assistant.pointer("/usage/cost/total"), Some(&json!(0.123)));

    let persisted = session
        .session_manager()
        .get_entries()
        .into_iter()
        .filter_map(|entry| match entry {
            crate::core::session_manager::SessionEntry::Message(message) => Some(message.message),
            _ => None,
        })
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("persisted assistant message");
    assert_eq!(persisted.pointer("/usage/cost/total"), Some(&json!(0.123)));
}

#[test]
fn emits_session_before_switch_and_session_start_for_new_and_resume_flows() {
    let harness = build_harness(HarnessConfig {
        recorded: &["session_before_switch", "session_shutdown", "session_start"],
        ..HarnessConfig::default()
    });

    assert_eq!(
        harness.events(),
        vec![start(SessionStartReason::Startup, None)]
    );
    harness.clear_events();

    let mut harness = harness;
    harness
        .runtime
        .session()
        .prompt("hello", None, None)
        .expect("prompt");
    let original_session_file = harness.runtime.session().session_file();

    let result = harness.runtime.new_session(NewSessionOptions::default());
    assert!(!result.cancelled);
    harness.runtime.session().bind_extensions();
    assert!(harness.runtime.session().messages().is_empty());
    let second_session_file = harness.runtime.session().session_file();
    assert_eq!(
        harness.events(),
        vec![
            before_switch(SessionBeforeSwitchReason::New, None),
            shutdown(SessionShutdownReason::New, second_session_file.as_deref()),
            start(SessionStartReason::New, original_session_file.as_deref()),
        ]
    );
    harness.clear_events();

    let switch = harness
        .runtime
        .switch_session(original_session_file.as_deref().unwrap())
        .expect("switch");
    assert!(!switch.cancelled);
    harness.runtime.session().bind_extensions();
    assert_eq!(
        harness.events(),
        vec![
            before_switch(
                SessionBeforeSwitchReason::Resume,
                original_session_file.as_deref()
            ),
            shutdown(
                SessionShutdownReason::Resume,
                original_session_file.as_deref()
            ),
            start(SessionStartReason::Resume, second_session_file.as_deref()),
        ]
    );
}

#[test]
fn honors_session_before_switch_cancellation_for_new_and_resume() {
    let mut harness = build_harness(HarnessConfig {
        recorded: &["session_before_switch", "session_start"],
        ..HarnessConfig::default()
    });

    harness
        .runtime
        .session()
        .prompt("hello", None, None)
        .expect("prompt");
    let original_session_file = harness.runtime.session().session_file();

    *harness.switch_cancel.lock().unwrap() = Some(SessionBeforeSwitchReason::New);
    let new_result = harness.runtime.new_session(NewSessionOptions::default());
    assert!(new_result.cancelled);
    assert_eq!(
        harness.runtime.session().session_file(),
        original_session_file
    );

    // Build a second, persisted session to resume into.
    let (_other, _other_cwd, other_session_file) = persisted_other_session(false);

    *harness.switch_cancel.lock().unwrap() = Some(SessionBeforeSwitchReason::Resume);
    let resume_result = harness
        .runtime
        .switch_session(other_session_file.as_deref().unwrap())
        .expect("switch");
    assert!(resume_result.cancelled);
    assert_eq!(
        harness.runtime.session().session_file(),
        original_session_file
    );
}

#[test]
fn emits_session_before_fork_and_session_start_and_honors_cancellation() {
    let mut harness = build_harness(HarnessConfig {
        recorded: &["session_before_fork", "session_shutdown", "session_start"],
        ..HarnessConfig::default()
    });
    harness.clear_events();

    harness
        .runtime
        .session()
        .prompt("hello", None, None)
        .expect("prompt");
    let user_message = harness.runtime.session().get_user_messages_for_forking()[0].clone();
    let previous_session_file = harness.runtime.session().session_file();

    let success = harness
        .runtime
        .fork(&user_message.entry_id, ForkOptions::default())
        .expect("fork");
    assert!(!success.cancelled);
    assert_eq!(success.selected_text.as_deref(), Some("hello"));
    harness.runtime.session().bind_extensions();
    let forked_file = harness.runtime.session().session_file();
    assert_eq!(
        harness.events(),
        vec![
            before_fork(&user_message.entry_id, ForkPosition::Before),
            shutdown(SessionShutdownReason::Fork, forked_file.as_deref()),
            start(SessionStartReason::Fork, previous_session_file.as_deref()),
        ]
    );
    // The forked session file is named `<timestamp>_<sessionId>`.
    let file_stem = std::path::Path::new(forked_file.as_deref().unwrap())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap()
        .to_string();
    assert!(file_stem.ends_with(&format!("_{}", harness.runtime.session().session_id())));

    harness.clear_events();
    *harness.fork_cancel.lock().unwrap() = true;
    let cancelled = harness
        .runtime
        .fork(&user_message.entry_id, ForkOptions::default())
        .expect("fork");
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.selected_text, None);
    assert_eq!(
        harness.events(),
        vec![before_fork(&user_message.entry_id, ForkPosition::Before)]
    );

    harness.clear_events();
    *harness.fork_cancel.lock().unwrap() = true;
    let cancelled_at = harness
        .runtime
        .fork(
            "missing-entry",
            ForkOptions {
                position: Some(ForkPosition::At),
            },
        )
        .expect("fork");
    assert!(cancelled_at.cancelled);
    assert_eq!(
        harness.events(),
        vec![before_fork("missing-entry", ForkPosition::At)]
    );
}

#[test]
fn reports_why_an_unflushed_session_cannot_be_forked() {
    let mut harness = build_harness(HarnessConfig::default());
    let session_file = harness.runtime.session().session_file();
    let leaf_id = harness
        .runtime
        .session()
        .session_manager()
        .get_leaf_id()
        .map(String::from);
    assert!(session_file.is_some());
    assert!(!std::path::Path::new(session_file.as_deref().unwrap()).exists());
    assert!(leaf_id.is_some());

    let error = harness
        .runtime
        .fork(
            leaf_id.as_deref().unwrap(),
            ForkOptions {
                position: Some(ForkPosition::At),
            },
        )
        .expect_err("unflushed fork should error");
    assert!(matches!(error, AgentSessionRuntimeError::SessionNotSaved));
    assert_eq!(
        error.to_string(),
        "This session has not been saved yet. Wait for the first assistant response before cloning or forking it."
    );
}

#[test]
fn duplicates_the_current_active_branch_when_forking_at_the_current_position() {
    let mut harness = build_harness(HarnessConfig::default());
    prompt_hello_again(&harness);

    let before = message_shapes(harness.runtime.session());
    let previous_session_file = harness.runtime.session().session_file();
    let leaf_id = require_leaf_id(&harness);

    fork_at_leaf(&mut harness, &leaf_id);
    assert_ne!(
        harness.runtime.session().session_file(),
        previous_session_file
    );
    assert_eq!(message_shapes(harness.runtime.session()), before);
}

#[test]
fn duplicates_the_current_active_branch_in_memory_when_forking_at_the_current_position() {
    let mut harness = build_harness(HarnessConfig {
        in_memory: true,
        ..HarnessConfig::default()
    });
    prompt_hello_again(&harness);

    let before = message_shapes(harness.runtime.session());
    let leaf_id = require_leaf_id(&harness);
    assert!(harness.runtime.session().session_file().is_none());

    fork_at_leaf(&mut harness, &leaf_id);
    assert!(harness.runtime.session().session_file().is_none());
    assert_eq!(message_shapes(harness.runtime.session()), before);
}

#[test]
fn throws_when_forking_with_an_invalid_entry_id() {
    let mut harness = build_harness(HarnessConfig::default());
    let error = harness
        .runtime
        .fork("missing-entry", ForkOptions::default())
        .expect_err("invalid entry should error");
    assert!(matches!(error, AgentSessionRuntimeError::InvalidForkEntry));
    assert_eq!(error.to_string(), "Invalid entry ID for forking");
}

#[test]
fn updates_the_runtime_session_cwd_on_cross_cwd_session_replacement() {
    let mut harness = build_harness(HarnessConfig::default());

    // A persisted session rooted in a different cwd. A user + assistant message
    // forces the deferred-flush write so the file (and its cwd header) exist on
    // disk for `switch_session` to open.
    let (_other, other_cwd, other_session_file) = persisted_other_session(true);

    harness
        .runtime
        .switch_session(other_session_file.as_deref().unwrap())
        .expect("switch");

    let canonical = |p: &str| std::fs::canonicalize(p).unwrap();
    assert_eq!(
        canonical(harness.runtime.session().session_manager().get_cwd()),
        canonical(&other_cwd)
    );
    assert_eq!(canonical(harness.runtime.cwd()), canonical(&other_cwd));
}

#[test]
fn runs_before_session_invalidate_after_shutdown_and_before_rebind() {
    let phases = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut harness = build_harness(HarnessConfig {
        recorded: &["session_shutdown"],
        phase_sink: Some(Arc::clone(&phases)),
        ..HarnessConfig::default()
    });

    let invalidate_phases = Arc::clone(&phases);
    harness
        .runtime
        .set_before_session_invalidate(Some(Box::new(move || {
            invalidate_phases
                .lock()
                .unwrap()
                .push("beforeSessionInvalidate".to_string());
        })));
    let rebind_phases = Arc::clone(&phases);
    harness
        .runtime
        .set_rebind_session(Some(Box::new(move |_session: &AgentSession| {
            rebind_phases
                .lock()
                .unwrap()
                .push("rebindSession".to_string());
        })));

    harness.runtime.new_session(NewSessionOptions::default());

    assert_eq!(
        *phases.lock().unwrap(),
        vec![
            "session_shutdown".to_string(),
            "beforeSessionInvalidate".to_string(),
            "rebindSession".to_string()
        ]
    );

    harness.runtime.set_before_session_invalidate(None);
    harness.runtime.set_rebind_session(None);
}

#[test]
#[ignore = "unit5: exercises the sdk create_agent_session model/thinking restore-on-resume path; the offline lifecycle factory builds sessions directly (to inject the recording runner) with a fixed faux-1 model and does not replay model_change/thinking_level_change restoration or register faux-2. The restore logic itself is covered by the sdk.rs suite."]
fn restores_model_and_thinking_state_from_the_destination_session() {}

#[test]
fn dispose_shuts_down_the_current_session() {
    let mut harness = build_harness(HarnessConfig {
        recorded: &["session_shutdown"],
        ..HarnessConfig::default()
    });
    harness.clear_events();
    harness.runtime.dispose();
    assert_eq!(
        harness.events(),
        vec![shutdown(SessionShutdownReason::Quit, None)]
    );
}
