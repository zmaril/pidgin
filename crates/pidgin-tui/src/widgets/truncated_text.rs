//! Byte-exact port of `vendor/pi/packages/tui/src/components/truncated-text.ts`.

use crate::renderer::Component;
use crate::width::{truncate_to_width, visible_width};

/// Text component that truncates to fit viewport width.
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    /// `new TruncatedText(text, paddingX = 0, paddingY = 0)`.
    pub fn new(text: &str, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
        }
    }
}

impl Component for TruncatedText {
    fn render(&self, width: usize) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();

        // Empty line padded to width.
        let empty_line = " ".repeat(width);

        // Add vertical padding above.
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        // Calculate available width after horizontal padding.
        let available_width = width.saturating_sub(self.padding_x * 2).max(1);

        // Take only the first line (stop at newline).
        let single_line_text = match self.text.find('\n') {
            Some(idx) => &self.text[..idx],
            None => &self.text,
        };

        // Truncate text if needed (accounting for ANSI codes).
        let display_text =
            truncate_to_width(single_line_text, available_width as i64, "...", false);

        // Add horizontal padding.
        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");

        // Pad line to exactly width characters.
        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible_width);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));

        result.push(final_line);

        // Add vertical padding below.
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate currently.
    }
}

/// One-shot render for a [`TruncatedText`], mirroring pi's
/// `new TruncatedText(text, paddingX, paddingY).render(width)`. Convenience
/// entry point so callers (e.g. the napi shim) need not bring the
/// [`Component`] trait into scope.
pub fn truncated_text_render(
    text: &str,
    padding_x: usize,
    padding_y: usize,
    width: usize,
) -> Vec<String> {
    TruncatedText::new(text, padding_x, padding_y).render(width)
}
