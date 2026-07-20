//! Interactive run loop: the live stdin input-pump, input-dispatch, and
//! resize/teardown layer that turns the caller-driven renderer + terminal into a
//! self-running interactive session. This is the Rust port of pi's TUI input
//! pump (`vendor/pi/packages/tui/src/tui.ts` — the `start`/`handleInput` stdin
//! subscription and `stdout.on("resize")` handler) plus the stdin subscription
//! of pi's `ProcessTerminal` (`terminal.ts` — `process.stdin.on("data")`).
//!
//! The pieces map onto pi as follows:
//!
//! * [`StdinReader`] is pi's `process.stdin` subscription: a dedicated thread
//!   doing blocking `read(2)` and delivering raw byte chunks to a sink. pi's
//!   Node event loop delivers `"data"` events; a reader thread + channel is the
//!   faithful Rust analogue.
//! * [`RunLoop`] is pi's `TUI` input pump: it feeds raw bytes to
//!   [`ProcessTerminal::feed`] (which owns the Kitty/paste/CSI-u byte transducer,
//!   so we never touch crossterm's `event::read`), dispatches each decoded
//!   [`TerminalInput`] to the focused component via [`Tui::handle_input`], drives
//!   the input/negotiation flush timers pi arms, honors the renderer's ~16 ms
//!   render coalescing through [`Tui::flush`], and forwards terminal resizes to
//!   [`ProcessTerminal::set_size`] with a forced full redraw.
//!
//! Teardown is the top risk: raw mode / Kitty / bracketed paste must always be
//! restored or a crash wedges the terminal. [`RunLoop`] guarantees it two ways —
//! a `Drop` guard that runs [`Tui::stop`] on every exit path (normal return or
//! panic unwind), and a panic hook installed by [`RunLoop::run`] that restores
//! the terminal directly even if unwinding is skipped.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::editor::Editor;
use crate::overlay::ComponentId;
use crate::renderer::{Component, RenderError, Tui};
use crate::terminal::{
    ProcessTerminal, Terminal, TerminalInput, KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS,
};
use crate::terminal_colors::{RgbColor, TerminalColorScheme};

/// StdinBuffer sequence-completion timeout in milliseconds. Mirrors
/// [`crate::StdinBufferOptions`]'s default (`timeout_ms = 10`): pi arms this
/// short timer when a raw read leaves an incomplete escape sequence buffered.
const INPUT_FLUSH_TIMEOUT_MS: u64 = 10;

/// Read-buffer size for the stdin reader thread. Any size works — `feed`
/// reassembles split sequences — so this is just a batching convenience.
const READ_CHUNK_BYTES: usize = 4096;

/// Default resize-poll cadence for [`RunLoop::run`]. pi is notified of resizes by
/// `stdout.on("resize")` (SIGWINCH under the hood); to stay dependency-free and
/// portable, the Rust run loop instead polls the terminal size on this cadence
/// and emits a [`LoopEvent::Resize`] when it changes. The dispatch that a resize
/// triggers (`set_size` + forced full redraw) is identical to pi's handler.
const RESIZE_POLL_MS: u64 = 150;

/// An event the [`RunLoop`] processes. The production loop receives these over a
/// channel fed by the stdin reader and the resize poller; tests feed a scripted
/// sequence directly through [`RunLoop::run_events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopEvent {
    /// A raw byte chunk read from stdin (pi's `process.stdin` `"data"`).
    Bytes(Vec<u8>),
    /// A terminal resize to `(columns, rows)` (pi's `stdout.on("resize")`).
    Resize(usize, usize),
    /// An explicit shutdown request; the loop stops after processing it.
    Shutdown,
}

