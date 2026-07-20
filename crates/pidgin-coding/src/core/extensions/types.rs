//! Extension type surface ported from
//! `packages/coding-agent/src/core/extensions/types.ts`.
//!
//! Only the two types on the exec-tools tool-registry critical path are ported
//! here: [`ToolDefinition`] (pi's `ToolDefinition`, `types.ts:439`) and
//! [`ExtensionContext`] (pi's `ExtensionContext`, `types.ts:304`). The extension
//! engine/loader and the remaining extension types are deferred to a later
//! thread.
//!
//! # Reuse of the agent-loop boundary
//!
//! pi's `ToolDefinition` is a superset of the agent runtime's `AgentTool`: same
//! metadata and the same `execute`/`prepareArguments` behavior, plus UI-render
//! hooks and an [`ExtensionContext`] threaded into `execute`. The port therefore
//! reuses [`pidgin_agent::types`] wholesale — [`AgentToolResult`],
//! [`AgentToolUpdateCallback`], and [`ToolExecutionMode`] — rather than
//! re-porting them, and mirrors [`pidgin_agent::types::AgentTool`]'s field and
//! closure shapes exactly, with the single documented addition of the `ctx`
//! argument on [`execute`](ToolDefinition::execute).
//!
//! # Render seam
//!
//! `renderCall` and `renderResult` (`types.ts:477`/`types.ts:480`) are ported as
//! optional stored closures ([`ToolRenderCall`]/[`ToolRenderResult`]) alongside
//! the borrow-struct [`ToolRenderContext`] (pi's `ToolRenderContext`,
//! `types.ts:409`) and [`ToolRenderResultOptions`] (`types.ts:401`). Only the
//! stateless subset of pi's `ToolRenderContext` is modelled — the fields the
//! renderers *read* (`args`, `cwd`, `expanded`, `isPartial`, `isError`,
//! `showImages`, `argsComplete`, `executionStarted`); the stateful redraw hooks
//! (`invalidate`, `toolCallId`, `state`, `lastComponent`) are intentionally
//! omitted, matching the stateless render approach. pi-ai's base `Tool` is not
//! ported either, so its `name`/`description`/`parameters` fields are inlined
//! here (as pi's interface flattens them).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_agent::types::{AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode};
use pidgin_ai::seams::AbortSignal;
use pidgin_tui::keybindings::KeybindingsManager;
use pidgin_tui::renderer::Component;

use crate::modes::interactive::theme::runtime::Theme;

// ---------------------------------------------------------------------------
// Extension context (`types.ts:304`)
// ---------------------------------------------------------------------------

/// Context passed to extension event handlers and tool `execute` (pi's
/// `ExtensionContext`, `types.ts:304`).
///
/// pi's interface exposes ~17 capability members (`ui`, `mode`, `cwd`,
/// `sessionManager`, `modelRegistry`, `abort()`, `compact()`, …), all of which
/// drag in large subsystems that are not yet ported. The tool-registry wrapper
/// on the exec-tools critical path reads **none** of them: it only needs a value
/// it can construct via a `ctxFactory` and hand to
/// [`ToolDefinition::execute`]. The port therefore models `ExtensionContext` as
/// an opaque marker trait — any host type may implement it — and defers the
/// capability surface to the full extension port.
///
/// # `ctx.ui` slice
///
/// The one capability member ported so far is [`ui`](ExtensionContext::ui), pi's
/// `ctx.ui`, narrowed to the **detached-custom + notify** subset needed by the
/// `/llama` mount seam (pi's `ctx.ui.custom(...)` / `ctx.ui.notify(...)`,
/// `extensions/llama/ui.ts:480`, `extensions/llama/index.ts:174`). It is a
/// **defaulted** method returning a no-op surface ([`NoopExtensionUi`]), so every
/// existing `impl ExtensionContext` (the tool-registry stubs plus the Python /
/// Deno command contexts) compiles unchanged — only a host that actually mounts
/// a TUI overlay overrides it. The extension-plane owner signed off on this
/// shared trait shape.
pub trait ExtensionContext {
    /// pi's `ctx.ui`, narrowed to the detached-custom + notify subset.
    ///
    /// DEFAULT = no-op surface, so every existing impl/bound compiles unchanged.
    /// A TUI host overrides this to return its live overlay-mounting surface.
    fn ui(&self) -> &dyn ExtensionUi {
        static NOOP: NoopExtensionUi = NoopExtensionUi;
        &NOOP
    }
}

