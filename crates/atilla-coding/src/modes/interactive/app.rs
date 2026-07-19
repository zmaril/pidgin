//! The interactive app-shell composition + the offline faux-turn run loop
//! (Unit 4, PR-4B).
//!
//! [`InteractiveShell`] builds pi's `InteractiveMode` container tree
//! (`modes/interactive/interactive-mode.ts:701-713`) on a
//! `Tui<ProcessTerminal>`: the header, loaded-resources, chat message list,
//! pending, status, editor, and footer regions in pi's exact child order, with
//! the [`Editor`] mounted as the focused prompt via
//! [`mount_focused_editor`](atilla_tui::mount_focused_editor). Header, loaded
//! resources, pending, status, and footer are **placeholders** here — a single
//! text line each — because that chrome (the real header/status/footer
//! components) is PR-4C; the load-bearing region for this slice is the chat
//! message list.
//!
//! The turn flow bypasses `AgentSession` (not yet ported) and drives
//! [`run_agent_loop`](atilla_agent::agent_loop::run_agent_loop) directly with the
//! faux provider, entirely offline. Per the locked `!Send` seam, turn execution
//! runs on a worker thread ([`TurnDriver`]); the editor's submit handler pushes
//! the user bubble and forwards the prompt to the worker, and the run loop drains
//! the worker's [`AgentEvent`] stream each frame and routes it to the chat region
//! via [`ChatState`].
//!
//! The core (`run_events`) is generic over the output sink and driven by supplied
//! [`ShellEvent`]s, so it runs headless in CI over an in-memory sink; the live
//! [`InteractiveShell::run`] adds the stdin reader thread and the channel pump
//! for a real TTY.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use atilla_agent::types::AgentEvent;
use atilla_tui::{
    mount_focused_editor, Editor, EditorOptions, EditorTheme, InputListenerResult, ProcessTerminal,
    RenderError, RunLoop, SelectListTheme, SharedLines, Terminal, TerminalInput, Tui,
};

use super::routing::{ChatRegion, ChatState};
use super::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use super::turn::{TurnCommand, TurnDriver};

/// The default 256-color interactive theme, embedded so the shell needs no theme
/// file on disk. Byte-identical to pi's `dark.json` (the PR-4A vector source).
const DARK_THEME_JSON: &str = include_str!("theme/dark.json");

/// Input-completion flush timeout (ms). Mirrors the run loop's private
/// `INPUT_FLUSH_TIMEOUT_MS`: a stalled escape sequence is flushed after this.
const INPUT_FLUSH_TIMEOUT_MS: u64 = 10;

/// Keyboard-protocol negotiation fragment timeout (ms). Mirrors the run loop's
/// `KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS`.
const NEGOTIATION_FRAGMENT_TIMEOUT_MS: u64 = 150;

/// The window within which a repeated Ctrl-C / Escape counts as a double press.
const DOUBLE_PRESS_WINDOW: Duration = Duration::from_millis(1000);

/// An event the interactive shell processes. Unlike the run loop's `LoopEvent`
/// this also carries [`ShellEvent::Agent`], the `Send` turn-worker output the
/// main thread drains and routes.
pub enum ShellEvent {
    /// A raw byte chunk read from stdin.
    Bytes(Vec<u8>),
    /// A terminal resize to `(columns, rows)`.
    Resize(usize, usize),
    /// A core agent event forwarded from the turn worker.
    Agent(AgentEvent),
    /// An explicit shutdown request.
    Shutdown,
}

/// The interactive shell: the composed container tree, the shared chat-region
/// state, and the offline turn worker, over a `Tui<ProcessTerminal<W>>`.
pub struct InteractiveShell<W: Write> {
    run_loop: RunLoop<W>,
    chat_state: Rc<RefCell<ChatState>>,
    #[allow(dead_code)]
    turn: TurnDriver,
    evt_tx: Sender<ShellEvent>,
    evt_rx: Receiver<ShellEvent>,
}

