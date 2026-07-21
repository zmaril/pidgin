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
//! The worker owns an [`AgentSessionRuntime`] for its whole lifetime (the runtime
//! and the [`AgentSession`] it wraps are both `!Send`, so neither crosses the
//! thread boundary): at startup it builds the runtime from the **offline echo**
//! session factory ([`build_offline_echo_runtime_factory`]) and `subscribe`s a
//! forwarding listener onto the runtime's current session; then each
//! [`TurnCommand::Prompt`] calls [`AgentSession::prompt`] on that session, which
//! runs the whole turn synchronously and emits its events through the listener. No
//! network, no API key (the offline runtime's faux-auth preflight is pre-seeded),
//! deterministic — the assistant reply echoes the last user message.
//!
//! [`TurnCommand::NewSession`], [`TurnCommand::Resume`], and [`TurnCommand::Fork`]
//! drive the runtime's session lifecycle (`/new`, `/resume <path>`, `/fork
//! <entry_id>`). Each swap tears down the current session and the factory rebuilds
//! a fresh one; a `rebind_session` hook re-subscribes the forwarder onto the new
//! current session so events keep flowing (pi's "unsubscribe old, subscribe new").

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use pidgin_ai::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};

use super::app::{ShellEvent, ShellNotifySink};
use crate::core::agent_session::{
    build_offline_echo_runtime_factory, build_offline_echo_session, create_agent_session_runtime,
    AgentSession, AgentSessionEvent, AgentSessionRuntimeFactoryOptions,
    CreateAgentSessionRuntimeFactory, ForkOptions, NewSessionOptions,
};
use crate::core::sdk::{build_create_runtime_factory, CreateAgentSessionRuntimeFixedOptions};
use crate::core::session_manager::SessionManager;
use crate::core::skills::get_agent_dir;

/// A zero-arg constructor for the runtime factory the worker drives. It is a
/// plain `fn` pointer (not a boxed closure) so it is `Send` and can cross into
/// the worker thread, where it is invoked to build the (`!Send`) factory locally.
pub type RuntimeFactoryMaker = fn() -> CreateAgentSessionRuntimeFactory;

/// The offline-echo factory maker: every turn echoes the last user message. Used
/// by [`TurnDriver::spawn`] and the shell's default/test path.
pub fn offline_echo_maker() -> CreateAgentSessionRuntimeFactory {
    build_offline_echo_runtime_factory()
}

/// The live factory maker: builds real, credentialed [`AgentSession`]s that reach
/// a provider (under `native-http`) via [`build_create_runtime_factory`]. The
/// model is resolved from settings / first-available; tools default to the full
/// coding set. Used by [`TurnDriver::spawn_live`].
pub fn live_maker() -> CreateAgentSessionRuntimeFactory {
    build_create_runtime_factory(CreateAgentSessionRuntimeFixedOptions {
        model: None,
        thinking_level: None,
        scoped_models: Vec::new(),
        no_tools: None,
        tools: None,
        exclude_tools: None,
        custom_tools: Vec::new(),
    })
}

/// The boxed unsubscribe handle returned by [`AgentSession::subscribe`]. Dropping
/// or calling it removes the forwarding listener from that session's registry.
type Unsubscribe = Box<dyn FnOnce()>;

/// A command sent from the main thread to the turn worker.
pub enum TurnCommand {
    /// Run a turn for the submitted prompt text.
    Prompt(String),
    /// Start a brand-new session in the current cwd (`/new`).
    NewSession,
    /// Resume (switch to) a persisted session file at the given path (`/resume`).
    Resume(String),
    /// Fork the current session at (or before) the given entry id (`/fork`).
    Fork(String),
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
    /// [`AgentSessionRuntime`](crate::core::agent_session::AgentSessionRuntime)
    /// rooted at `cwd` and forwards each [`AgentSessionEvent`] the current session
    /// emits over `evt_tx` (wrapped as [`ShellEvent::Session`]) to the run loop,
    /// re-subscribing after every `/new`, `/resume`, or `/fork` swap.
    pub fn spawn(evt_tx: Sender<ShellEvent>, cwd: String) -> Self {
        // Offline echo: agent_dir is a per-cwd throwaway (auth is faux-seeded, so
        // the dir is never read for credentials).
        let agent_dir = format!("{cwd}/.agent");
        Self::spawn_with(evt_tx, cwd, agent_dir, offline_echo_maker)
    }

