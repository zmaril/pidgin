//! `ToolExecution` — renders a tool call and its result.
//!
//! Port of pi's `modes/interactive/components/tool-execution.ts`
//! (`ToolExecutionComponent`), composing `atilla-tui`'s `Box`, `Text`, and
//! `Spacer`.
//!
//! ## Renderer-path scope (STOP-and-report boundary)
//!
//! pi delegates a tool's visible framing to the tool definition's `renderCall` /
//! `renderResult` closures. Those UI-render hooks are **deferred** on the Rust
//! [`ToolDefinition`](crate::core::extensions::types::ToolDefinition)
//! (`core/extensions/types.rs`: "UI-render hooks (`renderCall`/`renderResult`)
//! are deferred with the rest of the TUI layer"). No per-tool renderer, and no
//! `DiffComponent`, exists in Rust yet.
//!
//! This port therefore implements **exactly the branches pi takes when a tool
//! has no `renderCall` / `renderResult`**:
//! - no tool definition → the generic `formatToolExecution` text path;
//! - a tool definition without renderers → pi's `createCallFallback` /
//!   `createResultFallback` (the tool title + text output).
//!
//! Because those renderers are absent for *every* Rust tool definition (built-in
//! or extension), this component renders the fallback for every tool. Its output
//! is byte-identical to pi **only** for tools whose pi definition likewise lacks
//! `renderCall` / `renderResult`. Once the per-tool renderer suite +
//! `DiffComponent` + `ToolRenderContext` are ported, they slot into the branches
//! guarded here. Image content blocks (`getCapabilities().images`) are likewise
//! out of scope for this PR.

use atilla_tui::renderer::Component;
use atilla_tui::widgets::box_widget::BoxWidget;
use atilla_tui::widgets::text::BgFn;
use atilla_tui::{Spacer, Text};

use atilla_ai::types::ContentBlock;
use serde_json::Value;

use crate::core::extensions::types::{RenderShell, ToolDefinition};
use crate::core::tools::index::{create_all_tool_definitions, ToolsOptions};
use crate::core::tools::render_utils::get_text_output;
use crate::modes::interactive::theme::Theme;

/// Options controlling image rendering, mirroring pi's `ToolExecutionOptions`.
/// (Image rendering itself is out of scope for this PR; the fields are carried
/// for API parity and future use.)
#[derive(Clone, Copy, Debug)]
pub struct ToolExecutionOptions {
    /// pi's `showImages` (default `true`).
    pub show_images: bool,
    /// pi's `imageWidthCells` (default `60`).
    pub image_width_cells: usize,
}

impl Default for ToolExecutionOptions {
    fn default() -> Self {
        Self {
            show_images: true,
            image_width_cells: 60,
        }
    }
}

/// A tool result, mirroring the shape pi's `updateResult` consumes.
#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    /// Result content blocks (only text blocks are rendered in this PR).
    pub content: Vec<ContentBlock>,
    /// Whether the result is an error (selects the error background).
    pub is_error: bool,
    /// Opaque tool-specific details (unused by the fallback renderers).
    pub details: Value,
}

/// Component that renders a tool call and its result. Port of pi's
/// `ToolExecutionComponent` (non-renderer paths — see the module docs).
pub struct ToolExecution {
    theme: Theme,
    tool_name: String,
    tool_call_id: String,
    args: Value,
    #[allow(dead_code)]
    show_images: bool,
    #[allow(dead_code)]
    image_width_cells: usize,
    expanded: bool,
    is_partial: bool,
    execution_started: bool,
    args_complete: bool,
    result: Option<ToolExecutionResult>,
    has_definition: bool,
    render_shell: RenderShell,
}

impl ToolExecution {
    /// `new ToolExecutionComponent(toolName, toolCallId, args, options,
    /// toolDefinition, ui, cwd)`.
    ///
    /// `built_in_tool_definition` is resolved from `create_all_tool_definitions(cwd)`
    /// exactly as pi does; `has_definition` and `render_shell` follow pi's
    /// precedence. (The `ui`/`requestRender` seam is dropped — render is a pure
    /// function of state here.)
    pub fn new(
        tool_name: impl Into<String>,
        tool_call_id: impl Into<String>,
        args: Value,
        options: ToolExecutionOptions,
        tool_definition: Option<ToolDefinition>,
        cwd: &str,
        theme: Theme,
    ) -> Self {
        let tool_name = tool_name.into();
        let built_in_tool_definition = create_all_tool_definitions(cwd, ToolsOptions::default())
            .into_iter()
            .find(|(_, d)| d.name == tool_name)
            .map(|(_, d)| d);

        let has_definition = built_in_tool_definition.is_some() || tool_definition.is_some();
        let render_shell = resolve_render_shell(&built_in_tool_definition, &tool_definition);

        Self {
            theme,
            tool_name,
            tool_call_id: tool_call_id.into(),
            args,
            show_images: options.show_images,
            image_width_cells: options.image_width_cells,
            expanded: false,
            is_partial: true,
            execution_started: false,
            args_complete: false,
            result: None,
            has_definition,
            render_shell,
        }
    }

    /// The tool call id this component tracks (pi's `toolCallId`).
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    /// pi's `updateArgs(args)`.
    pub fn update_args(&mut self, args: Value) {
        self.args = args;
    }

    /// pi's `markExecutionStarted()`.
    pub fn mark_execution_started(&mut self) {
        self.execution_started = true;
    }

    /// pi's `setArgsComplete()`.
    pub fn set_args_complete(&mut self) {
        self.args_complete = true;
    }

