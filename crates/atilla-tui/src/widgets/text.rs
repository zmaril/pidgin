//! Byte-exact port of `vendor/pi/packages/tui/src/components/text.ts`.

use crate::renderer::Component;
use crate::text_util::apply_background_to_line;
use crate::width::{visible_width, wrap_text_with_ansi};

/// A background-styling function (pi's `customBgFn?: (text: string) => string`).
pub type BgFn = Box<dyn Fn(&str) -> String>;

/// Text component — displays multi-line text with word wrapping.
///
/// pi's cache fields are omitted: the render is a pure function of
/// `(text, width, custom_bg_fn)`, so caching does not affect output.
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<BgFn>,
}

impl Text {
    /// `new Text(text = "", paddingX = 1, paddingY = 1, customBgFn?)`.
    pub fn new(text: &str, padding_x: usize, padding_y: usize, custom_bg_fn: Option<BgFn>) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
            custom_bg_fn,
        }
    }

    /// `setText(text)`.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
    }

    /// `setCustomBgFn(customBgFn?)`.
    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<BgFn>) {
        self.custom_bg_fn = custom_bg_fn;
    }

    /// Shared render body so subclasses (e.g. `Loader`) can call `super.render`.
    pub(crate) fn render_lines(&self, width: usize) -> Vec<String> {
        // Don't render anything if there's no actual text.
        if self.text.is_empty() || self.text.trim().is_empty() {
            return Vec::new();
        }

        // Replace tabs with 3 spaces.
        let normalized_text = self.text.replace('\t', "   ");

        // Calculate content width (subtract left/right margins).
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);

        // Wrap text (this preserves ANSI codes but does NOT pad).
        let wrapped_lines = wrap_text_with_ansi(&normalized_text, content_width);

        // Add margins and background to each line.
        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let mut content_lines: Vec<String> = Vec::new();

        for line in &wrapped_lines {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");

            if let Some(bg_fn) = &self.custom_bg_fn {
                content_lines.push(apply_background_to_line(&line_with_margins, width, bg_fn));
            } else {
                let visible_len = visible_width(&line_with_margins);
                let padding_needed = width.saturating_sub(visible_len);
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        // Add top/bottom padding (empty lines).
        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            let line = match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn),
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }

        let mut result: Vec<String> =
            Vec::with_capacity(empty_lines.len() * 2 + content_lines.len());
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new("", 1, 1, None)
    }
}

impl Component for Text {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_lines(width)
    }

    fn invalidate(&mut self) {
        // Cache omitted; nothing to invalidate.
    }
}
