//! Extension-runner seam consumed by `AgentSession` (pi's `ExtensionRunner`
//! façade, `core/extensions/runner.ts`, ~1214 LOC).
//!
//! This is **PR0**: the trait, the net-new types, the four `bindCore` host
//! traits, and a no-op [`StubExtensionRunner`]. It defines the negotiated, locked
//! two-party contract the extension-plane owner implements from `pidgin-extensions`
//! (deno-backed); pidgin-coding ships only the trait and the always-compiled
//! stub, and **never names an pidgin-extensions type** (the crate dependency runs
//! `pidgin-extensions -> pidgin-coding` only). There is deliberately **no**
//! `AgentSession` struct and **no** real impl here — the deno-backed
//! `RealExtensionRunner` lives in `pidgin-extensions` behind its `deno` feature
//! and is injected by the CLI.
//!
//! The seam mirrors the [`ExtensionLoader`](super::loader::ExtensionLoader)
//! template in this same module: a sync trait (pi's `async emit*` become sync;
//! the host's JS-plane handle blocks internally), plus a unit-struct stub with
//! inert returns.
//!
//! # Locked seam contract
//!
//! * **Sync throughout, bare returns.** Every `emit*` is synchronous and returns
//!   its value directly (no `Result`); handler errors side-channel through
//!   [`ExtensionRunner::on_error`] / [`ExtensionRunner::emit_error`] in the real
//!   impl. pi's `Promise<X | undefined>` becomes `Option<X>`.
//! * **`&self` throughout + interior mutability.** `bind_core` / `set_ui_context`
//!   / `bind_command_context` / `on_error` mutate runner state, but the trait
//!   takes `&self`: the runner is a shared handle both `AgentSession` and the
//!   agent-tool hooks hold (pi passes it through a mutable
//!   `extensionRunnerRef.current`). This matches the `ExtensionLoader` seam and
//!   avoids threading `&mut` through the tool hooks.
//! * **Enum-dispatch generic `emit` + six dedicated emitters.** Rust `dyn` traits
//!   cannot have a generic method, so pi's `emit<TEvent>` becomes one
//!   [`ExtensionRunner::emit`] over [`ExtensionDispatchEvent`]. The six
//!   strongly-typed emitters (`emit_input`, `emit_before_agent_start`,
//!   `emit_resources_discover`, `emit_tool_call`, `emit_tool_result`,
//!   `emit_message_end`) stay as their own methods, exactly as pi excludes them
//!   from its generic `emit`.
//! * **`bindCore`'s callback groups are host traits**, not a struct of boxed
//!   closures: [`SessionControlHost`], [`SessionContextHost`],
//!   [`ProviderRegistrationHost`], and (for `bindCommandContext`)
//!   [`ExtensionCommandContextHost`]. `AgentSession` implements each in one `impl`
//!   block and passes `Arc<dyn ...>`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::command::{CommandContext, ResolvedCommand};
use crate::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use crate::core::extensions::events::agent::{AgentEndEvent, AgentSettledEvent, AgentStartEvent};
use crate::core::extensions::events::common::{
    AgentMessage, BuildSystemPromptOptions, ImageContent,
};
use crate::core::extensions::events::selection::{
    InputEventResult, InputSource, StreamingBehavior,
};
use crate::core::extensions::events::session::{
    ResourcesDiscoverReason, ResourcesDiscoverResult, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionBeforeForkEvent, SessionBeforeForkResult,
    SessionBeforeSwitchEvent, SessionBeforeSwitchResult, SessionBeforeTreeEvent,
    SessionBeforeTreeResult, SessionCompactEvent, SessionShutdownEvent, SessionStartEvent,
    SessionTreeEvent,
};
use crate::core::extensions::events::tool::{
    ToolCallEvent, ToolCallEventResult, ToolExecutionEndEvent, ToolExecutionStartEvent,
    ToolExecutionUpdateEvent, ToolResultEvent, ToolResultEventResult,
};
use crate::core::extensions::events::turn::{
    MessageEndEvent, MessageStartEvent, MessageUpdateEvent, TurnEndEvent, TurnStartEvent,
};
use crate::core::extensions::types::{ExtensionContext, ToolDefinition};
use crate::core::source_info::SourceInfo;

// ---------------------------------------------------------------------------
// Net-new type aliases
// ---------------------------------------------------------------------------

