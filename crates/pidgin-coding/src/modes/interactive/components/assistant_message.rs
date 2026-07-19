// straitjacket-allow-file:duplication — the OSC-133 zone-wrapping `render()`
// override is a faithful mirror of pi's identical override in `user-message.ts`
// (both `assistant-message.ts` and `user-message.ts` carry the same block); the
// duplication is intentional and matches upstream.
//! `AssistantMessage` — renders a complete assistant message.
//!
//! Byte-exact port of pi's
//! `modes/interactive/components/assistant-message.ts`
//! (`AssistantMessageComponent`). Composes `pidgin-tui`'s `Container`,
//! `Markdown`, `Spacer`, and `Text`; wraps its output in OSC-133 zone markers
//! unless it carries tool calls or renders empty.

use pidgin_tui::renderer::{Component, Container};
use pidgin_tui::{DefaultTextStyle, Markdown, Spacer, Text};

use pidgin_ai::types::{AssistantMessage as AiAssistantMessage, ContentBlock, StopReason};

use super::{
    get_markdown_theme, MarkdownComponent, OSC133_ZONE_END, OSC133_ZONE_FINAL, OSC133_ZONE_START,
};
use crate::modes::interactive::theme::Theme;

/// Wrap `text` in the theme's foreground color for `color`, matching pi's
/// `theme.fg`. A missing color (unreachable for the built-in themes) leaves the
/// text unstyled rather than panicking.
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// `true` if a content block contributes visible assistant content — a non-blank
/// text or thinking block. Mirrors pi's inline
/// `(c.type === "text" && c.text.trim()) || (c.type === "thinking" && c.thinking.trim())`.
fn is_visible(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Text { text, .. } => !text.trim().is_empty(),
        ContentBlock::Thinking { thinking, .. } => !thinking.trim().is_empty(),
        _ => false,
    }
}

/// Component that renders a complete assistant message. Port of pi's
/// `AssistantMessageComponent`.
pub struct AssistantMessage {
    content_container: Container,
    hide_thinking_block: bool,
    hidden_thinking_label: String,
    output_pad: usize,
    last_message: Option<AiAssistantMessage>,
    has_tool_calls: bool,
    theme: Theme,
}

impl AssistantMessage {
    /// `new AssistantMessageComponent(message?, hideThinkingBlock=false,
    /// markdownTheme=getMarkdownTheme(), hiddenThinkingLabel="Thinking...",
    /// outputPad=1)`.
    ///
    /// The markdown theme is derived from `theme` via [`get_markdown_theme`]
    /// (pi's default), rather than injected — byte-identical for the default
    /// path pi uses.
    pub fn new(
        message: Option<&AiAssistantMessage>,
        theme: Theme,
        hide_thinking_block: bool,
        hidden_thinking_label: impl Into<String>,
        output_pad: usize,
    ) -> Self {
        let mut component = Self {
            content_container: Container::new(),
            hide_thinking_block,
            hidden_thinking_label: hidden_thinking_label.into(),
            output_pad,
            last_message: None,
            has_tool_calls: false,
            theme,
        };
        if let Some(message) = message {
            component.update_content(message);
        }
        component
    }

