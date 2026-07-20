//! The stateful `Agent`, ported from `packages/agent/src/agent.ts`.
//!
//! pi's [`Agent`](https://github.com/earendil-works/pi) is a stateful wrapper
//! around the low-level [agent loop](crate::agent_loop). It owns the current
//! transcript ([`AgentState`]), emits lifecycle [`AgentEvent`]s to subscribers,
//! executes tools through the loop, and exposes queueing APIs for **steering**
//! (inject after the current assistant turn) and **follow-up** (run only after
//! the agent would otherwise stop) messages. Callers drive it with the
//! overloaded `prompt(...)` forms or [`Agent::continue_`].
//!
//! # Streaming adaptation (eager / synchronous)
//!
//! Per the crate convention (see [`crate::types`] and [`crate::agent_loop`]),
//! pidgin is synchronous and eager: there is no `tokio`, no `Promise`, and no
//! async-iterable event stream. pi's async lifecycle collapses as follows:
//!
//! - A `prompt()` / `continue_()` call runs the whole loop to completion
//!   **synchronously** and returns when the run (and every subscriber it fired)
//!   has settled. pi's `activeRun` promise, `waitForIdle()`, and the "already
//!   processing" guards are preserved for their observable effects: the guards
//!   still fire when a subscriber (which runs *inside* the synchronous run)
//!   re-enters `prompt()`/`continue_()`, and [`Agent::wait_for_idle`] is a no-op
//!   because a run is never in-flight across a suspension point.
//! - [`Agent::signal`] exposes the current run's cooperative
//!   [`AbortSignal`](pidgin_ai::seams::AbortSignal); [`Agent::abort`] trips it.
//!   Because the signal is shared with the loop and with subscribers, aborting
//!   from a subscriber is observed by the loop's cooperative abort checks.
//! - Every hook pi types as returning a `Promise<T>` is a synchronous closure
//!   returning `T`, matching [`crate::types`].
//! - Where pi throws (`prompt` while busy, `continue` from an assistant tail,
//!   …) the port returns [`AgentError`].
//!
//! Interior mutability: pi shares `this` freely across the emit sink, hooks, and
//! subscribers. The port stores all mutable runtime state in an
//! [`Arc`]`<`[`Mutex`]`<…>>`-backed [`AgentShared`], so the sink and hook
//! closures (which are `Arc<dyn Fn + Send + Sync>`) can reach it, and so a
//! subscriber can re-enter the agent. No lock is held across a subscriber or
//! hook callback, so re-entrant calls (`abort()`, a rejected `prompt()`) cannot
//! deadlock.
//!
//! Source of truth: `vendor/pi/packages/agent/src/agent.ts`.

// straitjacket-allow-file:duplication — this module is a faithful transcription
// of pi's `agent.ts`; the `#[cfg(test)]` scenarios mirror pi's ~19 cases, whose
// near-identical stream/tool scaffolding the clone detector reads as
// duplication.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::seams::clock::{Clock, SystemClock};
use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Message, Model,
    ModelCost, StopReason, StreamOptions, Usage, UsageCost,
};

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue, AgentEventSink};
use crate::types::{
    AfterToolCall, AgentContext, AgentEvent, AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage,
    AgentState, AgentTool, BeforeToolCall, ConvertToLlm, GetApiKey, GetFollowUpMessages,
    GetSteeringMessages, PrepareNextTurn, PrepareNextTurnContext, StreamFn, ThinkingLevel,
    ToolExecutionMode, TransformContext,
};

// `export type { QueueMode } from "./types.ts";` (`agent.ts:30`).
pub use crate::types::QueueMode;

// ---------------------------------------------------------------------------
// Defaults (`agent.ts:32-58`)
// ---------------------------------------------------------------------------

/// pi's `defaultConvertToLlm` (`agent.ts:32-36`): keep only `user` / `assistant`
/// / `toolResult` messages and reinterpret each as an LLM [`Message`].
fn default_convert_to_llm() -> ConvertToLlm {
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

/// pi's `EMPTY_USAGE` (`agent.ts:38-45`) as a JSON value, for the synthetic
/// failure message built by [`Agent::handle_run_failure`].
fn empty_usage_value() -> Value {
    json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
    })
}

/// pi's `DEFAULT_MODEL` (`agent.ts:47-58`): the `"unknown"` placeholder used
/// until a model is configured.
fn default_model() -> Model {
    Model {
        id: "unknown".to_string(),
        name: "unknown".to_string(),
        api: "unknown".to_string(),
        provider: "unknown".to_string(),
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

/// The default [`StreamFn`] used when [`AgentOptions::stream_fn`] is unset. pi
/// defaults to `streamSimple` (which routes to registered providers); the port
/// has no ambient registry, so the default encodes a provider-unavailable
/// failure the eager way — a terminal `error` event — exactly as a real
/// provider would when it cannot serve a request. Every test supplies its own
/// `stream_fn`, so this default is never exercised there.
fn default_stream_fn() -> StreamFn {
    Arc::new(|_model, _context, _options, _signal| {
        let message = AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: "unknown".to_string(),
            provider: "unknown".to_string(),
            model: "unknown".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0,
                cost: UsageCost::default(),
            },
            stop_reason: StopReason::Error,
            error_message: Some("No stream function configured".to_string()),
            timestamp: 0,
        };
        StreamResult {
            events: vec![AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: message.clone(),
            }],
            message,
        }
    })
}

