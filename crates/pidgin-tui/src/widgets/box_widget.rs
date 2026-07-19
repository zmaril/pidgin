//! Byte-exact port of `vendor/pi/packages/tui/src/components/box.ts`.
//!
//! pi's class is `Box`; the Rust struct is named `BoxWidget` to avoid shadowing
//! `std::boxed::Box`, which the module also uses for boxed children/closures.

use crate::renderer::Component;
use crate::text_util::apply_background_to_line;
use crate::widgets::text::BgFn;
use crate::width::visible_width;

/// Box component — a container that applies padding and background to all
/// children.
///
/// pi's sampling-based render cache is omitted: the render is a pure function of
/// `(children output, width, bg_fn)`, so caching does not affect output.
pub struct BoxWidget {
    /// Child components, in order.
    pub children: Vec<Box<dyn Component>>,
    padding_x: usize,
    padding_y: usize,
    bg_fn: Option<BgFn>,
}

impl BoxWidget {
    /// `new Box(paddingX = 1, paddingY = 1, bgFn?)`.
    pub fn new(padding_x: usize, padding_y: usize, bg_fn: Option<BgFn>) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn,
        }
    }

    /// `addChild(component)`.
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
    }

    /// `clear()`.
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// `setBgFn(bgFn?)`.
    pub fn set_bg_fn(&mut self, bg_fn: Option<BgFn>) {
        self.bg_fn = bg_fn;
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let vis_len = visible_width(line);
        let pad_needed = width.saturating_sub(vis_len);
        let padded = format!("{line}{}", " ".repeat(pad_needed));

        match &self.bg_fn {
            Some(bg_fn) => apply_background_to_line(&padded, width, bg_fn),
            None => padded,
        }
    }
}

impl Default for BoxWidget {
    fn default() -> Self {
        Self::new(1, 1, None)
    }
}

impl Component for BoxWidget {
    fn render(&self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left_pad = " ".repeat(self.padding_x);

        // Render all children.
        let mut child_lines: Vec<String> = Vec::new();
        for child in &self.children {
            let lines = child.render(content_width);
            for line in lines {
                child_lines.push(format!("{left_pad}{line}"));
            }
        }

        if child_lines.is_empty() {
            return Vec::new();
        }

        // Apply background and padding.
        let mut result: Vec<String> = Vec::new();

        // Top padding.
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        // Content.
        for line in &child_lines {
            result.push(self.apply_bg(line, width));
        }

        // Bottom padding.
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        result
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}
