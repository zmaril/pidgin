//! The first Rust→JS **blocking callback bridge** for atilla (bridge slice 1).
//!
//! Every other napi export in this crate is one-way, synchronous, JSON-in /
//! JSON-out. pi's agent tests are different in kind: the agent loop, driven in
//! Rust, must call *live JS closures* mid-run (`streamFn`, `convertToLlm`, …) and
//! block for their (possibly async) results without starving the Node event
//! loop. This module builds that path.
//!
//! # The primitive
//!
//! One [`napi::threadsafe_function::ThreadsafeFunction`] per run points at a
//! single JS *dispatcher* function. Every seam multiplexes through it via a
//! tagged JSON envelope `{ id, kind, payload }`. A Rust-side
//! `id -> std::sync::mpsc::SyncSender` registry lets the dispatcher deliver each
//! result back:
//!
//! ```text
//!   loop thread                                     JS event-loop thread
//!   -----------                                     --------------------
//!   id = next_id()
//!   (tx, rx) = sync_channel(1)
//!   pending.insert(id, tx)
//!   tsfn.call({id,kind,payload}, NonBlocking) ────► dispatcher(envelopeJson)
//!   rx.recv()   // BLOCKS this thread only            switch(kind) → real closure
//!      ▲                                              (await async work)
//!      └──── resolveBridge(id, json) ◄───────────────  bridge.resolveBridge(...)
//! ```
//!
//! Because the blocking `rx.recv()` runs on a **dedicated `std::thread`** spawned
//! off any ambient runtime — not the JS thread — Node keeps running microtasks,
//! timers, and promises that settle the JS closure. `NonBlocking` mode is pure
//! queue backpressure and never waits for JS execution; the resolve channel is
//! how a value comes back. No tokio, so no nested-runtime `block_on` hazard.
//!
//! # Hang-safety (the parked thread must always be released)
//!
//! The loop thread blocks on `rx.recv()`, so **every** JS seam path must resolve
//! the id exactly once. Three release paths exist:
//!
//! - [`AgentBridge::resolve_bridge`] — the normal success path.
//! - [`AgentBridge::resolve_bridge_error`] — the JS exception / promise-rejection
//!   path. Without it a thrown JS closure would park the Rust thread forever.
//! - [`AgentBridge::abort`] — trips the cooperative signal **and drains every
//!   outstanding id** with an aborted sentinel, so a mid-request abort unblocks
//!   the parked thread instead of deadlocking it.
//!
//! Resolving an unknown or already-resolved id is a no-op (never a panic), so
//! double-resolve races after an abort or error cannot crash the addon.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;
use serde_json::{json, Value};

use atilla_agent::agent_loop::{run_agent_loop, AgentEventSink};
use atilla_agent::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, ConvertToLlm, StreamFn, ThinkingLevel,
};
use atilla_ai::seams::provider::AbortSignal;
use atilla_ai::seams::StreamResult;
use atilla_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Message, Model, StopReason,
    StreamOptions, Usage, UsageCost,
};

/// The `kind` reserved for the terminal completion envelope. The JS dispatcher
/// resolves the run promise on this and never calls `resolveBridge` for it.
const KIND_COMPLETE: &str = "__complete__";
/// The `kind` for a fire-and-forget event forwarded from the loop's event sink.
const KIND_EVENT: &str = "event";

/// The outcome delivered back over a per-request resolve channel.
enum BridgeOutcome {
    /// JS produced a value (bare seam-result JSON string).
    Value(String),
    /// JS threw / rejected; carries the error JSON string.
    Error(String),
    /// The request was aborted while parked; the seam builds a safe fallback.
    Aborted,
}

/// Why a bridge round-trip did not yield a plain value.
enum BridgeError {
    /// JS surfaced an error for this seam.
    Errored(String),
    /// The signal was tripped while the request was parked.
    Aborted,
    /// The channel closed with no result (dispatcher gone) — treated as abort.
    Disconnected,
}

/// State shared between the JS thread (resolve/abort/join methods) and the
/// dedicated loop thread (id allocation, channel inserts, blocking recv).
struct BridgeShared {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, SyncSender<BridgeOutcome>>>,
    signal: AbortSignal,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl BridgeShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            signal: AbortSignal::new(),
            thread: Mutex::new(None),
        })
    }

    /// Deliver an outcome to the parked request `id`, if it is still pending.
    /// Unknown / already-resolved ids are a no-op (double-resolve safety).
    fn deliver(&self, id: u64, outcome: BridgeOutcome) {
        let tx = self.pending.lock().unwrap().remove(&id);
        if let Some(tx) = tx {
            // The receiver may already be gone (e.g. raced with abort-drain);
            // ignore the send error — it is not a crash condition.
            let _ = tx.send(outcome);
        }
    }

    /// Trip the abort signal and unblock every parked request with an aborted
    /// sentinel so no loop-thread `rx.recv()` deadlocks.
    fn abort(&self) {
        self.signal.abort();
        let drained: Vec<SyncSender<BridgeOutcome>> = self
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, tx)| tx)
            .collect();
        for tx in drained {
            let _ = tx.send(BridgeOutcome::Aborted);
        }
    }
}

