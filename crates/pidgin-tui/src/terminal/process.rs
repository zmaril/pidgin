//! `ProcessTerminal` — a real, crossterm-backed terminal backend, ported from
//! pi's `ProcessTerminal` (`vendor/pi/packages/tui/src/terminal.ts`).
//!
//! pi's `ProcessTerminal` drives raw stdin/stdout: it enables raw mode, turns on
//! bracketed paste and (on Windows) virtual-terminal input, negotiates the Kitty
//! keyboard protocol (falling back to xterm `modifyOtherKeys`), splits incoming
//! bytes into complete sequences, and restores all of that on teardown. This is
//! a faithful port with the I/O layer swapped for Rust:
//!
//! * **crossterm** is the OS backend only — raw mode enable/disable and the
//!   terminal size query. It never decodes keys.
//! * **[`keys`](crate::keys)** does all key decoding. [`ProcessTerminal::feed`]
//!   turns raw input bytes into forwarded key/paste sequences; callers run those
//!   strings through [`crate::keys::parse_key`] / [`crate::keys::matches_key`],
//!   exactly as pi wires `inputHandler` to its key parser.
//! * The two C addons are replaced by [`super::modifiers`] (macOS CoreGraphics
//!   modifier poll) and [`super::console_mode`] (Windows `SetConsoleMode`).
//!
//! The byte-level input pipeline ([`super::stdin_buffer`], [`super::negotiation`],
//! and the routing here) is pure and unit-tested on Linux CI. The parts that are
//! genuinely I/O — raw-mode toggling, reading stdin, resize signals — are thin
//! and best-effort; they are compile-checked on CI but exercised only on a real
//! TTY.

use std::io::Write;

use crate::keys;

use super::modifiers::{is_native_modifier_pressed, ModifierKey};
use super::negotiation::{
    is_negotiation_sequence_prefix, normalize_apple_terminal_input, parse_negotiation_sequence,
    NegotiationSequence, BRACKETED_PASTE_DISABLE, BRACKETED_PASTE_ENABLE,
    KITTY_KEYBOARD_PROTOCOL_QUERY, KITTY_PROTOCOL_DISABLE, MODIFY_OTHER_KEYS_DISABLE,
    MODIFY_OTHER_KEYS_ENABLE, TERMINAL_PROGRESS_ACTIVE_SEQUENCE, TERMINAL_PROGRESS_CLEAR_SEQUENCE,
};
use super::stdin_buffer::{StdinBuffer, StdinEvent};
use super::Terminal;

/// An input event produced by [`ProcessTerminal::feed`], mirroring what pi's
/// `inputHandler` receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    /// A complete key/escape sequence (already Apple-Terminal normalized). Feed
    /// this to [`crate::keys::parse_key`] / [`crate::keys::matches_key`].
    Key(String),
    /// The body of a bracketed paste (markers stripped). pi re-wraps this as
    /// `\x1b[200~<content>\x1b[201~` before delivery; a consumer wanting
    /// pi-identical bytes can re-wrap via [`TerminalInput::as_delivered`].
    Paste(String),
}

impl TerminalInput {
    /// The exact string pi's `inputHandler` receives for this event: key
    /// sequences verbatim, pastes re-wrapped in bracketed-paste markers.
    pub fn as_delivered(&self) -> String {
        match self {
            TerminalInput::Key(s) => s.clone(),
            TerminalInput::Paste(content) => format!("\x1b[200~{content}\x1b[201~"),
        }
    }
}

/// Internal result of classifying one sequence against the keyboard-protocol
/// negotiation state, mirroring pi's `readKeyboardProtocolNegotiationSequence`
/// return union (`NegotiationSequence | "pending" | undefined`).
enum NegRead {
    Pending,
    Sequence(NegotiationSequence),
    NotNegotiation,
}

