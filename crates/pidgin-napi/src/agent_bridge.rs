//! The Rust→JS **blocking callback bridge** for pidgin (bridge slices 1–3).
//!
//! Every other napi export in this crate is one-way, synchronous, JSON-in /
//! JSON-out. pi's agent tests are different in kind: the agent loop, driven in
//! Rust, must call *live JS closures* mid-run (`streamFn`, `convertToLlm`, and —
//! slice 2 — a tool's `execute` / `prepareArguments`) and block for their
//! (possibly async) results without starving the Node event loop. This module
//! builds that path.
//!
//! Slice 2 adds two seams on top of the slice-1 primitive: a blocking
//! `toolExecute` round-trip (the Rust-side `AgentTool.execute` closure dispatches
//! to the registered JS tool and decodes its `AgentToolResult`) and a
//! fire-and-forget [`AgentBridge::emit_tool_update`] push (a JS tool's `onUpdate`
//! reaches the loop's update callback via a per-`toolCallId` registry, with no
//! round-trip and no risk of deadlocking the parked loop thread).
//!
//! Slice 3 wires the eight loop hooks (`transformContext`, `getApiKey`,
//! `shouldStopAfterTurn`, `prepareNextTurn`, `getSteeringMessages`,
//! `getFollowUpMessages`, `beforeToolCall`, `afterToolCall`) as further blocking
//! round-trips over the same primitive — no new `#[napi]` method, no `lib.rs`
//! change. `run` replaces the eight hard `None`s in `AgentLoopConfig` with bridge
//! closures, wiring each only when the JS case declared it (the `hooks` flags on
//! `RunInput`). A `prepareNextTurn` snapshot's returned context has its tools
//! rebuilt with `build_bridge_tool`, the same reconstruction `run` uses.
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

// straitjacket-allow-file:duplication — the eight slice-3 hook seams share a
// deliberate serialize-args → `channel.call(kind, …)` → decode-with-fallback
// skeleton (the two nullary hooks, `getSteeringMessages` / `getFollowUpMessages`,
// are near-identical by design). Keeping each hook a self-contained closure reads
// far better than a macro/generic that erases the per-hook payload and fallback.

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

use pidgin_agent::agent_loop::{run_agent_loop, AgentEventSink};
use pidgin_agent::types::{
    AfterToolCall, AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage, AgentTool, AgentToolExecute,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCall, BeforeToolCallContext,
    BeforeToolCallResult, ConvertToLlm, GetApiKey, GetFollowUpMessages, GetSteeringMessages,
    PrepareArguments, PrepareNextTurn, PrepareNextTurnContext, ShouldStopAfterTurn,
    ShouldStopAfterTurnContext, StreamFn, ThinkingLevel, ToolExecutionMode, TransformContext,
};
use pidgin_ai::seams::provider::AbortSignal;
use pidgin_ai::seams::StreamResult;
use pidgin_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Context, Message, Model,
    StopReason, StreamOptions, Usage, UsageCost,
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

