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

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_agent::types::{AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode};
use pidgin_ai::seams::AbortSignal;
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
pub trait ExtensionContext {}

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