/// A render-tree child that draws a shared [`Editor`]. The editor's own
/// [`Component::render`] is empty because its real render (`render_lines`) needs
/// `&mut self`; this wrapper bridges that via interior mutability so an editor
/// can live in the render tree. Register the *same* `Rc<RefCell<Editor>>` as the
/// focus target (see [`mount_focused_editor`]) so input dispatch and rendering
/// share one editor. This is the composition primitive the interactive shell
/// reuses instead of reinventing it.
pub struct EditorView(pub Rc<RefCell<Editor>>);

impl Component for EditorView {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render_lines(width)
    }
}

/// Compose `editor` as the focused child of `tui`: add an [`EditorView`] render
/// child backed by `editor`, register the same `Rc<RefCell<Editor>>` as a
/// focusable component, and focus it. Returns the assigned [`ComponentId`]. This
/// is the minimal "editor as the prompt" wiring shared by the echo shell and the
/// interactive shell.
pub fn mount_focused_editor<T: Terminal>(
    tui: &mut Tui<T>,
    editor: Rc<RefCell<Editor>>,
) -> ComponentId {
    tui.add_child(Box::new(EditorView(Rc::clone(&editor))));
    let component: Rc<RefCell<dyn Component>> = editor;
    let id = tui.register_component(component);
    tui.set_focus(Some(id));
    id
}

