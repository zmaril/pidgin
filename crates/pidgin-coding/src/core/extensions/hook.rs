//! Hook contract: the event-name enum, the middleware outcome, thread affinity,
//! and the [`Hook`] trait.
//!
//! This is the internal contract sketched in `notes/startup/extensibility.md` §5
//! and `notes/startup/deep-hooks.md`: the Rust successor to pi's
//! `pi.on(event, handler)` hook surface. pi routes ~35 lifecycle hooks through
//! `ExtensionRunner.emit*` (`runner.ts`); many are not observers —  `tool_call`
//! is the permission gate (block / mutate `event.input`), `tool_result` patches
//! a result, `before_provider_request` replaces the payload, `context` rewrites
//! the message array. [`HookOutcome`] models those middleware shapes uniformly.
//!
//! # Async lowered to eager
//!
//! pi's handlers are `async`. Following the established convention of the
//! existing [`super::types::ToolDefinition`] port — which lowers pi's
//! `Promise<AgentToolResult>` `execute` to an eager synchronous closure — the
//! [`Hook::handle`] method here is synchronous and eager. The two-flavor async
//! dispatch machinery (trampoline vs. rendezvous, per deep-hooks.md §4) is not
//! built in this PR; it lands with the `ExtensionRunner` port. Keeping the trait
//! synchronous avoids pulling an async-trait dependency into a types-and-traits
//! PR while still capturing the contract shape.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/{types,runner}.ts`.

use serde::{Deserialize, Serialize};

use super::events::ExtensionEvent;
use super::types::ExtensionContext;

/// The name of a hook event a [`Hook`] subscribes to (pi's `on(event, …)`
/// overload set, `types.ts:1172`).
///
/// One variant per distinct pi event `type` — the 33 hook events. The serde
/// representation is pi's snake_case event-name string, so a [`HookEvent`]
/// round-trips through the same wire token that keys [`ExtensionEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// `project_trust` — resolve project-directory trust (has a result).
    ProjectTrust,
    /// `resources_discover` — contribute resource paths (has a result).
    ResourcesDiscover,
    /// `session_start` — a session started, loaded, or reloaded.
    SessionStart,
    /// `session_info_changed` — session metadata changed.
    SessionInfoChanged,
    /// `session_before_switch` — before switching sessions (cancellable).
    SessionBeforeSwitch,
    /// `session_before_fork` — before forking a session (cancellable).
    SessionBeforeFork,
    /// `session_before_compact` — before compaction (cancellable/customizable).
    SessionBeforeCompact,
    /// `session_compact` — after compaction.
    SessionCompact,
    /// `session_shutdown` — extension runtime is being torn down.
    SessionShutdown,
    /// `session_before_tree` — before session-tree navigation (cancellable).
    SessionBeforeTree,
    /// `session_tree` — after session-tree navigation.
    SessionTree,
    /// `context` — before each LLM call; can rewrite messages.
    Context,
    /// `before_provider_request` — can replace the outgoing payload.
    BeforeProviderRequest,
    /// `before_provider_headers` — can mutate request headers.
    BeforeProviderHeaders,
    /// `after_provider_response` — after a provider response is received.
    AfterProviderResponse,
    /// `before_agent_start` — after prompt submit, before the loop; can rewrite
    /// the system prompt.
    BeforeAgentStart,
    /// `agent_start` — an agent loop started.
    AgentStart,
    /// `agent_end` — an agent loop ended.
    AgentEnd,
    /// `agent_settled` — the run fully settled.
    AgentSettled,
    /// `turn_start` — a turn started.
    TurnStart,
    /// `turn_end` — a turn ended.
    TurnEnd,
    /// `message_start` — a message started.
    MessageStart,
    /// `message_update` — assistant-message streaming update.
    MessageUpdate,
    /// `message_end` — a message ended; can replace it.
    MessageEnd,
    /// `tool_execution_start` — a tool started executing.
    ToolExecutionStart,
    /// `tool_execution_update` — partial tool output.
    ToolExecutionUpdate,
    /// `tool_execution_end` — a tool finished executing.
    ToolExecutionEnd,
    /// `model_select` — a model was selected.
    ModelSelect,
    /// `thinking_level_select` — a thinking level was selected.
    ThinkingLevelSelect,
    /// `tool_call` — before a tool executes; the permission gate.
    ToolCall,
    /// `tool_result` — after a tool executes; can patch the result.
    ToolResult,
    /// `user_bash` — a user `!`/`!!` bash command.
    UserBash,
    /// `input` — user input received; can transform it.
    Input,
}

