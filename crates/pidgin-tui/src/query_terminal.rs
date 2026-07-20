//! Synchronous terminal background / color-scheme query state and driver methods
//! for [`Tui`], split out of `renderer.rs`. This is the write-and-arm half of
//! pi's `TUI.queryTerminalBackgroundColor` / `TUI.queryTerminalColorScheme`
//! (`vendor/pi/packages/tui/src/tui.ts`): pi returns a promise resolved by the
//! Node event loop, so this side only writes the query and arms a pending reply
//! slot; the response is consumed in [`Tui::handle_input`] (see `overlay.rs`) and
//! the pump that drives input to settlement lives in
//! [`RunLoop`](crate::RunLoop) (`app.rs`).
//!
//! pidgin's TUI stack is fully synchronous (a poll-based run loop, no async
//! reactor), so pi's single per-query queue collapses to a single reply slot and
//! the promise resolver + timer collapse to `settled` / `reply` flags. The bytes
//! written, the query semantics, and the timeout are identical to pi; only the
//! async shell is dropped.

// straitjacket-allow-file:duplication

use crate::renderer::Tui;
use crate::terminal::Terminal;
use crate::terminal_colors::{RgbColor, TerminalColorScheme};

/// Single-slot synchronous analogue of pi's `PendingOsc11BackgroundQuery`
/// (`{ settled, resolve, timer }`). Records only whether the query has settled
/// and the parsed reply (if any). Driven by
/// [`RunLoop::query_terminal_background_color`](crate::RunLoop::query_terminal_background_color).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PendingOsc11BackgroundQuery {
    /// Whether the query has been resolved (by a consumed response or a timeout).
    pub(crate) settled: bool,
    /// The parsed background color, once a response has been consumed. `None`
    /// while unsettled and after a timeout.
    pub(crate) reply: Option<RgbColor>,
}

/// Single-slot synchronous analogue of pi's `queryTerminalColorScheme` pending
/// state. pi subscribes a temporary `onTerminalColorSchemeChange` listener and
/// settles on the first report; the sync port records the pending resolution in
/// this slot, filled by `consume_terminal_color_scheme_report`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PendingColorSchemeQuery {
    /// Whether the query has been resolved (by a consumed report or a timeout).
    pub(crate) settled: bool,
    /// The reported color scheme, once a report has been consumed. `None` while
    /// unsettled and after a timeout.
    pub(crate) reply: Option<TerminalColorScheme>,
}

impl<T: Terminal> Tui<T> {
    /// Write the OSC 11 background-color query (`ESC ] 11 ; ? BEL`) and arm a
    /// pending reply slot. This is the write-and-arm half of pi's
    /// `queryTerminalBackgroundColor`; the synchronous pump
    /// ([`RunLoop::query_terminal_background_color`](crate::RunLoop::query_terminal_background_color))
    /// drives input until the response is consumed (settling the slot) or a
    /// deadline elapses. The exact bytes match pi's
    /// `this.terminal.write("\x1b]11;?\x07")`.
    pub fn write_terminal_background_query(&mut self) {
        self.pending_osc11_background_replies += 1;
        self.pending_osc11_background_query = Some(PendingOsc11BackgroundQuery::default());
        self.terminal.write("\x1b]11;?\x07");
    }

    /// Whether the in-flight OSC 11 background query has been settled, either by a
    /// consumed response or a timeout ([`Tui::settle_terminal_background_timeout`]).
    pub fn terminal_background_query_settled(&self) -> bool {
        self.pending_osc11_background_query
            .map(|q| q.settled)
            .unwrap_or(false)
    }

    /// Mark the in-flight OSC 11 background query as settled with no reply,
    /// mirroring pi's query timer resolving `undefined`. A late response arriving
    /// afterwards is consumed and discarded (the slot is already settled).
    pub fn settle_terminal_background_timeout(&mut self) {
        if let Some(query) = self.pending_osc11_background_query.as_mut() {
            query.settled = true;
        }
    }

    /// Take the parsed OSC 11 background reply, clearing the pending slot. Returns
    /// `None` if the query timed out before a response was consumed. Mirrors the
    /// value pi's `queryTerminalBackgroundColor` promise resolves to.
    pub fn take_terminal_background_reply(&mut self) -> Option<RgbColor> {
        self.pending_osc11_background_query
            .take()
            .and_then(|q| q.reply)
    }

    /// Write the DSR color-scheme query (`CSI ? 996 n`) and arm a pending reply
    /// slot. This is the write-and-arm half of pi's `queryTerminalColorScheme`;
    /// the pump drives input until a DEC 2031 report is consumed (settling the
    /// slot) or a deadline elapses. The exact bytes match pi's
    /// `this.terminal.write("\x1b[?996n")`.
    pub fn write_terminal_color_scheme_query(&mut self) {
        self.pending_color_scheme_query = Some(PendingColorSchemeQuery::default());
        self.terminal.write("\x1b[?996n");
    }