    /// Spawn a **live** turn worker: real, credentialed [`AgentSession`]s that
    /// reach a provider. `agent_dir` is the real config dir (`get_agent_dir()`)
    /// so `auth.json` / `models.json` resolve; the model is chosen from settings
    /// or the first available provider.
    pub fn spawn_live(evt_tx: Sender<ShellEvent>, cwd: String) -> Self {
        Self::spawn_with(evt_tx, cwd, get_agent_dir(), live_maker)
    }

    /// Spawn a worker driving the runtime the `make_factory` maker builds, rooted
    /// at `cwd` with `agent_dir` for credential/catalog resolution.
    pub fn spawn_with(
        evt_tx: Sender<ShellEvent>,
        cwd: String,
        agent_dir: String,
        make_factory: RuntimeFactoryMaker,
    ) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<TurnCommand>();
        let handle = thread::Builder::new()
            .name("pidgin-interactive-turn".to_string())
            .spawn(move || worker_loop(&cmd_rx, &evt_tx, cwd, agent_dir, make_factory))
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

/// The worker loop: own a real offline-echo [`AgentSessionRuntime`] and run one
/// turn per `Prompt`, driving the runtime's `/new`, `/resume`, and `/fork`
/// lifecycle on the matching commands, exiting on `Shutdown` or when the command
/// channel closes.
///
/// The runtime (and its current [`AgentSession`]) is constructed here, on the
/// worker thread, and never crosses the thread boundary (both are `!Send`); only
/// cloned [`AgentSessionEvent`]s flow back over `evt_tx`. If the runtime cannot be
/// built — a build-invariant faux-provider failure surfaces as a panic in the
/// factory, but a missing session cwd surfaces as
/// [`MissingSessionCwdError`](crate::core::session_cwd::MissingSessionCwdError)
/// here — the error is shown to the user as an assistant bubble and the worker
/// drains commands without running turns rather than propagating the failure.
///
/// # The rebind seam
///
/// A single forwarding listener is subscribed onto the runtime's *current*
/// session. Its boxed unsubscribe handle lives in an `Rc<RefCell<..>>` that a
/// `rebind_session` hook rewrites after every swap: the hook drops the outgoing
/// session's subscription (its session is already disposed) and subscribes the
/// forwarder onto the new current session, so events keep flowing across `/new`,
/// `/resume`, and `/fork` (pi's "unsubscribe old, subscribe new").
fn worker_loop(
    cmd_rx: &Receiver<TurnCommand>,
    evt_tx: &Sender<ShellEvent>,
    cwd: String,
    agent_dir: String,
    make_factory: RuntimeFactoryMaker,
) {
    let mut runtime = match create_agent_session_runtime(
        make_factory(),
        AgentSessionRuntimeFactoryOptions {
            cwd: cwd.clone(),
            agent_dir,
            session_manager: SessionManager::in_memory(&cwd),
            session_start_event: None,
        },
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            forward_notice(
                evt_tx,
                format!("Failed to start the offline session: {error}"),
            );
            drain_until_shutdown(cmd_rx);
            return;
        }
    };

    // Bind the host notify sink onto the current session's extension runner so a
    // `ctx.ui.notify` from the plane forwards into the shell's unified channel (as
    // `ShellEvent::Notify`). The offline-echo session's default `StubExtensionRunner`
    // inherits `bind_notify_sink`'s no-op, so this is inert offline but is the live
    // seam once a real (deno/combined) runner drives the session — additive either
    // way. Re-bound on every session swap by the rebind hook below.
    bind_notify_sink(runtime.session(), evt_tx);

