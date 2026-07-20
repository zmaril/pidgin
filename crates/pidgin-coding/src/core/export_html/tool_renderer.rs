// straitjacket-allow-file:color — test fixtures reproduce pi's ANSI color output verbatim.
//! Tool HTML rendering support for custom tools in the HTML export.
//!
//! Ported from pi's `core/export-html/tool-renderer.ts`. This module provides the
//! [`ToolHtmlRenderer`] trait, the blank-line trimming helpers, the set of tools
//! the HTML template renders itself, and [`pre_render_custom_tools`], which walks
//! session entries and asks an injected renderer to produce HTML for custom tools.
//!
//! pi's concrete `createToolHtmlRenderer` factory is intentionally NOT ported: it
//! drives pi-tui `Component`s at a fixed terminal width and converts their ANSI
//! output through [`ansi_lines_to_html`](super::ansi_to_html::ansi_lines_to_html).
//! That depends on the pi-tui port (pidgin-tui), which a sibling owns and which is
//! not yet on main. The trait here lets a caller inject a concrete renderer once it
//! exists; the trimming helpers it will rely on are ported and tested below.

use super::ansi_to_html::ansi_escape_end;
use super::types::{AgentMessage, ContentBlock, RenderedToolHtml, SessionEntry};
use serde_json::Value;
use std::collections::HashMap;

/// Tools rendered directly by the HTML template (not pre-rendered via the
/// TUI to ANSI to HTML pipeline). Mirrors pi's `TEMPLATE_RENDERED_TOOLS`.
pub const TEMPLATE_RENDERED_TOOLS: [&str; 5] = ["bash", "read", "write", "edit", "ls"];

fn is_template_rendered(name: &str) -> bool {
    TEMPLATE_RENDERED_TOOLS.contains(&name)
}

/// Collapsed/expanded HTML produced by a tool result renderer. Mirrors the
/// `{ collapsed?, expanded? }` return of pi's `renderResult`.
#[derive(Clone, Debug, Default)]
pub struct RenderedToolResult {
    pub collapsed: Option<String>,
    pub expanded: Option<String>,
}

/// Renders custom tool calls and results to HTML.
///
/// Mirrors pi's `ToolHtmlRenderer` interface. A concrete implementation (deferred
/// until pidgin-tui lands) invokes a tool's TUI renderer and converts the ANSI
/// output to HTML.
pub trait ToolHtmlRenderer {
    /// Render a tool call to HTML. Returns `None` if the tool has no custom
    /// renderer.
    fn render_call(&self, tool_call_id: &str, tool_name: &str, args: &Value) -> Option<String>;

    /// Render a tool result to collapsed/expanded HTML. Returns `None` if the tool
    /// has no custom renderer.
    fn render_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[ContentBlock],
        details: &Option<Value>,
        is_error: bool,
    ) -> Option<RenderedToolResult>;
}