/// A registered error listener (pi's `ExtensionErrorListener`, runner.ts).
///
/// Shared and thread-safe because the runner is a shared handle: any consumer
/// may register a listener via [`ExtensionRunner::on_error`].
pub type ExtensionErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

/// The unsubscribe closure returned by [`ExtensionRunner::on_error`] (pi's
/// `() => void`). Call-once, hence `FnOnce`.
pub type UnsubscribeFn = Box<dyn FnOnce() + Send>;

// ---------------------------------------------------------------------------
// Net-new value types
// ---------------------------------------------------------------------------

/// A flag value from the extension flag registry (pi's `getFlagValues` value =
/// `boolean | string`, runner.ts:486).
///
/// Net-new for this seam; deliberately **not** the CLI-arg `FlagValue` in
/// `pidgin-cli/args.rs` (a different surface with different variants).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagValue {
    /// A boolean flag value.
    Bool(bool),
    /// A string flag value.
    Str(String),
}

/// A tool paired with its provenance (pi's `RegisteredTool`, consumed by
/// `wrapRegisteredTools` / `_refreshToolRegistry`, runner.ts:2442/2485).
///
/// Net-new: the in-crate [`Registry`](super::registry::Registry) stores bare
/// [`ToolDefinition`]s; this pairs each with its [`SourceInfo`] so
/// `wrapRegisteredTools` can attribute it.
#[derive(Clone)]
pub struct RegisteredTool {
    /// The registered tool definition.
    pub tool: ToolDefinition,
    /// Where the tool came from (its registering extension / provenance).
    pub source_info: SourceInfo,
}

/// The UI context bound to the runner (pi's `setUIContext` `uiContext?`,
/// runner.ts:429).
///
/// Net-new. The concrete UI-context shape is owned by the TUI layer and lands
/// later; for now this carries an opaque payload so the seam is stable.
// TODO(unit5): widen `raw` to the ported UI-context struct once the TUI surface
// is ported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtensionUIContext {
    /// The opaque UI-context payload (pi's `uiContext`).
    pub raw: Value,
}

/// The runner's render mode (pi's `setUIContext` `mode = "print"`,
/// runner.ts:429).
///
/// Net-new. `Print` is the default (headless / non-interactive) mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtensionMode {
    /// Headless / non-interactive rendering (pi's `"print"`).
    #[default]
    Print,
    /// Interactive terminal rendering.
    Interactive,
}

// ---------------------------------------------------------------------------
// Generic-dispatch event enum + outcome
// ---------------------------------------------------------------------------

/// The events dispatched through the generic [`ExtensionRunner::emit`] (pi's
/// `emit<TEvent>` minus the six dedicated emitters, runner.ts:784). Each variant
/// wraps the already-ported event struct from
/// [`core::extensions::events`](super::events).
///
/// Enum-dispatch keeps these variants in one trait method (Rust `dyn` cannot
/// have a generic method) and lets the set grow without widening the trait.
pub enum ExtensionDispatchEvent {
    /// pi `agent_start`.
    AgentStart(AgentStartEvent),
    /// pi `agent_end`.
    AgentEnd(AgentEndEvent),
    /// pi `agent_settled`.
    AgentSettled(AgentSettledEvent),
    /// pi `turn_start`.
    TurnStart(TurnStartEvent),
    /// pi `turn_end`.
    TurnEnd(TurnEndEvent),
    /// pi `message_start`.
    MessageStart(MessageStartEvent),
    /// pi `message_update`.
    MessageUpdate(MessageUpdateEvent),
    /// pi `tool_execution_start`.
    ToolExecutionStart(ToolExecutionStartEvent),
    /// pi `tool_execution_update`.
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    /// pi `tool_execution_end`.
    ToolExecutionEnd(ToolExecutionEndEvent),
    /// pi `model_select`.
    // TODO(unit5): replace with a ported `ModelSelectEvent` (currently only a
    // model descriptor, opaque elsewhere).
    ModelSelect(Value),
    /// pi `thinking_level_changed`.
    // TODO(unit5): replace with a ported `ThinkingLevelChangedEvent` (currently
    // only a level, opaque elsewhere).
    ThinkingLevelChanged(Value),
    /// pi `session_start` (both the initial event and the `reason:"reload"`
    /// rebuild).
    SessionStart(SessionStartEvent),
    /// pi `session_before_switch` (returns a result via
    /// [`ExtensionEmitOutcome::BeforeSwitch`]). Emitted by
    /// [`AgentSessionRuntime`](crate::core::agent_session::AgentSessionRuntime)
    /// before a `/new` or `/resume` session replacement; a handler may cancel it.
    SessionBeforeSwitch(SessionBeforeSwitchEvent),
    /// pi `session_before_fork` (returns a result via
    /// [`ExtensionEmitOutcome::BeforeFork`]). Emitted by
    /// [`AgentSessionRuntime`](crate::core::agent_session::AgentSessionRuntime)
    /// before a `/fork`; a handler may cancel it.
    SessionBeforeFork(SessionBeforeForkEvent),
    /// pi `session_compact`.
    SessionCompact(SessionCompactEvent),
    /// pi `session_before_compact` (returns a result via
    /// [`ExtensionEmitOutcome::BeforeCompact`]).
    SessionBeforeCompact(SessionBeforeCompactEvent),
    /// pi `session_tree`.
    SessionTree(SessionTreeEvent),
    /// pi `session_before_tree` (returns a result via
    /// [`ExtensionEmitOutcome::BeforeTree`]).
    SessionBeforeTree(SessionBeforeTreeEvent),
    /// pi `entry_appended`.
    // TODO(unit5): replace with a ported `EntryAppendedEvent` (currently only an
    // entry, opaque elsewhere).
    EntryAppended(Value),
}