// ---------------------------------------------------------------------------
// Errors (the points where pi throws)
// ---------------------------------------------------------------------------

/// The synchronous errors pi's `Agent` throws. Each
/// [`Display`](std::fmt::Display) string is byte-identical to pi's
/// `throw new Error(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// `prompt()` called while a run is active (`agent.ts:338-341`).
    AlreadyProcessingPrompt,
    /// `continue()` called while a run is active (`agent.ts:349-351`).
    AlreadyProcessingContinue,
    /// `continue()` called with an empty transcript (`agent.ts:354-356`).
    NoMessagesToContinue,
    /// `continue()` called from an assistant tail with no queued messages
    /// (`agent.ts:371`).
    ContinueFromAssistant,
    /// `runWithLifecycle` re-entry guard (`agent.ts:470-472`). Unreachable in the
    /// eager model because the `prompt`/`continue` guards run first on the same
    /// thread; preserved for faithfulness.
    AlreadyProcessing,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::AlreadyProcessingPrompt => f.write_str(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.",
            ),
            AgentError::AlreadyProcessingContinue => {
                f.write_str("Agent is already processing. Wait for completion before continuing.")
            }
            AgentError::NoMessagesToContinue => f.write_str("No messages to continue from"),
            AgentError::ContinueFromAssistant => {
                f.write_str("Cannot continue from message role: assistant")
            }
            AgentError::AlreadyProcessing => f.write_str("Agent is already processing."),
        }
    }
}

impl std::error::Error for AgentError {}

// ---------------------------------------------------------------------------
// Prompt input (pi's overloaded `prompt(...)`, `agent.ts:335-337`)
// ---------------------------------------------------------------------------

/// The input to [`Agent::prompt`], mirroring pi's overloaded `prompt(...)`
/// signatures (`agent.ts:335-337`): a text string (with optional images), a
/// single [`AgentMessage`], or a batch of messages.
pub enum PromptInput {
    /// pi's `prompt(input: string, images?: ImageContent[])`. `images` are
    /// [`ContentBlock`]s (pi's `ImageContent`); an empty vec matches the
    /// text-only overload.
    Text {
        /// The user text.
        input: String,
        /// Optional trailing image content blocks.
        images: Vec<ContentBlock>,
    },
    /// pi's `prompt(message: AgentMessage)`.
    Message(AgentMessage),
    /// pi's `prompt(messages: AgentMessage[])`.
    Messages(Vec<AgentMessage>),
}

impl From<&str> for PromptInput {
    fn from(input: &str) -> Self {
        PromptInput::Text {
            input: input.to_string(),
            images: Vec::new(),
        }
    }
}

