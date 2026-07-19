//! The turn worker (Unit 4, PR-4B) — the session-actor that owns turn execution
//! off the render thread, offline, bypassing `AgentSession`.
//!
//! Per the locked interactive-shell seam, the `Tui` and its components are
//! `!Send` (they are `Rc<RefCell<..>>` graphs), so turn execution runs on a
//! dedicated worker thread and communicates only via channels of `Send` data:
//!
//! - a **command** channel (main -> worker): [`TurnCommand::Prompt`] carries the
//!   submitted prompt text;
//! - an **event** channel (worker -> main): each [`AgentEvent`] the loop emits is
//!   forwarded verbatim, wrapped as [`ShellEvent::Agent`], for the run loop to
//!   drain and route.
//!
//! The worker drives [`run_agent_loop`] directly with the **faux provider**'s
//! `StreamFn`, producing a canned assistant turn with real streaming deltas — no
//! network, no API key, deterministic. The forwarding sink is the exact
//! `Arc<dyn Fn(AgentEvent) + Send + Sync>` shape the loop already takes, so the
//! only glue is `move |ev| { let _ = evt_tx.send(ShellEvent::Agent(ev)); }`.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use atilla_agent::agent_loop::{run_agent_loop, AgentEventSink};
use atilla_agent::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, ConvertToLlm, StreamFn,
};
use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxProvider, FauxResponseStep,
    RegisterFauxProviderOptions,
};
use atilla_ai::seams::Provider;
use atilla_ai::{Message, StreamOptions};
use serde_json::{json, Value};

use super::app::ShellEvent;

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
    /// Spawn the turn worker. Each emitted [`AgentEvent`] is forwarded over
    /// `evt_tx` (wrapped as [`ShellEvent::Agent`]) to the run loop.
    pub fn spawn(evt_tx: Sender<ShellEvent>) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<TurnCommand>();
        let handle = thread::Builder::new()
            .name("atilla-interactive-turn".to_string())
            .spawn(move || worker_loop(&cmd_rx, &evt_tx))
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

/// The worker loop: run one faux turn per `Prompt`, exit on `Shutdown` or when
/// the command channel closes.
fn worker_loop(cmd_rx: &Receiver<TurnCommand>, evt_tx: &Sender<ShellEvent>) {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            TurnCommand::Prompt(text) => run_faux_turn(&text, evt_tx),
            TurnCommand::Shutdown => break,
        }
    }
}

/// Drive one canned assistant turn through [`run_agent_loop`] with the faux
/// provider, forwarding every emitted event to the run loop.
pub fn run_faux_turn(prompt: &str, evt_tx: &Sender<ShellEvent>) {
    let provider = Arc::new(FauxProvider::new(RegisterFauxProviderOptions::default()));
    provider.set_responses(faux_responses(prompt));
    let model = provider
        .get_model(None)
        .expect("faux provider has a default model");

    // A StreamFn backed by the faux provider: each call pops the next queued
    // response and replays it through the deterministic delta path (Start +
    // text deltas + Done), which the agent loop turns into
    // message_start/message_update/message_end events.
    let stream_fn: StreamFn = {
        let provider = Arc::clone(&provider);
        Arc::new(move |model, context, options, signal| {
            provider.stream(model, context, options, signal)
        })
    };

    let context = AgentContext {
        system_prompt: "You are the offline faux assistant.".to_string(),
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let config = AgentLoopConfig {
        stream_options: StreamOptions::default(),
        reasoning: None,
        model,
        convert_to_llm: identity_converter(),
        transform_context: None,
        get_api_key: None,
        should_stop_after_turn: None,
        prepare_next_turn: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        tool_execution: None,
        before_tool_call: None,
        after_tool_call: None,
    };

    // The forwarding sink: only `Send` data (AgentEvent) crosses the boundary.
    let sink: AgentEventSink = {
        let evt_tx = evt_tx.clone();
        Arc::new(move |event: AgentEvent| {
            let _ = evt_tx.send(ShellEvent::Agent(event));
        })
    };

    run_agent_loop(
        vec![user_message(prompt)],
        context,
        config,
        &sink,
        None,
        &stream_fn,
    );
}

/// The canned assistant turn the faux provider streams for `prompt`: a single
/// markdown reply that echoes the prompt. Deterministic; no network. Kept as a
/// plain-text turn so it streams cleanly through the delta path (tool-panel
/// routing is exercised by the router unit tests instead).
pub fn faux_responses(prompt: &str) -> Vec<FauxResponseStep> {
    let reply = format!(
        "Hello from the offline faux assistant.\n\n\
         You said: {prompt}\n\n\
         This turn streamed in with no network and no API key."
    );
    let message =
        faux_assistant_message(vec![faux_text(reply)], FauxAssistantOptions::default(), 0);
    vec![FauxResponseStep::from(message)]
}

/// Run a faux turn synchronously and collect the core [`AgentEvent`]s it emits,
/// in order. Deterministic (no worker thread, no wall-clock timing) — the entry
/// point headless tests use to feed a faux turn into the shell's router.
pub fn collect_faux_turn(prompt: &str) -> Vec<AgentEvent> {
    let (tx, rx) = std::sync::mpsc::channel::<ShellEvent>();
    run_faux_turn(prompt, &tx);
    drop(tx);
    rx.into_iter()
        .filter_map(|event| match event {
            ShellEvent::Agent(agent_event) => Some(agent_event),
            _ => None,
        })
        .collect()
}

/// A user [`AgentMessage`] value, matching the agent loop's message shape.
fn user_message(text: &str) -> AgentMessage {
    json!({ "role": "user", "content": text, "timestamp": 0 })
}

/// The identity converter: passes through only `user`/`assistant`/`toolResult`
/// messages, mirroring the agent-loop test suite's default converter.
fn identity_converter() -> ConvertToLlm {
    Arc::new(|messages: &[AgentMessage]| {
        messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role").and_then(Value::as_str)?;
                if matches!(role, "user" | "assistant" | "toolResult") {
                    serde_json::from_value::<Message>(m.clone()).ok()
                } else {
                    None
                }
            })
            .collect()
    })
}