    /// pi's `updateResult(result, isPartial=false)`.
    pub fn update_result(&mut self, result: ToolExecutionResult, is_partial: bool) {
        self.result = Some(result);
        self.is_partial = is_partial;
    }

    /// pi's `setExpanded(expanded)`.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// pi's `setShowImages(show)`.
    pub fn set_show_images(&mut self, show: bool) {
        self.show_images = show;
    }

    /// Whether execution has started (exposed for the shell's routing).
    pub fn execution_started(&self) -> bool {
        self.execution_started
    }

    /// Whether the arguments are complete (exposed for the shell's routing).
    pub fn args_complete(&self) -> bool {
        self.args_complete
    }

    /// Whether this component is expanded (exposed for the shell's routing).
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    /// The theme background color key for the current state, mirroring pi's
    /// `updateDisplay` bg selection: pending → `toolPendingBg`; error →
    /// `toolErrorBg`; success → `toolSuccessBg`.
    fn bg_color_key(&self) -> &'static str {
        if self.is_partial {
            "toolPendingBg"
        } else if self.result.as_ref().is_some_and(|r| r.is_error) {
            "toolErrorBg"
        } else {
            "toolSuccessBg"
        }
    }

    /// An owned background closure for `bg_color_key`, reproducing `theme.bg`.
    fn bg_fn(&self) -> BgFn {
        let theme = self.theme.clone();
        let key = self.bg_color_key().to_string();
        Box::new(move |text: &str| theme.bg(&key, text).unwrap_or_else(|_| text.to_string()))
    }

    /// Wrap `text` in the theme's foreground color for `color` (pi's `theme.fg`).
    fn fg(&self, color: &str, text: &str) -> String {
        self.theme
            .fg(color, text)
            .unwrap_or_else(|_| text.to_string())
    }

    /// pi's `createCallFallback`: `Text(theme.fg("toolTitle", theme.bold(name)))`.
    fn call_fallback(&self) -> Box<dyn Component> {
        let styled = self.fg("toolTitle", &self.theme.bold(&self.tool_name));
        Box::new(Text::new(&styled, 0, 0, None))
    }

    /// pi's `createResultFallback`: `Text(theme.fg("toolOutput", output))` when
    /// the text output is non-empty, else nothing.
    fn result_fallback(&self) -> Option<Box<dyn Component>> {
        let output = self.text_output();
        if output.is_empty() {
            return None;
        }
        let styled = self.fg("toolOutput", &output);
        Some(Box::new(Text::new(&styled, 0, 0, None)))
    }

    /// pi's `getTextOutput()` for text-only results: each text block is
    /// stripped/sanitized/CR-normalized (the ported [`get_text_output`]) and the
    /// blocks are joined with `\n`. (Image indicators are out of scope.)
    fn text_output(&self) -> String {
        let Some(result) = &self.result else {
            return String::new();
        };
        result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(get_text_output(text)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// pi's `formatToolExecution()` — the generic no-definition text body.
    fn format_tool_execution(&self) -> String {
        let mut text = self.fg("toolTitle", &self.theme.bold(&self.tool_name));
        // JSON.stringify(args, null, 2). serde_json pretty uses the same 2-space
        // indentation and `": "` separators; key order follows the parsed value.
        let content = serde_json::to_string_pretty(&self.args).unwrap_or_default();
        if !content.is_empty() {
            text.push_str(&format!("\n\n{content}"));
        }
        let output = self.text_output();
        if !output.is_empty() {
            text.push_str(&format!("\n{output}"));
        }
        text
    }
}

/// pi's `getRenderShell()` precedence over the (optional) built-in and extension
/// tool definitions.
fn resolve_render_shell(
    built_in: &Option<ToolDefinition>,
    tool_definition: &Option<ToolDefinition>,
) -> RenderShell {
    match (built_in, tool_definition) {
        (None, td) => td
            .as_ref()
            .and_then(|d| d.render_shell)
            .unwrap_or(RenderShell::Default),
        (Some(b), None) => b.render_shell.unwrap_or(RenderShell::Default),
        (Some(b), Some(td)) => td
            .render_shell
            .or(b.render_shell)
            .unwrap_or(RenderShell::Default),
    }
}

impl Component for ToolExecution {
    fn render(&self, width: usize) -> Vec<String> {
        if self.has_definition && self.render_shell == RenderShell::SelfRender {
            // Self-render shell: a plain Container (no background) of the call +
            // result fallbacks; render prepends a single blank line.
            let mut content_lines: Vec<String> = Vec::new();
            content_lines.extend(self.call_fallback().render(width));
            if self.result.is_some() {
                if let Some(rf) = self.result_fallback() {
                    content_lines.extend(rf.render(width));
                }
            }
            if content_lines.is_empty() {
                return Vec::new();
            }
            let mut lines = vec![String::new()];
            lines.extend(content_lines);
            return lines;
        }

        // Default shell / generic fallback: super.render == Spacer(1) then the
        // content component.
        let mut lines = Spacer::new(1).render(width);
        if self.has_definition {
            // Default shell: a background Box holding the call (+ result) fallback.
            let mut content_box = BoxWidget::new(1, 1, Some(self.bg_fn()));
            content_box.add_child(self.call_fallback());
            if self.result.is_some() {
                if let Some(rf) = self.result_fallback() {
                    content_box.add_child(rf);
                }
            }
            lines.extend(content_box.render(width));
        } else {
            // No definition: a single background Text of formatToolExecution.
            let mut content_text = Text::new("", 1, 1, Some(self.bg_fn()));
            content_text.set_text(&self.format_tool_execution());
            lines.extend(content_text.render(width));
        }
        lines
    }
}
