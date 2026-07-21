//! Byte-exact port of pi's interactive-mode status chrome
//! (`modes/interactive/components/status-indicator.ts`): the two-blank-line
//! [`IdleStatus`] shown between turns and the spinner-driven
//! [`WorkingStatusIndicator`] shown while a turn runs.
//!
//! ## Scope
//!
//! This ports the two indicators the offline interactive shell actually mounts:
//! [`IdleStatus`] (idle region) and [`WorkingStatusIndicator`] (a `Loader` with
//! the accent spinner + muted message). Both sit over the already-ported
//! `pidgin_tui` [`Loader`], whose animation is tick-driven ([`Loader::tick`])
//! rather than timer-driven, so render output stays deterministic.
//!
//! ## PR-4C follow-up
//!
//! The remaining pi `StatusIndicator` subclasses are deferred: `RetryStatusIndicator`
//! (needs a `CountdownTimer` + `keybinding-hints`), `CompactionStatusIndicator`
//! (compaction-reason label + interrupt hint), and `BranchSummaryStatusIndicator`.
//! They are triggered by retry / compaction / branch-summary events that the
//! offline faux demo never emits, and depend on the unported `CountdownTimer` and
//! `keyText` helpers. They land with the `AgentSessionEvent` seam.

use pidgin_tui::renderer::Component;
use pidgin_tui::widgets::loader::{ColorFn, Loader, LoaderIndicatorOptions};

use crate::modes::interactive::theme::Theme;

/// Which status the indicator represents. Mirrors pi's `StatusIndicatorKind`
/// union; only `working` is constructed this PR (see the module follow-up note).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusIndicatorKind {
    /// A turn is running.
    Working,
    /// A request is being retried (deferred).
    Retry,
    /// Context is being compacted (deferred).
    Compaction,
    /// A branch is being summarised (deferred).
    BranchSummary,
}

/// A status indicator: a [`Loader`] tagged with its [`StatusIndicatorKind`].
/// Mirrors pi's `StatusIndicator extends Loader`.
pub struct StatusIndicator {
    loader: Loader,
    /// The kind this indicator represents.
    pub kind: StatusIndicatorKind,
}

impl StatusIndicator {
    /// `new StatusIndicator(kind, ui, spinnerColorFn, messageColorFn, message,
    /// indicator?)`. The `ui: TUI` parameter is omitted (used only for
    /// `requestRender`, which has no bearing on render output).
    pub fn new(
        kind: StatusIndicatorKind,
        spinner_color_fn: ColorFn,
        message_color_fn: ColorFn,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        Self {
            loader: Loader::new(spinner_color_fn, message_color_fn, message, indicator),
            kind,
        }
    }

    /// `setMessage(message)`, forwarded to the loader.
    pub fn set_message(&mut self, message: &str) {
        self.loader.set_message(message);
    }

    /// Advance the spinner one animation frame (the body of pi's `setInterval`
    /// callback). Deterministic stand-in for the timer.
    pub fn tick(&mut self) {
        self.loader.tick();
    }
}

impl Component for StatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }
}

/// The working-turn spinner: accent-coloured spinner, muted message. Mirrors
/// pi's `WorkingStatusIndicator`.
pub struct WorkingStatusIndicator(StatusIndicator);

impl WorkingStatusIndicator {
    /// `new WorkingStatusIndicator(ui, message, indicator?)`: an accent spinner
    /// (`theme.fg("accent", …)`) with a muted message (`theme.fg("muted", …)`).
    pub fn new(theme: &Theme, message: &str, indicator: Option<LoaderIndicatorOptions>) -> Self {
        WorkingStatusIndicator(StatusIndicator::new(
            StatusIndicatorKind::Working,
            fg_color_fn(theme, "accent"),
            fg_color_fn(theme, "muted"),
            message,
            indicator,
        ))
    }

    /// `setMessage(message)`.
    pub fn set_message(&mut self, message: &str) {
        self.0.set_message(message);
    }

    /// Advance the spinner one frame.
    pub fn tick(&mut self) {
        self.0.tick();
    }
}

impl Component for WorkingStatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
}

/// The idle-region placeholder: two full-width blank lines. Mirrors pi's
/// `IdleStatus`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdleStatus;

impl Component for IdleStatus {
    fn render(&self, width: usize) -> Vec<String> {
        let empty_line = " ".repeat(width);
        vec![empty_line.clone(), empty_line]
    }
}

/// Build an owned `theme.fg(color, …)` styling closure from the theme's pre-baked
/// foreground escape: `{fgAnsi}{text}\x1b[39m`. Captured by value so the closure
/// is `'static`, matching the message components' `fg_style` helper.
fn fg_color_fn(theme: &Theme, color: &str) -> ColorFn {
    let ansi = theme.get_fg_ansi(color).unwrap_or_default();
    Box::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
}
