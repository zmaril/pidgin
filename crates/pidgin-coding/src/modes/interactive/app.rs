//! The interactive app-shell composition + the offline faux-turn run loop
//! (Unit 4, PR-4B).
//!
//! [`InteractiveShell`] builds pi's `InteractiveMode` container tree
//! (`modes/interactive/interactive-mode.ts:701-713`) on a
//! `Tui<ProcessTerminal>`: the header, loaded-resources, chat message list,
//! pending, status, editor, and footer regions in pi's exact child order, with
//! the [`Editor`] mounted as the focused prompt via
//! [`mount_focused_editor`](pidgin_tui::mount_focused_editor). Header, loaded
//! resources, pending, status, and footer are **placeholders** here — a single
//! text line each — because that chrome (the real header/status/footer
//! components) is PR-4C; the load-bearing region for this slice is the chat
//! message list.
//!
//! The turn flow drives a **real** [`AgentSession`](crate::core::agent_session)
//! running offline (echo): a typed prompt calls `AgentSession::prompt`, whose real
//! `AgentSessionEvent`s flow back to the message UI, with the assistant reply
//! echoing the last user message. Per the locked `!Send` seam (both the `Tui` and
//! the `AgentSession` are `!Send`), turn execution runs on a worker thread
//! ([`TurnDriver`]) that owns the session; the editor's submit handler pushes the
//! user bubble and forwards the prompt to the worker, and the run loop drains the
//! worker's cloned [`AgentSessionEvent`] stream each frame and routes it to the
//! chat region via [`ChatState`].
//!
//! The core (`run_events`) is generic over the output sink and driven by supplied
//! [`ShellEvent`]s, so it runs headless in CI over an in-memory sink; the live
//! [`InteractiveShell::run`] adds the stdin reader thread and the channel pump
//! for a real TTY.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use pidgin_tui::{
    mount_focused_editor, tui_keybindings, Editor, EditorOptions, EditorTheme, InputListenerResult,
    KeybindingsManager, ProcessTerminal, RenderError, RunLoop, SelectListTheme, SharedLines,
    Terminal, TerminalInput, Tui,
};

use super::components::{FooterComponent, FooterData, IdleStatus};
use super::extension_ui::{InputSource, TuiExtensionUi};
use super::routing::{ChatRegion, ChatState, StatusRegion, StatusSlot, StatusView};
use super::theme::{
    create_theme, parse_theme_json, ActiveTheme, ColorMode, InteractiveThemeController, RgbColor,
    TerminalAutoThemeDetector, TerminalBackgroundThemeDetector, TerminalTheme, Theme,
    ThemeControllerUi, ThemeSource, ThinkingLevel,
};
use super::turn::{TurnCommand, TurnDriver};
use crate::core::agent_session::AgentSessionEvent;
use crate::core::extensions::types::{ExtensionContext, ExtensionUi, NotifyLevel, UiError};
use crate::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
use crate::extensions::llama::{
    create_llama_provider, run_llama_command, LlamaClient, NotifyFn, DEFAULT_LLAMA_SERVER_URL,
};

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
/// this also carries [`ShellEvent::Session`], the `Send` turn-worker output the
/// main thread drains and routes.
pub enum ShellEvent {
    /// A raw byte chunk read from stdin.
    Bytes(Vec<u8>),
    /// A terminal resize to `(columns, rows)`.
    Resize(usize, usize),
    /// A session event forwarded (cloned) from the turn worker's `AgentSession`.
    /// Boxed: `AgentSessionEvent` is large relative to the other variants, so
    /// boxing keeps `ShellEvent` small (clippy `large_enum_variant`).
    Session(Box<AgentSessionEvent>),
    /// An explicit shutdown request.
    Shutdown,
}

/// `Tui<ProcessTerminal<W>>` as the theme controller's UI surface.
///
/// The `invalidate` / `request_render` / `set_terminal_color_scheme_notifications`
/// half is fully live. The terminal-query half (`query_terminal_background_color`
/// / `query_terminal_color_scheme`) reports `None` here: the synchronous OSC 11 /
/// DSR pump lives on [`RunLoop`] and needs its `LoopEvent` receiver, which the
/// shell's own [`ShellEvent`] channel does not expose. Returning `None` makes the
/// detectors fall back to the environment (`COLORFGBG`), exactly as they do on a
/// real query timeout.
///
/// PR follow-up: bridge the shell pump so `apply_from_settings` can issue live
/// OSC 11 / DSR queries (prereq C's [`RunLoop::query_terminal_background_color`] /
/// [`RunLoop::query_terminal_color_scheme`]).
impl<W: Write> TerminalBackgroundThemeDetector for Tui<ProcessTerminal<W>> {
    fn query_terminal_background_color(&mut self, _timeout: Duration) -> Option<RgbColor> {
        None
    }
}

