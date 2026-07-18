//! The public [`AgentHarness`], ported from
//! `packages/agent/src/harness/agent-harness.ts`.
//!
//! `AgentHarness` is the stateful driver that sits between an application and
//! the low-level [agent loop](crate::agent_loop). It owns the conversation
//! [`Session`], the active model/thinking-level/tool set, the steer/follow-up/
//! next-turn queues, and a subscriber/hook registry; it drives turns through the
//! loop, persists session save points, and integrates compaction and branch
//! (tree) summarization.
//!
//! # Streaming / synchronous adaptation
//!
//! pi's harness is `async`: `prompt()` returns a `Promise<AssistantMessage>`,
//! turns run "in the background" against a `runPromise`, and `waitForIdle()`
//! awaits it. The crate is **eager/synchronous** (see [`crate::types`]): a
//! [`AgentHarness::prompt`] runs the whole turn to completion and returns the
//! assistant message directly, so `runPromise`/[`AgentHarness::wait_for_idle`]
//! collapse to no-ops. Every pi `await hook(...)` becomes a synchronous call.
//!
//! # Where pi throws vs. where the loop can't propagate
//!
//! pi lets a hook exception thrown *inside* `runAgentLoop` propagate out and be
//! caught by `executeTurn`, which then reports a synthesized failure assistant
//! message (`emitRunFailure`). The crate's [`agent_loop`](crate::agent_loop)
//! hooks return plain values and cannot propagate an error out of the loop. The
//! port therefore records the first in-loop hook/session error on a per-run slot
//! and trips the run's abort signal (see [`HarnessInner::record_run_error`]);
//! once the loop unwinds, [`AgentHarness::prompt`] discards the loop's output and
//! synthesizes the same failure message pi would (ADAPTED, but observably
//! identical). Errors raised *before* the loop starts (queue/`before_agent_start`
//! hooks, turn-state construction) propagate out as an [`AgentHarnessError`],
//! exactly as pi's `prompt()` rejects.
//!
//! # Single-threaded interior + `Send`/`Sync` bridge
//!
//! The harness holds a [`Session`] (an `Rc`-based, `!Send` handle), so the whole
//! harness is single-threaded. The loop's [`AgentEventSink`] and
//! [`StreamFn`](crate::types::StreamFn), and every [`AgentLoopConfig`] hook, are
//! declared `Send + Sync`. The port bridges this with [`SendSync`], an
//! `unsafe`-asserted wrapper around the `Rc<HarnessInner>` the run closures
//! capture: the harness never actually crosses a thread (the loop invokes every
//! closure synchronously on the calling thread), so the assertion is sound. All
//! interior mutability lives in `RefCell`/`Cell` on [`HarnessInner`]; every emit
//! path clones the relevant handler list under a short borrow and drops it before
//! invoking user code, so re-entrant harness calls (a subscriber that steers,
//! sets the model, etc.) never double-borrow.
//!
//! Source of truth: `vendor/pi/packages/agent/src/harness/agent-harness.ts`.

// straitjacket-allow-file:duplication — the compaction/branch-summary drivers,
// the per-event emit wrappers, and the setModel/setThinkingLevel/setTools
// mutators are faithful parallel transcriptions of pi's one-method-per-shape
// source; the repeated borrow/emit/normalize shapes are intentional mirrors of
// pi, not extractable duplication.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};

use atilla_ai::seams::clock::{Clock, SystemClock};
use atilla_ai::seams::AbortSignal;
use atilla_ai::Model;

use crate::agent_loop::run_agent_loop;
use crate::harness::compaction::Models;
use crate::harness::env::ExecutionEnv;
use crate::harness::events::{
    AgentHarnessOwnEvent, AgentHarnessPhase, AgentHarnessResources, AgentHarnessStreamOptions,
    AgentHarnessStreamOptionsPatch, ModelUpdateEvent, NavigateTreeResult, ResourcesUpdateEvent,
    ThinkingLevelUpdateEvent, ToolsUpdateEvent, UpdateSource,
};
use crate::harness::options::PendingMessage;
use crate::harness::options::{
    AgentHarnessError, AgentHarnessErrorCode, AgentHarnessOptions, PendingActiveToolsChange,
    PendingModelChange, PendingSessionWrite, PendingThinkingLevelChange, ProviderStream,
    SystemPromptSource,
};
use crate::harness::prompt_templates::format_prompt_template_invocation;
use crate::harness::session::Session;
use crate::harness::skills::format_skill_invocation;
use crate::harness::types::SessionError;
use crate::types::{AgentEvent, AgentMessage, AgentTool, QueueMode, ThinkingLevel};

// Result types the per-event hooks return, keyed by event.
use crate::harness::events::AgentHarnessEventResult;

mod inner;
#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Send/Sync bridge for the single-threaded interior.
// ---------------------------------------------------------------------------