impl HookEvent {
    /// Every hook event, in pi's `ExtensionEvent`-union order.
    pub const ALL: [HookEvent; 33] = [
        HookEvent::ProjectTrust,
        HookEvent::ResourcesDiscover,
        HookEvent::SessionStart,
        HookEvent::SessionInfoChanged,
        HookEvent::SessionBeforeSwitch,
        HookEvent::SessionBeforeFork,
        HookEvent::SessionBeforeCompact,
        HookEvent::SessionCompact,
        HookEvent::SessionShutdown,
        HookEvent::SessionBeforeTree,
        HookEvent::SessionTree,
        HookEvent::Context,
        HookEvent::BeforeProviderRequest,
        HookEvent::BeforeProviderHeaders,
        HookEvent::AfterProviderResponse,
        HookEvent::BeforeAgentStart,
        HookEvent::AgentStart,
        HookEvent::AgentEnd,
        HookEvent::AgentSettled,
        HookEvent::TurnStart,
        HookEvent::TurnEnd,
        HookEvent::MessageStart,
        HookEvent::MessageUpdate,
        HookEvent::MessageEnd,
        HookEvent::ToolExecutionStart,
        HookEvent::ToolExecutionUpdate,
        HookEvent::ToolExecutionEnd,
        HookEvent::ModelSelect,
        HookEvent::ThinkingLevelSelect,
        HookEvent::ToolCall,
        HookEvent::ToolResult,
        HookEvent::UserBash,
        HookEvent::Input,
    ];

    /// pi's snake_case event-name string for this hook event.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::ProjectTrust => "project_trust",
            HookEvent::ResourcesDiscover => "resources_discover",
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionInfoChanged => "session_info_changed",
            HookEvent::SessionBeforeSwitch => "session_before_switch",
            HookEvent::SessionBeforeFork => "session_before_fork",
            HookEvent::SessionBeforeCompact => "session_before_compact",
            HookEvent::SessionCompact => "session_compact",
            HookEvent::SessionShutdown => "session_shutdown",
            HookEvent::SessionBeforeTree => "session_before_tree",
            HookEvent::SessionTree => "session_tree",
            HookEvent::Context => "context",
            HookEvent::BeforeProviderRequest => "before_provider_request",
            HookEvent::BeforeProviderHeaders => "before_provider_headers",
            HookEvent::AfterProviderResponse => "after_provider_response",
            HookEvent::BeforeAgentStart => "before_agent_start",
            HookEvent::AgentStart => "agent_start",
            HookEvent::AgentEnd => "agent_end",
            HookEvent::AgentSettled => "agent_settled",
            HookEvent::TurnStart => "turn_start",
            HookEvent::TurnEnd => "turn_end",
            HookEvent::MessageStart => "message_start",
            HookEvent::MessageUpdate => "message_update",
            HookEvent::MessageEnd => "message_end",
            HookEvent::ToolExecutionStart => "tool_execution_start",
            HookEvent::ToolExecutionUpdate => "tool_execution_update",
            HookEvent::ToolExecutionEnd => "tool_execution_end",
            HookEvent::ModelSelect => "model_select",
            HookEvent::ThinkingLevelSelect => "thinking_level_select",
            HookEvent::ToolCall => "tool_call",
            HookEvent::ToolResult => "tool_result",
            HookEvent::UserBash => "user_bash",
            HookEvent::Input => "input",
        }
    }

    /// Whether this hook carries a middleware result that can block, modify, or
    /// replace (routed through a dedicated `emitXxx` in pi's `runner.ts`), as
    /// opposed to an advisory observer. Advisory hooks fail open; middleware
    /// hooks — notably [`HookEvent::ToolCall`] (the permission gate) — fail
    /// closed. See [`FailurePolicy`].
    pub fn is_middleware(self) -> bool {
        matches!(
            self,
            HookEvent::ProjectTrust
                | HookEvent::ResourcesDiscover
                | HookEvent::SessionBeforeSwitch
                | HookEvent::SessionBeforeFork
                | HookEvent::SessionBeforeCompact
                | HookEvent::SessionBeforeTree
                | HookEvent::Context
                | HookEvent::BeforeProviderRequest
                | HookEvent::BeforeAgentStart
                | HookEvent::MessageEnd
                | HookEvent::ToolCall
                | HookEvent::ToolResult
                | HookEvent::UserBash
                | HookEvent::Input
        )
    }
}

