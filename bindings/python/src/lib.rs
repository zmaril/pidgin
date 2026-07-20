//! The pidgin Python native extension (PyO3 cdylib).
//!
//! Sibling of `bindings/php`: a standalone crate outside the root workspace that
//! reaches the engine through path deps and exposes a small, hand-written Python
//! surface. Where the PHP binding exposes only `Pidgin::version()`, this binding
//! also drives a real agent turn through [`pidgin_agent::agent_loop`], offline via
//! the faux provider or live against Anthropic.
//!
//! # Threading model (session actor)
//!
//! The turn machinery is driven on a dedicated worker OS thread that OWNS the
//! provider, the resolved model, and the running conversation. Python never
//! shares that state across threads: a [`Session`] holds only a command channel
//! into the worker, and events/results flow back over channels carrying owned
//! `Send` data (`String` deltas, `Result<String, String>` replies). This mirrors
//! the interactive shell's turn-worker pattern
//! (`crates/pidgin-coding/src/modes/interactive/turn.rs`), which exists precisely
//! because the session graph is `!Send`. Blocking waits release the GIL via
//! [`Python::allow_threads`] so other Python threads keep running.

use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use pidgin_agent::agent_loop::{run_agent_loop, AgentEventSink};
use pidgin_agent::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, ConvertToLlm, StreamFn,
};
use pidgin_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxProvider, FauxResponseStep,
    RegisterFauxProviderOptions,
};
use pidgin_ai::seams::Provider;
use pidgin_ai::types::Model;
use pidgin_ai::{Message, StreamOptions};
use serde_json::{json, Value};

const DEFAULT_LIVE_PROVIDER: &str = "anthropic";
const DEFAULT_LIVE_MODEL: &str = "claude-sonnet-4-5";

/// The pidgin engine version (workspace version), read through the `pidgin-core`
/// façade — the same authoritative version the Rust core reports.
#[pyfunction]
fn version() -> &'static str {
    pidgin_core::version()
}

// ---------------------------------------------------------------------------
// Worker protocol
// ---------------------------------------------------------------------------

/// An item streamed back from a `send_stream` turn.
enum StreamItem {
    /// A newly streamed suffix of assistant text.
    Delta(String),
    /// The turn failed; carries the error message.
    Error(String),
}

/// A command sent from a [`Session`] to its worker thread. All payloads are owned
/// `Send` values — the provider/turn state never crosses the boundary.
enum Command {
    /// Run one full turn and return the concatenated assistant text (blocking).
    Prompt {
        text: String,
        reply: SyncSender<Result<String, String>>,
    },
    /// Run one full turn, forwarding assistant text deltas over `stream_tx`.
    PromptStream {
        text: String,
        stream_tx: Sender<StreamItem>,
    },
    /// Stop the worker loop and let the thread exit.
    Shutdown,
}

/// How the worker completes a turn: the offline faux provider, or the live
/// Anthropic provider.
enum ProviderKind {
    Faux(Arc<FauxProvider>),
    #[cfg(feature = "live")]
    Live {
        provider: Arc<pidgin_ai::RegistryProvider>,
        api_key: Option<String>,
    },
}

/// Everything the worker thread owns for the lifetime of a session.
struct WorkerState {
    provider: ProviderKind,
    model: Model,
    system_prompt: String,
    /// The running conversation transcript (grows one turn at a time).
    history: Vec<AgentMessage>,
}

// ---------------------------------------------------------------------------
// Turn execution (the run_agent_loop recipe, replicated from turn.rs)
// ---------------------------------------------------------------------------

/// The identity converter: passes through only `user`/`assistant`/`toolResult`
/// messages, mirroring turn.rs's default converter.
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

/// A user [`AgentMessage`] value, matching the agent loop's message shape.
fn user_message(text: &str) -> AgentMessage {
    json!({ "role": "user", "content": text, "timestamp": 0 })
}

/// The canned assistant turn the faux provider streams for `prompt`: a single
/// markdown reply that echoes the prompt. Deterministic; no network. Mirrors
/// turn.rs's `faux_responses`.
fn faux_responses(prompt: &str) -> Vec<FauxResponseStep> {
    let reply = format!(
        "Hello from the offline faux assistant.\n\n\
         You said: {prompt}\n\n\
         This turn streamed in with no network and no API key."
    );
    let message =
        faux_assistant_message(vec![faux_text(reply)], FauxAssistantOptions::default(), 0);
    vec![FauxResponseStep::from(message)]
}

/// Concatenate the text content blocks of every assistant message in `messages`.
fn assistant_text(messages: &[AgentMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                    }
                }
            }
        }
    }
    out
}

/// Inspect returned messages for a terminal error/aborted assistant message.
fn turn_error(messages: &[AgentMessage]) -> Option<String> {
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let stop = message.get("stopReason").and_then(Value::as_str);
        if stop == Some("error") || stop == Some("aborted") {
            return Some(
                message
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Request {}", stop.unwrap_or("failed"))),
            );
        }
    }
    None
}