/// A dedicated thread that pulls raw bytes from a blocking source (`fd 0` in
/// production) and hands each chunk to a sink callback. This is the Rust analogue
/// of pi's `process.stdin.on("data")` subscription.
///
/// # Shutdown
///
/// The reader exits cleanly on any of: end-of-input (`read` returns `0`), the
/// sink returning `false` (its receiver was dropped), or [`StdinReader::stop`]
/// being observed after a read completes. A `read` blocked with no input pending
/// cannot be unblocked portably, so [`Drop`] signals the stop flag but does *not*
/// join a possibly-blocked thread — it is detached and exits at the next read
/// boundary (or process exit). Terminal teardown never depends on the reader
/// thread having exited (that is [`RunLoop`]'s job), so a lingering blocked read
/// cannot wedge the terminal.
pub struct StdinReader {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StdinReader {
    /// Spawn a reader over `source`, delivering each raw byte chunk to `sink`.
    /// `sink` returns `false` to request the reader stop (e.g. its channel
    /// receiver was dropped). `on_end` runs exactly once when the read loop ends
    /// for any reason (EOF, error, stop, or receiver gone), letting the run loop
    /// treat stdin end-of-input as a shutdown even while other channel senders
    /// (e.g. the resize poller) are still alive.
    pub fn spawn<R, S, E>(mut source: R, mut sink: S, on_end: E) -> Self
    where
        R: Read + Send + 'static,
        S: FnMut(Vec<u8>) -> bool + Send + 'static,
        E: FnOnce() + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("pidgin-tui-stdin".to_string())
            .spawn(move || {
                let mut buf = [0u8; READ_CHUNK_BYTES];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match source.read(&mut buf) {
                        Ok(0) => break, // EOF: the write end closed.
                        Ok(n) => {
                            if thread_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            if !sink(buf[..n].to_vec()) {
                                break; // Receiver gone.
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                on_end();
            })
            .expect("spawn stdin reader thread");
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Signal the reader to stop. Takes effect after the next `read` returns; a
    /// blocked read is not interrupted (see the type-level shutdown note).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether the reader thread has finished.
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .map(JoinHandle::is_finished)
            .unwrap_or(true)
    }

    /// Signal stop and join the reader thread. Only call this when the source is
    /// known to reach EOF (e.g. a closed pipe in tests); on a live TTY a blocked
    /// read would make this hang, which is why [`Drop`] does not join.
    pub fn join(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StdinReader {
    fn drop(&mut self) {
        // Signal stop but detach: a read blocked with no pending input cannot be
        // interrupted portably, and terminal teardown does not depend on this
        // thread having exited. The thread ends at its next read boundary.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Restore the controlling terminal to a sane cooked state, best-effort. Emits
/// exactly the sequences [`ProcessTerminal::stop`] would (show cursor, disable
/// bracketed paste, pop the Kitty keyboard protocol, disable `modifyOtherKeys`)
/// and disables raw mode. Safe to call from a panic hook: it touches only the
/// process-global terminal, never the (possibly poisoned) `Tui`.
fn restore_terminal_best_effort() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut out = std::io::stdout();
    // Show cursor, bracketed paste off, Kitty protocol pop, modifyOtherKeys off.
    let _ = out.write_all(b"\x1b[?25h\x1b[?2004l\x1b[<u\x1b[>4;0m");
    let _ = out.flush();
}

/// The interactive run loop over a [`ProcessTerminal`]-backed [`Tui`]. Composes
/// the already-ported renderer, terminal, and components into a running session:
/// raw bytes in, edits + renders out, clean teardown on exit.
///
/// The core (`start`, `feed_bytes`, `resize`, `run_events`) is generic over the
/// output sink `W` and driven entirely by supplied events, so it runs headless
/// in CI over an in-memory sink with scripted input. [`RunLoop::run`] adds the
/// live wiring (stdin reader thread, resize poller, timers, panic hook) for a
/// real TTY.
pub struct RunLoop<W: Write> {
    tui: Tui<ProcessTerminal<W>>,
    should_exit: Rc<Cell<bool>>,
    /// Bytes left over from a UTF-8 sequence split across reads, prepended to the
    /// next chunk so `feed` always sees valid UTF-8.
    utf8_tail: Vec<u8>,
    started: bool,
    stopped: bool,
}

impl<W: Write> RunLoop<W> {
    /// Build a run loop around a `Tui<ProcessTerminal<W>>`. The caller composes
    /// the component tree, registers the focused component, and installs any
    /// input listeners (e.g. the exit-policy listener) on the `Tui` before
    /// running.
    pub fn new(tui: Tui<ProcessTerminal<W>>) -> Self {
        Self {
            tui,
            should_exit: Rc::new(Cell::new(false)),
            utf8_tail: Vec::new(),
            started: false,
            stopped: false,
        }
    }

    /// A handle to the shared exit flag. The shell captures a clone (e.g. inside
    /// an [`crate::InputListenerResult`]-returning input listener) and sets it to
    /// request a clean shutdown; the loop checks it after every dispatched input.
    pub fn exit_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.should_exit)
    }

    /// Request a clean shutdown of the loop.
    pub fn request_exit(&self) {
        self.should_exit.set(true);
    }

    /// Shared access to the renderer (e.g. to inspect rendered output in tests).
    pub fn tui(&self) -> &Tui<ProcessTerminal<W>> {
        &self.tui
    }

    /// Mutable access to the renderer (e.g. to compose the tree before running).
    pub fn tui_mut(&mut self) -> &mut Tui<ProcessTerminal<W>> {
        &mut self.tui
    }

    /// Start the session: enter raw mode, negotiate the keyboard protocol, hide
    /// the cursor (via [`Tui::start`]), and render the first frame. Idempotent.
    pub fn start(&mut self) -> Result<(), RenderError> {
        if !self.started {
            self.tui.start();
            self.started = true;
            self.stopped = false;
        }
        self.tui.flush()
    }

    /// Stop the session, restoring the terminal (leave raw mode, disable
    /// Kitty/paste, show cursor) via [`Tui::stop`]. Idempotent; also run by the
    /// `Drop` guard.
    pub fn stop(&mut self) {
        if self.started && !self.stopped {
            self.tui.stop();
            self.stopped = true;
        }
    }

    /// Feed one raw byte chunk (pi's `stdin` `"data"`): reassemble any split
    /// UTF-8, run it through [`ProcessTerminal::feed`], dispatch the resulting
    /// inputs, and flush a coalesced render.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<(), RenderError> {
        let mut combined = std::mem::take(&mut self.utf8_tail);
        combined.extend_from_slice(bytes);
        let text = match std::str::from_utf8(&combined) {
            Ok(s) => s.to_string(),
            Err(err) => {
                let valid = err.valid_up_to();
                // Keep the trailing incomplete code point for the next chunk.
                self.utf8_tail = combined[valid..].to_vec();
                String::from_utf8_lossy(&combined[..valid]).into_owned()
            }
        };
        let inputs = self.tui.terminal_mut().feed(&text);
        self.dispatch(inputs)
    }

    /// Dispatch a batch of decoded inputs to the focused component (via
    /// [`Tui::handle_input`], which also runs the input listeners), then flush.
    fn dispatch(&mut self, inputs: Vec<TerminalInput>) -> Result<(), RenderError> {
        for input in inputs {
            // `as_delivered` re-wraps pastes in bracketed-paste markers, matching
            // the exact bytes pi's `inputHandler` (and thus the editor) receives.
            self.tui.handle_input(&input.as_delivered());
            if self.should_exit.get() {
                break;
            }
        }
        self.tui.flush()
    }

    /// Apply a terminal resize: update the cached size and force a full redraw,
    /// ported from pi's `stdout.on("resize")` handler.
    pub fn resize(&mut self, columns: usize, rows: usize) -> Result<(), RenderError> {
        self.tui.terminal_mut().set_size(columns, rows);
        self.tui.request_render(true);
        self.tui.flush()
    }

    /// The timeout to arm before the next `recv`, matching pi's fragment timers:
    /// a buffered negotiation reply gets the 150 ms Kitty-fragment timeout, a
    /// stalled input sequence gets the 10 ms completion timeout, otherwise block.
    fn pending_timeout(&self) -> Option<Duration> {
        let terminal = self.tui.terminal();
        if terminal.has_pending_negotiation() {
            Some(Duration::from_millis(
                KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS,
            ))
        } else if terminal.has_pending_input() {
            Some(Duration::from_millis(INPUT_FLUSH_TIMEOUT_MS))
        } else {
            None
        }
    }

    /// Fire whichever flush timer is due (input completion first, then a buffered
    /// negotiation reply) and dispatch anything it surfaces.
    fn fire_pending_timeout(&mut self) -> Result<(), RenderError> {
        if self.tui.terminal().has_pending_input() {
            let inputs = self.tui.terminal_mut().flush_input_timeout();
            self.dispatch(inputs)
        } else if self.tui.terminal().has_pending_negotiation() {
            let inputs = self.tui.terminal_mut().flush_negotiation_timeout();
            self.dispatch(inputs)
        } else {
            Ok(())
        }
    }

    /// Query the terminal's default background color with OSC 11
    /// (`ESC ] 11 ; ? BEL`), synchronously. The Rust port of pi's
    /// `TUI.queryTerminalBackgroundColor({ timeoutMs })`: pi returns a promise
    /// resolved by the Node event loop; pidgin's stack is fully synchronous, so
    /// this instead drives the existing input pump ([`RunLoop::feed_bytes`] ->
    /// [`Tui::handle_input`], which consumes the OSC 11 response) over `rx` until
    /// the pending query settles or `timeout` elapses. The bytes written, the
    /// query semantics, and the timeout are identical to pi; only the async shell
    /// is dropped. Returns the parsed [`RgbColor`], or `None` on timeout / parse
    /// failure.
    ///
    /// `rx` is the run loop's own event channel (the one [`RunLoop::run`] pumps),
    /// so the reader thread feeding it is shared with the main loop — a single
    /// stdin reader, never an abandoned one.
    pub fn query_terminal_background_color(
        &mut self,
        rx: &Receiver<LoopEvent>,
        timeout: Duration,
    ) -> Option<RgbColor> {
        self.tui.write_terminal_background_query();
        self.pump_until(rx, timeout, |tui| tui.terminal_background_query_settled());
        if !self.tui.terminal_background_query_settled() {
            self.tui.settle_terminal_background_timeout();
        }
        self.tui.take_terminal_background_reply()
    }

    /// Query the terminal's color-scheme preference with DSR (`CSI ? 996 n`),
    /// synchronously. The Rust port of pi's
    /// `TUI.queryTerminalColorScheme({ timeoutMs })`: drives the input pump over
    /// `rx` until a DEC 2031 color-scheme report is consumed (settling the pending
    /// query via the color-scheme listener path) or `timeout` elapses. Returns the
    /// reported [`TerminalColorScheme`], or `None` on timeout.
    pub fn query_terminal_color_scheme(
        &mut self,
        rx: &Receiver<LoopEvent>,
        timeout: Duration,
    ) -> Option<TerminalColorScheme> {
        self.tui.write_terminal_color_scheme_query();
        self.pump_until(rx, timeout, |tui| tui.terminal_color_scheme_query_settled());
        if !self.tui.terminal_color_scheme_query_settled() {
            self.tui.settle_terminal_color_scheme_timeout();
        }
        self.tui.take_terminal_color_scheme_reply()
    }

    /// Drive the input pump over `rx` until `settled` reports the awaited query
    /// resolved or the `timeout` deadline passes. Blocks on `recv_timeout` (never
    /// busy-waits) and routes each event through the same
    /// [`RunLoop::feed_bytes`]/[`RunLoop::resize`] handlers the main loop uses, so
    /// the OSC 11 / DSR responses reach [`Tui::handle_input`] and settle the
    /// pending slot exactly as during normal operation.
    fn pump_until<F>(&mut self, rx: &Receiver<LoopEvent>, timeout: Duration, mut settled: F)
    where
        F: FnMut(&Tui<ProcessTerminal<W>>) -> bool,
    {
        let deadline = Instant::now() + timeout;
        while !settled(&self.tui) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(LoopEvent::Bytes(bytes)) => {
                    if self.feed_bytes(&bytes).is_err() {
                        break;
                    }
                }
                Ok(LoopEvent::Resize(columns, rows)) => {
                    if self.resize(columns, rows).is_err() {
                        break;
                    }
                }
                Ok(LoopEvent::Shutdown) => break,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Drive the loop from a scripted event sequence. This is the headless,
    /// deterministic entry point used by tests: it exercises the exact same
    /// dispatch, resize, and exit-checking paths as the live loop, minus the
    /// stdin/resize threads and wall-clock timers. Runs [`RunLoop::start`] first
    /// and [`RunLoop::stop`] on the way out.
    pub fn run_events<I>(&mut self, events: I) -> Result<(), RenderError>
    where
        I: IntoIterator<Item = LoopEvent>,
    {
        self.start()?;
        for event in events {
            if self.should_exit.get() {
                break;
            }
            match event {
                LoopEvent::Bytes(bytes) => self.feed_bytes(&bytes)?,
                LoopEvent::Resize(columns, rows) => self.resize(columns, rows)?,
                LoopEvent::Shutdown => break,
            }
            if self.should_exit.get() {
                break;
            }
        }
        self.stop();
        Ok(())
    }

    /// Drive the loop from a live event channel, arming pi's fragment timers via
    /// `recv_timeout`. Exits when the exit flag is set, a [`LoopEvent::Shutdown`]
    /// arrives, or every sender is dropped (stdin EOF). Assumes the session was
    /// already started.
    fn run_channel(&mut self, rx: &Receiver<LoopEvent>) -> Result<(), RenderError> {
        loop {
            if self.should_exit.get() {
                break;
            }
            let event = match self.pending_timeout() {
                Some(timeout) => match rx.recv_timeout(timeout) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(event) => Some(event),
                    Err(_) => break,
                },
            };
            match event {
                Some(LoopEvent::Bytes(bytes)) => self.feed_bytes(&bytes)?,
                Some(LoopEvent::Resize(columns, rows)) => self.resize(columns, rows)?,
                Some(LoopEvent::Shutdown) => break,
                None => self.fire_pending_timeout()?,
            }
        }
        Ok(())
    }
}

impl RunLoop<std::io::Stdout> {
    /// Run a live interactive session over the real controlling terminal: start
    /// the session, spawn the stdin reader thread and resize poller, install the
    /// panic-hook teardown guard, and pump events until exit. Always restores the
    /// terminal on the way out (the `Drop` guard runs [`Tui::stop`]).
    ///
    /// Only exercised on a real TTY; the headless dispatch logic it wraps is
    /// covered by [`RunLoop::run_events`] over an in-memory sink in CI.
    pub fn run(&mut self) -> Result<(), RenderError> {
        // Panic-hook teardown guard: restore the terminal even if a panic skips
        // the `Drop`-based teardown (e.g. an abort). Chains the previous hook.
        let previous_hook = Arc::new(std::panic::take_hook());
        let hook_prev = Arc::clone(&previous_hook);
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_best_effort();
            hook_prev(info);
        }));