impl From<String> for PromptInput {
    fn from(input: String) -> Self {
        PromptInput::Text {
            input,
            images: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Agent-level hook aliases (`agent.ts:107-113`, `agent.ts:191-197`)
// ---------------------------------------------------------------------------

/// pi's `prepareNextTurn` agent option (`agent.ts:107-109`): the legacy
/// signal-only callback. Receives the active run's abort signal.
pub type PrepareNextTurnSignal =
    Arc<dyn Fn(Option<&AbortSignal>) -> Option<AgentLoopTurnUpdate> + Send + Sync>;

/// pi's `prepareNextTurnWithContext` agent option (`agent.ts:110-113`): the
/// context-aware callback. Preferred over [`PrepareNextTurnSignal`] when both
/// are set.
pub type PrepareNextTurnWithContext = Arc<
    dyn Fn(&PrepareNextTurnContext, Option<&AbortSignal>) -> Option<AgentLoopTurnUpdate>
        + Send
        + Sync,
>;

/// A lifecycle-event subscriber (pi's `(event, signal) => Promise<void> | void`,
/// `agent.ts:241`). Synchronous in the eager port; receives the active run's
/// abort signal.
pub type Listener = Arc<dyn Fn(&AgentEvent, &AbortSignal) + Send + Sync>;

// ---------------------------------------------------------------------------
// Initial state & options (`agent.ts:97-121`)
// ---------------------------------------------------------------------------

/// The `initialState` accepted by [`AgentOptions`] (pi's `Partial<Omit<AgentState,
/// "pendingToolCalls" | "isStreaming" | "streamingMessage" | "errorMessage">>`,
/// `agent.ts:98`). Unset fields take pi's `createMutableAgentState` defaults.
#[derive(Clone, Default)]
pub struct InitialAgentState {
    /// System prompt (default `""`).
    pub system_prompt: Option<String>,
    /// Active model (default the `"unknown"` placeholder).
    pub model: Option<Model>,
    /// Thinking level (default `off`).
    pub thinking_level: Option<ThinkingLevel>,
    /// Available tools (default empty).
    pub tools: Option<Vec<AgentTool>>,
    /// Initial transcript (default empty).
    pub messages: Option<Vec<AgentMessage>>,
}

/// Options for constructing an [`Agent`] (pi's `AgentOptions`, `agent.ts:97-121`).
///
/// # Omitted parity fields
///
/// pi's `AgentOptions` also carries `onPayload`, `onResponse`, `transport`,
/// `thinkingBudgets`, and `maxRetryDelayMs`. These are `SimpleStreamOptions`
/// pass-throughs that the ported [`AgentLoopConfig`] / [`StreamOptions`] do not
/// yet model (Wave 0 documented them as additive future work in
/// [`crate::types`]). They are omitted here rather than accepted-and-dropped;
/// `session_id`, `reasoning` (via `thinking_level`), and `tool_execution` — the
/// stream options that *do* have a destination — are threaded through.
#[derive(Default)]
pub struct AgentOptions {
    /// Seed state for the new agent.
    pub initial_state: Option<InitialAgentState>,
    /// Override the `AgentMessage[]` → `Message[]` converter (default
    /// [`default_convert_to_llm`]).
    pub convert_to_llm: Option<ConvertToLlm>,
    /// Optional context transform applied before conversion.
    pub transform_context: Option<TransformContext>,
    /// The streaming function (default a provider-unavailable stub).
    pub stream_fn: Option<StreamFn>,
    /// Optional dynamic API-key resolver.
    pub get_api_key: Option<GetApiKey>,
    /// Optional pre-execution tool hook.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional post-execution tool hook.
    pub after_tool_call: Option<AfterToolCall>,
    /// Legacy signal-only next-turn hook.
    pub prepare_next_turn: Option<PrepareNextTurnSignal>,
    /// Context-aware next-turn hook (preferred when both are set).
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContext>,
    /// Steering queue drain mode (default `one-at-a-time`).
    pub steering_mode: Option<QueueMode>,
    /// Follow-up queue drain mode (default `one-at-a-time`).
    pub follow_up_mode: Option<QueueMode>,
    /// Session id forwarded to providers for cache-aware backends.
    pub session_id: Option<String>,
    /// Tool-execution strategy (default `parallel`).
    pub tool_execution: Option<ToolExecutionMode>,
}

// ---------------------------------------------------------------------------
// Pending-message queue (`agent.ts:123-157`)
// ---------------------------------------------------------------------------

/// A FIFO queue of pending user messages with a drain [`QueueMode`] (pi's
/// `PendingMessageQueue`, `agent.ts:123-157`).
struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    /// Drain per [`QueueMode`]: `all` empties the queue, `one-at-a-time` pops the
    /// oldest single message (`agent.ts:139-152`).
    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

// ---------------------------------------------------------------------------
// Active run (`agent.ts:159-163`)
// ---------------------------------------------------------------------------

/// The in-flight run's handle (pi's `ActiveRun`, `agent.ts:159-163`). pi bundles
/// the settlement promise, its resolver, and the `AbortController`; the eager
/// port keeps only the cooperative [`AbortSignal`] — the promise/resolver model
/// suspension the synchronous run never needs.
struct ActiveRun {
    signal: AbortSignal,
}

// ---------------------------------------------------------------------------
// Shared runtime state
// ---------------------------------------------------------------------------

/// Interior-mutable state shared between the [`Agent`] handle, the emit sink, and
/// hook/subscriber closures. See the module docs on interior mutability.
struct AgentShared {
    state: Mutex<AgentState>,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener_id: AtomicU64,
    active_run: Mutex<Option<ActiveRun>>,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    session_id: Mutex<Option<String>>,

    // Construction-time configuration (pi's public `Agent` fields). The port sets
    // these once; only `session_id` and the two queue modes are mutable at
    // runtime, matching the fields pi's tests reassign.
    convert_to_llm: ConvertToLlm,
    transform_context: Option<TransformContext>,
    stream_fn: StreamFn,
    get_api_key: Option<GetApiKey>,
    // The tool-call and context-aware next-turn hooks are interior-mutable so a
    // post-construction caller can install or replace them (pi reassigns
    // `beforeToolCall` / `afterToolCall` / `prepareNextTurnWithContext` after
    // construction — `agent-session.ts:449-490`). Each read locks and clones,
    // preserving pi's "reads the current closure per run" semantics.
    before_tool_call: Mutex<Option<BeforeToolCall>>,
    after_tool_call: Mutex<Option<AfterToolCall>>,
    prepare_next_turn: Option<PrepareNextTurnSignal>,
    prepare_next_turn_with_context: Mutex<Option<PrepareNextTurnWithContext>>,
    tool_execution: ToolExecutionMode,
    clock: Arc<dyn Clock>,
}

fn new_state(initial: Option<InitialAgentState>) -> AgentState {
    let initial = initial.unwrap_or_default();
    AgentState {
        system_prompt: initial.system_prompt.unwrap_or_default(),
        model: initial.model.unwrap_or_else(default_model),
        thinking_level: initial.thinking_level.unwrap_or(ThinkingLevel::Off),
        tools: initial.tools.unwrap_or_default(),
        messages: initial.messages.unwrap_or_default(),
        is_streaming: false,
        streaming_message: None,
        pending_tool_calls: BTreeSet::new(),
        error_message: None,
    }
}

// ---------------------------------------------------------------------------
// Agent (`agent.ts:171-575`)
// ---------------------------------------------------------------------------

/// Stateful wrapper around the low-level [agent loop](crate::agent_loop) (pi's
/// `Agent`, `agent.ts:171`).
///
/// `Agent` owns the current transcript, emits lifecycle events, executes tools,
/// and exposes queueing APIs for steering and follow-up messages. Cloning an
/// `Agent` yields another handle to the **same** shared state (cheap `Arc`
/// clone), so it can be captured into subscribers and hooks that re-enter the
/// agent.
#[derive(Clone)]
pub struct Agent {
    shared: Arc<AgentShared>,
}

impl Default for Agent {
    fn default() -> Self {
        Self::new(AgentOptions::default())
    }
}

impl Agent {
    /// Construct an agent from `options` (pi's `constructor`, `agent.ts:210-229`),
    /// using the production [`SystemClock`] for the `Date.now()` reads pi does
    /// when stamping prompt / failure timestamps.
    pub fn new(options: AgentOptions) -> Self {
        Self::with_clock(options, Arc::new(SystemClock::new()))
    }

    /// Construct an agent driven by an injected [`Clock`], so tests can pin the
    /// timestamps stamped on generated prompt and failure messages.
    pub fn with_clock(options: AgentOptions, clock: Arc<dyn Clock>) -> Self {
        let steering_mode = options.steering_mode.unwrap_or(QueueMode::OneAtATime);
        let follow_up_mode = options.follow_up_mode.unwrap_or(QueueMode::OneAtATime);
        let shared = AgentShared {
            state: Mutex::new(new_state(options.initial_state)),
            listeners: Mutex::new(Vec::new()),
            next_listener_id: AtomicU64::new(0),
            active_run: Mutex::new(None),
            steering_queue: Mutex::new(PendingMessageQueue::new(steering_mode)),
            follow_up_queue: Mutex::new(PendingMessageQueue::new(follow_up_mode)),
            session_id: Mutex::new(options.session_id),
            convert_to_llm: options
                .convert_to_llm
                .unwrap_or_else(default_convert_to_llm),
            transform_context: options.transform_context,
            stream_fn: options.stream_fn.unwrap_or_else(default_stream_fn),
            get_api_key: options.get_api_key,
            before_tool_call: Mutex::new(options.before_tool_call),
            after_tool_call: Mutex::new(options.after_tool_call),
            prepare_next_turn: options.prepare_next_turn,
            prepare_next_turn_with_context: Mutex::new(options.prepare_next_turn_with_context),
            tool_execution: options
                .tool_execution
                .unwrap_or(ToolExecutionMode::Parallel),
            clock,
        };
        Self {
            shared: Arc::new(shared),
        }
    }

    // -- Subscriptions (`agent.ts:241-244`) --------------------------------

    /// Subscribe to lifecycle events (pi's `subscribe`, `agent.ts:241-244`).
    /// Returns a [`Subscription`] whose [`unsubscribe`](Subscription::unsubscribe)
    /// removes the listener.
    ///
    /// Listeners run synchronously in subscription order and are part of the
    /// current run's settlement; each receives the active run's abort signal.
    pub fn subscribe(&self, listener: Listener) -> Subscription {
        let id = self.shared.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.shared.listeners.lock().unwrap().push((id, listener));
        Subscription {
            shared: self.shared.clone(),
            id,
        }
    }

    // -- State (`agent.ts:251-253`) ----------------------------------------

    /// A snapshot of the current [`AgentState`] (pi's `get state`,
    /// `agent.ts:251-253`).
    ///
    /// pi returns a live, accessor-backed object; the port returns a clone. Use
    /// the dedicated setters ([`set_system_prompt`](Self::set_system_prompt),
    /// [`set_messages`](Self::set_messages), …) to mutate — they reproduce pi's
    /// copy-on-assign semantics for `tools` and `messages`.
    pub fn state(&self) -> AgentState {
        self.shared.state.lock().unwrap().clone()
    }

    /// The system prompt.
    pub fn system_prompt(&self) -> String {
        self.shared.state.lock().unwrap().system_prompt.clone()
    }

    /// Set the system prompt.
    pub fn set_system_prompt(&self, system_prompt: impl Into<String>) {
        self.shared.state.lock().unwrap().system_prompt = system_prompt.into();
    }

    /// The active model.
    pub fn model(&self) -> Model {
        self.shared.state.lock().unwrap().model.clone()
    }

    /// Set the active model.
    pub fn set_model(&self, model: Model) {
        self.shared.state.lock().unwrap().model = model;
    }

    /// The requested thinking level.
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.shared.state.lock().unwrap().thinking_level
    }

    /// Set the requested thinking level.
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.shared.state.lock().unwrap().thinking_level = level;
    }

    /// The available tools (a copy).
    pub fn tools(&self) -> Vec<AgentTool> {
        self.shared.state.lock().unwrap().tools.clone()
    }

    /// Replace the available tools (copies the provided vec, per pi's
    /// `set tools`).
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        self.shared.state.lock().unwrap().tools = tools;
    }