    // The active forwarder's unsubscribe handle, swapped by the rebind hook below.
    let unsubscribe: Rc<RefCell<Option<Unsubscribe>>> = Rc::new(RefCell::new(Some(
        subscribe_forwarder(runtime.session(), evt_tx.clone()),
    )));
    runtime.set_rebind_session(Some(Box::new({
        let evt_tx = evt_tx.clone();
        let unsubscribe = Rc::clone(&unsubscribe);
        move |new_session: &AgentSession| {
            // Drop the outgoing subscription first (bind the take() so its borrow
            // ends before we re-borrow), then forward from the new current session.
            let previous = unsubscribe.borrow_mut().take();
            drop(previous);
            *unsubscribe.borrow_mut() = Some(subscribe_forwarder(new_session, evt_tx.clone()));
            // Re-bind the notify sink onto the fresh session's runner (the swap
            // replaced the runner along with the session).
            bind_notify_sink(new_session, &evt_tx);
        }
    })));

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            // `prompt` runs the whole turn synchronously on the current session,
            // emitting its events via the forwarder. A `PromptError` (e.g. a
            // preflight rejection) is ignored here: it is offline-deterministic and
            // covered by the session's own tests.
            TurnCommand::Prompt(text) => {
                let _ = runtime.session().prompt(&text, None, None);
            }
            // `/new` tears down the current session and the factory rebuilds a fresh
            // one; the rebind hook re-subscribes the forwarder onto it. A cancelled
            // switch (there is no `session_before_switch` handler offline) would
            // leave the current session in place.
            TurnCommand::NewSession => {
                let _ = runtime.new_session(NewSessionOptions::default());
            }
            // `/resume <path>` swaps in a persisted session; on a runtime error the
            // current session is untouched and the failure is shown as a bubble.
            TurnCommand::Resume(path) => {
                if let Err(error) = runtime.switch_session(&path) {
                    forward_notice(evt_tx, format!("Could not resume session: {error}"));
                }
            }
            // `/fork <entry_id>` branches the current session; same error handling.
            TurnCommand::Fork(entry_id) => {
                if let Err(error) = runtime.fork(&entry_id, ForkOptions::default()) {
                    forward_notice(evt_tx, format!("Could not fork session: {error}"));
                }
            }
            TurnCommand::Shutdown => break,
        }
    }

    // Drop the active subscription before the runtime (and its session) go out of
    // scope, so no forwarding listener outlives the worker.
    let remaining = unsubscribe.borrow_mut().take();
    drop(remaining);
}

/// Bind a [`ShellNotifySink`] onto `session`'s extension runner, so a plane-side
/// `ctx.ui.notify` is forwarded over `evt_tx` as a [`ShellEvent::Notify`]. Called
/// at worker startup and after every session swap (each swap replaces the runner).
/// A no-op against the offline-echo `StubExtensionRunner` (whose `bind_notify_sink`
/// is the default no-op); live once a real runner drives the session.
fn bind_notify_sink(session: &AgentSession, evt_tx: &Sender<ShellEvent>) {
    session
        .extension_runner()
        .bind_notify_sink(Arc::new(ShellNotifySink::new(evt_tx.clone())));
}

/// Subscribe a forwarding listener onto `session`: each [`AgentSessionEvent`] it
/// emits is cloned and sent over `evt_tx` (wrapped as [`ShellEvent::Session`]).
/// Returns the boxed unsubscribe handle so the caller can drop the subscription
/// when the session is swapped out. The listener takes `&event`, so it clones
/// before sending — only `Send` data crosses the worker/main boundary.
fn subscribe_forwarder(session: &AgentSession, evt_tx: Sender<ShellEvent>) -> Unsubscribe {
    Box::new(
        session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            let _ = evt_tx.send(ShellEvent::Session(Box::new(event.clone())));
        })),
    )
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

/// Surface a worker-side notice to the user as a finished assistant message,
/// forwarded as [`AgentSessionEvent`]s so it flows through the same router path a
/// real turn does (a `message_start` + `message_end` pair renders one bubble).
/// Used for a runtime-build failure at startup and for `/resume` / `/fork` runtime
/// errors, neither of which should propagate out of the worker.
fn forward_notice(evt_tx: &Sender<ShellEvent>, text: String) {
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