/// State shared between the JS thread (resolve/abort/join/emit methods) and the
/// dedicated loop thread (id allocation, channel inserts, blocking recv).
struct BridgeShared {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, SyncSender<BridgeOutcome>>>,
    /// Per-`toolCallId` update callbacks, populated by the `toolExecute` bridge
    /// closure before it dispatches and removed after the round-trip settles.
    /// [`AgentBridge::emit_tool_update`] routes a JS `onUpdate` push to the
    /// matching loop-supplied callback via this map (fire-and-forget; no id).
    updates: Mutex<HashMap<String, AgentToolUpdateCallback>>,
    signal: AbortSignal,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl BridgeShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            updates: Mutex::new(HashMap::new()),
            signal: AbortSignal::new(),
            thread: Mutex::new(None),
        })
    }

    /// Register the loop-supplied update callback for a tool call so a JS
    /// `onUpdate` push (via [`AgentBridge::emit_tool_update`]) can reach it.
    fn register_update(&self, tool_call_id: &str, cb: AgentToolUpdateCallback) {
        self.updates
            .lock()
            .unwrap()
            .insert(tool_call_id.to_string(), cb);
    }

    /// Remove a tool call's update callback once its `toolExecute` round-trip
    /// settles. A late `emitToolUpdate` for the removed id is then a no-op.
    fn unregister_update(&self, tool_call_id: &str) {
        self.updates.lock().unwrap().remove(tool_call_id);
    }

    /// Fetch a clone of the update callback for a tool call, if still registered.
    fn update_callback(&self, tool_call_id: &str) -> Option<AgentToolUpdateCallback> {
        self.updates.lock().unwrap().get(tool_call_id).cloned()
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

    /// Push an interim tool-execution update from JS into the loop
    /// (fire-and-forget; no round-trip, no id). Called from inside a JS tool's
    /// `execute` via its `onUpdate` callback while the loop thread is parked on
    /// that tool's `toolExecute` id.
    ///
    /// # Why this cannot deadlock
    ///
    /// It runs on the Node/JS thread and invokes the loop-built
    /// [`AgentToolUpdateCallback`], which does exactly two hang-safe things: an
    /// `AtomicBool` (`acceptingUpdates`) check, then a `NonBlocking` TSFN enqueue
    /// of a `tool_execution_update` event. It never allocates a bridge id, never
    /// calls `rx.recv()`, and never touches the parked loop channel. An
    /// unknown / already-unregistered `tool_call_id` is a no-op (mirrors the
    /// double-resolve safety of `deliver`).
    #[napi(js_name = "emitToolUpdate")]
    pub fn emit_tool_update(&self, tool_call_id: String, partial_json: String) {
        let cb = self.shared.update_callback(&tool_call_id);
        if let (Some(cb), Ok(partial)) =
            (cb, serde_json::from_str::<AgentToolResult>(&partial_json))
        {
            cb(&partial);
        }
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
                tools,
                tool_execution,
                hooks,
            } = input;

            // Rebuild each AgentTool from its metadata, keying bridge closures by
            // tool name so `toolExecute` / `prepareArguments` round-trips resolve
            // against the right JS tool. Closures cannot cross the wire, so only
            // metadata is serialized (see `ToolMeta`).
            let agent_tools: Option<Vec<AgentTool>> = if tools.is_empty() {
                None
            } else {
                Some(
                    tools
                        .into_iter()
                        .map(|meta| build_bridge_tool(&channel, meta))
                        .collect(),
                )
            };

            let agent_context = AgentContext {
                system_prompt: context.system_prompt.unwrap_or_default(),
                messages: context.messages,
                tools: agent_tools,
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

            // Loop-hook seams (slice 3). Each is a BLOCKING Rust→JS→Rust
            // round-trip via `channel.call`: serialize the hook's args, dispatch
            // the new envelope kind, decode the result, with the documented safe
            // fallback on Err/abort (pi's "hooks must not throw" contract). Only
            // the hooks the JS case actually defined (per the `hooks` flags) are
            // wired; the rest stay `None` so the loop takes its default path.

            // transformContext: prune/rewrite the transcript before convertToLlm.
            // Fallback: identity (return the input messages unchanged).
            let tc_channel = channel.clone();
            let transform_context: TransformContext = Arc::new(
                move |messages: &[AgentMessage], signal: Option<&AbortSignal>| {
                    let aborted = signal.is_some_and(AbortSignal::is_aborted);
                    match tc_channel.call(
                        "transformContext",
                        json!({ "messages": messages, "aborted": aborted }),
                    ) {
                        Ok(v) => serde_json::from_value::<Vec<AgentMessage>>(v)
                            .unwrap_or_else(|_| messages.to_vec()),
                        Err(_) => messages.to_vec(),
                    }
                },
            );

            // getApiKey: dynamic per-call key resolver. The loop discards the
            // resolved value (pidgin-ai StreamOptions has no apiKey); wired for
            // parity. Fallback: null.
            let ak_channel = channel.clone();
            let get_api_key: GetApiKey = Arc::new(move |provider: &str| {
                match ak_channel.call("getApiKey", json!({ "provider": provider })) {
                    Ok(v) => serde_json::from_value::<Option<String>>(v).unwrap_or(None),
                    Err(_) => None,
                }
            });

            // getSteeringMessages: nullary; messages to inject before next turn.
            // Fallback: [].
            let sm_channel = channel.clone();
            let get_steering_messages: GetSteeringMessages =
                Arc::new(
                    move || match sm_channel.call("getSteeringMessages", json!({})) {
                        Ok(v) => serde_json::from_value::<Vec<AgentMessage>>(v).unwrap_or_default(),
                        Err(_) => Vec::new(),
                    },
                );

            // getFollowUpMessages: nullary; messages to re-enter the loop with.
            // Fallback: [].
            let fm_channel = channel.clone();
            let get_follow_up_messages: GetFollowUpMessages =
                Arc::new(
                    move || match fm_channel.call("getFollowUpMessages", json!({})) {
                        Ok(v) => serde_json::from_value::<Vec<AgentMessage>>(v).unwrap_or_default(),
                        Err(_) => Vec::new(),
                    },
                );

            // shouldStopAfterTurn: graceful-stop check after each turn (sees the
            // post-snapshot context). Fallback: false (don't force-stop).
            let ss_channel = channel.clone();
            let should_stop_after_turn: ShouldStopAfterTurn =
                Arc::new(move |ctx: &ShouldStopAfterTurnContext| {
                    match ss_channel.call(
                        "shouldStopAfterTurn",
                        json!({
                            "message": ctx.message,
                            "toolResults": ctx.tool_results,
                            "context": serialize_ctx(&ctx.context),
                            "newMessages": ctx.new_messages,
                        }),
                    ) {
                        Ok(v) => serde_json::from_value::<bool>(v).unwrap_or(false),
                        Err(_) => false,
                    }
                });

            // prepareNextTurn: sees pre-snapshot context; returns next-turn state.
            // A returned context's tools (ToolMeta[] on the wire) are rebuilt via
            // `build_bridge_tool`, exactly like `run`. Fallback: null (no snapshot).
            let pn_channel = channel.clone();
            let prepare_next_turn: PrepareNextTurn =
                Arc::new(move |ctx: &PrepareNextTurnContext| {
                    match pn_channel.call(
                        "prepareNextTurn",
                        json!({
                            "message": ctx.message,
                            "toolResults": ctx.tool_results,
                            "context": serialize_ctx(&ctx.context),
                            "newMessages": ctx.new_messages,
                        }),
                    ) {
                        Ok(v) => decode_turn_update(&pn_channel, v),
                        Err(_) => None,
                    }
                });

            // beforeToolCall: pre-execution hook (after arg validation). Fallback:
            // null (the loop's own post-hook abort re-check covers aborts).
            let bt_channel = channel.clone();
            let before_tool_call: BeforeToolCall = Arc::new(
                move |ctx: &mut BeforeToolCallContext, signal: Option<&AbortSignal>| {
                    let aborted = signal.is_some_and(AbortSignal::is_aborted);
                    match bt_channel.call(
                        "beforeToolCall",
                        json!({
                            "assistantMessage": ctx.assistant_message,
                            "toolCall": ctx.tool_call,
                            "args": ctx.args,
                            "context": serialize_ctx(&ctx.context),
                            "aborted": aborted,
                        }),
                    ) {
                        Ok(Value::Null) | Err(_) => None,
                        Ok(v) => {
                            // Faithful to pi: the JS hook may mutate the validated
                            // args in place. The dispatcher echoes the (possibly
                            // mutated) args back under `args`; adopt them so the
                            // loop executes with them. `block`/`reason` remain at
                            // the top level for BeforeToolCallResult.
                            if let Some(args) = v.get("args") {
                                ctx.args = args.clone();
                            }
                            serde_json::from_value::<BeforeToolCallResult>(v).ok()
                        }
                    }
                },
            );

            // afterToolCall: post-execution override hook. Fallback: null (keep the
            // executed result unchanged).
            let at_channel = channel.clone();
            let after_tool_call: AfterToolCall = Arc::new(
                move |ctx: &AfterToolCallContext, signal: Option<&AbortSignal>| {
                    let aborted = signal.is_some_and(AbortSignal::is_aborted);
                    match at_channel.call(
                        "afterToolCall",
                        json!({
                            "assistantMessage": ctx.assistant_message,
                            "toolCall": ctx.tool_call,
                            "args": ctx.args,
                            "result": ctx.result,
                            "isError": ctx.is_error,
                            "context": serialize_ctx(&ctx.context),
                            "aborted": aborted,
                        }),
                    ) {
                        Ok(Value::Null) | Err(_) => None,
                        Ok(v) => serde_json::from_value::<AfterToolCallResult>(v).ok(),
                    }
                },
            );

            let config = AgentLoopConfig {
                stream_options: stream_options.unwrap_or_default(),
                reasoning,
                model,
                convert_to_llm,
                transform_context: hooks.transform_context.then_some(transform_context),
                get_api_key: hooks.get_api_key.then_some(get_api_key),
                should_stop_after_turn: hooks
                    .should_stop_after_turn
                    .then_some(should_stop_after_turn),
                prepare_next_turn: hooks.prepare_next_turn.then_some(prepare_next_turn),
                get_steering_messages: hooks.get_steering_messages.then_some(get_steering_messages),
                get_follow_up_messages: hooks
                    .get_follow_up_messages
                    .then_some(get_follow_up_messages),
                tool_execution,
                before_tool_call: hooks.before_tool_call.then_some(before_tool_call),
                after_tool_call: hooks.after_tool_call.then_some(after_tool_call),
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
    /// Tool metadata (name/label/description/parameters/executionMode/
    /// hasPrepareArguments); the closures are rebuilt Rust-side as bridge seams.
    #[serde(default)]
    tools: Vec<ToolMeta>,
    /// Loop-level tool-execution mode (pi's `config.toolExecution`). The hybrid
    /// shim never routes `parallel` native, so in practice this is `sequential`
    /// or absent, but it is threaded through faithfully.
    #[serde(default)]
    tool_execution: Option<ToolExecutionMode>,
    /// Which loop hooks the JS case defined. Closures cannot cross the wire, so
    /// JS reports each hook's presence and Rust wires a bridge round-trip only
    /// for the ones flagged `true` (the rest stay `None`).
    #[serde(default)]
    hooks: HookFlags,
}

/// Presence flags for the eight loop hooks, mirroring the JS `config` keys.
/// Each `true` means the case defined that hook, so the `run` seam wires a
/// blocking bridge round-trip for it (see the hook closures in `run`).
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HookFlags {
    #[serde(default)]
    transform_context: bool,
    #[serde(default)]
    get_api_key: bool,
    #[serde(default)]
    should_stop_after_turn: bool,
    #[serde(default)]
    prepare_next_turn: bool,
    #[serde(default)]
    get_steering_messages: bool,
    #[serde(default)]
    get_follow_up_messages: bool,
    #[serde(default)]
    before_tool_call: bool,
    #[serde(default)]
    after_tool_call: bool,
}

/// Serializable tool metadata carried in the `run` payload. `AgentTool` holds
/// closures and cannot cross the wire, so JS sends only these fields and Rust
/// reconstructs the tool with bridge `execute` / `prepare_arguments` seams.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolMeta {
    name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Value,
    #[serde(default)]
    execution_mode: Option<ToolExecutionMode>,
    #[serde(default)]
    has_prepare_arguments: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunContext {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    messages: Vec<AgentMessage>,
}

/// Reconstruct an [`AgentTool`] from its wire metadata, wiring `execute` (and,
/// when present, `prepare_arguments`) as bridge closures that round-trip to the
/// registered JS tool by name.
fn build_bridge_tool(channel: &Arc<BridgeChannel>, meta: ToolMeta) -> AgentTool {
    let ToolMeta {
        name,
        label,
        description,
        parameters,
        execution_mode,
        has_prepare_arguments,
    } = meta;

    // execute seam: register the per-call update callback so `emitToolUpdate`
    // can reach it, dispatch a blocking `toolExecute` round-trip, then decode the
    // JS `AgentToolResult`. JS throw / abort / disconnect all resolve to a clean
    // error result so the parked loop thread is always released.
    let exec_channel = channel.clone();
    let exec_name = name.clone();
    let execute: AgentToolExecute = Arc::new(move |id, args, _signal, update_cb| {
        if let Some(cb) = update_cb {
            exec_channel.shared.register_update(id, cb.clone());
        }
        let out = exec_channel.call(
            "toolExecute",
            json!({
                "toolCallId": id,
                "toolName": exec_name,
                "args": args,
                "aborted": exec_channel.shared.signal.is_aborted(),
            }),
        );
        exec_channel.shared.unregister_update(id);
        match out {
            Ok(value) => serde_json::from_value::<AgentToolResult>(value)
                .unwrap_or_else(|_| error_tool_result("invalid toolExecute result")),
            Err(BridgeError::Errored(msg)) => error_tool_result(&msg),
            Err(BridgeError::Aborted) | Err(BridgeError::Disconnected) => {
                error_tool_result("Operation aborted")
            }
        }
    });

    // prepareArguments seam: a pure sync transform on the wire — round-trip the
    // raw args and decode the rewritten value. On any failure fall back to the
    // input args unchanged (identity), matching pi's "no-op when unavailable".
    let prepare_arguments: Option<PrepareArguments> = has_prepare_arguments.then(|| {
        let prep_channel = channel.clone();
        let prep_name = name.clone();
        let prepare: PrepareArguments = Arc::new(move |args: &Value| {
            match prep_channel.call(
                "prepareArguments",
                json!({ "toolName": prep_name, "args": args }),
            ) {
                Ok(value) => value,
                Err(_) => args.clone(),
            }
        });
        prepare
    });

    AgentTool {
        name,
        description,
        parameters,
        label,
        prepare_arguments,
        execute,
        execution_mode,
    }
}

/// Project an [`AgentContext`] to the wire shape the hook seams send to JS:
/// `{ systemPrompt, messages, tools: ToolMeta[] }`. `AgentContext` carries
/// closures and is not serde, so `tools` is reduced to the same `ToolMeta`
/// metadata `run` sends (JS maps back to the live tool by name, and a returned
/// context re-serializes to `ToolMeta[]` for `build_bridge_tool` to rebuild).
fn serialize_ctx(ctx: &AgentContext) -> Value {
    let tools: Vec<Value> = ctx
        .tools
        .as_ref()
        .map(|ts| ts.iter().map(serialize_tool_meta).collect())
        .unwrap_or_default();
    json!({
        "systemPrompt": ctx.system_prompt,
        "messages": ctx.messages,
        "tools": tools,
    })
}

/// Project an [`AgentTool`] to its wire `ToolMeta` (the inverse of
/// [`build_bridge_tool`]'s input): closures collapse to `hasPrepareArguments`.
fn serialize_tool_meta(tool: &AgentTool) -> Value {
    json!({
        "name": tool.name,
        "label": tool.label,
        "description": tool.description,
        "parameters": tool.parameters,
        "executionMode": tool.execution_mode,
        "hasPrepareArguments": tool.prepare_arguments.is_some(),
    })
}

/// The `prepareNextTurn` resolve value parsed at the boundary: a mirror of pi's
/// `AgentLoopTurnUpdate`. `context.tools` arrive as `ToolMeta[]` and are rebuilt
/// into bridge [`AgentTool`]s exactly as in `run`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnUpdateIn {
    #[serde(default)]
    context: Option<CtxIn>,
    #[serde(default)]
    model: Option<Model>,
    #[serde(default)]
    thinking_level: Option<ThinkingLevel>,
}

/// The context half of a [`TurnUpdateIn`]: system prompt + transcript + tool
/// metadata to rebuild.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CtxIn {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    messages: Vec<AgentMessage>,
    #[serde(default)]
    tools: Vec<ToolMeta>,
}

