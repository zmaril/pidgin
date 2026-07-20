//! Byte-exact port of pi's interactive-mode `DynamicBorder`
//! (`modes/interactive/components/dynamic-border.ts`): a full-width horizontal
//! rule (`─`) that stretches to the viewport width, coloured by a caller-supplied
//! function.
//!
//! ## Divergence — no global theme default
//!
//! pi's constructor defaults its `color` argument to `theme.fg("border", str)`
//! using the module-global `theme`. The Rust port has no global theme instance
//! (the interactive [`Theme`](crate::modes::interactive::theme::Theme) is threaded
//! explicitly), so the colour function is a **required** constructor argument.
//! This matches pi's own guidance in the source: extensions loaded via jiti must
//! always pass an explicit colour function because the global `theme` may be
//! undefined in the extension module cache — every construction site in the llama
//! UI already does so (`(text) => theme.fg("accent", text)`).

use pidgin_tui::renderer::Component;

/// A text-colouring function (pi's `(str: string) => string`).
pub type ColorFn = Box<dyn Fn(&str) -> String>;

/// Dynamic border component that adjusts to viewport width. Mirrors pi's
/// `DynamicBorder`.
pub struct DynamicBorder {
    color: ColorFn,
}

impl DynamicBorder {
    /// `new DynamicBorder(color)`. The colour function wraps the rendered rule
    /// (pi defaults it to `theme.fg("border", …)`; see the module divergence note
    /// — the Rust port requires it explicitly).
    pub fn new(color: ColorFn) -> Self {
        Self { color }
    }

    /// Shared render body (`render`): a single line of `─` repeated to
    /// `max(1, width)`, passed through the colour function.
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        vec![(self.color)(&"\u{2500}".repeat(width.max(1)))]
    }
}

impl Component for DynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_lines(width)
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate currently.
    }
}
