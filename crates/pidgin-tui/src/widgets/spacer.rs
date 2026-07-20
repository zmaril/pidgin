//! Byte-exact port of `vendor/pi/packages/tui/src/components/spacer.ts`.

use crate::renderer::Component;

/// Spacer component that renders empty lines.
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    /// `new Spacer(lines = 1)`.
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }

    /// `setLines(lines)`.
    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Component for Spacer {
    fn render(&self, _width: usize) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.lines {
            result.push(String::new());
        }
        result
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate currently.
    }
}
