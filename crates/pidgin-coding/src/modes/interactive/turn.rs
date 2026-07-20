//! The turn worker (Unit 5) — the session-actor that owns a **real**
//! [`AgentSession`] and runs turns off the render thread, offline (echo).
//!
//! Per the locked interactive-shell seam, the `Tui` and its components are
//! `!Send` (they are `Rc<RefCell<..>>` graphs) and so is [`AgentSession`], so turn
//! execution runs on a dedicated worker thread and communicates only via channels
//! of `Send` data:
//!
//! - a **command** channel (main -> worker): [`TurnCommand::Prompt`] carries the
//!   submitted prompt text;
//! - an **event** channel (worker -> main): each [`AgentSessionEvent`] the session
//!   emits is cloned and forwarded, wrapped as [`ShellEvent::Session`], for the run
//!   loop to drain and route.
//!
//! The worker owns the [`AgentSession`] for its whole lifetime (it is `!Send`, so
//! it never crosses the thread boundary): at startup it builds an **offline echo**
//! session ([`build_offline_echo_session`]) and `subscribe`s a forwarding
//! listener; then each [`TurnCommand::Prompt`] calls [`AgentSession::prompt`],
//! which runs the whole turn synchronously and emits its events through the
//! listener. No network, no API key (the offline runtime's faux-auth preflight is
//! pre-seeded), deterministic — the assistant reply echoes the last user message.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use pidgin_ai::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};

use super::app::ShellEvent;
use crate::core::agent_session::{build_offline_echo_session, AgentSessionEvent, OfflineEchoError};

/// A command sent from the main thread to the turn worker.
pub enum TurnCommand {
    /// Run a turn for the submitted prompt text.
    Prompt(String),
    /// Stop the worker loop and let the thread exit.
    Shutdown,
}

/// Owns the turn-worker thread and the command channel into it. Dropping (or
/// [`TurnDriver::shutdown`]) stops the worker cleanly.
pub struct TurnDriver {
    cmd_tx: Sender<TurnCommand>,
    handle: Option<JoinHandle<()>>,
}

impl TurnDriver {
    /// Spawn the turn worker over `cwd`. The worker builds a real offline-echo
    /// [`AgentSession`] rooted at `cwd` and forwards each emitted
    /// [`AgentSessionEvent`] over `evt_tx` (wrapped as [`ShellEvent::Session`]) to
    /// the run loop.
    pub fn spawn(evt_tx: Sender<ShellEvent>, cwd: String) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<TurnCommand>();
        let handle = thread::Builder::new()
            .name("pidgin-interactive-turn".to_string())
            .spawn(move || worker_loop(&cmd_rx, &evt_tx, cwd))
            .expect("spawn interactive turn worker thread");
        Self {
            cmd_tx,
            handle: Some(handle),
        }
    }

    /// Queue a prompt for the worker to run (non-blocking).
    pub fn prompt(&self, text: String) {
        let _ = self.cmd_tx.send(TurnCommand::Prompt(text));
    }

    /// A clone of the command sender, for the editor submit handler to forward
    /// prompts without holding the whole driver.
    pub fn sender(&self) -> Sender<TurnCommand> {
        self.cmd_tx.clone()
    }

    /// Signal the worker to stop and join its thread.
    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(TurnCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for TurnDriver {
    fn drop(&mut self) {
        // Best-effort stop on drop: the worker exits when the command channel is
        // dropped even if `Shutdown` was never sent.
        let _ = self.cmd_tx.send(TurnCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The worker loop: own a real offline-echo [`AgentSession`] and run one turn per
/// `Prompt`, exiting on `Shutdown` or when the command channel closes.
///
/// The [`AgentSession`] is constructed here, on the worker thread, and never
/// crosses the thread boundary (it is `!Send`); only cloned [`AgentSessionEvent`]s
/// flow back over `evt_tx`. If the session cannot be built (a faux-provider
/// registration failure — see [`OfflineEchoError`]), the error is surfaced to the
/// user as an assistant bubble and the worker drains commands without running
/// turns rather than panicking.
fn worker_loop(cmd_rx: &Receiver<TurnCommand>, evt_tx: &Sender<ShellEvent>, cwd: String) {
    let session = match build_offline_echo_session(cwd) {
        Ok(session) => session,
        Err(error) => {
            forward_start_error(evt_tx, &error);
            drain_until_shutdown(cmd_rx);
            return;
        }
    };

    // Forward every session event to the render loop. The listener takes `&event`,
    // so it must clone before sending: only `Send` data crosses the boundary.
    let _unsubscribe = session.subscribe(Arc::new({
        let evt_tx = evt_tx.clone();
        move |event: &AgentSessionEvent| {
            let _ = evt_tx.send(ShellEvent::Session(Box::new(event.clone())));
        }
    }));

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            // `prompt` runs the whole turn synchronously, emitting its events via
            // the subscriber above. A `PromptError` (e.g. a preflight rejection) is
            // ignored here: it is offline-deterministic and covered by the session's
            // own tests.
            TurnCommand::Prompt(text) => {
                let _ = session.prompt(&text, None, None);
            }
            TurnCommand::Shutdown => break,
        }
    }
}

/// Drain the command channel until `Shutdown` or close, running no turns. Used
/// after a session-build failure so the worker thread still exits cleanly on
/// shutdown instead of dropping the channel early.
fn drain_until_shutdown(cmd_rx: &Receiver<TurnCommand>) {
    while let Ok(cmd) = cmd_rx.recv() {
        if matches!(cmd, TurnCommand::Shutdown) {
            break;
        }
    }
}

/// Surface a session-build failure to the user as a finished assistant message,
/// forwarded as [`AgentSessionEvent`]s so it flows through the same router path a
/// real turn does (a `message_start` + `message_end` pair renders one bubble).
fn forward_start_error(evt_tx: &Sender<ShellEvent>, error: &OfflineEchoError) {
    let text = format!("Failed to start the offline session: {error}");
    let message = faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0);
    let value = serde_json::to_value(message).expect("assistant message serializes");
    let _ = evt_tx.send(ShellEvent::Session(Box::new(
        AgentSessionEvent::MessageStart {
            message: value.clone(),
        },
    )));
    let _ = evt_tx.send(ShellEvent::Session(Box::new(
        AgentSessionEvent::MessageEnd { message: value },
    )));
}

/// Drive a real offline-echo [`AgentSession`] synchronously and collect the
/// [`AgentSessionEvent`]s it emits for `prompt`, in order. Deterministic (no
/// worker thread, no wall-clock timing) — the entry point headless tests use to
/// feed a real turn's events into the shell's router. Panics only if the offline
/// session cannot be built, which is a test-environment failure.
pub fn collect_offline_echo_turn(prompt: &str, cwd: String) -> Vec<AgentSessionEvent> {
    use std::sync::Mutex;

    let session = build_offline_echo_session(cwd).expect("build offline echo session");
    let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _unsubscribe = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
        sink.lock().unwrap().push(event.clone());
    }));
    let _ = session.prompt(prompt, None, None);
    let collected = events.lock().unwrap().clone();
    collected
}