    /// Whether the in-flight DSR color-scheme query has been settled, either by a
    /// consumed report or a timeout ([`Tui::settle_terminal_color_scheme_timeout`]).
    pub fn terminal_color_scheme_query_settled(&self) -> bool {
        self.pending_color_scheme_query
            .map(|q| q.settled)
            .unwrap_or(false)
    }

    /// Mark the in-flight DSR color-scheme query as settled with no reply,
    /// mirroring pi's query timer resolving `undefined`.
    pub fn settle_terminal_color_scheme_timeout(&mut self) {
        if let Some(query) = self.pending_color_scheme_query.as_mut() {
            query.settled = true;
        }
    }

    /// Take the reported color scheme, clearing the pending slot. Returns `None`
    /// if the query timed out before a report was consumed. Mirrors the value
    /// pi's `queryTerminalColorScheme` promise resolves to.
    pub fn take_terminal_color_scheme_reply(&mut self) -> Option<TerminalColorScheme> {
        self.pending_color_scheme_query.take().and_then(|q| q.reply)
    }
}

#[cfg(test)]
mod tests {
    use crate::overlay::ComponentId;
    use crate::renderer::{Component, SharedLines, Tui};
    use crate::terminal::LoggingTerminal;
    use crate::terminal_colors::{RgbColor, TerminalColorScheme};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Build a `Tui` with a single focused, focusable component and return its id,
    /// so a consumed input can be distinguished from one delivered to the focus.
    fn focused_tui() -> (Tui<LoggingTerminal>, ComponentId) {
        let mut tui = Tui::new(LoggingTerminal::new(20, 5), false);
        let focusable: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(SharedLines::new()));
        let id = tui.register_component(focusable);
        tui.set_focus(Some(id));
        (tui, id)
    }

    #[test]
    fn terminal_query_methods_emit_exact_bytes() {
        // The write-and-arm halves of pi's queryTerminal* methods emit the exact
        // OSC 11 background query and DSR color-scheme query bytes.
        let mut tui = Tui::new(LoggingTerminal::new(20, 5), false);

        tui.write_terminal_background_query();
        assert_eq!(tui.take_writes(), "\x1b]11;?\x07");

        tui.write_terminal_color_scheme_query();
        assert_eq!(tui.take_writes(), "\x1b[?996n");
    }

    #[test]
    fn osc11_background_response_settles_pending_query() {
        // With a query in flight, an OSC 11 response is consumed (not delivered)
        // and settles the pending slot with the parsed color.
        let (mut tui, _id) = focused_tui();

        // No query armed yet: an OSC 11 frame is NOT intercepted (pi's
        // `pendingOsc11BackgroundReplies <= 0` guard) and reaches the component.
        tui.handle_input("\x1b]11;rgb:ffff/ffff/ffff\x07");
        assert_eq!(tui.input_deliveries().len(), 1);

        tui.write_terminal_background_query();
        let _ = tui.take_writes();
        assert!(!tui.terminal_background_query_settled());

        // The response is consumed (no new delivery) and settles the slot.
        tui.handle_input("\x1b]11;rgb:ffff/0000/8080\x07");
        assert_eq!(tui.input_deliveries().len(), 1, "response must be consumed");
        assert!(tui.terminal_background_query_settled());
        assert_eq!(
            tui.take_terminal_background_reply(),
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        );
        // Slot cleared after taking.
        assert!(!tui.terminal_background_query_settled());
    }

    #[test]
    fn osc11_background_timeout_yields_no_reply() {
        // A settled-by-timeout query returns None; a late response is then
        // consumed and discarded (the slot is already settled).
        let mut tui = Tui::new(LoggingTerminal::new(20, 5), false);
        tui.write_terminal_background_query();
        tui.settle_terminal_background_timeout();
        assert!(tui.terminal_background_query_settled());
        assert_eq!(tui.take_terminal_background_reply(), None);
    }

    #[test]
    fn color_scheme_query_settles_from_report() {
        // A DSR color-scheme query is settled by the next DEC 2031 report,
        // resolving the reported scheme.
        let mut tui = Tui::new(LoggingTerminal::new(20, 5), false);
        tui.write_terminal_color_scheme_query();
        let _ = tui.take_writes();
        assert!(!tui.terminal_color_scheme_query_settled());

        tui.handle_input("\x1b[?997;1n");
        assert!(tui.terminal_color_scheme_query_settled());
        assert_eq!(
            tui.take_terminal_color_scheme_reply(),
            Some(TerminalColorScheme::Dark)
        );
    }
}