impl<W: Write> TerminalAutoThemeDetector for Tui<ProcessTerminal<W>> {
    fn query_terminal_color_scheme(&mut self, _timeout: Duration) -> Option<TerminalTheme> {
        None
    }
}

impl<W: Write> ThemeControllerUi for Tui<ProcessTerminal<W>> {
    fn invalidate(&mut self) {
        Tui::invalidate(self);
    }
    fn request_render(&mut self) {
        self.request_render(true);
    }
    fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
        Tui::set_terminal_color_scheme_notifications(self, enabled);
    }
}

/// A render-thread shell-intercepted command: an editor line the shell handles
/// itself on the render/main thread rather than forwarding to the turn worker as
/// an [`AgentSession`](crate::core::agent_session) prompt.
///
/// # Why this is a render-thread intercept (a documented pidgin divergence)
///
/// `/llama` is intercepted here and driven **directly on the render/main thread**
/// — it is *not* routed through `AgentSession` extension-dispatch. This is the
/// same class of divergence (and carries the same justification) as pidgin's
/// `/new` · `/resume` · `/fork` shell commands, and is sanctioned by the
/// team-memory policy `builtin-ext-rust-native-policy`: a native builtin may be
/// serviced on the render thread when the ported extension-dispatch path cannot
/// reach the live TUI surface.
///
/// The forcing constraint is pidgin's locked `!Send` thread split: the
/// [`AgentSession`](crate::core::agent_session) (and the extension-runner that
/// would dispatch `/llama`) live on the **worker** thread, while the [`Tui`] —
/// which `run_llama_command` must mount its overlay onto — lives on the
/// **render/main** thread. Threading a live [`TuiExtensionUi`] over the `&mut Tui`
/// out to the worker, mounting an overlay there, and driving its input pump would
/// need a cross-thread interactive mount-and-drive handoff, which does not exist.
/// So the shell mounts the llama TUI where the `Tui` already is: on the render
/// thread, from the pump, once `on_submit` has returned and the `&mut Tui` borrow
/// it held is free.
///
/// ## Future slice (Option A) — NOT to be built ad hoc
///
/// The faithful long-term path is to route `/llama` through `AgentSession`
/// extension-dispatch: a native `ExtensionRunner` + a `create_command_context`
/// that mints a live [`TuiExtensionUi`], plus the cross-thread mount-and-drive
/// handoff that carries the interactive overlay from the worker back to the render
/// thread. That cross-thread interactive handoff is *adjacent* to the parked
/// reentrant / preemptible-primitives packet and must land with it — it must not
/// be built ad hoc.
enum PendingShellCommand {
    /// `/llama` — mount the llama model-manager overlay on the render thread.
    Llama,
}

/// The interactive shell: the composed container tree, the shared chat-region
/// state, the shared active theme, the theme controller, and the offline turn
/// worker, over a `Tui<ProcessTerminal<W>>`.
pub struct InteractiveShell<W: Write> {
    run_loop: RunLoop<W>,
    chat_state: Rc<RefCell<ChatState>>,
    /// The shared active theme. All themed components are built from this handle
    /// at construction; a live post-construction swap does not yet reach them (see
    /// [`InteractiveShell::new`]).
    #[allow(dead_code)]
    active: Rc<ActiveTheme>,
    /// Drives startup theme application and (once wired) live preview / set / auto
    /// sync.
    controller: InteractiveThemeController,
    /// Whether [`InteractiveShell::apply_startup_theme`] has run.
    theme_applied: bool,
    #[allow(dead_code)]
    turn: TurnDriver,
    evt_tx: Sender<ShellEvent>,
    evt_rx: Receiver<ShellEvent>,
    /// The render-thread shell command recorded by the editor's `on_submit` (which
    /// runs while the run loop already holds `&mut Tui`, so it cannot mount inline).
    /// The pump drains it after `on_submit` returns, when the `&mut Tui` is free.
    pending_shell_command: Rc<RefCell<Option<PendingShellCommand>>>,
}