/// Real terminal backend over an output sink `W`. Construct with
/// [`ProcessTerminal::new`] for the controlling terminal, drive input with
/// [`ProcessTerminal::feed`], and bracket a session with the [`Terminal`]
/// `start`/`stop` methods.
pub struct ProcessTerminal<W: Write> {
    out: W,
    columns: usize,
    rows: usize,

    /// Whether to toggle real raw mode through crossterm. Disabled in unit tests
    /// so the escape-sequence protocol can be asserted without a TTY.
    manage_raw_mode: bool,
    was_raw: bool,

    is_apple_terminal: bool,

    stdin_buffer: StdinBuffer,
    negotiation_buffer: String,

    kitty_protocol_active: bool,
    modify_other_keys_active: bool,
    keyboard_protocol_pushed: bool,
    progress_active: bool,
}

fn detect_size() -> (usize, usize) {
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        if cols > 0 && rows > 0 {
            return (cols as usize, rows as usize);
        }
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&r| r > 0)
        .unwrap_or(24);
    (cols, rows)
}

fn detect_apple_terminal() -> bool {
    cfg!(target_os = "macos") && std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal")
}

impl<W: Write> ProcessTerminal<W> {
    /// Build a backend over `out`, querying the current terminal size (falling
    /// back to `COLUMNS`/`LINES` then 80x24, as pi does).
    pub fn new(out: W) -> Self {
        let (columns, rows) = detect_size();
        Self::with_state(out, columns, rows, true)
    }

    /// Build a backend with explicit dimensions (size known out of band).
    pub fn with_size(out: W, columns: usize, rows: usize) -> Self {
        Self::with_state(out, columns, rows, true)
    }

    fn with_state(out: W, columns: usize, rows: usize, manage_raw_mode: bool) -> Self {
        Self {
            out,
            columns,
            rows,
            manage_raw_mode,
            was_raw: false,
            is_apple_terminal: detect_apple_terminal(),
            stdin_buffer: StdinBuffer::default(),
            negotiation_buffer: String::new(),
            kitty_protocol_active: false,
            modify_other_keys_active: false,
            keyboard_protocol_pushed: false,
            progress_active: false,
        }
    }

    /// Enable or disable real raw-mode management (via crossterm). On by
    /// default; turn it off when the sink is not the controlling terminal (e.g.
    /// tests) so `start`/`stop` only emit their escape sequences.
    pub fn manage_raw_mode(mut self, yes: bool) -> Self {
        self.manage_raw_mode = yes;
        self
    }

