//! Byte-exact port of `vendor/pi/packages/tui/src/components/loader.ts`.
//!
//! pi's `Loader extends Text`; the Rust `Loader` composes an inner [`Text`]
//! (constructed as `super("", 1, 0)`) and forwards `super.render` to it. The
//! animation timer is not modeled — frame advancement is driven explicitly via
//! [`Loader::tick`], mirroring pi's `setInterval` callback body, so render output
//! is deterministic. pi's `ui.requestRender()` side effect is omitted (it does
//! not affect render output).

use crate::renderer::Component;
use crate::widgets::text::Text;

/// A color-styling function (pi's `(str: string) => string`).
pub type ColorFn = Box<dyn Fn(&str) -> String>;

/// Animation/indicator configuration (pi's `LoaderIndicatorOptions`).
#[derive(Debug, Clone, Default)]
pub struct LoaderIndicatorOptions {
    /// Animation frames. Use an empty vector to hide the indicator.
    pub frames: Option<Vec<String>>,
    /// Frame interval in milliseconds for animated indicators.
    pub interval_ms: Option<u32>,
}

fn default_frames() -> Vec<String> {
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

const DEFAULT_INTERVAL_MS: u32 = 80;

/// Loader component that updates with an optional spinning animation.
pub struct Loader {
    inner: Text,
    frames: Vec<String>,
    #[allow(dead_code)]
    interval_ms: u32,
    current_frame: usize,
    render_indicator_verbatim: bool,
    spinner_color_fn: ColorFn,
    message_color_fn: ColorFn,
    message: String,
}

impl Loader {
    /// `new Loader(ui, spinnerColorFn, messageColorFn, message = "Loading...", indicator?)`.
    ///
    /// The `ui: TUI` parameter is omitted (used only for `requestRender`, which
    /// has no bearing on render output).
    pub fn new(
        spinner_color_fn: ColorFn,
        message_color_fn: ColorFn,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        let mut loader = Self {
            inner: Text::new("", 1, 0, None),
            frames: default_frames(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message: message.to_string(),
        };
        loader.set_indicator(indicator);
        loader
    }

    /// `setMessage(message)`.
    pub fn set_message(&mut self, message: &str) {
        self.message = message.to_string();
        self.update_display();
    }

    /// `setIndicator(indicator?)`.
    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.render_indicator_verbatim = indicator.is_some();
        self.frames = match indicator.as_ref().and_then(|i| i.frames.clone()) {
            Some(frames) => frames,
            None => default_frames(),
        };
        self.interval_ms = match indicator.as_ref().and_then(|i| i.interval_ms) {
            Some(ms) if ms > 0 => ms,
            _ => DEFAULT_INTERVAL_MS,
        };
        self.current_frame = 0;
        // start(): updateDisplay + restartAnimation (timer skipped).
        self.update_display();
    }

    /// Advance one animation frame — the body of pi's `setInterval` callback.
    /// Only meaningful when there is more than one frame.
    pub fn tick(&mut self) {
        self.current_frame = (self.current_frame + 1) % self.frames.len();
        self.update_display();
    }

    fn update_display(&mut self) {
        let frame = self
            .frames
            .get(self.current_frame)
            .cloned()
            .unwrap_or_default();
        let rendered_frame = if self.render_indicator_verbatim {
            frame.clone()
        } else {
            (self.spinner_color_fn)(&frame)
        };
        let indicator = if !frame.is_empty() {
            format!("{rendered_frame} ")
        } else {
            String::new()
        };
        let text = format!("{indicator}{}", (self.message_color_fn)(&self.message));
        self.inner.set_text(&text);
    }
}

impl Component for Loader {
    fn render(&self, width: usize) -> Vec<String> {
        let mut result: Vec<String> = vec![String::new()];
        result.extend(self.inner.render_lines(width));
        result
    }
}