impl<W: Write> InteractiveShell<W> {
    /// Build the shell over `terminal`, composing pi's container tree and wiring
    /// the editor submit handler to the turn worker.
    pub fn new(terminal: ProcessTerminal<W>) -> Self {
        let rows = terminal.rows();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // The shared active theme every themed component is built from. Seeded
        // with the embedded dark theme; the controller's `init` (below) resolves
        // the startup name from settings against the terminal environment.
        let active = Rc::new(ActiveTheme::new(default_theme()));

        let mut tui = Tui::new(terminal, true);

        // (1) header — placeholder chrome (PR-4C).
        let header = SharedLines::new();
        header.set(vec![
            "pidgin interactive shell (offline echo)".to_string(),
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

        // (5) status — the real status region (PR-4C): an `IdleStatus`
        // (two blank lines) by default, swapped to a `WorkingStatusIndicator`
        // while a turn runs. The router (`ChatState`) flips the shared slot.
        let status_slot: StatusSlot = Rc::new(RefCell::new(StatusView::Idle(IdleStatus)));
        tui.add_child(Box::new(StatusRegion::new(Rc::clone(&status_slot))));

        // (6) widget-above — deferred extension slot; empty placeholder.
        tui.add_child(Box::new(SharedLines::new()));

        // The shared chat-region state the router mutates and the submit handler
        // appends user bubbles to.
        let chat_state = Rc::new(RefCell::new(ChatState::new(
            Rc::clone(&entries),
            status_slot,
            active.current().clone(),
            cwd.clone(),
        )));

        // The turn worker (offline faux) and the event channel it forwards to.
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<ShellEvent>();
        let turn = TurnDriver::spawn(evt_tx.clone(), cwd.clone());

        // (7) editor — the focused prompt. Submit pushes the user bubble and
        // forwards the prompt to the worker (pi's `setupEditorSubmitHandler` ->
        // `session.prompt`, here `-> AgentSession::prompt` on the worker).
        let pending_shell_command: Rc<RefCell<Option<PendingShellCommand>>> =
            Rc::new(RefCell::new(None));

        let mut editor = Editor::new(editor_theme(), EditorOptions::default());
        editor.set_terminal_rows(rows);
        let submit_state = Rc::clone(&chat_state);
        let cmd_tx = turn.sender();
        let submit_pending = Rc::clone(&pending_shell_command);
        editor.on_submit = Some(Box::new(move |line: String| {
            if line.is_empty() {
                return;
            }
            // Render-thread shell-intercept: `/llama` is handled by the shell on
            // the render/main thread (NOT routed through the worker's `AgentSession`
            // extension-dispatch) — a documented pidgin divergence, same class and
            // justification as `/new` · `/resume` · `/fork`, sanctioned by the
            // team-memory policy `builtin-ext-rust-native-policy`. See
            // [`PendingShellCommand`] for the full rationale and the Option A future
            // slice. `on_submit` runs while the run loop already holds `&mut Tui`, so
            // it cannot mount the overlay inline; it records a pending render-thread
            // command that the pump drains once that borrow is free. Any args are
            // ignored (`run_llama_command` takes none). Must precede `classify_submit`
            // so `/llama` is intercepted here rather than falling through as a prompt.
            let trimmed = line.trim();
            if trimmed == "/llama" || trimmed.starts_with("/llama ") {
                *submit_pending.borrow_mut() = Some(PendingShellCommand::Llama);
                return;
            }
            // Intercept the session-lifecycle slash commands before a line would
            // otherwise be sent as a prompt (pi routes `/new`, `/resume`, `/fork`
            // through the runtime rather than the model).
            match classify_submit(&line) {
                SubmitAction::Prompt(text) => {
                    submit_state.borrow_mut().push_user_message(&text);
                    let _ = cmd_tx.send(TurnCommand::Prompt(text));
                }
                SubmitAction::NewSession => {
                    submit_state
                        .borrow_mut()
                        .push_notice("Starting a new session.");
                    let _ = cmd_tx.send(TurnCommand::NewSession);
                }
                SubmitAction::Resume(path) => {
                    let _ = cmd_tx.send(TurnCommand::Resume(path));
                }
                SubmitAction::Fork(entry_id) => {
                    let _ = cmd_tx.send(TurnCommand::Fork(entry_id));
                }
                SubmitAction::Notice(message) => {
                    submit_state.borrow_mut().push_notice(&message);
                }
            }
        }));
        let editor = Rc::new(RefCell::new(editor));
        mount_focused_editor(&mut tui, Rc::clone(&editor));

        // (8) widget-below — deferred extension slot; empty placeholder.
        tui.add_child(Box::new(SharedLines::new()));

        // (9) footer — the real `FooterComponent` (PR-4C). Live token/context/cost
        // stats and git branch / session name arrive with the unported
        // `AgentSession`, so those are zeroed / `None` here; cwd is live and the
        // full layout (pwd line + stats line + model) renders even zeroed.
        let footer = FooterComponent::new(footer_data(cwd), active.current().clone());
        tui.add_child(Box::new(footer));

        // The theme controller (pi's `InteractiveThemeController`, constructed at
        // `interactive-mode.ts:482`). Its `on_changed` mirrors pi's
        // `updateEditorBorderColor`: recolor the editor border from the *current*
        // theme (thinking level "off", not in bash mode — the offline shell tracks
        // neither yet). `show_error` is a documented no-op: the shell has no error
        // surface wired (header/status are placeholder chrome), so a theme-load
        // failure is swallowed here; the exact error wording is still exercised by
        // the controller's unit tests.
        let settings = Rc::new(RefCell::new(SettingsManager::in_memory(
            Default::default(),
            SettingsManagerCreateOptions::default(),
        )));
        let on_changed = {
            let active = Rc::clone(&active);
            let editor = Rc::clone(&editor);
            Box::new(move || {
                let theme = active.current().clone();
                editor
                    .borrow_mut()
                    .set_border_color(Box::new(move |t: &str| {
                        theme
                            .get_thinking_border_color(ThinkingLevel::Off, t)
                            .unwrap_or_else(|_| t.to_string())
                    }));
            }) as Box<dyn Fn()>
        };
        let controller = InteractiveThemeController::new(
            Rc::clone(&settings),
            Rc::clone(&active),
            theme_source(),
            Box::new(|_message: &str| { /* no error surface wired yet */ }),
            on_changed,
        );

        let mut run_loop = RunLoop::new(tui);
        install_exit_policy(&mut run_loop);

        Self {
            run_loop,
            chat_state,
            active,
            controller,
            theme_applied: false,
            turn,
            evt_tx,
            evt_rx,
            pending_shell_command,
        }
    }

    /// Apply the saved / auto / detected theme once, at startup (pi awaits
    /// `themeController.applyFromSettings()` after `ui.start()`,
    /// `interactive-mode.ts:722`). Idempotent: only the first call runs.
    ///
    /// **Escape-hatch note.** This swaps the shared [`ActiveTheme`] and recolors
    /// the editor border (via the controller's `on_changed`), which is live. The
    /// chat message / footer / status components, however, snapshot the theme by
    /// value at construction (each bakes `theme.fg` / `theme.bg` into `'static`
    /// closures, diverging from pi's live `theme` Proxy reads), so a theme swapped
    /// in here after they are built does not reach them. Converting those
    /// components to read through the [`ActiveTheme`] handle is a PR follow-up (see
    /// the PR body); the controller and its startup application are landed and
    /// exercised in full.
    fn apply_startup_theme(&mut self) {
        if self.theme_applied {
            return;
        }
        self.theme_applied = true;
        self.controller.apply_from_settings(self.run_loop.tui_mut());
        // pi's `updateEditorBorderColor` (this shell's `on_changed`) ends with
        // `ui.requestRender()`. A `'static` `on_changed` closure cannot hold the
        // `Tui`, so the shell requests the post-application render here, standing in
        // for that call so the startup-applied theme (and recolored editor border)
        // reach the first frame.
        self.run_loop.tui_mut().request_render(true);
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
        self.apply_startup_theme();
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
            ShellEvent::Bytes(bytes) => {
                // Feeding the bytes may fire the editor's `on_submit` (e.g. Enter on
                // a `/llama` line), which records a pending render-thread command;
                // drain it now that the run loop's `&mut Tui` borrow is free.
                self.run_loop.feed_bytes(&bytes)?;
                self.take_and_run_pending()
            }
            ShellEvent::Resize(columns, rows) => self.run_loop.resize(columns, rows),
            ShellEvent::Session(event) => {
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

    /// Drain and run any render-thread shell command the editor's `on_submit`
    /// recorded (see [`PendingShellCommand`]). Called from the pump right after an
    /// input feed, so the `&mut Tui` the run loop held during `on_submit` is free.
    fn take_and_run_pending(&mut self) -> Result<(), RenderError> {
        let pending = self.pending_shell_command.borrow_mut().take();
        match pending {
            Some(PendingShellCommand::Llama) => self.mount_llama(),
            None => Ok(()),
        }
    }

    /// Mount and drive the llama model-manager overlay on the render thread (pi's
    /// `/llama` handler; see [`PendingShellCommand`] for why this runs here rather
    /// than through worker-side extension-dispatch).
    ///
    /// Constructs the synchronous [`LlamaClient`] + [`LlamaProviderController`]
    /// against the default server (`http://127.0.0.1:8080`), a [`TuiExtensionUi`]
    /// over the live `&mut Tui` and an input source that pulls decoded chunks off
    /// the shell's own event channel, and a concrete sized [`ExtensionContext`]
    /// whose `ui()` returns that host, then calls [`run_llama_command`] — the exact
    /// construction the `llama_mount_seam` seam test mirrors, only with the live
    /// shell's `Tui` and channel in place of the test's mock terminal and scripted
    /// input.
    fn mount_llama(&mut self) -> Result<(), RenderError> {
        // Client + provider against the default llama.cpp management server. Off the
        // `native-http` feature (the default lean build) no live transport is bound,
        // so the catalog read fails fast and `run_llama_command` shows its
        // connection-error dialog — the shell surfaces the same "unavailable" frame
        // it would for a real down server.
        let transport = llama_transport();
        let env: Arc<dyn pidgin_ai::seams::storage::ExecutionEnv> =
            Arc::new(pidgin_ai::seams::storage::SystemEnv::new());
        let client = match LlamaClient::new(Arc::clone(&transport), DEFAULT_LLAMA_SERVER_URL, None)
        {
            Ok(client) => Rc::new(client),
            // DEFAULT_LLAMA_SERVER_URL is a compile-time-valid http URL, so this is
            // unreachable; surface it as a notice rather than panicking.
            Err(error) => {
                self.chat_state
                    .borrow_mut()
                    .push_notice(&format!("/llama unavailable: {error}"));
                return self.render();
            }
        };
        let provider = Rc::new(create_llama_provider(transport, env));

        // `run_llama_command`'s owned notification sink (its `'static` loop cannot
        // borrow the host): route each informational notice to the shell's chat
        // notice surface.
        let notify: NotifyFn = {
            let chat = Rc::clone(&self.chat_state);
            Rc::new(move |message: &str, _level: NotifyLevel| {
                chat.borrow_mut().push_notice(message);
            })
        };

        let theme = self.active.current().clone();
        let keybindings = KeybindingsManager::new(tui_keybindings(), Vec::new());
        let chat_for_error = Rc::clone(&self.chat_state);

        // Disjoint field borrows: the input source reads `self.evt_rx` (shared)
        // while the host holds `self.run_loop`'s `&mut Tui` (exclusive). During the
        // synchronous mount the pump is parked here, so this input source is the
        // sole reader of the channel; it yields raw decoded key chunks (Enter /
        // arrows / etc., the same form the seam test scripts) and drops any
        // non-input event (a modal overlay owns the loop while it is up).
        let error_message = {
            let input: InputSource<'_> = {
                let evt_rx = &self.evt_rx;
                Box::new(move || loop {
                    match evt_rx.recv() {
                        Ok(ShellEvent::Bytes(bytes)) => {
                            return Some(String::from_utf8_lossy(&bytes).into_owned())
                        }
                        Ok(_) => continue,
                        Err(_) => return None,
                    }
                })
            };
            let tui = self.run_loop.tui_mut();
            let host = TuiExtensionUi::new(tui, theme, keybindings, input);
            let ctx = LlamaMountCtx { ui: &host };
            match run_llama_command(&ctx, client, provider, notify) {
                Ok(()) | Err(UiError::Unavailable) => None,
                // The host's `custom` already notified its own sink; the shell's
                // notice surface is separate, so surface the failure here too.
                Err(UiError::Failed(message)) => Some(message),
            }
        };

        if let Some(message) = error_message {
            chat_for_error.borrow_mut().push_notice(&message);
        }
        // Repaint the base chat now the overlay is unmounted.
        self.render()
    }
}

/// A concrete, sized [`ExtensionContext`] for the render-thread `/llama` mount:
/// its `ui()` returns the live [`TuiExtensionUi`] host. Mirrors the seam test's
/// `HostCtx`; being sized on the render thread means `run_llama_command`'s
/// `C: ExtensionContext` binds without any `?Sized` widening.
struct LlamaMountCtx<'a> {
    ui: &'a dyn ExtensionUi,
}

impl ExtensionContext for LlamaMountCtx<'_> {
    fn ui(&self) -> &dyn ExtensionUi {
        self.ui
    }
}

/// The HTTP transport backing the render-thread `/llama` client.
///
/// Under `native-http` (the shipped CLI binary's default) this is the live
/// reqwest transport, so `/llama` reaches a real llama.cpp server. Off the feature
/// (the lean default build, incl. `cargo test`) no live transport exists, so the
/// transport fails every request with a connection error — `run_llama_command`
/// then shows its "unavailable" dialog exactly as for a down server. Mirrors
/// [`crate::modes::print`]'s `native-http` split for the builtin registry.
#[cfg(feature = "native-http")]
fn llama_transport() -> Arc<dyn pidgin_ai::seams::http::HttpTransport> {
    Arc::new(pidgin_ai::seams::ReqwestTransport::builder().build())
}

#[cfg(not(feature = "native-http"))]
fn llama_transport() -> Arc<dyn pidgin_ai::seams::http::HttpTransport> {
    use pidgin_ai::seams::http::{HostTransport, HttpRequest, HttpResponse};
    Arc::new(HostTransport::new(|_request: &HttpRequest| {
        // `fetch failed` is the message `run_llama_command`'s `is_connection_error`
        // recognizes, so this maps to the "Could not connect to the server." dialog.
        Err::<HttpResponse, _>(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "fetch failed",
        ))
    }))
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
        self.apply_startup_theme();