// ---------------------------------------------------------------------------
// ctx.ui surface (detached-custom + notify slice of pi's `ExtensionUi`)
// ---------------------------------------------------------------------------

/// The narrowed `ctx.ui` surface: the two members the llama mount seam drives
/// (pi's `ExtensionUi.custom` / `ExtensionUi.notify`).
///
/// # Detached-only scoping (do not widen)
///
/// [`custom`](ExtensionUi::custom) is deliberately **monomorphic**,
/// **void-returning**, and **run-to-completion** (`FnOnce`): it mounts a
/// Rust-native, built-in view detached from the caller and drives its `run`
/// future to `done()`. pi's TypeScript `ctx.ui.custom<T>(...)` is generic and can
/// return a value to a re-entrant JS-authored view; that reentrant case stays
/// **parked / unrepresentable** here on purpose — modelling it would require a
/// `<T>` and a returned value the sync Rust shell cannot faithfully drive. Keep
/// `custom` scoped to detached Rust-native built-ins only.
pub trait ExtensionUi {
    /// pi's `ctx.ui.custom(factory)`: mount a detached custom view built by
    /// `factory` and drive its `run` future to completion. Returns
    /// [`UiError::Unavailable`] when no interactive surface is mounted (the
    /// no-op default), or [`UiError::Failed`] when the view's `run` resolves with
    /// an error.
    fn custom(&self, factory: CustomFactory<'_>) -> Result<(), UiError>;
    /// pi's `ctx.ui.notify(message, level)`: surface a transient notification.
    fn notify(&self, message: &str, level: NotifyLevel);
}

/// The one-shot builder handed to [`ExtensionUi::custom`] (pi's
/// `(tui, theme, keybindings, done) => Component` custom-factory callback,
/// re-shaped as a `FnOnce` over a [`CustomHost`]). Called exactly once by the
/// host with a live [`CustomHost`]; it returns the mounted [`CustomMount`].
pub type CustomFactory<'f> = Box<dyn FnOnce(&dyn CustomHost) -> CustomMount + 'f>;

/// The mounted view produced by a [`CustomFactory`]: the renderable component and
/// the future that drives it (pi's returned `Component` plus the `run(...).then`
/// completion promise, unified into one value).
pub struct CustomMount {
    /// The component the host mounts as a focused overlay.
    pub component: Rc<dyn Component>,
    /// The driver future the host polls to completion. `Ok(())` means the view
    /// finished normally (pi's `done()`); `Err(msg)` means it failed and the host
    /// should `notify` the message at [`NotifyLevel::Error`] then unmount (pi's
    /// `notify(error); done()`).
    pub run: Pin<Box<dyn Future<Output = Result<(), String>>>>,
}

/// The live surface a [`CustomFactory`] reads while constructing its view (pi's
/// `tui` / `theme` / `keybindings` custom-factory arguments).
///
/// Additively extensible: accessors are added here as views need them (the
/// [`LlamaView`](crate::extensions::llama::LlamaView) ctor needs
/// theme/keybindings/request-render). Extending `CustomHost` never changes the
/// [`ExtensionUi`] / [`ExtensionContext`] signatures.
pub trait CustomHost {
    /// The active theme (pi's `theme` factory argument).
    fn theme(&self) -> &Theme;
    /// The keybindings manager (pi's `keybindings` factory argument).
    fn keybindings(&self) -> &KeybindingsManager;
    /// Request a re-render of the mounted view (pi's `tui.requestRender()`).
    fn request_render(&self);
    /// Register the mounted view's keyboard-input sink.
    ///
    /// pi delivers input straight to the mounted component's `handleInput`. The
    /// Rust [`CustomMount::component`] is a shared `Rc<dyn Component>` used for
    /// rendering, and [`Component::handle_input`] takes `&mut self`, so a view
    /// with **interior-mutable** input (its `handle_input` takes `&self`)
    /// registers that `&self` input closure here. The host pumps each raw input
    /// chunk into it — via the overlay focus dispatch — so input reaches the view
    /// exactly as pi's event loop delivers it.
    fn set_input_handler(&self, handler: Rc<dyn Fn(&str)>);
}

/// The severity of an [`ExtensionUi::notify`] message (pi's notify level union
/// `"info" | "warning" | "error"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    /// An informational message (pi's default level).
    Info,
    /// A warning message.
    Warning,
    /// An error message.
    Error,
}

