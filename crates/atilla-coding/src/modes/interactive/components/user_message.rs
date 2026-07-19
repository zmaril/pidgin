// straitjacket-allow-file:duplication — the OSC-133 zone-wrapping `render()`
// override is a faithful mirror of pi's identical override in
// `assistant-message.ts` (both components carry the same block); the
// duplication is intentional and matches upstream.
//! `UserMessage` — renders a user message.
//!
//! Byte-exact port of pi's `modes/interactive/components/user-message.ts`
//! (`UserMessageComponent`). Composes `atilla-tui`'s `Box`, `Container`, and
//! `Markdown`; wraps its output in OSC-133 zone markers.

use atilla_tui::renderer::{Component, Container};
use atilla_tui::widgets::box_widget::BoxWidget;
use atilla_tui::{DefaultTextStyle, Markdown, MarkdownOptions};

use super::{
    get_markdown_theme, MarkdownComponent, OSC133_ZONE_END, OSC133_ZONE_FINAL, OSC133_ZONE_START,
};
use crate::modes::interactive::theme::Theme;

/// Component that renders a user message. Port of pi's `UserMessageComponent`.
pub struct UserMessage {
    text: String,
    output_pad: usize,
    theme: Theme,
    container: Container,
}

impl UserMessage {
    /// `new UserMessageComponent(text, markdownTheme=getMarkdownTheme(),
    /// outputPad=1)`. The markdown theme is derived from `theme` via
    /// [`get_markdown_theme`] (pi's default).
    pub fn new(text: impl Into<String>, theme: Theme, output_pad: usize) -> Self {
        let mut component = Self {
            text: text.into(),
            output_pad,
            theme,
            container: Container::new(),
        };
        component.rebuild();
        component
    }

    /// pi's `setOutputPad`.
    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        self.rebuild();
    }

    /// pi's private `rebuild()` — a background `Box` wrapping the message text as
    /// themed markdown.
    fn rebuild(&mut self) {
        self.container.clear();

        let bg_theme = self.theme.clone();
        let mut content_box = BoxWidget::new(
            self.output_pad,
            1,
            Some(Box::new(move |content: &str| {
                bg_theme
                    .bg("userMessageBg", content)
                    .unwrap_or_else(|_| content.to_string())
            })),
        );

        let color_theme = self.theme.clone();
        let default_text_style = DefaultTextStyle {
            color: Some(Box::new(move |content: &str| {
                color_theme
                    .fg("userMessageText", content)
                    .unwrap_or_else(|_| content.to_string())
            })),
            ..Default::default()
        };
        content_box.add_child(Box::new(MarkdownComponent(Markdown::new(
            self.text.clone(),
            0,
            0,
            get_markdown_theme(&self.theme),
            Some(default_text_style),
            Some(MarkdownOptions {
                preserve_ordered_list_markers: true,
                preserve_backslash_escapes: true,
            }),
        ))));

        self.container.add_child(Box::new(content_box));
    }
}

impl Component for UserMessage {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.container.render(width);
        if lines.is_empty() {
            return lines;
        }

        let last = lines.len() - 1;
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}