/// The cross-thread dispatcher channel: the TSFN plus the shared registry. Lives
/// on the loop thread; its seam closures call [`BridgeChannel::call`].
struct BridgeChannel {
    tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal>,
    shared: Arc<BridgeShared>,
}

impl BridgeChannel {
    /// Dispatch one seam envelope and block this thread for its result.
    fn call(&self, kind: &str, payload: Value) -> std::result::Result<Value, BridgeError> {
        // Fast-path: already aborted → don't even dispatch.
        if self.shared.signal.is_aborted() {
            return Err(BridgeError::Aborted);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel::<BridgeOutcome>(1);
        self.shared.pending.lock().unwrap().insert(id, tx);

        let envelope = json!({
            "id": id,
            "kind": kind,
            "payload": payload,
            "aborted": self.shared.signal.is_aborted(),
        })
        .to_string();
        self.tsfn
            .call(envelope, ThreadsafeFunctionCallMode::NonBlocking);

        match rx.recv() {
            Ok(BridgeOutcome::Value(s)) => serde_json::from_str(&s)
                .map_err(|e| BridgeError::Errored(format!("bridge decode error: {e}"))),
            Ok(BridgeOutcome::Error(s)) => Err(BridgeError::Errored(extract_error_message(&s))),
            Ok(BridgeOutcome::Aborted) => Err(BridgeError::Aborted),
            Err(_) => Err(BridgeError::Disconnected),
        }
    }

    /// Fire-and-forget forward of a loop event to JS (no round-trip, no id).
    fn dispatch_event(&self, event: &AgentEvent) {
        let envelope = json!({
            "id": 0,
            "kind": KIND_EVENT,
            "payload": event,
        })
        .to_string();
        self.tsfn
            .call(envelope, ThreadsafeFunctionCallMode::NonBlocking);
    }

    /// Deliver the terminal completion envelope carrying the run result JSON.
    fn complete(&self, payload: Value) {
        let envelope = json!({
            "id": 0,
            "kind": KIND_COMPLETE,
            "payload": payload,
        })
        .to_string();
        self.tsfn
            .call(envelope, ThreadsafeFunctionCallMode::NonBlocking);
    }
}

/// Build the JSON-decoding TSFN from the JS dispatcher (unbounded queue, so
/// `NonBlocking` never returns `QueueFull`). Runs on the JS thread.
fn make_tsfn(dispatcher: JsFunction) -> Result<ThreadsafeFunction<String, ErrorStrategy::Fatal>> {
    dispatcher.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<String>| {
        Ok(vec![ctx.env.create_string(&ctx.value)?])
    })
}

/// The Rust-backed callback bridge, exposed to JavaScript as `AgentBridge`.
///
/// A single instance owns one run. The JS shim constructs it, calls one of the
/// spawn entry points (`spikeEcho`, `spikeConcurrent`, `run`) passing the
/// dispatcher, and drives the resolve/abort surface from the event-loop thread.
#[napi(js_name = "AgentBridge")]
pub struct AgentBridge {
    shared: Arc<BridgeShared>,
}