/// An `unsafe`-asserted `Send + Sync` wrapper used only to satisfy the loop's
/// `Send + Sync` closure bounds while capturing the harness's single-threaded
/// `Rc<HarnessInner>`.
///
/// Sound because the harness never crosses a thread: [`run_agent_loop`] invokes
/// the sink, stream function, and every config hook synchronously on the calling
/// thread, and the wrapper is dropped when the run returns. It is never sent to
/// or shared with another thread.
pub(super) struct SendSync<T>(T);

// SAFETY: see the type-level doc — the wrapped value is confined to a single
// thread; the impls exist only to type-check the loop's closure bounds.
unsafe impl<T> Send for SendSync<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for SendSync<T> {}

impl<T> SendSync<T> {
    /// Borrow the wrapped value. Calling this in a closure body forces the
    /// closure to capture the whole `SendSync` (which is `Send + Sync`) rather
    /// than disjointly capturing the inner `!Send` field.
    pub(super) fn get(&self) -> &T {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// The subscriber event union and handler types (`types.ts` + `agent-harness.ts`).
// ---------------------------------------------------------------------------

/// The event passed to a [`AgentHarness::subscribe`] listener: either an event
/// the harness itself emits, or a loop-level [`AgentEvent`] re-broadcast. Mirrors
/// pi's `AgentHarnessEvent = AgentEvent | AgentHarnessOwnEvent`.
#[derive(Debug, Clone)]
pub enum AgentHarnessEvent {
    /// An event the harness emits (`AgentHarnessOwnEvent`). Boxed: the own-event
    /// union is much larger than a loop [`AgentEvent`], so boxing keeps the
    /// combined enum small.
    Own(Box<AgentHarnessOwnEvent>),
    /// A loop-level event re-broadcast to subscribers.
    Loop(AgentEvent),
}

/// A `subscribe(...)` listener. Infallible in the port: pi listeners may throw
/// (caught and normalized to a `hook` error), but no ported scenario relies on a
/// throwing subscriber, so the port keeps subscribers infallible and reserves
/// error propagation for the typed [`OwnHandler`] hooks (which the `context`
/// failure path exercises).
pub type Subscriber = Rc<dyn Fn(&AgentHarnessEvent, Option<&AbortSignal>)>;

/// A typed per-event hook registered via [`AgentHarness::on`]. Receives the
/// harness own-event and returns an optional [`AgentHarnessEventResult`], or
/// `Err(message)` to signal a thrown hook error (pi's `throw`), which the harness
/// normalizes into an [`AgentHarnessError`] with code
/// [`hook`](AgentHarnessErrorCode::Hook).
pub type OwnHandler =
    Rc<dyn Fn(&AgentHarnessOwnEvent) -> Result<Option<AgentHarnessEventResult>, String>>;

/// The disposer returned by [`AgentHarness::subscribe`]/[`AgentHarness::on`].
pub type Unsubscribe = Box<dyn Fn()>;
// ---------------------------------------------------------------------------
// Free helpers (`agent-harness.ts:37-135`).
// ---------------------------------------------------------------------------

/// Build a `user` [`AgentMessage`] (`createUserMessage`, `agent-harness.ts:37`).
pub(super) fn create_user_message(
    text: &str,
    images: Option<&[Value]>,
    now_ms: i64,
) -> AgentMessage {
    let mut content = vec![json!({ "type": "text", "text": text })];
    if let Some(images) = images {
        content.extend(images.iter().cloned());
    }
    json!({ "role": "user", "content": content, "timestamp": now_ms })
}

/// Build a synthesized failure `assistant` [`AgentMessage`]
/// (`createFailureMessage`, `agent-harness.ts:43`).
pub(super) fn create_failure_message(
    model: &Model,
    error: &str,
    aborted: bool,
    now_ms: i64,
) -> AgentMessage {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "" }],
        "api": model.api,
        "provider": model.provider,
        "model": model.id,
        "stopReason": if aborted { "aborted" } else { "error" },
        "errorMessage": error,
        "timestamp": now_ms,
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        },
    })
}

/// Deep-clone stream options (`cloneStreamOptions`, `agent-harness.ts:64`).
pub(super) fn clone_stream_options(
    options: &AgentHarnessStreamOptions,
) -> AgentHarnessStreamOptions {
    options.clone()
}

/// Return the names that occur more than once, in first-duplicate order
/// (`findDuplicateNames`, `agent-harness.ts:72`).
pub(super) fn find_duplicate_names(names: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = Vec::new();
    let mut dup_seen = std::collections::HashSet::new();
    for name in names {
        if seen.contains(name) && dup_seen.insert(name.clone()) {
            duplicates.push(name.clone());
        }
        seen.insert(name.clone());
    }
    duplicates
}