    /// Whether the Kitty keyboard protocol is currently active (`kittyProtocolActive`).
    pub fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }

    /// Whether xterm `modifyOtherKeys` is currently active.
    pub fn modify_other_keys_active(&self) -> bool {
        self.modify_other_keys_active
    }

    /// Update cached dimensions from a resize (crossterm `Event::Resize`, which
    /// the caller pumps). Equivalent to pi's `stdout.on("resize")` refresh.
    pub fn set_size(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
    }

    fn emit(&mut self, data: &str) {
        let _ = self.out.write_all(data.as_bytes());
        let _ = self.out.flush();
    }

    // --- input pipeline ----------------------------------------------------

    /// Feed raw input bytes (as decoded UTF-8) and return the key/paste events
    /// produced, mirroring the chain
    /// `stdin -> StdinBuffer -> negotiation filter -> inputHandler` in pi.
    pub fn feed(&mut self, data: &str) -> Vec<TerminalInput> {
        let events = self.stdin_buffer.process(data);
        self.route_stdin_events(events)
    }

    /// Flush a stalled [`StdinBuffer`] remainder as input (pi's 10 ms completion
    /// timer). Route the flushed sequences through the same negotiation filter.
    pub fn flush_input_timeout(&mut self) -> Vec<TerminalInput> {
        let events = self.stdin_buffer.flush();
        self.route_stdin_events(events)
    }

    /// Route a batch of [`StdinBuffer`] events into forwarded input, mirroring
    /// pi's `stdinBuffer.on("data")` / `on("paste")` handlers: data sequences go
    /// through the negotiation filter, paste bodies are forwarded directly. pi
    /// re-wraps paste bodies for the editor; we carry the body and expose the
    /// wrapped form via [`TerminalInput::as_delivered`].
    fn route_stdin_events(&mut self, events: Vec<StdinEvent>) -> Vec<TerminalInput> {
        let mut out = Vec::new();
        for event in events {
            match event {
                StdinEvent::Data(sequence) => self.route_data_sequence(&sequence, &mut out),
                StdinEvent::Paste(content) => out.push(TerminalInput::Paste(content)),
            }
        }
        out
    }

    /// Flush a buffered (split) keyboard-protocol reply as ordinary input (pi's
    /// 150 ms fragment timeout).
    pub fn flush_negotiation_timeout(&mut self) -> Vec<TerminalInput> {
        let mut out = Vec::new();
        self.flush_negotiation_buffer_as_input(&mut out);
        out
    }

    /// True when a partial keyboard-protocol reply is buffered and the caller
    /// should arm the fragment-timeout flush.
    pub fn has_pending_negotiation(&self) -> bool {
        !self.negotiation_buffer.is_empty()
    }

    /// True when the [`StdinBuffer`] holds an incomplete sequence remainder and
    /// the caller should arm the input-completion flush (pi's 10 ms timer). Lets a
    /// run loop schedule [`ProcessTerminal::flush_input_timeout`] only when there
    /// is actually a stalled fragment, matching pi's `StdinBuffer` timeout arming.
    pub fn has_pending_input(&self) -> bool {
        self.stdin_buffer.has_pending()
    }

    fn route_data_sequence(&mut self, sequence: &str, out: &mut Vec<TerminalInput>) {
        match self.read_negotiation_sequence(sequence, out) {
            NegRead::Pending => {
                // Wait briefly for the rest of a split Kitty/DA reply; the caller
                // arms `flush_negotiation_timeout`.
            }
            NegRead::Sequence(negotiation) => self.handle_negotiation_sequence(negotiation),
            NegRead::NotNegotiation => self.forward_input_sequence(sequence, out),
        }
    }

    fn read_negotiation_sequence(
        &mut self,
        sequence: &str,
        out: &mut Vec<TerminalInput>,
    ) -> NegRead {
        if !self.negotiation_buffer.is_empty() {
            let buffered = format!("{}{sequence}", self.negotiation_buffer);
            if let Some(negotiation) = parse_negotiation_sequence(&buffered) {
                self.negotiation_buffer.clear();
                return NegRead::Sequence(negotiation);
            }
            if is_negotiation_sequence_prefix(&buffered) {
                self.negotiation_buffer = buffered;
                return NegRead::Pending;
            }
            self.flush_negotiation_buffer_as_input(out);
        }

        if let Some(negotiation) = parse_negotiation_sequence(sequence) {
            return NegRead::Sequence(negotiation);
        }
        if is_negotiation_sequence_prefix(sequence) {
            self.negotiation_buffer = sequence.to_string();
            return NegRead::Pending;
        }
        NegRead::NotNegotiation
    }

    fn flush_negotiation_buffer_as_input(&mut self, out: &mut Vec<TerminalInput>) {
        if self.negotiation_buffer.is_empty() {
            return;
        }
        let sequence = std::mem::take(&mut self.negotiation_buffer);
        self.forward_input_sequence(&sequence, out);
    }

    fn handle_negotiation_sequence(&mut self, negotiation: NegotiationSequence) {
        self.negotiation_buffer.clear();
        match negotiation {
            NegotiationSequence::KittyFlags(flags) => {
                if flags != 0 {
                    self.disable_modify_other_keys();
                    if !self.kitty_protocol_active {
                        self.kitty_protocol_active = true;
                        keys::set_kitty_protocol_active(true);
                    }
                } else {
                    self.enable_modify_other_keys();
                }
            }
            NegotiationSequence::DeviceAttributes => {
                if !self.kitty_protocol_active {
                    self.enable_modify_other_keys();
                }
            }
        }
    }

    fn forward_input_sequence(&mut self, sequence: &str, out: &mut Vec<TerminalInput>) {
        let is_apple = sequence == "\r" && self.is_apple_terminal;
        let shift = is_apple && is_native_modifier_pressed(ModifierKey::Shift);
        let input = normalize_apple_terminal_input(sequence, is_apple, shift);
        out.push(TerminalInput::Key(input));
    }

    fn enable_modify_other_keys(&mut self) {
        if self.kitty_protocol_active || self.modify_other_keys_active {
            return;
        }
        self.emit(MODIFY_OTHER_KEYS_ENABLE);
        self.modify_other_keys_active = true;
    }

    fn disable_modify_other_keys(&mut self) {
        if !self.modify_other_keys_active {
            return;
        }
        self.emit(MODIFY_OTHER_KEYS_DISABLE);
        self.modify_other_keys_active = false;
    }

    fn query_and_enable_kitty_protocol(&mut self) {
        self.stdin_buffer = StdinBuffer::default();
        self.keyboard_protocol_pushed = true;
        self.negotiation_buffer.clear();
        self.emit(KITTY_KEYBOARD_PROTOCOL_QUERY);
    }

    // --- raw mode ----------------------------------------------------------

    fn enter_raw_mode(&mut self) {
        if !self.manage_raw_mode {
            return;
        }
        self.was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        let _ = crossterm::terminal::enable_raw_mode();
    }

    fn leave_raw_mode(&mut self) {
        if !self.manage_raw_mode {
            return;
        }
        // pi restores the previous raw state: keep raw if it was raw, else clear.
        if !self.was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    // --- rich terminal operations (all faithful escape emitters) -----------

    /// Move the cursor by `lines` (negative = up, positive = down), as pi's
    /// `moveBy`.
    pub fn move_by(&mut self, lines: i32) {
        match lines.cmp(&0) {
            std::cmp::Ordering::Greater => self.emit(&format!("\x1b[{lines}B")),
            std::cmp::Ordering::Less => self.emit(&format!("\x1b[{}A", -lines)),
            std::cmp::Ordering::Equal => {}
        }
    }

    /// Clear the current line (`\x1b[K`).
    pub fn clear_line(&mut self) {
        self.emit("\x1b[K");
    }

    /// Clear from the cursor to end of screen (`\x1b[J`).
    pub fn clear_from_cursor(&mut self) {
        self.emit("\x1b[J");
    }

    /// Clear the screen and home the cursor (`\x1b[2J\x1b[H`).
    pub fn clear_screen(&mut self) {
        self.emit("\x1b[2J\x1b[H");
    }

    /// Set the terminal window title (OSC 0).
    pub fn set_title(&mut self, title: &str) {
        self.emit(&format!("\x1b]0;{title}\x07"));
    }

    /// Toggle the OSC 9;4 progress indicator. When active the caller should call
    /// [`ProcessTerminal::progress_keepalive`] every
    /// [`super::negotiation::TERMINAL_PROGRESS_KEEPALIVE_MS`] ms (pi's interval).
    pub fn set_progress(&mut self, active: bool) {
        if active {
            self.emit(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            self.progress_active = true;
        } else {
            self.progress_active = false;
            self.emit(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
    }

    /// Re-emit the progress-active sequence (pi's keepalive interval tick). No-op
    /// unless progress is active.
    pub fn progress_keepalive(&mut self) {
        if self.progress_active {
            self.emit(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
        }
    }

    /// Disable the keyboard protocol ahead of exit, mirroring the protocol
    /// teardown in pi's `drainInput`. The timed stdin byte-drain itself is an
    /// I/O concern left to the run loop.
    pub fn drain_input(&mut self) {
        self.disable_keyboard_protocol();
    }

    /// Pop the Kitty keyboard protocol (if pushed/active) and `modifyOtherKeys`,
    /// clearing the negotiation buffer. Shared teardown for `drain_input` and
    /// `stop`, matching the identical sequence in pi's `drainInput` / `stop`.
    fn disable_keyboard_protocol(&mut self) {
        let should_disable = self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.negotiation_buffer.clear();
        if should_disable {
            self.emit(KITTY_PROTOCOL_DISABLE);
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            keys::set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();
    }
}

impl<W: Write> Terminal for ProcessTerminal<W> {
    fn columns(&self) -> usize {
        self.columns
    }

    fn rows(&self) -> usize {
        self.rows
    }

    fn write(&mut self, data: &str) {
        self.emit(data);
    }

    fn hide_cursor(&mut self) {
        self.emit("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.emit("\x1b[?25h");
    }

    fn start(&mut self) {
        // Save previous raw state and enter raw mode first (Windows VT input must
        // run after, since raw mode resets console flags).
        self.enter_raw_mode();

        // Enable bracketed paste.
        self.emit(BRACKETED_PASTE_ENABLE);

        // Refresh dimensions (pi issues SIGWINCH here; we re-query size).
        if self.manage_raw_mode {
            let (columns, rows) = detect_size();
            self.columns = columns;
            self.rows = rows;
        }

        // On Windows, add ENABLE_VIRTUAL_TERMINAL_INPUT after raw mode.
        super::console_mode::enable_virtual_terminal_input();

        // Query Kitty keyboard protocol; DA sentinel drives the modifyOtherKeys
        // fallback when there is no Kitty reply.
        self.query_and_enable_kitty_protocol();
    }

    fn stop(&mut self) {
        if self.progress_active {
            self.emit(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
            self.progress_active = false;
        }

        // Disable bracketed paste.
        self.emit(BRACKETED_PASTE_DISABLE);

        // Pop the Kitty protocol / modifyOtherKeys (unless drain_input already did).
        self.disable_keyboard_protocol();

        self.stdin_buffer.clear();

        // Restore raw mode last.
        self.leave_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ProcessTerminal` writing into an in-memory buffer with raw-mode
    /// management off, so protocol byte streams are deterministic on CI.
    fn test_terminal() -> ProcessTerminal<Vec<u8>> {
        ProcessTerminal::with_size(Vec::new(), 80, 24).manage_raw_mode(false)
    }

    fn output(term: &ProcessTerminal<Vec<u8>>) -> String {
        String::from_utf8(term.out.clone()).unwrap()
    }

    #[test]
    fn start_emits_bracketed_paste_then_kitty_query() {
        let mut term = test_terminal();
        term.start();
        assert_eq!(output(&term), "\x1b[?2004h\x1b[>7u\x1b[?u\x1b[c");
        assert!(term.keyboard_protocol_pushed);
    }

    #[test]
    fn stop_after_start_disables_paste_and_kitty() {
        let mut term = test_terminal();
        term.start();
        term.out.clear();
        term.stop();
        // Bracketed paste off, then Kitty protocol pop (pushed during start).
        assert_eq!(output(&term), "\x1b[?2004l\x1b[<u");
        assert!(!term.keyboard_protocol_pushed);
    }

    #[test]
    fn feed_forwards_plain_key_sequences() {
        let mut term = test_terminal();
        let events = term.feed("\x1b[A");
        assert_eq!(events, vec![TerminalInput::Key("\x1b[A".to_string())]);
    }

    #[test]
    fn feed_splits_batched_keys() {
        let mut term = test_terminal();
        let events = term.feed("abc");
        assert_eq!(
            events,
            vec![
                TerminalInput::Key("a".to_string()),
                TerminalInput::Key("b".to_string()),
                TerminalInput::Key("c".to_string()),
            ]
        );
    }

    #[test]
    fn feed_emits_paste_event() {
        let mut term = test_terminal();
        let events = term.feed("\x1b[200~pasted\x1b[201~");
        assert_eq!(events, vec![TerminalInput::Paste("pasted".to_string())]);
        assert_eq!(
            events[0].as_delivered(),
            "\x1b[200~pasted\x1b[201~".to_string()
        );
    }

    #[test]
    fn kitty_flags_reply_activates_protocol_and_is_swallowed() {
        let mut term = test_terminal();
        // A complete Kitty flags reply is consumed (not forwarded) and turns on
        // the protocol.
        let events = term.feed("\x1b[?7u");
        assert!(events.is_empty(), "negotiation reply must not be forwarded");
        assert!(term.kitty_protocol_active());
        assert!(keys::is_kitty_protocol_active());
        // Reset global for other tests.
        keys::set_kitty_protocol_active(false);
    }

    #[test]
    fn device_attributes_reply_enables_modify_other_keys() {
        let mut term = test_terminal();
        let events = term.feed("\x1b[?62;1;6c");
        assert!(events.is_empty());
        assert!(term.modify_other_keys_active());
        assert_eq!(output(&term), MODIFY_OTHER_KEYS_ENABLE);
    }

    #[test]
    fn incomplete_prefix_is_buffered_then_flushed_as_input() {
        let mut term = test_terminal();
        // `\x1b[` is incomplete at the StdinBuffer level, so it is held there and
        // nothing surfaces yet.
        let events = term.feed("\x1b[");
        assert!(events.is_empty());
        assert!(!term.has_pending_negotiation());

        // The StdinBuffer completion timeout surfaces `\x1b[` as a data event;
        // the negotiation layer then recognises it as a protocol prefix and
        // buffers it (pending), waiting for the rest of a possible split reply.
        let flushed = term.flush_input_timeout();
        assert!(flushed.is_empty());
        assert!(term.has_pending_negotiation());

        // The negotiation fragment timeout finally flushes it as ordinary input.
        let final_input = term.flush_negotiation_timeout();
        assert_eq!(final_input, vec![TerminalInput::Key("\x1b[".to_string())]);
        assert!(!term.has_pending_negotiation());
    }

    #[test]
    fn kitty_reply_split_across_reads_activates_protocol() {
        let mut term = test_terminal();
        // First chunk is incomplete at the StdinBuffer level (no CSI final byte).
        assert!(term.feed("\x1b[?7").is_empty());
        // The completing byte forms `\x1b[?7u`, a Kitty flags reply: consumed,
        // not forwarded, and it turns the protocol on.
        let events = term.feed("u");
        assert!(events.is_empty());
        assert!(term.kitty_protocol_active());
        keys::set_kitty_protocol_active(false);
    }

    #[test]
    fn progress_active_then_clear() {
        let mut term = test_terminal();
        term.set_progress(true);
        assert_eq!(output(&term), TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
        term.out.clear();
        term.set_progress(false);
        assert_eq!(output(&term), TERMINAL_PROGRESS_CLEAR_SEQUENCE);
    }

    #[test]
    fn move_by_directions() {
        let mut term = test_terminal();
        term.move_by(3);
        term.move_by(-2);
        term.move_by(0);
        assert_eq!(output(&term), "\x1b[3B\x1b[2A");
    }

    #[test]
    fn clear_and_title_sequences() {
        let mut term = test_terminal();
        term.clear_line();
        term.clear_from_cursor();
        term.clear_screen();
        term.set_title("hi");
        assert_eq!(output(&term), "\x1b[K\x1b[J\x1b[2J\x1b[H\x1b]0;hi\x07");
    }

    #[test]
    fn hide_show_cursor_sequences() {
        let mut term = test_terminal();
        term.hide_cursor();
        term.show_cursor();
        assert_eq!(output(&term), "\x1b[?25l\x1b[?25h");
    }
}