/// Why [`ExtensionUi::custom`] could not mount / complete.
#[derive(Debug)]
pub enum UiError {
    /// No interactive surface is available to mount on (the no-op default, and
    /// pi's non-TUI guard `ctx.mode !== "tui"`).
    Unavailable,
    /// The mounted view's `run` future resolved with an error message.
    Failed(String),
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiError::Unavailable => f.write_str("no interactive UI surface available"),
            UiError::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for UiError {}

/// The no-op [`ExtensionUi`] returned by [`ExtensionContext::ui`]'s default: no
/// surface to mount on, so `custom` reports [`UiError::Unavailable`] and `notify`
/// is a sink. Keeps every non-TUI `impl ExtensionContext` working unchanged.
pub struct NoopExtensionUi;

impl ExtensionUi for NoopExtensionUi {
    fn custom(&self, _factory: CustomFactory<'_>) -> Result<(), UiError> {
        Err(UiError::Unavailable)
    }
    fn notify(&self, _message: &str, _level: NotifyLevel) {}
}

// ---------------------------------------------------------------------------
// Render shell (`types.ts:456`)
// ---------------------------------------------------------------------------

/// Controls whether the tool-execution component renders the standard colored
/// shell or the tool renders its own framing (pi's `renderShell`,
/// `types.ts:456`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderShell {
    /// Render the standard colored shell around the tool result (`"default"`).
    Default,
    /// The tool renders its own framing (`"self"`).
    #[serde(rename = "self")]
    SelfRender,
}

// ---------------------------------------------------------------------------
// Tool definition closures (`types.ts:461`, `types.ts:472`)
// ---------------------------------------------------------------------------

/// Optional compatibility shim that rewrites raw tool-call arguments before
/// schema validation (pi's `ToolDefinition.prepareArguments`, `types.ts:461`).
///
/// pi types this `(args: unknown) => Static<TParams>`; the port keeps both sides
/// opaque as owned [`Value`]s. This is the by-value analog of
/// [`pidgin_agent::types::PrepareArguments`] (which borrows); the extension
/// registry takes ownership of the raw arguments, so the by-value shape matches
/// pi's call site.
pub type PrepareArguments = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Executes a tool call (pi's `ToolDefinition.execute`, `types.ts:472`).
///
/// This mirrors [`pidgin_agent::types::AgentToolExecute`] exactly — the
/// tool-call id, the validated arguments (`Static<TParams>` → [`Value`]), the
/// optional abort signal, and the optional update callback — with the **single**
/// addition of a fifth `ctx: &dyn ExtensionContext` argument. The tool-registry
/// wrapper injects `ctx` and drops it when adapting a [`ToolDefinition`] down to
/// an `AgentTool`. pi returns `Promise<AgentToolResult<TDetails>>`; the eager
/// port returns [`AgentToolResult`] directly.
pub type ToolDefinitionExecute = Arc<
    dyn Fn(
            &str,
            &Value,
            Option<&AbortSignal>,
            Option<&AgentToolUpdateCallback>,
            &dyn ExtensionContext,
        ) -> AgentToolResult
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Render seam (`types.ts:401`, `types.ts:409`, `types.ts:477`, `types.ts:480`)
// ---------------------------------------------------------------------------

/// Rendering options for tool results (pi's `ToolRenderResultOptions`,
/// `types.ts:401`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolRenderResultOptions {
    /// Whether the result view is expanded (pi's `expanded`).
    pub expanded: bool,
    /// Whether this is a partial/streaming result (pi's `isPartial`).
    pub is_partial: bool,
}