/// The result of a generic [`ExtensionRunner::emit`]. Most events return nothing;
/// the `session_before_*` events return a typed cancel/override result (pi's
/// `RunnerEmitResult<TEvent>` conditional, runner.ts:149).
pub enum ExtensionEmitOutcome {
    /// No handler-supplied result (pi's `undefined`).
    None,
    /// `session_before_compact` -> cancel or replacement compaction.
    BeforeCompact(SessionBeforeCompactResult),
    /// `session_before_tree` -> cancel or summary override.
    BeforeTree(SessionBeforeTreeResult),
    /// `session_before_switch` -> cancel the `/new` or `/resume` switch.
    BeforeSwitch(SessionBeforeSwitchResult),
    /// `session_before_fork` -> cancel the `/fork`.
    BeforeFork(SessionBeforeForkResult),
}

// ---------------------------------------------------------------------------
// bindCore host traits
// ---------------------------------------------------------------------------

/// pi `bindCore` `actions` — the session-control callbacks `AgentSession` passes
/// into the runner-managed extension context (runner.ts:311).
///
/// Net-new. `AgentSession` implements this in one `impl` block and passes
/// `Arc<dyn SessionControlHost>`. `get_all_tools` returns opaque [`Value`]
/// entries for now (pi's `ToolInfo`).
// TODO(unit5): type `get_all_tools`' element once `AgentSession::getAllTools`
// lands its `ToolInfo` shape.
pub trait SessionControlHost: Send + Sync {
    /// pi `sendMessage`.
    fn send_message(&self, content: &Value, options: Option<&Value>);
    /// pi `sendUserMessage`.
    fn send_user_message(&self, content: &Value, options: Option<&Value>);
    /// pi `appendEntry` -> the new entry id.
    fn append_entry(&self, custom_type: &str, data: &Value) -> String;
    /// pi `setSessionName`.
    fn set_session_name(&self, name: &str);
    /// pi `getSessionName`.
    fn get_session_name(&self) -> Option<String>;
    /// pi `setLabel`.
    fn set_label(&self, entry_id: &str, label: &str);
    /// pi `getActiveTools`.
    fn get_active_tools(&self) -> Vec<String>;
    /// pi `getAllTools` (each entry pi's `ToolInfo`, opaque here).
    fn get_all_tools(&self) -> Vec<Value>;
    /// pi `setActiveTools`.
    fn set_active_tools(&self, names: &[String]);
    /// pi `refreshTools`.
    fn refresh_tools(&self);
    /// pi `getCommands`.
    fn get_commands(&self) -> Vec<ResolvedCommand>;
    /// pi `setModel`.
    fn set_model(&self, model: &Value);
    /// pi `getThinkingLevel`.
    fn get_thinking_level(&self) -> pidgin_agent::types::ThinkingLevel;
    /// pi `setThinkingLevel`.
    fn set_thinking_level(&self, level: pidgin_agent::types::ThinkingLevel);
}