impl<W: Write> InteractiveShell<W> {
    /// Build the shell over `terminal`, composing pi's container tree and wiring
    /// the editor submit handler to the turn worker.
    pub fn new(terminal: ProcessTerminal<W>) -> Self {
        let rows = terminal.rows();
        let theme = default_theme();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let mut tui = Tui::new(terminal, true);

        // (1) header — placeholder chrome (PR-4C).
        let header = SharedLines::new();
        header.set(vec![
            "atilla interactive shell (offline faux turn)".to_string(),
            "type a message and press Enter; Ctrl-C twice / Esc twice / Ctrl-D to exit".to_string(),
            String::new(),
        ]);
        tui.add_child(Box::new(header));

        // (2) loaded resources — placeholder chrome (PR-4C).
        tui.add_child(Box::new(SharedLines::new()));

        // (3) chat message list — the load-bearing region.
        let entries = Rc::new(RefCell::new(Vec::new()));
        tui.add_child(Box::new(ChatRegion::new(Rc::clone(&entries))));

        // (4) pending messages — placeholder (PR-4C).
        tui.add_child(Box::new(SharedLines::new()));

        // (5) status — placeholder chrome (PR-4C); the router updates this line.
        let status = SharedLines::new();
        let status_handle = status.handle();
        tui.add_child(Box::new(status));

        // (6) widget-above — deferred extension slot; empty placeholder.
        tui.add_child(Box::new(SharedLines::new()));

        // The shared chat-region state the router mutates and the submit handler
        // appends user bubbles to.
        let chat_state = Rc::new(RefCell::new(ChatState::new(
            Rc::clone(&entries),
            status_handle,
            theme.clone(),
            cwd,
        )));

        // The turn worker (offline faux) and the event channel it forwards to.
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<ShellEvent>();
        let turn = TurnDriver::spawn(evt_tx.clone());

        // (7) editor — the focused prompt. Submit pushes the user bubble and
        // forwards the prompt to the worker (pi's `setupEditorSubmitHandler` ->
        // `session.prompt`, here `-> run_agent_loop` on the worker).
        let mut editor = Editor::new(editor_theme(), EditorOptions::default());
        editor.set_terminal_rows(rows);
        let submit_state = Rc::clone(&chat_state);
        let cmd_tx = turn.sender();
        editor.on_submit = Some(Box::new(move |line: String| {
            if line.is_empty() {
                return;
            }
            submit_state.borrow_mut().push_user_message(&line);
            let _ = cmd_tx.send(TurnCommand::Prompt(line));
        }));
        let editor = Rc::new(RefCell::new(editor));
        mount_focused_editor(&mut tui, Rc::clone(&editor));

        // (8) widget-below — deferred extension slot; empty placeholder.
        tui.add_child(Box::new(SharedLines::new()));

        // (9) footer — placeholder chrome (PR-4C).
        let footer = SharedLines::new();
        footer.set(vec![
            String::new(),
            "faux provider - no network".to_string(),
        ]);
        tui.add_child(Box::new(footer));

        let mut run_loop = RunLoop::new(tui);
        install_exit_policy(&mut run_loop);

        Self {
            run_loop,
            chat_state,
            turn,
            evt_tx,
            evt_rx,
        }
    }

    /// Shared access to the renderer (e.g. to inspect rendered output in tests).
    pub fn run_loop(&self) -> &RunLoop<W> {
        &self.run_loop
    }

    /// A sender for injecting [`ShellEvent`]s (e.g. a test feeding a faux turn's
    /// events, or the live stdin reader).
    pub fn event_sender(&self) -> Sender<ShellEvent> {
        self.evt_tx.clone()
    }