/// Remove ANSI SGR escape sequences (`\x1b[...m`) from a line, mirroring pi's
/// `ANSI_ESCAPE_REGEX` in the blank-line check.
fn strip_ansi(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(j) = ansi_escape_end(bytes, i) {
            i = j + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// A rendered line is blank if it has no visible content once ANSI codes are
/// stripped. Mirrors pi's `isBlankRenderedLine`.
fn is_blank_rendered_line(line: &str) -> bool {
    strip_ansi(line).trim().is_empty()
}

/// Trim leading and trailing blank (TUI spacing) lines from a rendered result.
/// Mirrors pi's `trimRenderedResultLines`.
pub fn trim_rendered_result_lines<S: AsRef<str> + Clone>(lines: &[S]) -> Vec<S> {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && is_blank_rendered_line(lines[start].as_ref()) {
        start += 1;
    }
    while end > start && is_blank_rendered_line(lines[end - 1].as_ref()) {
        end -= 1;
    }
    lines[start..end].to_vec()
}

/// Pre-render custom tools to HTML using their injected renderer.
///
/// Mirrors pi's `preRenderCustomTools`: it scans assistant messages for tool
/// calls and tool-result messages, asking the renderer to produce HTML for any
/// tool the HTML template does not render itself.
pub fn pre_render_custom_tools(
    entries: &[SessionEntry],
    tool_renderer: &dyn ToolHtmlRenderer,
) -> HashMap<String, RenderedToolHtml> {
    let mut rendered_tools: HashMap<String, RenderedToolHtml> = HashMap::new();

    for entry in entries {
        let SessionEntry::Message(entry) = entry else {
            continue;
        };

        match &entry.message {
            AgentMessage::Assistant(msg) => {
                // Find tool calls in assistant messages.
                for block in &msg.content {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } = block
                    {
                        if !is_template_rendered(name) {
                            if let Some(call_html) = tool_renderer.render_call(id, name, arguments)
                            {
                                rendered_tools.insert(
                                    id.clone(),
                                    RenderedToolHtml {
                                        call_html: Some(call_html),
                                        ..RenderedToolHtml::default()
                                    },
                                );
                            }
                        }
                    }
                }
            }
            AgentMessage::ToolResult(msg) => {
                // Find tool results.
                let tool_name = msg.tool_name.as_str();
                // Only render if we have a pre-rendered call OR it's not template-rendered.
                let existing = rendered_tools.get(&msg.tool_call_id).cloned();
                if existing.is_some() || !is_template_rendered(tool_name) {
                    if let Some(rendered) = tool_renderer.render_result(
                        &msg.tool_call_id,
                        tool_name,
                        &msg.content,
                        &msg.details,
                        msg.is_error,
                    ) {
                        let mut merged = existing.unwrap_or_default();
                        merged.result_html_collapsed = rendered.collapsed;
                        merged.result_html_expanded = rendered.expanded;
                        rendered_tools.insert(msg.tool_call_id.clone(), merged);
                    }
                }
            }
            AgentMessage::User(_) => {}
        }
    }

    rendered_tools
}

#[cfg(test)]
mod tests {
    use super::super::ansi_to_html::ansi_lines_to_html;
    use super::*;
    use crate::core::export_html::types::{EntryBase, MessageEntry, ToolResultMessage};

    #[test]
    fn trims_tui_spacing_lines_from_custom_tool_result() {
        // Ported from export-html-whitespace.test.ts: a custom tool result whose
        // TUI renderer emits leading/trailing blank spacing lines, plus an ANSI
        // red line, must render as two clean ansi-line divs.
        let lines = ["", "\u{1b}[31mone\u{1b}[0m", "two", ""];
        let trimmed = trim_rendered_result_lines(&lines);
        assert_eq!(
            ansi_lines_to_html(&trimmed),
            r#"<div class="ansi-line"><span style="color:#800000">one</span></div><div class="ansi-line">two</div>"#
        );
    }

    #[test]
    fn trim_handles_ansi_only_blank_lines() {
        // A line that is only ANSI codes (no visible text) counts as blank.
        let lines = ["\u{1b}[0m", "content", "\u{1b}[31m\u{1b}[0m"];
        let trimmed = trim_rendered_result_lines(&lines);
        assert_eq!(trimmed, vec!["content"]);
    }

    struct StubRenderer;
    impl ToolHtmlRenderer for StubRenderer {
        fn render_call(&self, _id: &str, tool_name: &str, _args: &Value) -> Option<String> {
            Some(format!("<call>{tool_name}</call>"))
        }
        fn render_result(
            &self,
            _id: &str,
            _tool_name: &str,
            _result: &[ContentBlock],
            _details: &Option<Value>,
            _is_error: bool,
        ) -> Option<RenderedToolResult> {
            Some(RenderedToolResult {
                collapsed: None,
                expanded: Some("<result/>".to_string()),
            })
        }
    }

    #[test]
    fn pre_render_skips_template_rendered_tools_and_renders_custom() {
        let entries = vec![
            SessionEntry::Message(MessageEntry {
                base: EntryBase {
                    id: "e1".to_string(),
                    parent_id: None,
                    timestamp: "t".to_string(),
                },
                message: AgentMessage::Assistant(super::super::types::AssistantMessage {
                    content: vec![
                        ContentBlock::ToolCall {
                            id: "tc-bash".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({}),
                            thought_signature: None,
                        },
                        ContentBlock::ToolCall {
                            id: "tc-custom".to_string(),
                            name: "weather".to_string(),
                            arguments: serde_json::json!({}),
                            thought_signature: None,
                        },
                    ],
                    api: None,
                    provider: None,
                    model: None,
                    response_model: None,
                    response_id: None,
                    usage: None,
                    stop_reason: None,
                    error_message: None,
                    diagnostics: None,
                    timestamp: 0,
                }),
            }),
            SessionEntry::Message(MessageEntry {
                base: EntryBase {
                    id: "e2".to_string(),
                    parent_id: Some("e1".to_string()),
                    timestamp: "t".to_string(),
                },
                message: AgentMessage::ToolResult(ToolResultMessage {
                    tool_call_id: "tc-custom".to_string(),
                    tool_name: "weather".to_string(),
                    content: vec![],
                    details: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp: 0,
                }),
            }),
        ];

        let rendered = pre_render_custom_tools(&entries, &StubRenderer);
        // bash is template-rendered, so it is skipped entirely.
        assert!(!rendered.contains_key("tc-bash"));
        // The custom tool has both a call and a result rendered.
        let custom = rendered.get("tc-custom").expect("custom tool rendered");
        assert_eq!(custom.call_html.as_deref(), Some("<call>weather</call>"));
        assert_eq!(custom.result_html_expanded.as_deref(), Some("<result/>"));
        assert!(custom.result_html_collapsed.is_none());
    }
}