impl WorkerState {
    /// Build the [`StreamFn`] backed by the owned provider. For the live path the
    /// resolved api key is threaded into `StreamOptions`.
    fn stream_fn(&self) -> (StreamFn, StreamOptions) {
        match &self.provider {
            ProviderKind::Faux(provider) => {
                let provider = Arc::clone(provider);
                let stream_fn: StreamFn = Arc::new(move |model, context, options, signal| {
                    provider.stream(model, context, options, signal)
                });
                (stream_fn, StreamOptions::default())
            }
            #[cfg(feature = "live")]
            ProviderKind::Live { provider, api_key } => {
                let provider = Arc::clone(provider);
                let stream_fn: StreamFn = Arc::new(move |model, context, options, signal| {
                    provider.stream(model, context, options, signal)
                });
                // StreamOptions is #[non_exhaustive], so it can't be built with a
                // struct literal here; mutate a default instead.
                let mut opts = StreamOptions::default();
                opts.api_key = api_key.clone();
                (stream_fn, opts)
            }
        }
    }

    /// Run one full agent turn for `text`. `on_delta` is invoked for each streamed
    /// assistant text delta. Returns the concatenated assistant text, or an error
    /// message if the turn terminated in error.
    fn run_turn<F>(&mut self, text: &str, on_delta: F) -> Result<String, String>
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        // The faux provider needs its canned reply queued before each turn.
        if let ProviderKind::Faux(provider) = &self.provider {
            provider.set_responses(faux_responses(text));
        }

        let (stream_fn, stream_options) = self.stream_fn();
        let model = self.model.clone();

        let context = AgentContext {
            system_prompt: self.system_prompt.clone(),
            messages: self.history.clone(),
            tools: Some(Vec::new()),
        };

        let config = AgentLoopConfig {
            stream_options,
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

        // Forwarding sink: extract streamed text deltas from MessageUpdate events.
        let on_delta = Arc::new(on_delta);
        let sink: AgentEventSink = {
            let on_delta = Arc::clone(&on_delta);
            Arc::new(move |event: AgentEvent| {
                if let AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } = event
                {
                    if let pidgin_ai::AssistantMessageEvent::TextDelta { delta, .. } =
                        *assistant_message_event
                    {
                        if !delta.is_empty() {
                            on_delta(delta);
                        }
                    }
                }
            })
        };

        let new_messages = run_agent_loop(
            vec![user_message(text)],
            context,
            config,
            &sink,
            None,
            &stream_fn,
        );

        if let Some(err) = turn_error(&new_messages) {
            return Err(err);
        }

        let reply = assistant_text(&new_messages);
        // Persist the turn (prompt + generated messages) into the transcript.
        self.history.extend(new_messages);
        Ok(reply)
    }
}