/// Apply a stream-option patch (`applyStreamOptionsPatch`, `agent-harness.ts:82`).
///
/// Scalar fields override when `Some` and are left untouched when `None` (the
/// crate's [`AgentHarnessStreamOptionsPatch`] models scalar delete as "leave", by
/// prior-wave design). Header/metadata patches merge key-by-key: a header key
/// mapped to `None` (pi's `undefined`) or a metadata key mapped to `Value::Null`
/// deletes that key; a merged map that becomes empty collapses to `None`.
pub(super) fn apply_stream_options_patch(
    base: &AgentHarnessStreamOptions,
    patch: Option<&AgentHarnessStreamOptionsPatch>,
) -> AgentHarnessStreamOptions {
    let mut result = clone_stream_options(base);
    let Some(patch) = patch else {
        return result;
    };

    if patch.transport.is_some() {
        result.transport = patch.transport;
    }
    if patch.timeout_ms.is_some() {
        result.timeout_ms = patch.timeout_ms;
    }
    if patch.max_retries.is_some() {
        result.max_retries = patch.max_retries;
    }
    if patch.max_retry_delay_ms.is_some() {
        result.max_retry_delay_ms = patch.max_retry_delay_ms;
    }
    if patch.cache_retention.is_some() {
        result.cache_retention = patch.cache_retention.clone();
    }

    if let Some(header_patch) = &patch.headers {
        let mut headers = result.headers.clone().unwrap_or_default();
        for (key, value) in header_patch {
            match value {
                Some(v) => {
                    headers.insert(key.clone(), v.clone());
                }
                None => {
                    headers.remove(key);
                }
            }
        }
        result.headers = if headers.is_empty() {
            None
        } else {
            Some(headers)
        };
    }

    if let Some(metadata_patch) = &patch.metadata {
        let mut metadata = result.metadata.clone().unwrap_or_default();
        for (key, value) in metadata_patch {
            if value.is_null() {
                metadata.remove(key);
            } else {
                metadata.insert(key.clone(), value.clone());
            }
        }
        result.metadata = if metadata.is_empty() {
            None
        } else {
            Some(metadata)
        };
    }

    result
}

/// pi's `ThinkingLevel` string form, as passed to `compact`/`generateSummary`.
pub(super) fn thinking_level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// `normalizeHookError` (`agent-harness.ts:137`) — a thrown hook message becomes
/// a `hook`-coded [`AgentHarnessError`]. (The port carries only the message, so
/// the `SessionError`/`CompactionError`/`BranchSummaryError` refinement pi does
/// via `instanceof` is not available for hook throws.)
pub(super) fn normalize_hook_error(message: String) -> AgentHarnessError {
    AgentHarnessError::new(AgentHarnessErrorCode::Hook, message)
}

pub(super) fn session_error(err: SessionError) -> AgentHarnessError {
    AgentHarnessError::new(AgentHarnessErrorCode::Session, err.message)
}

/// Extract the concatenated text from a message-entry `content`
/// (string or `{type:"text",text}[]`), mirroring `navigateTree`'s editor-text
/// projection.
pub(super) fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
// ---------------------------------------------------------------------------
// Per-turn state (`AgentHarnessTurnState`, `agent-harness.ts:141`).
// ---------------------------------------------------------------------------

/// A snapshot of the harness state for one turn, rebuilt at every save point
/// (`prepareNextTurn`). Mirrors pi's `AgentHarnessTurnState`.
#[derive(Clone)]
pub(super) struct TurnState {
    pub(super) messages: Vec<AgentMessage>,
    pub(super) stream_options: AgentHarnessStreamOptions,
    pub(super) session_id: String,
    pub(super) system_prompt: String,
    pub(super) model: Model,
    pub(super) thinking_level: ThinkingLevel,
    pub(super) active_tools: Vec<AgentTool>,
}

// ---------------------------------------------------------------------------
// Interior state.
// ---------------------------------------------------------------------------

/// The harness's interior-mutable state, shared between the [`AgentHarness`]
/// handle and the run closures via `Rc`. Every field that changes at runtime is
/// behind a `Cell`/`RefCell`; immutable construction inputs (`env`, `session`,
/// `models`, `stream`, `system_prompt`, `clock`) are plain fields.
pub(super) struct HarnessInner {
    pub(super) env: Box<dyn ExecutionEnv>,
    pub(super) session: Session,
    pub(super) models: Box<dyn Models>,
    pub(super) stream: ProviderStream,
    pub(super) system_prompt: Option<SystemPromptSource>,
    pub(super) clock: Arc<dyn Clock>,

    pub(super) phase: Cell<AgentHarnessPhase>,
    pub(super) run_abort: RefCell<Option<AbortSignal>>,
    pub(super) run_error: RefCell<Option<AgentHarnessError>>,
    pub(super) suppress: Cell<bool>,
    pub(super) pending_session_writes: RefCell<Vec<PendingSessionWrite>>,
    pub(super) model: RefCell<Model>,
    pub(super) thinking_level: Cell<ThinkingLevel>,
    pub(super) stream_options: RefCell<AgentHarnessStreamOptions>,
    pub(super) resources: RefCell<AgentHarnessResources>,
    pub(super) tools: RefCell<Vec<AgentTool>>,
    pub(super) active_tool_names: RefCell<Vec<String>>,
    pub(super) steer_queue: RefCell<Vec<AgentMessage>>,
    pub(super) steering_mode: Cell<QueueMode>,
    pub(super) follow_up_queue: RefCell<Vec<AgentMessage>>,
    pub(super) follow_up_mode: Cell<QueueMode>,
    pub(super) next_turn_queue: RefCell<Vec<AgentMessage>>,
    pub(super) active_turn_state: RefCell<Option<TurnState>>,