        let result = self.run_live();

        // Restore the previous panic hook.
        let _ = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| previous_hook(info)));

        self.stop();
        result
    }

    fn run_live(&mut self) -> Result<(), RenderError> {
        self.start()?;

        let (tx, rx) = mpsc::channel::<LoopEvent>();

        // Stdin reader thread (pi's `process.stdin.on("data")`). When stdin ends
        // (EOF / redirect closed) the reader signals a shutdown so the loop exits
        // even though the resize poller still holds a channel sender.
        let bytes_tx = tx.clone();
        let end_tx = tx.clone();
        let reader = StdinReader::spawn(
            std::io::stdin(),
            move |chunk| bytes_tx.send(LoopEvent::Bytes(chunk)).is_ok(),
            move || {
                let _ = end_tx.send(LoopEvent::Shutdown);
            },
        );

        // Resize poller (pi's `stdout.on("resize")`, adapted to a size poll).
        let resize_stop = Arc::new(AtomicBool::new(false));
        let resize_handle = spawn_resize_poller(tx.clone(), Arc::clone(&resize_stop));

        // Drop our own retained sender so the channel closes once the threads do.
        drop(tx);

        let result = self.run_channel(&rx);

        // Tear down the helper threads.
        resize_stop.store(true, Ordering::Relaxed);
        reader.stop();
        if let Some(handle) = resize_handle {
            let _ = handle.join();
        }

        result
    }
}