/// The worker loop: run one turn per command, exit on `Shutdown` or channel close.
fn worker_loop(mut state: WorkerState, cmd_rx: &Receiver<Command>) {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Command::Prompt { text, reply } => {
                let result = state.run_turn(&text, |_| {});
                let _ = reply.send(result);
            }
            Command::PromptStream { text, stream_tx } => {
                let tx = stream_tx.clone();
                let result = state.run_turn(&text, move |delta| {
                    let _ = tx.send(StreamItem::Delta(delta));
                });
                if let Err(err) = result {
                    let _ = stream_tx.send(StreamItem::Error(err));
                }
                // Dropping stream_tx closes the channel -> StopIteration.
            }
            Command::Shutdown => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider construction
// ---------------------------------------------------------------------------

/// Read the first configured api key for `provider_id` from the environment.
fn read_api_key_env(provider_id: &str) -> Option<String> {
    let vars = pidgin_ai::get_api_key_env_vars(provider_id)?;
    for var in vars {
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Build the offline faux provider and its default model.
fn build_faux() -> (ProviderKind, Model) {
    let provider = Arc::new(FauxProvider::new(RegisterFauxProviderOptions::default()));
    let model = provider
        .get_model(None)
        .expect("faux provider has a default model");
    (ProviderKind::Faux(provider), model)
}

/// Build the live provider (Anthropic) and resolve the requested model. The
/// provider is constructed offline; no network call is made here.
#[cfg(feature = "live")]
fn build_live(provider_id: &str, model_id: &str) -> Result<(ProviderKind, Model), String> {
    use pidgin_ai::seams::clock::{Clock, SystemClock};
    use pidgin_ai::seams::http::HttpTransport;
    use pidgin_ai::seams::ReqwestTransport;

    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::new());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let provider = Arc::new(pidgin_ai::provider_from_catalog_with_transport(
        provider_id,
        &transport,
        &clock,
    ));

    let models = provider.get_models();
    let model = models
        .iter()
        .find(|m| m.id == model_id)
        .or_else(|| models.first())
        .cloned()
        .ok_or_else(|| format!("no models available for provider '{provider_id}'"))?;

    let api_key = read_api_key_env(provider_id);
    Ok((ProviderKind::Live { provider, api_key }, model))
}

#[cfg(not(feature = "live"))]
fn build_live(_provider_id: &str, _model_id: &str) -> Result<(ProviderKind, Model), String> {
    Err("this build has the `live` feature disabled; construct Session with faux=True".to_string())
}

// ---------------------------------------------------------------------------
// Python surface
// ---------------------------------------------------------------------------

/// A pidgin chat session. Owns a dedicated worker thread that drives the agent
/// loop; `send`/`send_stream` round-trip one turn through it.
#[pyclass]
struct Session {
    // A pyclass must be `Sync`; `Sender` is `!Sync`, so it lives behind a Mutex.
    // Sends are non-blocking, so the lock is never held across a wait.
    cmd_tx: Mutex<Sender<Command>>,
    handle: Option<JoinHandle<()>>,
}

#[pymethods]
impl Session {
    /// Construct a session. `faux=True` runs offline through the faux provider
    /// (no network, no api key). Otherwise the live Anthropic path is used, with
    /// the api key read from the environment at construction time.
    #[new]
    #[pyo3(signature = (model = None, provider = None, system_prompt = None, faux = false))]
    fn new(
        model: Option<String>,
        provider: Option<String>,
        system_prompt: Option<String>,
        faux: bool,
    ) -> PyResult<Self> {
        let (provider_kind, resolved_model) = if faux {
            build_faux()
        } else {
            let provider_id = provider.as_deref().unwrap_or(DEFAULT_LIVE_PROVIDER);
            let model_id = model.as_deref().unwrap_or(DEFAULT_LIVE_MODEL);
            build_live(provider_id, model_id).map_err(PyRuntimeError::new_err)?
        };

        let state = WorkerState {
            provider: provider_kind,
            model: resolved_model,
            system_prompt: system_prompt
                .unwrap_or_else(|| "You are the offline faux assistant.".to_string()),
            history: Vec::new(),
        };

        let (cmd_tx, cmd_rx) = channel::<Command>();
        let handle = thread::Builder::new()
            .name("pidgin-python-turn".to_string())
            .spawn(move || worker_loop(state, &cmd_rx))
            .map_err(|e| PyRuntimeError::new_err(format!("failed to spawn worker: {e}")))?;

        Ok(Self {
            cmd_tx: Mutex::new(cmd_tx),
            handle: Some(handle),
        })
    }

    /// Run one full agent turn for `message` and return the concatenated
    /// assistant text. Blocking; releases the GIL while waiting on the worker.
    fn send(&self, py: Python<'_>, message: String) -> PyResult<String> {
        let (reply_tx, reply_rx) = sync_channel::<Result<String, String>>(1);
        self.dispatch(Command::Prompt {
            text: message,
            reply: reply_tx,
        })?;

        let result = py.allow_threads(move || reply_rx.recv());
        match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(err)) => Err(PyRuntimeError::new_err(err)),
            Err(_) => Err(PyRuntimeError::new_err("session worker dropped the reply")),
        }
    }

    /// Run one full agent turn for `message`, returning an iterator that yields
    /// assistant text deltas as they stream in.
    fn send_stream(&self, message: String) -> PyResult<PidginStream> {
        let (item_tx, item_rx) = channel::<StreamItem>();
        self.dispatch(Command::PromptStream {
            text: message,
            stream_tx: item_tx,
        })?;
        Ok(PidginStream {
            rx: Mutex::new(item_rx),
        })
    }
}

impl Session {
    /// Send a command to the worker, mapping a dead worker to a Python error.
    fn dispatch(&self, cmd: Command) -> PyResult<()> {
        self.cmd_tx
            .lock()
            .expect("command channel mutex poisoned")
            .send(cmd)
            .map_err(|_| PyRuntimeError::new_err("session worker is gone"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(tx) = self.cmd_tx.lock() {
            let _ = tx.send(Command::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// An iterator over assistant text deltas for one streamed turn. Yields each
/// delta as a `str`; raises `StopIteration` when the turn ends and
/// `RuntimeError` if the turn failed.
#[pyclass]
struct PidginStream {
    // A pyclass must be `Sync`; `Receiver` is `!Sync`, so it lives behind a Mutex.
    rx: Mutex<Receiver<StreamItem>>,
}

#[pymethods]
impl PidginStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Block (with the GIL released) for the next delta. `None` maps to
    /// `StopIteration`; an error item raises `RuntimeError`.
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let item = py.allow_threads(|| self.rx.lock().expect("stream mutex poisoned").recv());
        match item {
            Ok(StreamItem::Delta(delta)) => Ok(Some(delta)),
            Ok(StreamItem::Error(err)) => Err(PyRuntimeError::new_err(err)),
            Err(_) => Ok(None),
        }
    }
}

/// The `pidgin` extension module.
#[pymodule]
fn pidgin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_class::<Session>()?;
    m.add_class::<PidginStream>()?;
    Ok(())
}