/// pi `bindCore` `contextActions` — the read/control callbacks `AgentSession`
/// passes into the runner-managed extension context (runner.ts:311).
///
/// Net-new. `get_context_usage` returns opaque [`Value`] (pi's context-usage
/// snapshot).
pub trait SessionContextHost: Send + Sync {
    /// pi `getModel`.
    fn get_model(&self) -> Value;
    /// pi `isIdle`.
    fn is_idle(&self) -> bool;
    /// pi `isProjectTrusted`.
    fn is_project_trusted(&self) -> bool;
    /// pi `getSignal`.
    fn get_signal(&self) -> pidgin_ai::seams::AbortSignal;
    /// pi `abort`.
    fn abort(&self);
    /// pi `hasPendingMessages`.
    fn has_pending_messages(&self) -> bool;
    /// pi `shutdown`.
    fn shutdown(&self);
    /// pi `getContextUsage`.
    fn get_context_usage(&self) -> Option<Value>;
    /// pi `compact`.
    fn compact(&self);
    /// pi `getSystemPrompt`.
    fn get_system_prompt(&self) -> String;
    /// pi `getSystemPromptOptions`.
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
}

/// pi `bindCore` optional `providerActions` — provider-registration callbacks
/// (runner.ts:311).
///
/// Net-new. Passed as `Option<Arc<dyn ProviderRegistrationHost>>`; absent when
/// the session does not expose provider registration.
pub trait ProviderRegistrationHost: Send + Sync {
    /// pi `registerProvider`.
    fn register_provider(&self, provider: &Value);
    /// pi `registerNativeProvider`.
    fn register_native_provider(&self, provider: &Value);
    /// pi `unregisterProvider`.
    fn unregister_provider(&self, id: &str);
}

/// pi `bindCommandContext` `actions` — the command-context args/flags accessors
/// (pi's `ExtensionCommandContextActions`, runner.ts:410).
///
/// Net-new. Passed as `Option<Arc<dyn ExtensionCommandContextHost>>`.
pub trait ExtensionCommandContextHost: Send + Sync {
    /// The raw argument string for the current command invocation.
    fn get_args(&self) -> String;
    /// The resolved flag values in scope for the current command invocation.
    fn get_flags(&self) -> BTreeMap<String, FlagValue>;
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// The coding-side extension-runner seam (pi's `ExtensionRunner` façade,
/// runner.ts). Sync; the host impl blocks internally. Mirrors the
/// [`ExtensionLoader`](super::loader::ExtensionLoader) seam: pidgin-coding defines
/// it and ships a no-op [`StubExtensionRunner`], the deno host in
/// `pidgin-extensions` provides the real impl, the CLI injects it.
///
/// All methods take `&self`; the real impl uses interior mutability (its JS-plane
/// handle locks its own state).
pub trait ExtensionRunner: Send + Sync {
    // ---- lifecycle -------------------------------------------------------
    /// Fire `session_shutdown` (pi free fn `emitSessionShutdownEvent`,
    /// agent-session L2583).
    fn emit_session_shutdown(&self, event: SessionShutdownEvent);

    // ---- generic dispatch (enum-dispatch) --------------------------------
    /// pi's `emit<TEvent>` for every event except the six dedicated emitters
    /// (runner.ts:784).
    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome;

    // ---- dedicated emitters (stronger types) -----------------------------
    /// pi `emitMessageEnd` (runner.ts:818) -> optional replacement message.
    fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage>;