    pub(super) subscribers: RefCell<Vec<(u64, Subscriber)>>,
    pub(super) on_handlers: RefCell<BTreeMap<String, Vec<(u64, OwnHandler)>>>,
    pub(super) next_id: Cell<u64>,
}
// ---------------------------------------------------------------------------
// The public harness.
// ---------------------------------------------------------------------------

/// Stateful driver over the low-level [agent loop](crate::agent_loop). Mirrors
/// pi's `AgentHarness`.
///
/// Cloning yields another handle to the **same** shared interior (a cheap `Rc`
/// clone), so it can be captured into subscribers/hooks that re-enter the
/// harness.
#[derive(Clone)]
pub struct AgentHarness {
    inner: Rc<HarnessInner>,
}

impl std::fmt::Debug for AgentHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHarness")
            .field("phase", &self.inner.phase.get())
            .finish_non_exhaustive()
    }
}

impl AgentHarness {
    /// Construct a harness from `options`, using the production [`SystemClock`]
    /// for the `Date.now()` reads pi does when stamping message timestamps.
    /// Mirrors pi's `constructor` (`agent-harness.ts:183`), returning
    /// [`AgentHarnessError`] (code `invalid_argument`) where pi throws for a
    /// duplicate/unknown tool name.
    pub fn new(options: AgentHarnessOptions) -> Result<Self, AgentHarnessError> {
        Self::with_clock(options, Arc::new(SystemClock::new()))
    }

    /// Construct a harness driven by an injected [`Clock`], so tests can pin the
    /// timestamps stamped on generated prompt/failure messages.
    pub fn with_clock(
        options: AgentHarnessOptions,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AgentHarnessError> {
        let tools = options.tools.unwrap_or_default();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        validate_unique_names(&tool_names, "Duplicate tool name(s)")?;

        let active_tool_names = match options.active_tool_names {
            Some(names) => names,
            None => tool_names.clone(),
        };
        validate_unique_names(&active_tool_names, "Duplicate active tool name(s)")?;
        validate_tool_names(&active_tool_names, &tools)?;

        let inner = HarnessInner {
            env: options.env,
            session: options.session,
            models: options.models,
            stream: options.stream,
            system_prompt: options.system_prompt,
            clock,
            phase: Cell::new(AgentHarnessPhase::Idle),
            run_abort: RefCell::new(None),
            run_error: RefCell::new(None),
            suppress: Cell::new(false),
            pending_session_writes: RefCell::new(Vec::new()),
            model: RefCell::new(options.model),
            thinking_level: Cell::new(options.thinking_level.unwrap_or(ThinkingLevel::Off)),
            stream_options: RefCell::new(
                options
                    .stream_options
                    .map(|o| clone_stream_options(&o))
                    .unwrap_or_default(),
            ),
            resources: RefCell::new(options.resources.unwrap_or_default()),
            tools: RefCell::new(tools),
            active_tool_names: RefCell::new(active_tool_names),
            steer_queue: RefCell::new(Vec::new()),
            steering_mode: Cell::new(options.steering_mode.unwrap_or(QueueMode::OneAtATime)),
            follow_up_queue: RefCell::new(Vec::new()),
            follow_up_mode: Cell::new(options.follow_up_mode.unwrap_or(QueueMode::OneAtATime)),
            next_turn_queue: RefCell::new(Vec::new()),
            active_turn_state: RefCell::new(None),
            subscribers: RefCell::new(Vec::new()),
            on_handlers: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(0),
        };
        Ok(Self {
            inner: Rc::new(inner),
        })
    }

    /// The execution environment (pi's `readonly env`).
    pub fn env(&self) -> &dyn ExecutionEnv {
        self.inner.env.as_ref()
    }

    // -- Subscriptions / hooks (`agent-harness.ts:1003`, `agent-harness.ts:1015`).

    /// Subscribe to every harness/loop event (pi's `subscribe`). Returns a
    /// disposer that removes the listener.
    pub fn subscribe(&self, listener: Subscriber) -> Unsubscribe {
        let id = self.inner.alloc_id();
        self.inner.subscribers.borrow_mut().push((id, listener));
        let inner = self.inner.clone();
        Box::new(move || {
            inner.subscribers.borrow_mut().retain(|(i, _)| *i != id);
        })
    }

    /// Register a typed per-event hook (pi's `on(type, handler)`). Returns a
    /// disposer that removes the handler. `event_type` is the pi event
    /// discriminant (e.g. `"context"`, `"tool_result"`,
    /// `"before_provider_request"`).
    pub fn on(&self, event_type: &str, handler: OwnHandler) -> Unsubscribe {
        let id = self.inner.alloc_id();
        self.inner
            .on_handlers
            .borrow_mut()
            .entry(event_type.to_string())
            .or_default()
            .push((id, handler));
        let inner = self.inner.clone();
        let event_type = event_type.to_string();
        Box::new(move || {
            if let Some(list) = inner.on_handlers.borrow_mut().get_mut(&event_type) {
                list.retain(|(i, _)| *i != id);
            }
        })
    }

    // -- Getters (`agent-harness.ts:829`, `848`, `867`, `902`, `930`, `938`, `946`, `962`).

    /// The active model.
    pub fn get_model(&self) -> Model {
        self.inner.model.borrow().clone()
    }

    /// The active thinking level.
    pub fn get_thinking_level(&self) -> ThinkingLevel {
        self.inner.thinking_level.get()
    }

    /// All tools, in insertion order.
    pub fn get_tools(&self) -> Vec<AgentTool> {
        self.inner.tools.borrow().clone()
    }

    /// The active tools, in `active_tool_names` order.
    pub fn get_active_tools(&self) -> Vec<AgentTool> {
        let tools = self.inner.tools.borrow();
        self.inner
            .active_tool_names
            .borrow()
            .iter()
            .filter_map(|n| tools.iter().find(|t| &t.name == n).cloned())
            .collect()
    }

    /// The steering queue drain mode.
    pub fn get_steering_mode(&self) -> QueueMode {
        self.inner.steering_mode.get()
    }

    /// The follow-up queue drain mode.
    pub fn get_follow_up_mode(&self) -> QueueMode {
        self.inner.follow_up_mode.get()
    }

    /// The current resources (skills/prompt templates), copied.
    pub fn get_resources(&self) -> AgentHarnessResources {
        self.inner.get_resources()
    }

    /// The current stream options, cloned.
    pub fn get_stream_options(&self) -> AgentHarnessStreamOptions {
        clone_stream_options(&self.inner.stream_options.borrow())
    }

    /// The current lifecycle phase.
    pub fn phase(&self) -> AgentHarnessPhase {
        self.inner.phase.get()
    }

    // -- Queue-mode setters (`agent-harness.ts:934`, `942`).

    /// Set the steering queue drain mode.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner.steering_mode.set(mode);
    }