#[napi]
impl AgentBridge {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            shared: BridgeShared::new(),
        }
    }

    /// Deliver a JS-produced value to the parked request `id` (success path).
    /// Unknown / duplicate ids are a no-op.
    #[napi(js_name = "resolveBridge")]
    pub fn resolve_bridge(&self, id: f64, json: String) {
        self.shared.deliver(id as u64, BridgeOutcome::Value(json));
    }

    /// Deliver a JS error to the parked request `id` (exception / rejection
    /// path). Without this a thrown JS closure would hang the loop thread.
    /// Unknown / duplicate ids are a no-op.
    #[napi(js_name = "resolveBridgeError")]
    pub fn resolve_bridge_error(&self, id: f64, json: String) {
        self.shared.deliver(id as u64, BridgeOutcome::Error(json));
    }

    /// Trip the cooperative abort signal and unblock every in-flight request.
    #[napi(js_name = "abort")]
    pub fn abort(&self) {
        self.shared.abort();
    }

    /// Whether the abort signal has been tripped (for a JS `AbortSignal` proxy).
    #[napi(js_name = "isAborted")]
    pub fn is_aborted(&self) -> bool {
        self.shared.signal.is_aborted()
    }

    /// Join the dedicated loop thread. The JS shim calls this once it receives
    /// the completion envelope so the thread is fully reaped and the TSFN
    /// reference released before the process is allowed to exit.
    #[napi(js_name = "join")]
    pub fn join(&self) {
        let handle = self.shared.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// STEP-A primitive spike: prove the NonBlocking-dispatch + resolve-channel
    /// round-trip works from a dedicated off-runtime thread. For each string in
    /// `inputs_json` (a JSON `string[]`), issue one `kind:"echo"` round-trip and
    /// collect the JS-produced result, then complete with the array. Serial:
    /// exactly one id is outstanding at a time.
    #[napi(js_name = "spikeEcho")]
    pub fn spike_echo(&self, dispatcher: JsFunction, inputs_json: String) -> Result<()> {
        let inputs: Vec<String> = serde_json::from_str(&inputs_json)
            .map_err(|e| Error::from_reason(format!("invalid inputs: {e}")))?;
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker(tsfn, move |channel| {
            let mut results: Vec<Value> = Vec::new();
            for input in inputs {
                match channel.call("echo", json!({ "value": input })) {
                    Ok(v) => results.push(v),
                    Err(BridgeError::Errored(msg)) => {
                        results.push(json!({ "__bridge_error": msg }));
                    }
                    Err(BridgeError::Aborted) | Err(BridgeError::Disconnected) => {
                        results.push(json!({ "__aborted": true }));
                    }
                }
            }
            json!({ "results": results })
        });
        Ok(())
    }

    /// CONDITION-F proof: issue `n` concurrent outstanding requests (dispatch all
    /// before blocking on any), then block for each. The JS side resolves them
    /// **out of order**; correct `id -> channel` routing means each `rx` still
    /// receives its own value. Completes with the in-order results.
    #[napi(js_name = "spikeConcurrent")]
    pub fn spike_concurrent(&self, dispatcher: JsFunction, n: u32) -> Result<()> {
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker(tsfn, move |channel| {
            // Dispatch every request first, parking none, so all ids are
            // outstanding simultaneously.
            let mut receivers = Vec::new();
            for i in 0..n {
                let id = channel.shared.next_id.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = sync_channel::<BridgeOutcome>(1);
                channel.shared.pending.lock().unwrap().insert(id, tx);
                let envelope = json!({
                    "id": id,
                    "kind": "echoConcurrent",
                    "payload": { "index": i },
                    "aborted": false,
                })
                .to_string();
                channel
                    .tsfn
                    .call(envelope, ThreadsafeFunctionCallMode::NonBlocking);
                receivers.push((i, rx));
            }
            // Now block for each, in index order. JS may have resolved in any
            // order; routing by id guarantees correctness.
            let mut results: Vec<Value> = Vec::new();
            for (i, rx) in receivers {
                match rx.recv() {
                    Ok(BridgeOutcome::Value(s)) => {
                        results.push(serde_json::from_str(&s).unwrap_or(Value::Null));
                    }
                    _ => results.push(json!({ "index": i, "failed": true })),
                }
            }
            json!({ "results": results })
        });
        Ok(())
    }

    /// STEP-B driver: run the agent loop on a dedicated thread, wiring `streamFn`
    /// and `convertToLlm` through the bridge. `payload_json` is
    /// `{ prompts, context: { systemPrompt, messages }, model, streamOptions?,
    /// reasoning? }`. Completes with `{ messages }` (the run's `AgentMessage[]`).
    #[napi(js_name = "run")]
    pub fn run(&self, dispatcher: JsFunction, payload_json: String) -> Result<()> {
        let input: RunInput = serde_json::from_str(&payload_json)
            .map_err(|e| Error::from_reason(format!("invalid run payload: {e}")))?;
        let tsfn = make_tsfn(dispatcher)?;

        self.spawn_worker(tsfn, move |channel| {
            let RunInput {
                prompts,
                context,
                model,
                stream_options,
                reasoning,
            } = input;

            let agent_context = AgentContext {
                system_prompt: context.system_prompt.unwrap_or_default(),
                messages: context.messages,
                tools: None,
            };

            // convertToLlm seam: serialize messages, round-trip, decode Message[].
            let convert_channel = channel.clone();
            let convert_to_llm: ConvertToLlm = Arc::new(move |messages: &[AgentMessage]| {
                match convert_channel.call("convertToLlm", json!({ "messages": messages })) {
                    Ok(value) => serde_json::from_value::<Vec<Message>>(value)
                        .unwrap_or_else(|_| default_convert_to_llm(messages)),
                    // pi contract: convertToLlm must not throw — safe fallback.
                    Err(_) => default_convert_to_llm(messages),
                }
            });

            // streamFn seam: serialize the request, round-trip, decode the eager
            // StreamResult. Errors/aborts encode a terminal error message (pi's
            // contract: never throw out of streamFn).
            let stream_channel = channel.clone();
            let stream_fn: StreamFn = Arc::new(
                move |model: &Model,
                      ctx: &Context,
                      options: Option<&StreamOptions>,
                      signal: Option<&AbortSignal>| {
                    let aborted = signal.is_some_and(AbortSignal::is_aborted);
                    let payload = json!({
                        "model": model,
                        "context": ctx,
                        "options": options,
                        "aborted": aborted,
                    });
                    match stream_channel.call("streamFn", payload) {
                        Ok(value) => decode_stream_result(value).unwrap_or_else(|| {
                            error_stream_result(model, "invalid streamFn result", false)
                        }),
                        Err(BridgeError::Errored(msg)) => error_stream_result(model, &msg, false),
                        Err(BridgeError::Aborted) | Err(BridgeError::Disconnected) => {
                            error_stream_result(model, "Operation aborted", true)
                        }
                    }
                },
            );

            let config = AgentLoopConfig {
                stream_options: stream_options.unwrap_or_default(),
                reasoning,
                model,
                convert_to_llm,
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

            // Forward every loop event to JS (fire-and-forget; no round-trip).
            let event_channel = channel.clone();
            let emit: AgentEventSink = Arc::new(move |event: AgentEvent| {
                event_channel.dispatch_event(&event);
            });

            let signal = channel.shared.signal.clone();
            let messages = run_agent_loop(
                prompts,
                agent_context,
                config,
                &emit,
                Some(&signal),
                &stream_fn,
            );

            json!({ "messages": messages })
        });
        Ok(())
    }

    /// Create the loop thread: it runs `work`, delivers the completion envelope,
    /// then drops its TSFN clone so Node's event loop can drain and exit. The
    /// join handle is stored for [`AgentBridge::join`].
    fn spawn_worker<F>(&self, tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal>, work: F)
    where
        F: FnOnce(Arc<BridgeChannel>) -> Value + Send + 'static,
    {
        let shared = self.shared.clone();
        let handle = std::thread::spawn(move || {
            let channel = Arc::new(BridgeChannel { tsfn, shared });
            let result = work(channel.clone());
            channel.complete(result);
            // `channel` (and its TSFN) drops here, releasing the JS reference so
            // the Node event loop can finish and the process exit cleanly.
        });
        *self.shared.thread.lock().unwrap() = Some(handle);
    }
}

/// The `run` payload shape parsed at the boundary.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunInput {
    #[serde(default)]
    prompts: Vec<AgentMessage>,
    context: RunContext,
    model: Model,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
    #[serde(default)]
    reasoning: Option<ThinkingLevel>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunContext {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    messages: Vec<AgentMessage>,
}

/// Extract the human-readable message from a `resolveBridgeError` payload. The
/// JS shim sends `{ "__bridge_error": "message" }`; fall back to the raw string
/// when it is a bare message rather than the tagged object.
fn extract_error_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v.get("__bridge_error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| raw.to_string())
}

/// pi's `defaultConvertToLlm`: keep only user / assistant / toolResult messages
/// (the safe fallback when the JS hook is unavailable or errors).
fn default_convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| serde_json::from_value::<Message>(m.clone()).ok())
        .collect()
}

/// Decode a JS-returned `{ events, message }` into an eager [`StreamResult`].
/// `StreamResult` is serialize-only, so decode into a local mirror.
fn decode_stream_result(value: Value) -> Option<StreamResult> {
    #[derive(serde::Deserialize)]
    struct StreamResultIn {
        events: Vec<AssistantMessageEvent>,
        message: AssistantMessage,
    }
    let parsed: StreamResultIn = serde_json::from_value(value).ok()?;
    Some(StreamResult {
        events: parsed.events,
        message: parsed.message,
    })
}

/// Build a terminal error/aborted [`StreamResult`] so the loop ends cleanly
/// instead of the parked thread hanging. pi encodes failure in the final
/// message (never a throw), so the loop treats it as a terminal turn.
fn error_stream_result(model: &Model, message: &str, aborted: bool) -> StreamResult {
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
        stop_reason: if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        },
        error_message: Some(message.to_string()),
        timestamp: 0,
    };
    StreamResult {
        events: Vec::new(),
        message: assistant,
    }
}