    /// pi `emitInput` (runner.ts:1174).
    fn emit_input(
        &self,
        text: &str,
        images: Option<&[ImageContent]>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult;

    /// pi `emitBeforeAgentStart` (runner.ts:1059) -> `Promise<... | undefined>`.
    fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<&[ImageContent]>,
        system_prompt: &str,
        system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult>;

    /// pi `emitResourcesDiscover` (runner.ts:1125).
    fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult;

    /// pi `emitToolCall` (runner.ts:910) -> optional block decision.
    fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult>;

    /// pi `emitToolResult` (runner.ts:860) -> optional replacement content.
    fn emit_tool_result(&self, event: &ToolResultEvent) -> Option<ToolResultEventResult>;

    // ---- sync queries (non-emitting) -------------------------------------
    /// pi `hasHandlers` (runner.ts:565). The `&str`->`HookEvent` adaptation is
    /// the impl's concern.
    fn has_handlers(&self, event_type: &str) -> bool;
    /// pi `getCommand` (runner.ts:644).
    fn get_command(&self, name: &str) -> Option<ResolvedCommand>;
    /// pi `getRegisteredCommands` (runner.ts:635).
    fn get_registered_commands(&self) -> Vec<ResolvedCommand>;
    /// pi `getAllRegisteredTools` (runner.ts:447).
    fn get_all_registered_tools(&self) -> Vec<RegisteredTool>;
    /// pi `getFlagValues` (runner.ts:486) — `Map<string, boolean | string>`.
    fn get_flag_values(&self) -> BTreeMap<String, FlagValue>;
    /// pi `createCommandContext` (runner.ts:736).
    fn create_command_context(&self) -> Box<dyn CommandContext>;

    // ---- binding / mutation ----------------------------------------------
    /// pi `bindCore(actions, contextActions, providerActions?)` (runner.ts:311).
    fn bind_core(
        &self,
        actions: Arc<dyn SessionControlHost>,
        context_actions: Arc<dyn SessionContextHost>,
        provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    );
    /// pi `setUIContext(uiContext?, mode = "print")` (runner.ts:429).
    fn set_ui_context(&self, ui_context: Option<ExtensionUIContext>, mode: ExtensionMode);
    /// pi `bindCommandContext(actions?)` (runner.ts:410).
    fn bind_command_context(&self, actions: Option<Arc<dyn ExtensionCommandContextHost>>);
    /// pi `onError(listener): () => void` (runner.ts:555).
    fn on_error(&self, listener: ExtensionErrorListener) -> UnsubscribeFn;
    /// pi `emitError(error)` (runner.ts:559).
    fn emit_error(&self, error: ExtensionError);
    /// pi `invalidate(message)` (runner.ts:539).
    fn invalidate(&self, message: &str);
}

// ---------------------------------------------------------------------------
// StubExtensionRunner (always compiled, no-op)
// ---------------------------------------------------------------------------

/// No-op runner for default (V8-free) builds — the runner-seam analog of
/// [`StubExtensionLoader`](super::loader::StubExtensionLoader). Every emit is
/// inert, every query empty. Injected as the default until the extension-plane
/// host's real impl is provided.
#[derive(Debug, Clone, Default)]
pub struct StubExtensionRunner;

impl ExtensionRunner for StubExtensionRunner {
    fn emit_session_shutdown(&self, _event: SessionShutdownEvent) {}

    fn emit(&self, _event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        ExtensionEmitOutcome::None
    }

    fn emit_message_end(&self, _event: &MessageEndEvent) -> Option<AgentMessage> {
        None
    }

    fn emit_input(
        &self,
        _text: &str,
        _images: Option<&[ImageContent]>,
        _source: InputSource,
        _streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult {
        InputEventResult::Continue
    }

    fn emit_before_agent_start(
        &self,
        _prompt: &str,
        _images: Option<&[ImageContent]>,
        _system_prompt: &str,
        _system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult> {
        None
    }

    fn emit_resources_discover(
        &self,
        _cwd: &str,
        _reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        ResourcesDiscoverResult::default()
    }

    fn emit_tool_call(&self, _event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        None
    }

    fn emit_tool_result(&self, _event: &ToolResultEvent) -> Option<ToolResultEventResult> {
        None
    }

    fn has_handlers(&self, _event_type: &str) -> bool {
        false
    }

    fn get_command(&self, _name: &str) -> Option<ResolvedCommand> {
        None
    }

    fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        Vec::new()
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        Vec::new()
    }

    fn get_flag_values(&self) -> BTreeMap<String, FlagValue> {
        BTreeMap::new()
    }

    fn create_command_context(&self) -> Box<dyn CommandContext> {
        Box::new(StubCommandContext)
    }

    fn bind_core(
        &self,
        _actions: Arc<dyn SessionControlHost>,
        _context_actions: Arc<dyn SessionContextHost>,
        _provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    ) {
    }

    fn set_ui_context(&self, _ui_context: Option<ExtensionUIContext>, _mode: ExtensionMode) {}

    fn bind_command_context(&self, _actions: Option<Arc<dyn ExtensionCommandContextHost>>) {}

    fn on_error(&self, _listener: ExtensionErrorListener) -> UnsubscribeFn {
        Box::new(|| {})
    }

    fn emit_error(&self, _error: ExtensionError) {}

    fn invalidate(&self, _message: &str) {}
}

/// The trivial [`CommandContext`] the [`StubExtensionRunner`] mints from
/// [`ExtensionRunner::create_command_context`]. A unit impl of
/// [`ExtensionContext`] + [`CommandContext`] with no state.
struct StubCommandContext;

impl ExtensionContext for StubCommandContext {}
impl CommandContext for StubCommandContext {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::events::session::SessionShutdownReason;
    use crate::core::extensions::events::turn::MessageEndEvent;