    /// Drive the shell from a scripted event sequence. The headless, deterministic
    /// entry point used by tests: it exercises the same feed / route / render
    /// paths as the live loop, minus the stdin thread and wall-clock timers.
    pub fn run_events<I>(&mut self, events: I) -> Result<(), RenderError>
    where
        I: IntoIterator<Item = ShellEvent>,
    {
        let exit = self.run_loop.exit_flag();
        self.run_loop.start()?;
        for event in events {
            if exit.get() {
                break;
            }
            self.process_event(event)?;
            if exit.get() {
                break;
            }
        }
        self.run_loop.stop();
        Ok(())
    }

    /// Process one shell event: feed stdin bytes, apply a resize, or route an
    /// agent event to the chat region and re-render.
    fn process_event(&mut self, event: ShellEvent) -> Result<(), RenderError> {
        match event {
            ShellEvent::Bytes(bytes) => self.run_loop.feed_bytes(&bytes),
            ShellEvent::Resize(columns, rows) => self.run_loop.resize(columns, rows),
            ShellEvent::Agent(event) => {
                self.chat_state.borrow_mut().handle_event(&event);
                self.render()
            }
            ShellEvent::Shutdown => {
                self.run_loop.request_exit();
                Ok(())
            }
        }
    }

    /// Force a redraw and flush (used after a routed agent event mutates the
    /// chat region out of band from any input).
    fn render(&mut self) -> Result<(), RenderError> {
        self.run_loop.tui_mut().request_render(true);
        self.run_loop.tui_mut().flush()
    }
}

impl InteractiveShell<std::io::Stdout> {
    /// Run a live interactive session over the real controlling terminal: start
    /// the renderer, spawn the stdin reader thread, and pump the unified event
    /// channel (stdin bytes + forwarded agent events) until exit. The run loop's
    /// `Drop` guard restores the terminal on every exit path.
    ///
    /// Live terminal-resize polling is intentionally omitted for this offline
    /// demo (the headless `run_events` path still handles [`ShellEvent::Resize`]);
    /// it lands with the real chrome in a later unit.
    pub fn run(&mut self) -> Result<(), RenderError> {
        self.run_loop.start()?;

        // Stdin reader thread (pi's `process.stdin.on("data")`): forward each raw
        // chunk as a `ShellEvent::Bytes`; stdin EOF becomes a `Shutdown`.
        let bytes_tx = self.evt_tx.clone();
        let end_tx = self.evt_tx.clone();
        let reader = atilla_tui::StdinReader::spawn(
            std::io::stdin(),
            move |chunk| bytes_tx.send(ShellEvent::Bytes(chunk)).is_ok(),
            move || {
                let _ = end_tx.send(ShellEvent::Shutdown);
            },
        );

        let result = self.pump();

        reader.stop();
        self.run_loop.stop();
        result
    }

    /// The live channel pump. Mirrors the run loop's private `run_channel`,
    /// arming pi's fragment timers via `recv_timeout`, but drains the shell's
    /// unified [`ShellEvent`] channel (stdin bytes + forwarded agent events).
    fn pump(&mut self) -> Result<(), RenderError> {
        // straitjacket-allow-file:duplication — this pump faithfully mirrors
        // `atilla_tui::app::RunLoop::run_channel` (timer arming + recv_timeout +
        // dispatch); it re-implements it here only to interleave the agent-event
        // channel the private method cannot see. The two are intentional mirrors.
        let exit = self.run_loop.exit_flag();
        loop {
            if exit.get() {
                break;
            }
            let event = match self.pending_timeout() {
                Some(timeout) => match self.evt_rx.recv_timeout(timeout) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match self.evt_rx.recv() {
                    Ok(event) => Some(event),
                    Err(_) => break,
                },
            };
            match event {
                Some(ShellEvent::Bytes(bytes)) => self.run_loop.feed_bytes(&bytes)?,
                Some(ShellEvent::Resize(columns, rows)) => self.run_loop.resize(columns, rows)?,
                Some(ShellEvent::Agent(agent_event)) => {
                    self.chat_state.borrow_mut().handle_event(&agent_event);
                    self.render()?;
                }
                Some(ShellEvent::Shutdown) => break,
                None => self.fire_pending_timeout()?,
            }
        }
        Ok(())
    }