    /// Set the follow-up queue drain mode.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner.follow_up_mode.set(mode);
    }

    /// Replace the curated stream options (`setStreamOptions`,
    /// `agent-harness.ts:966`).
    pub fn set_stream_options(&self, stream_options: AgentHarnessStreamOptions) {
        *self.inner.stream_options.borrow_mut() = clone_stream_options(&stream_options);
    }

    // -- Prompt entry points (`agent-harness.ts:608`, `623`, `640`).

    /// Submit a prompt and run a turn to completion (pi's `prompt`). Returns the
    /// last assistant [`AgentMessage`].
    pub fn prompt(
        &self,
        text: &str,
        images: Option<&[Value]>,
    ) -> Result<AgentMessage, AgentHarnessError> {
        self.run_turn(AgentHarnessErrorCode::Busy, |harness| {
            let turn_state = harness.inner.create_turn_state()?;
            harness.execute_turn(turn_state, text, images)
        })
    }

    /// Invoke a skill by name, then run a turn (pi's `skill`).
    pub fn skill(
        &self,
        name: &str,
        additional_instructions: Option<&str>,
    ) -> Result<AgentMessage, AgentHarnessError> {
        self.run_turn(AgentHarnessErrorCode::Busy, |harness| {
            let turn_state = harness.inner.create_turn_state()?;
            let resources = harness.inner.get_resources();
            let skill = resources
                .skills
                .as_ref()
                .and_then(|skills| skills.iter().find(|s| s.name == name))
                .cloned()
                .ok_or_else(|| {
                    AgentHarnessError::new(
                        AgentHarnessErrorCode::InvalidArgument,
                        format!("Unknown skill: {name}"),
                    )
                })?;
            let text = format_skill_invocation(&skill, additional_instructions);
            harness.execute_turn(turn_state, &text, None)
        })
    }

    /// Invoke a prompt template by name with positional args, then run a turn
    /// (pi's `promptFromTemplate`).
    pub fn prompt_from_template(
        &self,
        name: &str,
        args: &[&str],
    ) -> Result<AgentMessage, AgentHarnessError> {
        self.run_turn(AgentHarnessErrorCode::Busy, |harness| {
            let turn_state = harness.inner.create_turn_state()?;
            let resources = harness.inner.get_resources();
            let template = resources
                .prompt_templates
                .as_ref()
                .and_then(|templates| templates.iter().find(|t| t.name == name))
                .cloned()
                .ok_or_else(|| {
                    AgentHarnessError::new(
                        AgentHarnessErrorCode::InvalidArgument,
                        format!("Unknown prompt template: {name}"),
                    )
                })?;
            let text = format_prompt_template_invocation(&template, args);
            harness.execute_turn(turn_state, &text, None)
        })
    }

    /// The shared phase-guard + idle-reset wrapper for the three prompt entry
    /// points (pi's identical `if phase !== "idle" throw busy; phase="turn"; …
    /// catch { phase="idle" }`).
    fn run_turn(
        &self,
        _busy: AgentHarnessErrorCode,
        body: impl FnOnce(&Self) -> Result<AgentMessage, AgentHarnessError>,
    ) -> Result<AgentMessage, AgentHarnessError> {
        if self.inner.phase.get() != AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::busy("AgentHarness is busy"));
        }
        self.inner.phase.set(AgentHarnessPhase::Turn);
        match body(self) {
            Ok(message) => Ok(message),
            Err(error) => {
                self.inner.phase.set(AgentHarnessPhase::Idle);
                Err(error)
            }
        }
    }

    /// The turn driver (pi's `executeTurn`, `agent-harness.ts:531`).
    fn execute_turn(
        &self,
        turn_state: TurnState,
        text: &str,
        images: Option<&[Value]>,
    ) -> Result<AgentMessage, AgentHarnessError> {
        let inner = &self.inner;
        let now = inner.clock.now_ms();

        // Assemble the prompt messages, prepending any queued next-turn messages
        // (errors here propagate — pi rejects `prompt()`).
        let mut messages: Vec<AgentMessage> = vec![create_user_message(text, images, now)];
        {
            let queued: Vec<AgentMessage> = inner.next_turn_queue.borrow_mut().drain(..).collect();
            if !queued.is_empty() {
                if let Err(error) = inner.emit_queue_update() {
                    inner.next_turn_queue.borrow_mut().splice(0..0, queued);
                    return Err(error);
                }
                let user = messages.remove(0);
                messages = queued;
                messages.push(user);
            }
        }

        // before_agent_start hook (errors propagate — pi rejects `prompt()`).
        let before = inner.emit_before_agent_start(text, images, &turn_state)?;
        if let Some(before) = &before {
            if let Some(hook_messages) = &before.messages {
                messages.extend(hook_messages.iter().cloned());
            }
        }
        let system_prompt_override = before.and_then(|b| b.system_prompt);

        // Set up the run.
        let signal = AbortSignal::new();
        *inner.run_abort.borrow_mut() = Some(signal.clone());
        *inner.run_error.borrow_mut() = None;
        inner.suppress.set(false);
        let model = turn_state.model.clone();
        let context = inner.create_context(&turn_state, system_prompt_override.as_deref());
        *inner.active_turn_state.borrow_mut() = Some(turn_state);

        let sink = inner.make_sink(&signal);
        let stream_fn = inner.make_stream_fn();
        let config = inner.make_loop_config();

        let new_messages =
            run_agent_loop(messages, context, config, &sink, Some(&signal), &stream_fn);

        // Resolve the outcome. A recorded in-loop error means the whole run is
        // replaced by a synthesized failure message (pi's `emitRunFailure`).
        let result = if let Some(error) = inner.run_error.borrow_mut().take() {
            inner.suppress.set(false);
            let failure =
                create_failure_message(&model, &error.message, false, inner.clock.now_ms());
            inner.drive_run_failure(failure.clone(), &signal);
            Ok(failure)
        } else {
            last_assistant(&new_messages).ok_or_else(|| {
                AgentHarnessError::new(
                    AgentHarnessErrorCode::InvalidState,
                    "AgentHarness prompt completed without an assistant message",
                )
            })
        };

        let _ = inner.flush_pending_session_writes();
        *inner.run_abort.borrow_mut() = None;
        *inner.active_turn_state.borrow_mut() = None;
        result
    }

    // -- Queueing (`agent-harness.ts:657`, `663`, `669`, `674`).

    /// Queue a steering message (pi's `steer`). Errors while idle.
    pub fn steer(&self, text: &str, images: Option<&[Value]>) -> Result<(), AgentHarnessError> {
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidState,
                "Cannot steer while idle",
            ));
        }
        let now = self.inner.clock.now_ms();
        self.inner
            .steer_queue
            .borrow_mut()
            .push(create_user_message(text, images, now));
        self.inner.emit_queue_update()
    }

    /// Queue a follow-up message (pi's `followUp`). Errors while idle.
    pub fn follow_up(&self, text: &str, images: Option<&[Value]>) -> Result<(), AgentHarnessError> {
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidState,
                "Cannot follow up while idle",
            ));
        }
        let now = self.inner.clock.now_ms();
        self.inner
            .follow_up_queue
            .borrow_mut()
            .push(create_user_message(text, images, now));
        self.inner.emit_queue_update()
    }

    /// Queue a message to prepend to the next turn (pi's `nextTurn`).
    pub fn next_turn(&self, text: &str, images: Option<&[Value]>) -> Result<(), AgentHarnessError> {
        let now = self.inner.clock.now_ms();
        self.inner
            .next_turn_queue
            .borrow_mut()
            .push(create_user_message(text, images, now));
        self.inner.emit_queue_update()
    }

    /// Append a message to the session, honoring the phase (pi's `appendMessage`).
    /// While idle it writes through; mid-run it is deferred to the pending-writes
    /// queue so it lands after agent-emitted messages.
    pub fn append_message(&self, message: AgentMessage) -> Result<(), AgentHarnessError> {
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            self.inner
                .session
                .append_message(message)
                .map(|_| ())
                .map_err(session_error)
        } else {
            self.inner
                .pending_session_writes
                .borrow_mut()
                .push(PendingSessionWrite::Message(PendingMessage { message }));
            Ok(())
        }
    }

    // -- Model / thinking / tools / resources setters.

    /// Set the active model (pi's `setModel`).
    pub fn set_model(&self, model: Model) -> Result<(), AgentHarnessError> {
        let previous = self.inner.model.borrow().clone();
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            self.inner
                .session
                .append_model_change(&model.provider, &model.id)
                .map_err(session_error)?;
        } else {
            self.inner
                .pending_session_writes
                .borrow_mut()
                .push(PendingSessionWrite::ModelChange(PendingModelChange {
                    provider: model.provider.clone(),
                    model_id: model.id.clone(),
                }));
        }
        *self.inner.model.borrow_mut() = model.clone();
        self.inner.emit_own(
            AgentHarnessOwnEvent::ModelUpdate(ModelUpdateEvent {
                model,
                previous_model: Some(previous),
                source: UpdateSource::Set,
            }),
            None,
        );
        Ok(())
    }

    /// Set the active thinking level (pi's `setThinkingLevel`).
    pub fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), AgentHarnessError> {
        let previous = self.inner.thinking_level.get();
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            self.inner
                .session
                .append_thinking_level_change(thinking_level_str(level))
                .map_err(session_error)?;
        } else {
            self.inner.pending_session_writes.borrow_mut().push(
                PendingSessionWrite::ThinkingLevelChange(PendingThinkingLevelChange {
                    thinking_level: thinking_level_str(level).to_string(),
                }),
            );
        }
        self.inner.thinking_level.set(level);
        self.inner.emit_own(
            AgentHarnessOwnEvent::ThinkingLevelUpdate(ThinkingLevelUpdateEvent {
                level,
                previous_level: previous,
            }),
            None,
        );
        Ok(())
    }

    /// Replace the tool set and optionally the active tools (pi's `setTools`).
    pub fn set_tools(
        &self,
        tools: Vec<AgentTool>,
        active_tool_names: Option<Vec<String>>,
    ) -> Result<(), AgentHarnessError> {
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        validate_unique_names(&names, "Duplicate tool name(s)")?;
        let next_active = match active_tool_names {
            Some(names) => names,
            None => self.inner.active_tool_names.borrow().clone(),
        };
        validate_tool_names(&next_active, &tools)?;

        let previous_tool_names: Vec<String> = self
            .inner
            .tools
            .borrow()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let previous_active = self.inner.active_tool_names.borrow().clone();

        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            self.inner
                .session
                .append_active_tools_change(next_active.clone())
                .map_err(session_error)?;
        } else {
            self.inner.pending_session_writes.borrow_mut().push(
                PendingSessionWrite::ActiveToolsChange(PendingActiveToolsChange {
                    active_tool_names: next_active.clone(),
                }),
            );
        }
        *self.inner.tools.borrow_mut() = tools;
        *self.inner.active_tool_names.borrow_mut() = next_active.clone();
        let tool_names: Vec<String> = self
            .inner
            .tools
            .borrow()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        self.inner.emit_own(
            AgentHarnessOwnEvent::ToolsUpdate(ToolsUpdateEvent {
                tool_names,
                previous_tool_names,
                active_tool_names: next_active,
                previous_active_tool_names: previous_active,
                source: UpdateSource::Set,
            }),
            None,
        );
        Ok(())
    }

    /// Replace the active tool names (pi's `setActiveTools`).
    pub fn set_active_tools(&self, tool_names: Vec<String>) -> Result<(), AgentHarnessError> {
        validate_tool_names(&tool_names, &self.inner.tools.borrow())?;
        let previous_tool_names: Vec<String> = self
            .inner
            .tools
            .borrow()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let previous_active = self.inner.active_tool_names.borrow().clone();
        if self.inner.phase.get() == AgentHarnessPhase::Idle {
            self.inner
                .session
                .append_active_tools_change(tool_names.clone())
                .map_err(session_error)?;
        } else {
            self.inner.pending_session_writes.borrow_mut().push(
                PendingSessionWrite::ActiveToolsChange(PendingActiveToolsChange {
                    active_tool_names: tool_names.clone(),
                }),
            );
        }
        *self.inner.active_tool_names.borrow_mut() = tool_names.clone();
        let all_names: Vec<String> = self
            .inner
            .tools
            .borrow()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        self.inner.emit_own(
            AgentHarnessOwnEvent::ToolsUpdate(ToolsUpdateEvent {
                tool_names: all_names,
                previous_tool_names,
                active_tool_names: tool_names,
                previous_active_tool_names: previous_active,
                source: UpdateSource::Set,
            }),
            None,
        );
        Ok(())
    }

    /// Replace the harness resources (pi's `setResources`).
    pub fn set_resources(&self, resources: AgentHarnessResources) {
        let previous = self.inner.get_resources();
        *self.inner.resources.borrow_mut() = AgentHarnessResources {
            skills: resources.skills.clone(),
            prompt_templates: resources.prompt_templates.clone(),
        };
        let next = self.inner.get_resources();
        self.inner.emit_own(
            AgentHarnessOwnEvent::ResourcesUpdate(ResourcesUpdateEvent {
                resources: next,
                previous_resources: previous,
            }),
            None,
        );
    }

    // -- Abort / idle (`agent-harness.ts:970`, `999`).

    /// Abort an in-flight run: clears the steer/follow-up queues (preserving
    /// next-turn), trips the run signal, and emits `queue_update`/`abort`. Mirrors
    /// pi's `abort`. Returns the drained queues.
    pub fn abort(&self) -> Result<crate::harness::events::AbortResult, AgentHarnessError> {
        let cleared_steer: Vec<AgentMessage> = self.inner.steer_queue.borrow().clone();
        let cleared_follow_up: Vec<AgentMessage> = self.inner.follow_up_queue.borrow().clone();
        self.inner.steer_queue.borrow_mut().clear();
        self.inner.follow_up_queue.borrow_mut().clear();
        if let Some(signal) = self.inner.run_abort.borrow().as_ref() {
            signal.abort();
        }
        // Sync port: subscribers are infallible and the run has already settled,
        // so the queue_update/abort emits and waitForIdle cannot error.
        self.inner.emit_own(
            AgentHarnessOwnEvent::QueueUpdate(self.inner.queue_snapshot()),
            None,
        );
        self.inner.emit_own(
            AgentHarnessOwnEvent::Abort(crate::harness::events::AbortEvent {
                cleared_steer: cleared_steer.clone(),
                cleared_follow_up: cleared_follow_up.clone(),
            }),
            None,
        );
        Ok(crate::harness::events::AbortResult {
            cleared_steer,
            cleared_follow_up,
        })
    }

    /// Wait for the harness to settle (pi's `waitForIdle`). A no-op in the
    /// synchronous port: [`prompt`](Self::prompt) already runs to completion.
    pub fn wait_for_idle(&self) {}

    // -- Compaction (`agent-harness.ts:686`).

    /// Compact the session history (pi's `compact`). Requires idle.
    pub fn compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<crate::harness::events::CompactResult, AgentHarnessError> {
        if self.inner.phase.get() != AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::busy("compact() requires idle harness"));
        }
        self.inner.phase.set(AgentHarnessPhase::Compaction);
        let result = self.inner.do_compact(custom_instructions);
        self.inner.phase.set(AgentHarnessPhase::Idle);
        result
    }

    // -- Tree navigation (`agent-harness.ts:732`).

    /// Navigate the session tree to `target_id`, optionally summarizing the
    /// abandoned branch (pi's `navigateTree`). Requires idle.
    pub fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, AgentHarnessError> {
        if self.inner.phase.get() != AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::busy(
                "navigateTree() requires idle harness",
            ));
        }
        self.inner.phase.set(AgentHarnessPhase::BranchSummary);
        let result = self.inner.do_navigate_tree(target_id, options);
        self.inner.phase.set(AgentHarnessPhase::Idle);
        result
    }
}