    fn shutdown_event() -> SessionShutdownEvent {
        SessionShutdownEvent {
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        }
    }

    #[test]
    fn stub_emitters_are_inert() {
        let runner = StubExtensionRunner;

        runner.emit_session_shutdown(shutdown_event());

        assert!(matches!(
            runner.emit(&ExtensionDispatchEvent::AgentStart(AgentStartEvent {})),
            ExtensionEmitOutcome::None
        ));

        let message_end = MessageEndEvent {
            message: Value::Null,
        };
        assert!(runner.emit_message_end(&message_end).is_none());

        assert_eq!(
            runner.emit_input("hi", None, InputSource::Interactive, None),
            InputEventResult::Continue
        );

        let opts = Value::Null;
        assert!(runner
            .emit_before_agent_start("prompt", None, "system", &opts)
            .is_none());

        assert_eq!(
            runner.emit_resources_discover("/cwd", ResourcesDiscoverReason::Startup),
            ResourcesDiscoverResult::default()
        );

        let call = ToolCallEvent {
            tool_call_id: "id".to_string(),
            tool_name: "bash".to_string(),
            input: Value::Null,
        };
        assert!(runner.emit_tool_call(&call).is_none());

        let result = ToolResultEvent {
            tool_call_id: "id".to_string(),
            tool_name: "bash".to_string(),
            input: Value::Null,
            content: Vec::new(),
            is_error: false,
            details: Value::Null,
        };
        assert!(runner.emit_tool_result(&result).is_none());
    }

    #[test]
    fn stub_queries_are_empty() {
        let runner = StubExtensionRunner;

        assert!(!runner.has_handlers("input"));
        assert!(runner.get_command("foo").is_none());
        assert!(runner.get_registered_commands().is_empty());
        assert!(runner.get_all_registered_tools().is_empty());
        assert!(runner.get_flag_values().is_empty());
        // A command context is produced (a valid `CommandContext` trait object).
        let _ctx: Box<dyn CommandContext> = runner.create_command_context();
    }

    #[test]
    fn stub_on_error_returns_a_callable_unsubscribe() {
        let runner = StubExtensionRunner;
        let listener: ExtensionErrorListener = Arc::new(|_err: &ExtensionError| {});
        let unsubscribe = runner.on_error(listener);
        // The unsubscribe closure is call-once and inert.
        unsubscribe();

        runner.emit_error(ExtensionError {
            extension_path: "/ext".to_string(),
            event: "input".to_string(),
            error: "boom".to_string(),
            stack: None,
        });
        runner.invalidate("stale");
    }

    #[test]
    fn stub_is_object_safe_through_a_box() {
        // Coerce to a trait object and drive every method through it, proving
        // object-safety and that the seam is usable as `Box<dyn ExtensionRunner>`.
        let runner: Box<dyn ExtensionRunner> = Box::new(StubExtensionRunner);

        runner.emit_session_shutdown(shutdown_event());
        assert!(matches!(
            runner.emit(&ExtensionDispatchEvent::AgentEnd(AgentEndEvent {
                messages: Vec::new(),
            })),
            ExtensionEmitOutcome::None
        ));
        assert_eq!(
            runner.emit_input("x", None, InputSource::Rpc, Some(StreamingBehavior::Steer)),
            InputEventResult::Continue
        );
        assert!(runner.get_registered_commands().is_empty());
        assert!(runner.get_flag_values().is_empty());

        // set_ui_context / bind_command_context accept their net-new types.
        runner.set_ui_context(Some(ExtensionUIContext::default()), ExtensionMode::Print);
        runner.bind_command_context(None);
        assert_eq!(ExtensionMode::default(), ExtensionMode::Print);
    }

    #[test]
    fn flag_value_variants_are_distinct() {
        assert_ne!(FlagValue::Bool(true), FlagValue::Str("true".to_string()));
        assert_eq!(FlagValue::Bool(true), FlagValue::Bool(true));
    }
}