/// Decode a `prepareNextTurn` resolve value into an [`AgentLoopTurnUpdate`],
/// rebuilding any returned context's tools as bridge seams. `null` (no snapshot)
/// and an undecodable value both map to `None` (the loop keeps current state).
fn decode_turn_update(channel: &Arc<BridgeChannel>, value: Value) -> Option<AgentLoopTurnUpdate> {
    if value.is_null() {
        return None;
    }
    let parsed: TurnUpdateIn = serde_json::from_value(value).ok()?;
    let context = parsed.context.map(|c| AgentContext {
        system_prompt: c.system_prompt.unwrap_or_default(),
        messages: c.messages,
        tools: if c.tools.is_empty() {
            None
        } else {
            Some(
                c.tools
                    .into_iter()
                    .map(|meta| build_bridge_tool(channel, meta))
                    .collect(),
            )
        },
    });
    Some(AgentLoopTurnUpdate {
        context,
        model: parsed.model,
        thinking_level: parsed.thinking_level,
    })
}

/// Build a plain error [`AgentToolResult`] (mirror of the agent loop's
/// `create_error_tool_result`) so a JS throw / abort settles the parked loop
/// thread with a clean error-text result instead of hanging.
fn error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::Text {
            text: message.to_string(),
            text_signature: None,
        }],
        details: json!({}),
        added_tool_names: None,
        terminate: None,
    }
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
            cost: UsageCost::default(),
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