/// Options for [`AgentHarness::navigate_tree`] (pi's inline `options` object).
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    /// Request a branch summary of the abandoned branch.
    pub summarize: bool,
    /// Instructions appended to (or replacing) the default summary prompt.
    pub custom_instructions: Option<String>,
    /// Replace the default prompt with `custom_instructions` instead of appending.
    pub replace_instructions: Option<bool>,
    /// Optional label to apply to the summarized branch.
    pub label: Option<String>,
}
// ---------------------------------------------------------------------------
// Name validation (`agent-harness.ts:450`, `456`).
// ---------------------------------------------------------------------------

pub(super) fn validate_unique_names(
    names: &[String],
    message: &str,
) -> Result<(), AgentHarnessError> {
    let duplicates = find_duplicate_names(names);
    if !duplicates.is_empty() {
        return Err(AgentHarnessError::new(
            AgentHarnessErrorCode::InvalidArgument,
            format!("{message}: {}", duplicates.join(", ")),
        ));
    }
    Ok(())
}

pub(super) fn validate_tool_names(
    tool_names: &[String],
    tools: &[AgentTool],
) -> Result<(), AgentHarnessError> {
    validate_unique_names(tool_names, "Duplicate active tool name(s)")?;
    let missing: Vec<String> = tool_names
        .iter()
        .filter(|name| !tools.iter().any(|t| &t.name == *name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(AgentHarnessError::new(
            AgentHarnessErrorCode::InvalidArgument,
            format!("Unknown tool(s): {}", missing.join(", ")),
        ));
    }
    Ok(())
}

/// The last `assistant` message in `messages` (pi's reverse scan in
/// `executeTurn`).
pub(super) fn last_assistant(messages: &[AgentMessage]) -> Option<AgentMessage> {
    messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .cloned()
}