/// The result of running a hook (the design's `HookOutcome`,
/// `extensibility.md` §5 / `deep-hooks.md` §1).
///
/// This is pidgin's uniform successor to pi's per-event result types
/// (`ToolCallEventResult`, `ContextEventResult`, `BeforeProviderRequestEventResult`,
/// …). The core applies the outcome to the real `&mut` event on the Rust side:
/// a [`HookOutcome::Modify`] writes its value back into the event, a
/// [`HookOutcome::Block`] short-circuits the loop.
#[derive(Debug, Clone, PartialEq)]
pub enum HookOutcome {
    /// Pure observation — leave the event unchanged (advisory hooks; a
    /// `tool_call` handler that neither blocks nor mutates).
    Continue,
    /// Mutate the event in place with the supplied value — e.g. `tool_call`
    /// patching `event.input`.
    Modify(serde_json::Value),
    /// Replace the event's payload wholesale — e.g. `before_provider_request`
    /// swapping the outgoing payload.
    Replace(serde_json::Value),
    /// Block the gated operation — the `tool_call` permission gate returning
    /// `{ block: true, reason }`.
    Block {
        /// A human-readable reason for the block.
        reason: String,
    },
}

impl HookOutcome {
    /// Whether this outcome blocks the gated operation.
    pub fn is_block(&self) -> bool {
        matches!(self, HookOutcome::Block { .. })
    }

    /// Whether this outcome leaves the event unchanged.
    pub fn is_continue(&self) -> bool {
        matches!(self, HookOutcome::Continue)
    }
}

/// The fail policy applied when a hook times out or panics (`deep-hooks.md` §4,
/// §7). The permission gate is fail-closed; advisory hooks are fail-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    /// On failure, continue as if the hook returned [`HookOutcome::Continue`].
    FailOpen,
    /// On failure, block the gated operation.
    FailClosed,
}

impl HookEvent {
    /// The [`FailurePolicy`] for this hook. Only the [`HookEvent::ToolCall`]
    /// permission gate fails closed; every other hook fails open.
    pub fn failure_policy(self) -> FailurePolicy {
        match self {
            HookEvent::ToolCall => FailurePolicy::FailClosed,
            _ => FailurePolicy::FailOpen,
        }
    }
}

/// Which thread an extension is allowed to run on (`extensibility.md` §5,
/// `deep-hooks.md` §4–§5).
///
/// The core scheduler consults this to pick a dispatch flavor: [`AnyThread`] is
/// trampolined under the host lock (Python under the GIL), [`HostThreadOnly`]
/// pins the extension to the host's owning thread (Node via a threadsafe
/// function, PHP/Ruby via a rendezvous pump), and [`OwnRuntime`] routes the
/// embedded `deno_core` JS plane on its own thread. Types only — no dispatch is
/// wired in this PR.
///
/// [`AnyThread`]: Affinity::AnyThread
/// [`HostThreadOnly`]: Affinity::HostThreadOnly
/// [`OwnRuntime`]: Affinity::OwnRuntime
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    /// A worker thread may call the extension under the host lock (Python;
    /// trampoline).
    AnyThread,
    /// Pinned to the host's owning thread (Node, PHP, Ruby).
    HostThreadOnly,
    /// The embedded `deno_core` JS plane, on the runtime's own thread.
    OwnRuntime,
}