/// Context passed to tool renderers (pi's `ToolRenderContext`, `types.ts:409`).
///
/// A borrow-struct carrying only the **stateless** subset pi's renderers read.
/// pi's stateful redraw hooks (`invalidate`, `toolCallId`, `state`,
/// `lastComponent`) are intentionally omitted: the port renders each tool row as
/// a pure function of its inputs, so those fields have no analog here. Field
/// names mirror pi's, adapted to snake_case.
///
/// INTENTIONAL DEVIATION (no silent deviation): `ToolRenderContext` is a
/// faithful SUBSET of pi's `ToolRenderContext` (`extensions/types.ts:409`): it
/// omits `toolCallId`, `invalidate`, `lastComponent`, and `state`, and drops
/// pi's `TState`/`TArgs` generics. Verified against pi: none of the
/// edit/read/write/ls/grep/find renderers read those fields to determine
/// rendered OUTPUT — `lastComponent` is used only as a mutate-in-place redraw
/// optimization, and write's streaming highlight cache converges to a
/// `lastComponent`-independent full rebuild on the final (`argsComplete`)
/// frame. The one case that needs more is bash's `renderResult`, which reads
/// `state` (`startedAt`/`endedAt` → the "Elapsed/Took Xs" duration line) and
/// `invalidate` (1s live-timer repaint); that renderer is deferred (needs
/// visual-truncate + keybinding-hints + a deterministic clock), and when it's
/// built `ToolRenderContext` will gain concrete (non-generic)
/// `state`/`invalidate`-equivalent fields — a struct + construction-site
/// extension, NOT a closure-signature change, since the struct is passed by
/// reference.
pub struct ToolRenderContext<'a> {
    /// Current tool-call arguments, shared across call/result renders for the
    /// same tool call (pi's `args`).
    pub args: &'a Value,
    /// Working directory for this tool execution (pi's `cwd`).
    pub cwd: &'a str,
    /// Whether the tool execution has started (pi's `executionStarted`).
    pub execution_started: bool,
    /// Whether the tool-call arguments are complete (pi's `argsComplete`).
    pub args_complete: bool,
    /// Whether the tool result is partial/streaming (pi's `isPartial`).
    pub is_partial: bool,
    /// Whether the result view is expanded (pi's `expanded`).
    pub expanded: bool,
    /// Whether inline images are currently shown in the TUI (pi's `showImages`).
    pub show_images: bool,
    /// Whether the current result is an error (pi's `isError`).
    pub is_error: bool,
}

/// Custom rendering for a tool-call display (pi's `ToolDefinition.renderCall`,
/// `types.ts:477`).
///
/// Mirrors [`ToolDefinitionExecute`]'s `Arc<dyn Fn ... + Send + Sync>` trait
/// bound one-for-one, so attaching a renderer never changes
/// [`ToolDefinition`]'s auto-trait status. The `Send + Sync` binds the closure,
/// not the returned [`Component`], which may itself be `!Send`. pi passes
/// `(args, theme, context)`; the port matches, with `args` re-exposed on the
/// [`ToolRenderContext`] as pi does.
pub type ToolRenderCall =
    Arc<dyn Fn(&Value, &Theme, &ToolRenderContext) -> Box<dyn Component> + Send + Sync>;

/// Custom rendering for a tool-result display (pi's
/// `ToolDefinition.renderResult`, `types.ts:480`).
///
/// Mirrors [`ToolDefinitionExecute`]'s trait bound exactly (see
/// [`ToolRenderCall`]). pi passes `(result, options, theme, context)`; the port
/// takes the ported [`AgentToolResult`] as the result type.
pub type ToolRenderResult = Arc<
    dyn Fn(
            &AgentToolResult,
            &ToolRenderResultOptions,
            &Theme,
            &ToolRenderContext,
        ) -> Box<dyn Component>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Tool definition (`types.ts:439`)
// ---------------------------------------------------------------------------