        // Stdin reader thread (pi's `process.stdin.on("data")`): forward each raw
        // chunk as a `ShellEvent::Bytes`; stdin EOF becomes a `Shutdown`.
        let bytes_tx = self.evt_tx.clone();
        let end_tx = self.evt_tx.clone();
        let reader = pidgin_tui::StdinReader::spawn(
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
        // `pidgin_tui::app::RunLoop::run_channel` (timer arming + recv_timeout +
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
                Some(ShellEvent::Bytes(bytes)) => {
                    self.run_loop.feed_bytes(&bytes)?;
                    // Enter on a `/llama` line records a pending render-thread
                    // command in `on_submit`; mount it now the `&mut Tui` is free.
                    self.take_and_run_pending()?;
                }
                Some(ShellEvent::Resize(columns, rows)) => self.run_loop.resize(columns, rows)?,
                Some(ShellEvent::Session(session_event)) => {
                    self.chat_state.borrow_mut().handle_event(&session_event);
                    self.render()?;
                }
                Some(ShellEvent::Shutdown) => break,
                None => {
                    self.fire_pending_timeout()?;
                    // A flushed input timeout can also complete a `/llama` submit.
                    self.take_and_run_pending()?;
                }
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

/// The theme-source the controller threads into `ActiveTheme` loads: no custom
/// themes directory (only the built-in `dark`/`light` are reachable in the
/// offline shell), the 256-color mode the shell renders in, and the process
/// environment for `COLORFGBG`-based terminal detection (pi's module theme
/// functions read this from global config / `process.env`).
fn theme_source() -> ThemeSource {
    ThemeSource {
        dirs: Default::default(),
        mode: Some(ColorMode::Color256),
        env: std::env::vars().collect(),
    }
}

/// Assemble the footer's [`FooterData`] from what the offline shell has today: a
/// live `cwd` (abbreviated against `$HOME`) and everything else zeroed / `None`.
/// Token/context/cost stats, git branch, session name, and the model id all live
/// on the unported `AgentSession`; they land when that seam does.
fn footer_data(cwd: String) -> FooterData {
    FooterData {
        cwd,
        home: std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok()),
        git_branch: None,
        session_name: None,
        total_input: 0,
        total_output: 0,
        total_cache_read: 0,
        total_cache_write: 0,
        latest_cache_hit_rate: None,
        total_cost: 0.0,
        using_subscription: false,
        context_percent: Some(0.0),
        context_window: 0,
        auto_compact_enabled: true,
        experimental: false,
        model_id: None,
        provider: String::new(),
        thinking: None,
        provider_count: 1,
        extension_statuses: std::collections::BTreeMap::new(),
    }
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

/// What a submitted editor line resolves to once slash commands are intercepted.
///
/// The submit handler classifies each non-empty line with [`classify_submit`]
/// before it reaches the turn worker: session-lifecycle slash commands (`/new`,
/// `/resume`, `/fork`) become their own worker commands (or a deferred-feature
/// [`Notice`](SubmitAction::Notice)), and everything else is an ordinary
/// [`Prompt`](SubmitAction::Prompt).
#[derive(Debug, PartialEq, Eq)]
enum SubmitAction {
    /// A normal prompt line — echoed as a user bubble and run as a turn.
    Prompt(String),
    /// `/new` — start a brand-new session on the real loop.
    NewSession,
    /// `/resume <path>` — resume the persisted session at `path`.
    Resume(String),
    /// `/fork <entry_id>` — fork the current session at `entry_id`.
    Fork(String),
    /// A slash command that only shows an inline notice (a bare `/resume` or
    /// `/fork`, whose interactive selector UI is deferred to a follow-up slice).
    Notice(String),
}

/// The inline notice shown for a bare `/resume` (its session selector is deferred).
const RESUME_SELECTOR_DEFERRED: &str =
    "Session selector is not wired yet — pass a session path: /resume <path>";
/// The inline notice shown for a bare `/fork` (its entry selector is deferred).
const FORK_SELECTOR_DEFERRED: &str =
    "Entry selector is not wired yet — pass an entry id: /fork <entry_id>";

/// Classify a submitted editor line, intercepting the session-lifecycle slash
/// commands before they would otherwise be sent as a prompt.
///
/// The command is the first whitespace-delimited token; its argument is the
/// remainder, trimmed. `/new` ignores any argument; `/resume` and `/fork` require
/// one (a bare form resolves to a deferred-selector [`SubmitAction::Notice`]).
/// Any other line — including one that merely starts with `/` — is a
/// [`SubmitAction::Prompt`] carrying the original text verbatim.
fn classify_submit(line: &str) -> SubmitAction {
    let trimmed = line.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim).unwrap_or("");
    match command {
        "/new" => SubmitAction::NewSession,
        "/resume" if arg.is_empty() => SubmitAction::Notice(RESUME_SELECTOR_DEFERRED.to_string()),
        "/resume" => SubmitAction::Resume(arg.to_string()),
        "/fork" if arg.is_empty() => SubmitAction::Notice(FORK_SELECTOR_DEFERRED.to_string()),
        "/fork" => SubmitAction::Fork(arg.to_string()),
        _ => SubmitAction::Prompt(line.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_submit, SubmitAction, FORK_SELECTOR_DEFERRED, RESUME_SELECTOR_DEFERRED};

    #[test]
    fn new_command_maps_to_new_session_ignoring_arguments() {
        assert_eq!(classify_submit("/new"), SubmitAction::NewSession);
        assert_eq!(classify_submit("  /new  "), SubmitAction::NewSession);
        // A trailing argument is ignored — `/new` always starts a fresh session.
        assert_eq!(classify_submit("/new whatever"), SubmitAction::NewSession);
    }

    #[test]
    fn resume_requires_a_path_argument() {
        assert_eq!(
            classify_submit("/resume /tmp/session.jsonl"),
            SubmitAction::Resume("/tmp/session.jsonl".to_string())
        );
        assert_eq!(
            classify_submit("/resume"),
            SubmitAction::Notice(RESUME_SELECTOR_DEFERRED.to_string())
        );
    }

    #[test]
    fn fork_requires_an_entry_id_argument() {
        assert_eq!(
            classify_submit("/fork entry-42"),
            SubmitAction::Fork("entry-42".to_string())
        );
        assert_eq!(
            classify_submit("/fork"),
            SubmitAction::Notice(FORK_SELECTOR_DEFERRED.to_string())
        );
    }

    #[test]
    fn plain_lines_and_unknown_slashes_stay_prompts() {
        assert_eq!(
            classify_submit("hello there"),
            SubmitAction::Prompt("hello there".to_string())
        );
        // A word that merely starts with `/new` is not the command token.
        assert_eq!(
            classify_submit("/newsletter"),
            SubmitAction::Prompt("/newsletter".to_string())
        );
        // An unrecognized slash command is passed through verbatim as a prompt.
        assert_eq!(
            classify_submit("/unknown arg"),
            SubmitAction::Prompt("/unknown arg".to_string())
        );
    }
}