    /// pi's `setHideThinkingBlock`.
    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.hide_thinking_block = hide;
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// pi's `setHiddenThinkingLabel`.
    pub fn set_hidden_thinking_label(&mut self, label: impl Into<String>) {
        self.hidden_thinking_label = label.into();
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// pi's `setOutputPad`.
    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// pi's `updateContent(message)` — rebuild the content container from the
    /// message, in content order.
    pub fn update_content(&mut self, message: &AiAssistantMessage) {
        self.last_message = Some(message.clone());
        self.content_container.clear();

        let content = &message.content;
        let has_visible_content = content.iter().any(is_visible);
        if has_visible_content {
            self.content_container.add_child(Box::new(Spacer::new(1)));
        }

        // Render content in order.
        let mut i = 0;
        while i < content.len() {
            match &content[i] {
                ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                    // Assistant text messages with no background — trim the text.
                    // paddingY=0 avoids extra spacing before tool executions.
                    self.content_container
                        .add_child(Box::new(MarkdownComponent(Markdown::new(
                            text.trim().to_string(),
                            self.output_pad,
                            0,
                            get_markdown_theme(&self.theme),
                            None,
                            None,
                        ))));
                }
                ContentBlock::Thinking { .. } => {
                    let mut thinking_blocks: Vec<String> = Vec::new();
                    while i < content.len() {
                        match &content[i] {
                            ContentBlock::Thinking { thinking, .. } => {
                                let trimmed = thinking.trim();
                                if !trimmed.is_empty() {
                                    thinking_blocks.push(trimmed.to_string());
                                }
                            }
                            _ => break,
                        }
                        i += 1;
                    }
                    i -= 1;

                    if thinking_blocks.is_empty() {
                        i += 1;
                        continue;
                    }

                    // Add spacing only when another visible assistant content
                    // block follows, avoiding a superfluous blank line before
                    // separately-rendered tool execution blocks.
                    let has_visible_content_after =
                        content.get(i + 1..).unwrap_or(&[]).iter().any(is_visible);

                    if self.hide_thinking_block {
                        // One static label per run of thinking blocks when hidden.
                        let styled = self.theme.italic(&fg(
                            &self.theme,
                            "thinkingText",
                            &self.hidden_thinking_label,
                        ));
                        self.content_container.add_child(Box::new(Text::new(
                            &styled,
                            self.output_pad,
                            0,
                            None,
                        )));
                    } else {
                        // Render each run of thinking blocks as one Markdown section.
                        let theme = self.theme.clone();
                        let default_text_style = DefaultTextStyle {
                            color: Some(Box::new(move |text: &str| {
                                fg(&theme, "thinkingText", text)
                            })),
                            italic: true,
                            ..Default::default()
                        };
                        self.content_container.add_child(Box::new(MarkdownComponent(
                            Markdown::new(
                                thinking_blocks.join("\n\n"),
                                self.output_pad,
                                0,
                                get_markdown_theme(&self.theme),
                                Some(default_text_style),
                                None,
                            ),
                        )));
                    }
                    if has_visible_content_after {
                        self.content_container.add_child(Box::new(Spacer::new(1)));
                    }
                }
                _ => {}
            }
            i += 1;
        }

        // Incomplete/failed surfacing after partial content. For aborted/error
        // tool calls the tool-execution components show the error; length stops
        // can happen before a tool call is complete, so surface them here too.
        let has_tool_calls = content
            .iter()
            .any(|c| matches!(c, ContentBlock::ToolCall { .. }));
        self.has_tool_calls = has_tool_calls;

        if message.stop_reason == StopReason::Length {
            self.content_container.add_child(Box::new(Spacer::new(1)));
            let text = fg(
                &self.theme,
                "error",
                "Error: Model stopped because it reached the maximum output token limit. The response may be incomplete.",
            );
            self.content_container
                .add_child(Box::new(Text::new(&text, self.output_pad, 0, None)));
        } else if !has_tool_calls {
            match message.stop_reason {
                StopReason::Aborted => {
                    let abort_message = match &message.error_message {
                        Some(m) if m != "Request was aborted" => m.clone(),
                        _ => "Operation aborted".to_string(),
                    };
                    self.content_container.add_child(Box::new(Spacer::new(1)));
                    let text = fg(&self.theme, "error", &abort_message);
                    self.content_container.add_child(Box::new(Text::new(
                        &text,
                        self.output_pad,
                        0,
                        None,
                    )));
                }
                StopReason::Error => {
                    let error_msg = message
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "Unknown error".to_string());
                    self.content_container.add_child(Box::new(Spacer::new(1)));
                    let text = fg(&self.theme, "error", &format!("Error: {error_msg}"));
                    self.content_container.add_child(Box::new(Text::new(
                        &text,
                        self.output_pad,
                        0,
                        None,
                    )));
                }
                _ => {}
            }
        }
    }
}

impl Component for AssistantMessage {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.content_container.render(width);
        if self.has_tool_calls || lines.is_empty() {
            return lines;
        }

        let last = lines.len() - 1;
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn invalidate(&mut self) {
        self.content_container.invalidate();
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }
}
