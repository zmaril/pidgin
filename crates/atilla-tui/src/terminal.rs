//! Terminal sink/event boundary for the renderer, ported from pi's
//! `terminal.ts` interface (`vendor/pi/packages/tui/src/terminal.ts`).
//!
//! The renderer treats the terminal strictly as an ANSI sink plus a size
//! source; all diff/layout logic lives in [`crate::renderer`]. Two backends are
//! provided:
//!
//! * [`LoggingTerminal`] — an in-memory sink that records every `write()` call
//!   (and nothing else), the exact equivalent of pi's test-suite
//!   `LoggingVirtualTerminal.getWrites()`. It is what the byte-exact vector
//!   replay asserts against. Note that pi's `hideCursor`/`showCursor`/bracketed-
//!   paste writes bypass the logged `write()` path (they go straight to xterm),
//!   so they are deliberately excluded from the recorded stream here too.
//! * [`CrosstermTerminal`] — a real backend that emits to any [`std::io::Write`]
//!   using crossterm for cursor visibility and bracketed paste. crossterm is
//!   kept strictly as the output/event backend.

use std::io::Write;

/// Minimal terminal interface the renderer drives. Mirrors the subset of pi's
/// `Terminal` used by `doRender`: dimensions, `write`, cursor visibility, and
/// start/stop. Only `write` is part of the byte-exact diff stream.
pub trait Terminal {
    /// Current terminal width in columns.
    fn columns(&self) -> usize;
    /// Current terminal height in rows.
    fn rows(&self) -> usize;
    /// Write raw bytes to the terminal. This is the only method whose output is
    /// part of the differential render stream.
    fn write(&mut self, data: &str);
    /// Hide the hardware cursor. Not part of the diff stream.
    fn hide_cursor(&mut self);
    /// Show the hardware cursor. Not part of the diff stream.
    fn show_cursor(&mut self);
    /// Begin a session (enable bracketed paste, etc.). Not part of the stream.
    fn start(&mut self);
    /// End a session and restore state. Not part of the stream.
    fn stop(&mut self);
}

/// In-memory logging sink. Records only `write()` payloads, matching pi's
/// `LoggingVirtualTerminal.getWrites()` used by the renderer conformance tests.
#[derive(Debug, Clone)]
pub struct LoggingTerminal {
    columns: usize,
    rows: usize,
    writes: Vec<String>,
}

impl LoggingTerminal {
    /// Create a logging terminal with the given dimensions.
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns,
            rows,
            writes: Vec::new(),
        }
    }

    /// Resize the terminal. The caller is responsible for requesting a render
    /// afterward (pi wires this through the resize handler).
    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
    }

    /// Concatenation of every recorded `write()` since the last clear.
    pub fn get_writes(&self) -> String {
        self.writes.concat()
    }

    /// Drop all recorded writes (equivalent to `clearWrites()`).
    pub fn clear_writes(&mut self) {
        self.writes.clear();
    }
}

impl Terminal for LoggingTerminal {
    fn columns(&self) -> usize {
        self.columns
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn write(&mut self, data: &str) {
        self.writes.push(data.to_string());
    }
    // hide/show cursor and start/stop bypass the logged stream, exactly as pi's
    // VirtualTerminal routes them straight to xterm rather than through write().
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn start(&mut self) {}
    fn stop(&mut self) {}
}

/// Real terminal backend that emits ANSI to a [`std::io::Write`] using crossterm
/// for cursor visibility and bracketed paste. Dimensions come from
/// `crossterm::terminal::size()` unless explicitly overridden.
pub struct CrosstermTerminal<W: Write> {
    out: W,
    columns: usize,
    rows: usize,
}

impl<W: Write> CrosstermTerminal<W> {
    /// Build a backend over `out`, querying the current terminal size. Falls
    /// back to 80x24 if the size cannot be determined (e.g. not a TTY).
    pub fn new(out: W) -> Self {
        let (columns, rows) = crossterm::terminal::size()
            .map(|(c, r)| (c as usize, r as usize))
            .unwrap_or((80, 24));
        Self { out, columns, rows }
    }

    /// Build a backend with explicit dimensions (useful when the size is known
    /// out of band, or `out` is not the controlling terminal).
    pub fn with_size(out: W, columns: usize, rows: usize) -> Self {
        Self { out, columns, rows }
    }

    /// Update the cached dimensions (call from a SIGWINCH/resize handler).
    pub fn set_size(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
    }
}

impl<W: Write> Terminal for CrosstermTerminal<W> {
    fn columns(&self) -> usize {
        self.columns
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn write(&mut self, data: &str) {
        let _ = self.out.write_all(data.as_bytes());
        let _ = self.out.flush();
    }
    fn hide_cursor(&mut self) {
        let _ = crossterm::execute!(self.out, crossterm::cursor::Hide);
    }
    fn show_cursor(&mut self) {
        let _ = crossterm::execute!(self.out, crossterm::cursor::Show);
    }
    fn start(&mut self) {
        let _ = crossterm::execute!(self.out, crossterm::event::EnableBracketedPaste);
    }
    fn stop(&mut self) {
        let _ = crossterm::execute!(self.out, crossterm::event::DisableBracketedPaste);
    }
}