/// A registered hook (the design's `Hook` trait, `extensibility.md` §5).
///
/// Every extension mechanism — the embedded JS plane and each host-language
/// binding — lowers its event handlers onto this trait so the core dispatch is
/// uniform. [`handle`](Hook::handle) receives the concrete event payload by
/// mutable reference (the core owns the real event and applies the returned
/// [`HookOutcome`] to it) plus the [`ExtensionContext`] pi threads into every
/// handler.
///
/// The method is synchronous and eager (see the module docs); the actual async,
/// affinity-aware dispatch lands with the `ExtensionRunner` port.
pub trait Hook: Send + Sync {
    /// The event this hook subscribes to.
    fn event(&self) -> HookEvent;

    /// Run the hook against a mutable event payload, returning its outcome.
    fn handle(&self, event: &mut ExtensionEvent, ctx: &dyn ExtensionContext) -> HookOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::events::{ProjectTrustEvent, ToolCallEvent};
    use serde_json::json;

    struct NoopCtx;
    impl ExtensionContext for NoopCtx {}

    /// A blocking `tool_call` gate, exercising the [`Hook`] trait end to end.
    struct BlockBashHook;
    impl Hook for BlockBashHook {
        fn event(&self) -> HookEvent {
            HookEvent::ToolCall
        }
        fn handle(&self, event: &mut ExtensionEvent, _ctx: &dyn ExtensionContext) -> HookOutcome {
            match event {
                ExtensionEvent::ToolCall(ToolCallEvent { tool_name, .. })
                    if tool_name == "bash" =>
                {
                    HookOutcome::Block {
                        reason: "bash disabled".into(),
                    }
                }
                _ => HookOutcome::Continue,
            }
        }
    }

    #[test]
    fn hook_event_names_are_unique_and_snake_case() {
        use std::collections::BTreeSet;
        let names: BTreeSet<&str> = HookEvent::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(names.len(), 33, "event names must be unique");
        for event in HookEvent::ALL {
            // serde name matches the as_str token.
            let wire = serde_json::to_value(event).unwrap();
            assert_eq!(wire, json!(event.as_str()));
            let restored: HookEvent = serde_json::from_value(wire).unwrap();
            assert_eq!(restored, event);
        }
    }

    #[test]
    fn tool_call_is_the_only_fail_closed_hook() {
        for event in HookEvent::ALL {
            let expected = if event == HookEvent::ToolCall {
                FailurePolicy::FailClosed
            } else {
                FailurePolicy::FailOpen
            };
            assert_eq!(event.failure_policy(), expected, "{event:?}");
        }
    }

    #[test]
    fn hook_outcome_variants() {
        assert!(HookOutcome::Continue.is_continue());
        assert!(!HookOutcome::Continue.is_block());
        assert!(HookOutcome::Block {
            reason: "no".into()
        }
        .is_block());
        assert_eq!(
            HookOutcome::Modify(json!({ "a": 1 })),
            HookOutcome::Modify(json!({ "a": 1 })),
        );
        assert_ne!(
            HookOutcome::Modify(json!(1)),
            HookOutcome::Replace(json!(1)),
        );
    }

    #[test]
    fn hook_trait_dispatches_over_event_payload() {
        let hook = BlockBashHook;
        assert_eq!(hook.event(), HookEvent::ToolCall);

        let mut bash = ExtensionEvent::ToolCall(ToolCallEvent {
            tool_call_id: "tc1".into(),
            tool_name: "bash".into(),
            input: json!({ "command": "rm -rf /" }),
        });
        assert_eq!(
            hook.handle(&mut bash, &NoopCtx),
            HookOutcome::Block {
                reason: "bash disabled".into()
            },
        );

        let mut trust = ExtensionEvent::ProjectTrust(ProjectTrustEvent {
            cwd: "/repo".into(),
        });
        assert_eq!(hook.handle(&mut trust, &NoopCtx), HookOutcome::Continue);
    }

    #[test]
    fn affinity_variants_are_distinct() {
        assert_ne!(Affinity::AnyThread, Affinity::HostThreadOnly);
        assert_ne!(Affinity::HostThreadOnly, Affinity::OwnRuntime);
    }
}