/// Tool definition for `registerTool()` (pi's `ToolDefinition`, `types.ts:439`).
///
/// A superset of [`pidgin_agent::types::AgentTool`]: the same metadata and
/// behavior fields (mirrored one-for-one) plus prompt pass-through hints, an
/// [`ExtensionContext`]-aware [`execute`](Self::execute), and the optional
/// UI-render hooks ([`render_call`](Self::render_call)/[`render_result`](Self::render_result)).
///
/// Runtime-only (carries closures); not serde. Where the metadata fields would
/// cross pi's wire they use pi's camelCase names (`promptSnippet`,
/// `promptGuidelines`, `renderShell`, `executionMode`); [`RenderShell`] and
/// [`ToolExecutionMode`] carry their own serde renaming.
#[derive(Clone)]
pub struct ToolDefinition {
    /// Tool name used in LLM tool calls (inlined pi-ai `Tool.name`).
    pub name: String,
    /// Human-readable label for UI display.
    pub label: String,
    /// Description for the LLM (inlined pi-ai `Tool.description`).
    pub description: String,
    /// TypeBox `TSchema` parameter schema (inlined pi-ai `Tool.parameters`),
    /// kept opaque — never re-derived.
    pub parameters: Value,
    /// Per-tool execution-mode override; falls back to the loop default when
    /// `None` (pi's `executionMode`).
    pub execution_mode: Option<ToolExecutionMode>,
    /// Execute the tool call. See [`ToolDefinitionExecute`].
    pub execute: ToolDefinitionExecute,
    /// Optional shim rewriting raw tool-call arguments before schema validation
    /// (pi's `prepareArguments`).
    pub prepare_arguments: Option<PrepareArguments>,
    /// Optional one-line snippet for the "Available tools" section of the
    /// default system prompt (pi's `promptSnippet`).
    pub prompt_snippet: Option<String>,
    /// Optional guideline bullets appended to the system-prompt Guidelines
    /// section when this tool is active (pi's `promptGuidelines`).
    pub prompt_guidelines: Option<Vec<String>>,
    /// Whether the execution component renders the standard shell or the tool
    /// frames itself (pi's `renderShell`).
    pub render_shell: Option<RenderShell>,
    /// Optional custom rendering for the tool-call display (pi's `renderCall`).
    /// See [`ToolRenderCall`]. Stored but not invoked here — the TUI
    /// tool-execution component drives it.
    pub render_call: Option<ToolRenderCall>,
    /// Optional custom rendering for the tool-result display (pi's
    /// `renderResult`). See [`ToolRenderResult`].
    pub render_result: Option<ToolRenderResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::ContentBlock;
    use serde_json::json;

    /// Trivial [`ExtensionContext`] used to construct a [`ToolDefinition`].
    struct NoopCtx;
    impl ExtensionContext for NoopCtx {}

    #[test]
    fn render_shell_serde_round_trip() {
        assert_eq!(
            serde_json::to_value(RenderShell::Default).unwrap(),
            json!("default")
        );
        assert_eq!(
            serde_json::to_value(RenderShell::SelfRender).unwrap(),
            json!("self")
        );
        assert_eq!(
            serde_json::from_value::<RenderShell>(json!("default")).unwrap(),
            RenderShell::Default
        );
        assert_eq!(
            serde_json::from_value::<RenderShell>(json!("self")).unwrap(),
            RenderShell::SelfRender
        );
    }

    #[test]
    fn tool_definition_constructs_with_trivial_execute() {
        let tool = ToolDefinition {
            name: "read".into(),
            label: "Read".into(),
            description: "Read a file".into(),
            parameters: json!({ "type": "object" }),
            execution_mode: Some(ToolExecutionMode::Parallel),
            execute: Arc::new(|_id, _args, _signal, _on_update, _ctx| AgentToolResult {
                content: vec![ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                }],
                details: json!(null),
                added_tool_names: None,
                terminate: None,
            }),
            prepare_arguments: Some(Arc::new(|args| args)),
            prompt_snippet: Some("read <path>".into()),
            prompt_guidelines: Some(vec!["Prefer reading before editing.".into()]),
            render_shell: Some(RenderShell::Default),
            render_call: None,
            render_result: None,
        };

        // Metadata is plain data.
        assert_eq!(tool.name, "read");
        assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Parallel));

        // The execute closure runs with an ExtensionContext threaded through.
        let ctx = NoopCtx;
        let result = (tool.execute)("call_1", &json!({}), None, None, &ctx);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "content": [{ "type": "text", "text": "ok" }], "details": null })
        );

        // prepareArguments is an identity shim here.
        let prepared = (tool.prepare_arguments.as_ref().unwrap())(json!({ "path": "a" }));
        assert_eq!(prepared, json!({ "path": "a" }));
    }
}