    /// The transcript (a copy).
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.shared.state.lock().unwrap().messages.clone()
    }

    /// Replace the transcript (copies the provided vec, per pi's `set messages`).
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        self.shared.state.lock().unwrap().messages = messages;
    }

    /// Append a single message to the transcript (pi's
    /// `agent.state.messages.push(...)`).
    pub fn push_message(&self, message: AgentMessage) {
        self.shared.state.lock().unwrap().messages.push(message);
    }

    /// Whether a run is currently processing.
    pub fn is_streaming(&self) -> bool {
        self.shared.state.lock().unwrap().is_streaming
    }

    /// The partial assistant message for the current streamed response, if any.
    pub fn streaming_message(&self) -> Option<AgentMessage> {
        self.shared.state.lock().unwrap().streaming_message.clone()
    }

    /// The tool-call ids currently executing.
    pub fn pending_tool_calls(&self) -> BTreeSet<String> {
        self.shared.state.lock().unwrap().pending_tool_calls.clone()
    }

    /// The error message from the most recent failed / aborted assistant turn.
    pub fn error_message(&self) -> Option<String> {
        self.shared.state.lock().unwrap().error_message.clone()
    }

    // -- Queue modes (`agent.ts:256-271`) ----------------------------------

    /// The steering queue drain mode (pi's `get steeringMode`).
    pub fn steering_mode(&self) -> QueueMode {
        self.shared.steering_queue.lock().unwrap().mode
    }

    /// Set the steering queue drain mode (pi's `set steeringMode`).
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.shared.steering_queue.lock().unwrap().mode = mode;
    }

    /// The follow-up queue drain mode (pi's `get followUpMode`).
    pub fn follow_up_mode(&self) -> QueueMode {
        self.shared.follow_up_queue.lock().unwrap().mode
    }

    /// Set the follow-up queue drain mode (pi's `set followUpMode`).
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.shared.follow_up_queue.lock().unwrap().mode = mode;
    }

    // -- Post-construction hook installation --------------------------------
    //
    // pi reassigns the agent's mutable `beforeToolCall` / `afterToolCall` /
    // `prepareNextTurnWithContext` fields after construction (its
    // `AgentSession._installAgentToolHooks`, `agent-session.ts:449-490`). These
    // setters give a caller that holds a pre-built [`Agent`] the same ability to
    // install or replace those hooks; each run re-reads the current closure.

    /// Install or replace the pre-execution tool hook (pi's
    /// `agent.beforeToolCall = ...`).
    pub fn set_before_tool_call(&self, hook: Option<BeforeToolCall>) {
        *self.shared.before_tool_call.lock().unwrap() = hook;
    }

    /// Install or replace the post-execution tool hook (pi's
    /// `agent.afterToolCall = ...`).
    pub fn set_after_tool_call(&self, hook: Option<AfterToolCall>) {
        *self.shared.after_tool_call.lock().unwrap() = hook;
    }

    /// Install or replace the context-aware next-turn hook (pi's
    /// `agent.prepareNextTurnWithContext = ...`).
    pub fn set_prepare_next_turn_with_context(&self, hook: Option<PrepareNextTurnWithContext>) {
        *self.shared.prepare_next_turn_with_context.lock().unwrap() = hook;
    }

    // -- Queues (`agent.ts:274-302`) ---------------------------------------

    /// Queue a message to inject after the current assistant turn (pi's `steer`).
    pub fn steer(&self, message: AgentMessage) {
        self.shared.steering_queue.lock().unwrap().enqueue(message);
    }

    /// Queue a message to run only after the agent would otherwise stop (pi's
    /// `followUp`).
    pub fn follow_up(&self, message: AgentMessage) {
        self.shared.follow_up_queue.lock().unwrap().enqueue(message);
    }

    /// Remove all queued steering messages (pi's `clearSteeringQueue`).
    pub fn clear_steering_queue(&self) {
        self.shared.steering_queue.lock().unwrap().clear();
    }

    /// Remove all queued follow-up messages (pi's `clearFollowUpQueue`).
    pub fn clear_follow_up_queue(&self) {
        self.shared.follow_up_queue.lock().unwrap().clear();
    }

    /// Remove all queued steering and follow-up messages (pi's `clearAllQueues`).
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// True when either queue still holds pending messages (pi's
    /// `hasQueuedMessages`).
    pub fn has_queued_messages(&self) -> bool {
        self.shared.steering_queue.lock().unwrap().has_items()
            || self.shared.follow_up_queue.lock().unwrap().has_items()
    }

    // -- Run control (`agent.ts:305-332`) ----------------------------------

    /// The active run's abort signal, if a run is active (pi's `get signal`).
    /// Returns a clone; because [`AbortSignal`] is `Arc`-backed, observing abort
    /// through it works.
    pub fn signal(&self) -> Option<AbortSignal> {
        self.shared
            .active_run
            .lock()
            .unwrap()
            .as_ref()
            .map(|run| run.signal.clone())
    }

    /// Abort the current run, if one is active (pi's `abort`).
    pub fn abort(&self) {
        if let Some(run) = self.shared.active_run.lock().unwrap().as_ref() {
            run.signal.abort();
        }
    }

    /// Resolve when the current run and its listeners have finished (pi's
    /// `waitForIdle`).
    ///
    /// In the eager model a `prompt()` / `continue_()` call already ran the loop
    /// and all its listeners to completion before returning, so a run is never
    /// in-flight across a suspension point and this is a no-op.
    pub fn wait_for_idle(&self) {}

    /// Clear transcript, runtime state, and queued messages (pi's `reset`,
    /// `agent.ts:324-332`).
    pub fn reset(&self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.messages = Vec::new();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
            state.error_message = None;
        }
        self.clear_follow_up_queue();
        self.clear_steering_queue();
    }

    // -- Driving (`agent.ts:335-375`) --------------------------------------

    /// Start a new prompt (pi's overloaded `prompt`, `agent.ts:335-345`). Errors
    /// with [`AgentError::AlreadyProcessingPrompt`] when a run is active.
    pub fn prompt(&self, input: PromptInput) -> Result<(), AgentError> {
        if self.shared.active_run.lock().unwrap().is_some() {
            return Err(AgentError::AlreadyProcessingPrompt);
        }
        let messages = self.normalize_prompt_input(input);
        self.run_prompt_messages(messages, false)
    }

    /// Convenience for pi's `prompt(input: string, images?: ImageContent[])`.
    pub fn prompt_text(
        &self,
        input: impl Into<String>,
        images: Vec<ContentBlock>,
    ) -> Result<(), AgentError> {
        self.prompt(PromptInput::Text {
            input: input.into(),
            images,
        })
    }

    /// Convenience for pi's `prompt(message: AgentMessage)`.
    pub fn prompt_message(&self, message: AgentMessage) -> Result<(), AgentError> {
        self.prompt(PromptInput::Message(message))
    }

    /// Convenience for pi's `prompt(messages: AgentMessage[])`.
    pub fn prompt_messages(&self, messages: Vec<AgentMessage>) -> Result<(), AgentError> {
        self.prompt(PromptInput::Messages(messages))
    }

    /// Continue from the current transcript (pi's `continue`, `agent.ts:348-375`).
    /// Named `continue_` because `continue` is a Rust keyword.
    ///
    /// The last message must be a `user` or `toolResult` message — unless it is
    /// an `assistant` message and a steering or follow-up message is queued, in
    /// which case that queued batch drives a new run (steering first, with the
    /// loop's initial steering poll skipped so it is not drained twice).
    pub fn continue_(&self) -> Result<(), AgentError> {
        if self.shared.active_run.lock().unwrap().is_some() {
            return Err(AgentError::AlreadyProcessingContinue);
        }

        let last_message = self.shared.state.lock().unwrap().messages.last().cloned();
        let Some(last_message) = last_message else {
            return Err(AgentError::NoMessagesToContinue);
        };

        if last_message.get("role").and_then(Value::as_str) == Some("assistant") {
            let queued_steering = self.shared.steering_queue.lock().unwrap().drain();
            if !queued_steering.is_empty() {
                return self.run_prompt_messages(queued_steering, true);
            }

            let queued_follow_ups = self.shared.follow_up_queue.lock().unwrap().drain();
            if !queued_follow_ups.is_empty() {
                return self.run_prompt_messages(queued_follow_ups, false);
            }

            return Err(AgentError::ContinueFromAssistant);
        }

        self.run_continuation()
    }

    // -- Internals (`agent.ts:377-518`) ------------------------------------

    /// pi's `normalizePromptInput` (`agent.ts:377-394`).
    fn normalize_prompt_input(&self, input: PromptInput) -> Vec<AgentMessage> {
        match input {
            PromptInput::Messages(messages) => messages,
            PromptInput::Message(message) => vec![message],
            PromptInput::Text { input, images } => {
                let mut content: Vec<Value> = vec![json!({ "type": "text", "text": input })];
                for image in images {
                    content.push(serde_json::to_value(image).unwrap_or(Value::Null));
                }
                vec![json!({
                    "role": "user",
                    "content": content,
                    "timestamp": self.shared.clock.now_ms(),
                })]
            }
        }
    }

    /// pi's `runPromptMessages` (`agent.ts:396-410`).
    fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), AgentError> {
        self.run_with_lifecycle(|signal| {
            let context = self.create_context_snapshot();
            let config = self.create_loop_config(skip_initial_steering_poll, signal);
            let sink = self.make_sink();
            let stream_fn = self.shared.stream_fn.clone();
            run_agent_loop(messages, context, config, &sink, Some(signal), &stream_fn);
            Ok(())
        })
    }

    /// pi's `runContinuation` (`agent.ts:412-422`).
    ///
    /// pi's `runAgentLoopContinue` throws for an empty / assistant-tailed context
    /// and the throw is caught by `runWithLifecycle`. Here the port routes the
    /// [`AgentLoopError`](crate::agent_loop::AgentLoopError) to the same failure
    /// path — though `continue_` has already pre-guarded both cases, so the `Err`
    /// arm is structurally preserved but unreachable.
    fn run_continuation(&self) -> Result<(), AgentError> {
        self.run_with_lifecycle(|signal| {
            let context = self.create_context_snapshot();
            let config = self.create_loop_config(false, signal);
            let sink = self.make_sink();
            let stream_fn = self.shared.stream_fn.clone();
            match run_agent_loop_continue(context, config, &sink, Some(signal), &stream_fn) {
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    /// pi's `createContextSnapshot` (`agent.ts:424-430`).
    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.shared.state.lock().unwrap();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }

    /// pi's `createLoopConfig` (`agent.ts:432-467`).
    fn create_loop_config(
        &self,
        skip_initial_steering_poll: bool,
        signal: &AbortSignal,
    ) -> AgentLoopConfig {
        let (model, thinking_level) = {
            let state = self.shared.state.lock().unwrap();
            (state.model.clone(), state.thinking_level)
        };
        let session_id = self.shared.session_id.lock().unwrap().clone();

        // reasoning: thinkingLevel === "off" ? undefined : thinkingLevel.
        let reasoning = match thinking_level {
            ThinkingLevel::Off => None,
            other => Some(other),
        };

        // prepareNextTurn: prefer the context-aware hook, else the legacy hook,
        // passing the active run's signal. Only present when either is set.
        let pnt_with_context = self
            .shared
            .prepare_next_turn_with_context
            .lock()
            .unwrap()
            .clone();
        let pnt_legacy = self.shared.prepare_next_turn.clone();
        let pnt_signal = signal.clone();
        let prepare_next_turn: Option<PrepareNextTurn> =
            if pnt_with_context.is_some() || pnt_legacy.is_some() {
                Some(Arc::new(move |context: &PrepareNextTurnContext| {
                    if let Some(hook) = &pnt_with_context {
                        hook(context, Some(&pnt_signal))
                    } else if let Some(hook) = &pnt_legacy {
                        hook(Some(&pnt_signal))
                    } else {
                        None
                    }
                }))
            } else {
                None
            };

        // getSteeringMessages: the first poll returns [] when
        // skipInitialSteeringPoll is set (continue() already drained the steering
        // batch), then drains normally.
        let skip = Arc::new(AtomicBool::new(skip_initial_steering_poll));
        let steering_shared = self.shared.clone();
        let get_steering: GetSteeringMessages = Arc::new(move || {
            if skip.swap(false, Ordering::SeqCst) {
                return Vec::new();
            }
            steering_shared.steering_queue.lock().unwrap().drain()
        });

        let follow_up_shared = self.shared.clone();
        let get_follow_up: GetFollowUpMessages =
            Arc::new(move || follow_up_shared.follow_up_queue.lock().unwrap().drain());

        // `StreamOptions` is `#[non_exhaustive]`, so it cannot be built with a
        // struct literal from this crate; set the fields on a default value.
        let mut stream_options = StreamOptions::default();
        stream_options.session_id = session_id;

        AgentLoopConfig {
            stream_options,
            reasoning,
            model,
            convert_to_llm: self.shared.convert_to_llm.clone(),
            transform_context: self.shared.transform_context.clone(),
            get_api_key: self.shared.get_api_key.clone(),
            should_stop_after_turn: None,
            prepare_next_turn,
            get_steering_messages: Some(get_steering),
            get_follow_up_messages: Some(get_follow_up),
            tool_execution: Some(self.shared.tool_execution),
            before_tool_call: self.shared.before_tool_call.lock().unwrap().clone(),
            after_tool_call: self.shared.after_tool_call.lock().unwrap().clone(),
        }
    }

    /// Build the emit sink that reduces state and fires listeners for each loop
    /// event (pi passes `(event) => this.processEvents(event)` as the loop's
    /// emit, `agent.ts:405`).
    fn make_sink(&self) -> AgentEventSink {
        let agent = self.clone();
        Arc::new(move |event: AgentEvent| {
            agent.process_events(event);
        })
    }

    /// pi's `runWithLifecycle` (`agent.ts:469-492`).
    ///
    /// The executor returns `Err(message)` where pi's executor throws; that
    /// routes to [`handle_run_failure`](Self::handle_run_failure), the eager
    /// analog of pi's `try/catch`.
    fn run_with_lifecycle<F>(&self, executor: F) -> Result<(), AgentError>
    where
        F: FnOnce(&AbortSignal) -> Result<(), String>,
    {
        {
            let mut active_run = self.shared.active_run.lock().unwrap();
            if active_run.is_some() {
                return Err(AgentError::AlreadyProcessing);
            }
            *active_run = Some(ActiveRun {
                signal: AbortSignal::new(),
            });
        }

        {
            let mut state = self.shared.state.lock().unwrap();
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let signal = self
            .shared
            .active_run
            .lock()
            .unwrap()
            .as_ref()
            .expect("active run set above")
            .signal
            .clone();

        let result = executor(&signal);
        if let Err(message) = result {
            self.handle_run_failure(&message, signal.is_aborted());
        }
        self.finish_run();
        Ok(())
    }

    /// pi's `handleRunFailure` (`agent.ts:494-510`): emit a full synthetic
    /// lifecycle (`message_start` → `message_end` → `turn_end` → `agent_end`) for
    /// an empty assistant message carrying the failure.
    ///
    /// Reachable only via [`run_continuation`](Self::run_continuation)'s
    /// `AgentLoopError` arm, which `continue_` pre-guards — so this is
    /// structurally preserved but unreachable in the eager model, where a
    /// provider failure surfaces as an `error` event flowing through the loop's
    /// normal error path instead of a thrown executor.
    fn handle_run_failure(&self, error: &str, aborted: bool) {
        let model = self.shared.state.lock().unwrap().model.clone();
        let failure: AgentMessage = json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "" }],
            "api": model.api,
            "provider": model.provider,
            "model": model.id,
            "usage": empty_usage_value(),
            "stopReason": if aborted { "aborted" } else { "error" },
            "errorMessage": error,
            "timestamp": self.shared.clock.now_ms(),
        });
        self.process_events(AgentEvent::MessageStart {
            message: failure.clone(),
        });
        self.process_events(AgentEvent::MessageEnd {
            message: failure.clone(),
        });
        self.process_events(AgentEvent::TurnEnd {
            message: failure.clone(),
            tool_results: Vec::new(),
        });
        self.process_events(AgentEvent::AgentEnd {
            messages: vec![failure],
        });
    }

    /// pi's `finishRun` (`agent.ts:512-518`).
    fn finish_run(&self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
        }
        *self.shared.active_run.lock().unwrap() = None;
    }

    /// pi's `processEvents` (`agent.ts:527-574`): reduce internal state for a loop
    /// event, then run every listener with the active run's signal.
    ///
    /// No lock is held across the listener callbacks, so a subscriber may
    /// re-enter the agent (e.g. call [`abort`](Self::abort) or a rejected
    /// [`prompt`](Self::prompt)) without deadlocking.
    fn process_events(&self, event: AgentEvent) {
        {
            let mut state = self.shared.state.lock().unwrap();
            match &event {
                AgentEvent::MessageStart { message } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageUpdate { message, .. } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state.streaming_message = None;
                    state.messages.push(message.clone());
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    state.pending_tool_calls.remove(tool_call_id);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    // pi: assistant message with a non-empty errorMessage.
                    if message.get("role").and_then(Value::as_str) == Some("assistant") {
                        if let Some(error) = message
                            .get("errorMessage")
                            .and_then(Value::as_str)
                            .filter(|error| !error.is_empty())
                        {
                            state.error_message = Some(error.to_string());
                        }
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state.streaming_message = None;
                }
                _ => {}
            }
        }

        // pi throws "Agent listener invoked outside active run" when no signal is
        // available; unreachable here because processEvents only runs inside a
        // lifecycle (the emit sink and handleRunFailure), where active_run is set.
        let signal = self
            .shared
            .active_run
            .lock()
            .unwrap()
            .as_ref()
            .map(|run| run.signal.clone())
            .expect("Agent listener invoked outside active run");

        let listeners: Vec<Listener> = self
            .shared
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener(&event, &signal);
        }
    }

    /// Set the session id (pi's public `agent.sessionId = ...`).
    pub fn set_session_id(&self, session_id: Option<String>) {
        *self.shared.session_id.lock().unwrap() = session_id;
    }

    /// The session id (pi's public `agent.sessionId`).
    pub fn session_id(&self) -> Option<String> {
        self.shared.session_id.lock().unwrap().clone()
    }
}

/// The handle returned by [`Agent::subscribe`]; call
/// [`unsubscribe`](Self::unsubscribe) to remove the listener (pi returns a
/// `() => void`, `agent.ts:243`).
pub struct Subscription {
    shared: Arc<AgentShared>,
    id: u64,
}

impl Subscription {
    /// Remove the associated listener.
    pub fn unsubscribe(&self) {
        self.shared
            .listeners
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests;