    /// The timeout to arm before the next `recv`, matching pi's fragment timers.
    fn pending_timeout(&self) -> Option<Duration> {
        let terminal = self.run_loop.tui().terminal();
        if terminal.has_pending_negotiation() {
            Some(Duration::from_millis(NEGOTIATION_FRAGMENT_TIMEOUT_MS))
        } else if terminal.has_pending_input() {
            Some(Duration::from_millis(INPUT_FLUSH_TIMEOUT_MS))
        } else {
            None
        }
    }

    /// Fire whichever flush timer is due and dispatch anything it surfaces.
    fn fire_pending_timeout(&mut self) -> Result<(), RenderError> {
        if self.run_loop.tui().terminal().has_pending_input() {
            let inputs = self.run_loop.tui_mut().terminal_mut().flush_input_timeout();
            self.dispatch_inputs(inputs)
        } else if self.run_loop.tui().terminal().has_pending_negotiation() {
            let inputs = self
                .run_loop
                .tui_mut()
                .terminal_mut()
                .flush_negotiation_timeout();
            self.dispatch_inputs(inputs)
        } else {
            Ok(())
        }
    }

    /// Dispatch decoded inputs to the focused component, then flush.
    fn dispatch_inputs(&mut self, inputs: Vec<TerminalInput>) -> Result<(), RenderError> {
        let exit = self.run_loop.exit_flag();
        let tui = self.run_loop.tui_mut();
        for input in inputs {
            tui.handle_input(&input.as_delivered());
            if exit.get() {
                break;
            }
        }
        tui.flush()
    }
}

/// Build the default 256-color interactive theme from the embedded dark theme.
fn default_theme() -> Theme {
    let theme_json = parse_theme_json(DARK_THEME_JSON).expect("embedded dark.json parses");
    create_theme(&theme_json, Some(ColorMode::Color256), None).expect("create dark theme")
}

/// A minimal editor theme (dim border, plain autocomplete) — enough for a real
/// prompt. Mirrors the echo shell's theme.
fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
        select_list: SelectListTheme {
            selected_prefix: Box::new(|t: &str| format!("\x1b[36m{t}\x1b[39m")),
            selected_text: Box::new(|t: &str| format!("\x1b[36m{t}\x1b[39m")),
            description: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
            scroll_info: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
            no_match: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
        },
    }
}

/// Install the shell-level exit policy as an input listener (pi's
/// `addInputListener`): Ctrl-C and Esc require a double press within the window;
/// Ctrl-D exits immediately. All are consumed so they never reach the editor.
fn install_exit_policy<W: Write>(run_loop: &mut RunLoop<W>) {
    use std::time::Instant;
    let exit = run_loop.exit_flag();
    let mut last_ctrl_c: Option<Instant> = None;
    let mut last_escape: Option<Instant> = None;
    run_loop
        .tui_mut()
        .add_input_listener(move |data: &str| match data {
            "\x03" => {
                let now = Instant::now();
                let doubled = last_ctrl_c
                    .map(|t| now.duration_since(t) <= DOUBLE_PRESS_WINDOW)
                    .unwrap_or(false);
                if doubled {
                    exit.set(true);
                } else {
                    last_ctrl_c = Some(now);
                }
                InputListenerResult::consumed()
            }
            "\x1b" => {
                let now = Instant::now();
                let doubled = last_escape
                    .map(|t| now.duration_since(t) <= DOUBLE_PRESS_WINDOW)
                    .unwrap_or(false);
                if doubled {
                    exit.set(true);
                } else {
                    last_escape = Some(now);
                }
                InputListenerResult::consumed()
            }
            "\x04" => {
                exit.set(true);
                InputListenerResult::consumed()
            }
            _ => InputListenerResult::pass(),
        });
}