/// Spawn the resize poller thread. Emits [`LoopEvent::Resize`] whenever the
/// terminal size changes. Returns `None` if the initial size query fails (not a
/// TTY), in which case resize events are simply never generated.
fn spawn_resize_poller(tx: Sender<LoopEvent>, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let mut last = crossterm::terminal::size().ok()?;
    thread::Builder::new()
        .name("pidgin-tui-resize".to_string())
        .spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(RESIZE_POLL_MS));
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    if tx
                        .send(LoopEvent::Resize(size.0 as usize, size.1 as usize))
                        .is_err()
                    {
                        break; // Loop finished, receiver dropped.
                    }
                }
            }
        })
        .ok()
}

impl<W: Write> Drop for RunLoop<W> {
    fn drop(&mut self) {
        // RAII teardown guard: guarantee the terminal is restored on every exit
        // path, including panic unwind, even if `stop` was not called explicitly.
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    // --- StdinReader loopback test (Unit 1) --------------------------------

    #[test]
    fn stdin_reader_delivers_bytes_over_a_pipe_then_exits_on_eof() {
        // A connected socket pair stands in for an OS pipe: write to one end,
        // the reader thread reads the other and forwards chunks.
        let (mut writer, reader_end) = UnixStream::pair().expect("socketpair");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (end_tx, end_rx) = mpsc::channel::<()>();
        let reader = StdinReader::spawn(
            reader_end,
            move |chunk| tx.send(chunk).is_ok(),
            move || {
                let _ = end_tx.send(());
            },
        );

        writer.write_all(b"hello").expect("write");
        writer.flush().expect("flush");
        let first = rx.recv().expect("first chunk delivered");
        assert_eq!(first, b"hello".to_vec());

        writer.write_all(b" world").expect("write");
        writer.flush().expect("flush");
        let second = rx.recv().expect("second chunk delivered");
        assert_eq!(second, b" world".to_vec());

        // Closing the write end yields EOF; the reader thread must exit cleanly
        // and fire its end signal exactly once.
        drop(writer);
        reader.join();
        assert_eq!(end_rx.recv(), Ok(()), "on_end must fire on EOF");
        assert!(end_rx.recv().is_err(), "on_end must fire exactly once");
        // Channel closed after the reader dropped its sender.
        assert!(rx.recv().is_err());
    }

    #[test]
    fn stdin_reader_stops_when_sink_receiver_is_dropped() {
        let (mut writer, reader_end) = UnixStream::pair().expect("socketpair");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader = StdinReader::spawn(reader_end, move |chunk| tx.send(chunk).is_ok(), || {});

        // Drop the receiver: the next delivered chunk makes the sink return false.
        drop(rx);
        writer.write_all(b"x").expect("write");
        writer.flush().expect("flush");

        // The reader observes the send failure and exits without needing EOF.
        for _ in 0..200 {
            if reader.is_finished() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(reader.is_finished(), "reader should stop on receiver drop");
    }

    // --- Synchronous terminal-query pump -----------------------------------

    /// A headless run loop over an in-memory sink, matching the `run_events`
    /// tests' construction.
    fn headless_run_loop() -> RunLoop<Vec<u8>> {
        let terminal = ProcessTerminal::with_size(Vec::new(), 80, 24).manage_raw_mode(false);
        RunLoop::new(Tui::new(terminal, false))
    }

    #[test]
    fn query_background_color_resolves_from_pumped_response() {
        // An OSC 11 response already queued on the channel is pumped through
        // feed_bytes -> handle_input, consumed, and parsed before the deadline.
        let mut run_loop = headless_run_loop();
        let (tx, rx) = mpsc::channel::<LoopEvent>();
        tx.send(LoopEvent::Bytes(b"\x1b]11;rgb:ffff/0000/8080\x07".to_vec()))
            .expect("queue response");

        let rgb = run_loop.query_terminal_background_color(&rx, Duration::from_secs(5));
        assert_eq!(
            rgb,
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        );
    }

    #[test]
    fn query_background_color_times_out_to_none() {
        // No response arrives; the pump blocks on recv_timeout until the deadline
        // and resolves to None. `tx` is kept alive so the Timeout arm (not
        // Disconnected) is exercised.
        let mut run_loop = headless_run_loop();
        let (_tx, rx) = mpsc::channel::<LoopEvent>();

        let rgb = run_loop.query_terminal_background_color(&rx, Duration::from_millis(20));
        assert_eq!(rgb, None);
    }

    #[test]
    fn query_color_scheme_resolves_from_pumped_report() {
        // A DEC 2031 color-scheme report queued on the channel settles the DSR
        // color-scheme query.
        let mut run_loop = headless_run_loop();
        let (tx, rx) = mpsc::channel::<LoopEvent>();
        tx.send(LoopEvent::Bytes(b"\x1b[?997;2n".to_vec()))
            .expect("queue report");

        let scheme = run_loop.query_terminal_color_scheme(&rx, Duration::from_secs(5));
        assert_eq!(scheme, Some(TerminalColorScheme::Light));
    }

    #[test]
    fn query_color_scheme_times_out_to_none() {
        let mut run_loop = headless_run_loop();
        let (_tx, rx) = mpsc::channel::<LoopEvent>();

        let scheme = run_loop.query_terminal_color_scheme(&rx, Duration::from_millis(20));
        assert_eq!(scheme, None);
    }
}
