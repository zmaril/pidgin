//! The Rust→JS **async-oneshot callback bridge** for pidgin — the `call_async`
//! sibling of [`crate::agent_bridge`]'s blocking `call`.
//!
//! # Where this sits in the bridge family
//!
//! There are two working Rust→JS rendezvous shapes in-tree, and this module is
//! the third member of that family:
//!
//! - **blocking `call`** ([`crate::agent_bridge`]): a dedicated `std::thread`
//!   blocks on `std::sync::mpsc::SyncSender::recv` until JS resolves the id. Used
//!   by the agent loop trio (`streamFn`, `convertToLlm`, `toolExecute`, the eight
//!   hooks). Already proven.
//! - **async-oneshot `call_async`** (THIS module): the Rust caller instead
//!   `.await`s a [`tokio::sync::oneshot`] that JS resolves. The worker runs inside
//!   a current-thread tokio runtime (NOT the Node thread), so many concurrent
//!   tasks each `.await` their own JS promise while the single worker multiplexes.
//!   This is the *await-a-JS-promise-under-Rust* shape — the file-mutation-queue
//!   admit/await/release seam is its first consumer.
//! - **fire-and-forget `emit`** (shared shape; here as
//!   [`AsyncChannel::emit`]): `tsfn.call(.., NonBlocking)` with no id, no reply,
//!   no recv. Cannot deadlock by construction. This module carries its own `emit`
//!   because the file-mutation-queue proof needs a Rust→JS *release* push
//!   (`fmqRelease`) to complement the `call_async` *acquire*; the loop-facing
//!   `emit`/`emit_tool_update` seams stay in [`crate::agent_bridge`].
//!
//! All three share the slice-1 tagged JSON envelope `{ id, kind, payload,
//! aborted }` and one `id -> reply-channel` registry; they differ only in the
//! reply channel type (std `SyncSender` = block a thread; tokio `oneshot` =
//! `.await`).
//!
//! # The primitive
//!
//! One [`napi::threadsafe_function::ThreadsafeFunction`] per run points at a
//! single JS *dispatcher* function. Every seam multiplexes through it via the
//! envelope. A Rust-side `id -> tokio::sync::oneshot::Sender` registry lets the
//! dispatcher deliver each result back:
//!
//! ```text
//!   worker thread (current-thread tokio rt)         JS event-loop thread
//!   ---------------------------------------         --------------------
//!   id = next_id()
//!   (tx, rx) = oneshot::channel()
//!   pending.insert(id, tx)
//!   tsfn.call({id,kind,payload}, NonBlocking) ────► dispatcher(envelopeJson)
//!   rx.await   // yields this TASK only                switch(kind) → real closure
//!      ▲                                              (await async work)
//!      └──── resolveBridge(id, json) ◄───────────────  bridge.resolveBridge(...)
//! ```
//!
//! Because the awaiting task runs on a **dedicated worker `std::thread`** hosting
//! a fresh `new_current_thread` tokio runtime — never the Node thread — Node keeps
//! running microtasks, timers, and promises that settle the JS closure. `.await`
//! (unlike `recv`) does not even block the worker OS thread: the runtime drives
//! other tasks while one awaits, so N concurrent `call_async`s multiplex on one
//! thread. `NonBlocking` mode is pure queue backpressure and never waits for JS.
//!
//! # Deadlock-avoidance invariant (enforced)
//!
//! The blocking-recv-on-the-JS-thread hazard (from the loop-trio reuse
//! assessment): a Rust→JS round-trip deadlocks iff the wait runs ON the Node/JS
//! thread, because the value can only arrive when Node runs a microtask
//! (`resolveBridge`), and a blocked/occupied JS thread never runs one. This
//! module keeps the wait off the Node thread by **construction and by guard**:
//!
//! 1. **Construction.** The `.await` only ever happens inside the worker `FnOnce`
//!    passed to [`AsyncBridge::spawn_worker_async`], which builds a fresh
//!    `Builder::new_current_thread` runtime (mirroring
//!    `pidgin-extensions/src/runtime.rs`) and `block_on`s the work future on a
//!    dedicated `std::thread`. We never `block_on` an ambient/multi-thread runtime
//!    (no nested-`block_on` panic hazard) and never touch the Node thread. No
//!    `#[napi]` method exposes a `.await`/`recv`; the napi surface is exclusively
//!    non-blocking (`resolveBridge`, `resolveBridgeError`, `abort`, `join`).
//! 2. **Captured-ThreadId guard (release HARD-FAIL, not just `debug_assert`).**
//!    The Node/JS thread id is captured at [`AsyncBridge::new`]. Before the worker
//!    `block_on`, [`assert_off_js_thread`] **panics with a clear message in
//!    release** if the current thread is the JS thread. At the top of every
//!    `call_async` `.await`, [`AsyncChannel::call_async`] returns
//!    `Err(BridgeError::Errored("bridge misuse: …"))` — a loud release error, not
//!    a silent no-op — if it is ever driven on the JS thread. The dedicated worker
//!    thread's id is also captured (for the diagnostic `debug_assert`).
//!
//! # Hang-safety (the awaiting task must always be released) — conditions A–J
//!
//! The worker task `.await`s the oneshot, so **every** JS seam path must resolve
//! the id exactly once. The three release paths are ported verbatim from
//! [`crate::agent_bridge`] (doc there, "Hang-safety"):
//!
//! - [`AsyncBridge::resolve_bridge`] — the normal success path **(condition, part of B/G)**.
//! - [`AsyncBridge::resolve_bridge_error`] — the JS exception / promise-rejection
//!   path **(A)**. Without it a thrown JS closure would park the awaiting task
//!   forever.
//! - [`AsyncBridge::abort`] — trips the cooperative signal **and drains every
//!   outstanding id** with an aborted sentinel **(B)**, so a mid-request abort
//!   wakes the awaiting task instead of leaking a parked oneshot.
//!
//! Resolving an unknown or already-resolved id is a no-op (never a panic), so
//! double-resolve races after an abort or error cannot crash the addon **(E)**.
//! Correct `id -> channel` routing means concurrent out-of-order resolution still
//! delivers each value to its own awaiter **(F)**. The event loop keeps running
//! while the worker awaits **(G)**. The process must still exit 0 on its own — no
//! lingering handle — which is **(C)**; the fourth error mode, a channel closed
//! with no result, maps to `BridgeError::Disconnected` ⇒ abort-equivalent **(D)**.
//!
//! The async-oneshot variant ADDS three conditions (from the design note §3):
//!
//! - **(H) no worker-runtime leak.** [`AsyncBridge::spawn_worker_async`] owns a
//!   current-thread tokio runtime on the worker `std::thread`; it `block_on`s to
//!   completion and drops the runtime (and its TSFN clone) before the thread
//!   exits, so [`AsyncBridge::join`] can reap it and Node can exit — (C) for the
//!   async variant.
//! - **(I) oneshot-drop ⇒ Disconnected, not panic.** `rx.await` returning
//!   `Err(_)` (the sender dropped by a raced abort-drain) maps to
//!   `BridgeError::Disconnected`. `abort` sends `BridgeOutcome::Aborted` into the
//!   oneshot *before* dropping it, exactly as the blocking drain does, so the
//!   awaiter wakes with `Aborted` rather than a bare drop.
//! - **(J) single-resolution across channel kinds.** The pending entry is removed
//!   from the map on first `deliver`, so (E) double-resolve safety holds for async
//!   ids identically.
//!
//! # OUT OF SCOPE for this primitive (do not extend it ad hoc)
//!
//! This bridge is strictly **JSON-in / JSON-out and forward-only** (Rust→JS→Rust,
//! one direction per id). The following are deliberately NOT served here and must
//! not be bolted on:
//!
//! - **Live-Node-object cases** — `ChildProcess` (bash spawn), `Readable` /
//!   `Writable` streams, live `EventEmitter` / `AbortSignal` objects, and
//!   `Session` handles with method identity. WHY: **V8 handles never cross the
//!   JSON boundary** (the same rule as `pidgin-extensions/src/runtime.rs`: "only
//!   plain data … V8 handles never leave the owning thread"). A `ChildProcess`
//!   needs live method dispatch (`kill()`, `.stdout.on`) and object identity
//!   across many calls — a Rust-owned state machine + persistent dispatcher + live
//!   proxy (the NativeAgent verdict), a separate deferred slice, not this seam.
//! - **Reentrant / suspend-resume** — the extension OAuth `login` packet, where JS
//!   calls back INTO Rust mid-execution and AWAITS a Rust reply. This primitive is
//!   forward-only; reentrancy is a **bidirectional rendezvous — a DIFFERENT
//!   primitive** in the same parked decision packet
//!   (`ext-oauth-login-reentrant-primitive-parked`). Its login packet stays
//!   parked; this forward-only sibling is the buildable-now half.
//! - **Object-identity assertions** (`.toBe` on `state.model/tools/messages`): a
//!   JSON boundary cannot preserve JS reference identity.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, ThreadId};

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use pidgin_ai::seams::provider::AbortSignal;

/// The `kind` reserved for the terminal completion envelope. The JS dispatcher
/// resolves the run promise on this and never calls `resolveBridge` for it.
const KIND_COMPLETE: &str = "__complete__";

/// The outcome delivered back over a per-request oneshot reply channel. Mirror of
/// `agent_bridge::BridgeOutcome`.
enum BridgeOutcome {
    /// JS produced a value (bare seam-result JSON string).
    Value(String),
    /// JS threw / rejected; carries the error JSON string.
    Error(String),
    /// The request was aborted while parked; the seam builds a safe fallback.
    Aborted,
}

/// Why a bridge round-trip did not yield a plain value. Mirror of
/// `agent_bridge::BridgeError`.
#[derive(Debug)]
enum BridgeError {
    /// JS surfaced an error for this seam (also the deadlock-guard hard-fail).
    Errored(String),
    /// The signal was tripped while the request was parked (condition B/I).
    Aborted,
    /// The oneshot closed with no result (dispatcher/sender gone) — treated as
    /// abort (condition D/I).
    Disconnected,
}

/// State shared between the JS thread (resolve/abort/join methods) and the
/// dedicated worker thread (id allocation, channel inserts, `.await`).
struct BridgeShared {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<BridgeOutcome>>>,
    signal: AbortSignal,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// The Node/JS thread that constructed the bridge and owns the TSFN. The
    /// deadlock guard fails loud if a `.await`/`block_on` ever runs here.
    js_thread: ThreadId,
    /// The dedicated worker thread hosting the tokio runtime (captured when the
    /// worker starts); used only for the diagnostic `debug_assert`.
    worker_thread: Mutex<Option<ThreadId>>,
}

impl BridgeShared {
    fn new(js_thread: ThreadId) -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            signal: AbortSignal::new(),
            thread: Mutex::new(None),
            js_thread,
            worker_thread: Mutex::new(None),
        })
    }

    /// Deliver an outcome to the awaiting request `id`, if it is still pending.
    /// Unknown / already-resolved ids are a no-op (double-resolve safety — E/J).
    fn deliver(&self, id: u64, outcome: BridgeOutcome) {
        let tx = self.pending.lock().unwrap().remove(&id);
        if let Some(tx) = tx {
            // The receiver may already be gone (e.g. raced with abort-drain);
            // ignore the send error — it is not a crash condition.
            let _ = tx.send(outcome);
        }
    }

    /// Trip the abort signal and wake every awaiting request with an aborted
    /// sentinel so no oneshot is leaked/parked (condition B/I).
    fn abort(&self) {
        self.signal.abort();
        let drained: Vec<oneshot::Sender<BridgeOutcome>> = self
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, tx)| tx)
            .collect();
        for tx in drained {
            // Send Aborted BEFORE the sender drops, so the awaiter wakes with
            // Aborted rather than a bare `Err(_)` recv-drop (condition I).
            let _ = tx.send(BridgeOutcome::Aborted);
        }
    }
}

/// Panic with a clear message if the current thread is the JS thread. The release
/// HARD-FAIL half of the deadlock guard (fires in release, not only debug). Used
/// at the `block_on` boundary, where a violation would truly wedge Node.
fn assert_off_js_thread(js_thread: ThreadId, ctx: &str) {
    if std::thread::current().id() == js_thread {
        panic!(
            "async bridge misuse: {ctx} would drive the worker runtime ON the JS \
             thread, deadlocking Node (a JS resolve can never arrive while its own \
             thread is parked in block_on)"
        );
    }
}

/// The cross-thread dispatcher channel: the TSFN plus the shared registry. Lives
/// on the worker thread; its seam closures call [`AsyncChannel::call_async`].
struct AsyncChannel {
    tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal>,
    shared: Arc<BridgeShared>,
}

impl AsyncChannel {
    /// Dispatch one seam envelope and `.await` its JS-resolved result. Unlike the
    /// blocking `call`, this yields the current TASK (not the OS thread), so other
    /// concurrent `call_async`s keep making progress on the same worker runtime.
    async fn call_async(
        &self,
        kind: &str,
        payload: Value,
    ) -> std::result::Result<Value, BridgeError> {
        // Deadlock guard (release hard-fail, loud Err not silent no-op): a
        // `call_async` awaited on the JS thread could never be resolved.
        let current = std::thread::current().id();
        if current == self.shared.js_thread {
            return Err(BridgeError::Errored(
                "bridge misuse: call_async awaited on the JS thread would deadlock".to_string(),
            ));
        }
        debug_assert_eq!(
            Some(current),
            *self.shared.worker_thread.lock().unwrap(),
            "call_async must run on the dedicated worker thread"
        );

        // Fast-path: already aborted → don't even dispatch.
        if self.shared.signal.is_aborted() {
            return Err(BridgeError::Aborted);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel::<BridgeOutcome>();
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

        match rx.await {
            Ok(BridgeOutcome::Value(s)) => serde_json::from_str(&s)
                .map_err(|e| BridgeError::Errored(format!("bridge decode error: {e}"))),
            Ok(BridgeOutcome::Error(s)) => Err(BridgeError::Errored(extract_error_message(&s))),
            Ok(BridgeOutcome::Aborted) => Err(BridgeError::Aborted),
            // Sender dropped with no value (raced abort-drain, dispatcher gone) —
            // treated as abort-equivalent (condition D/I).
            Err(_) => Err(BridgeError::Disconnected),
        }
    }

    /// Fire-and-forget push to JS (no id, no reply, no recv). Deadlock-immune by
    /// construction: a bare `NonBlocking` enqueue. The file-mutation-queue proof
    /// uses this for the `fmqRelease` half that complements the `call_async`
    /// `fmqAcquire`.
    fn emit(&self, kind: &str, payload: Value) {
        let envelope = json!({
            "id": 0,
            "kind": kind,
            "payload": payload,
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

/// The Rust-backed async-oneshot callback bridge, exposed to JavaScript as
/// `AsyncBridge`.
///
/// A single instance owns one run. The JS shim constructs it, calls one of the
/// spawn entry points passing the dispatcher, and drives the
/// resolve/abort/join surface from the event-loop thread.
#[napi(js_name = "AsyncBridge")]
pub struct AsyncBridge {
    shared: Arc<BridgeShared>,
}

#[napi]
impl AsyncBridge {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // Runs on the Node/JS thread: capture its id for the deadlock guard.
        Self {
            shared: BridgeShared::new(std::thread::current().id()),
        }
    }

    /// Deliver a JS-produced value to the awaiting request `id` (success path).
    /// Unknown / duplicate ids are a no-op (E/J).
    #[napi(js_name = "resolveBridge")]
    pub fn resolve_bridge(&self, id: f64, json: String) {
        self.shared.deliver(id as u64, BridgeOutcome::Value(json));
    }

    /// Deliver a JS error to the awaiting request `id` (exception / rejection
    /// path — condition A). Without this a thrown JS closure would park the
    /// awaiting task forever. Unknown / duplicate ids are a no-op.
    #[napi(js_name = "resolveBridgeError")]
    pub fn resolve_bridge_error(&self, id: f64, json: String) {
        self.shared.deliver(id as u64, BridgeOutcome::Error(json));
    }

    /// Trip the cooperative abort signal and wake every in-flight awaiter
    /// (condition B/I).
    #[napi(js_name = "abort")]
    pub fn abort(&self) {
        self.shared.abort();
    }

    /// Whether the abort signal has been tripped (for a JS `AbortSignal` proxy).
    #[napi(js_name = "isAborted")]
    pub fn is_aborted(&self) -> bool {
        self.shared.signal.is_aborted()
    }

    /// Join the dedicated worker thread. The JS shim calls this once it receives
    /// the completion envelope so the thread (and its tokio runtime + TSFN clone)
    /// is fully reaped before the process is allowed to exit — conditions C/H.
    #[napi(js_name = "join")]
    pub fn join(&self) {
        let handle = self.shared.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// PROOF entry — serial async echoes. For each string in `inputs_json` (a JSON
    /// `string[]`), issue one `kind:"echo"` `call_async` round-trip and collect
    /// the JS-resolved result, then complete with the array. Proves the basic
    /// `.await`-a-JS-value round-trip (b), the rejection path (A) as an inline
    /// `__bridge_error`, and event-loop-not-starved (G) when the JS handler awaits
    /// real async work.
    #[napi(js_name = "spikeEcho")]
    pub fn spike_echo(&self, dispatcher: JsFunction, inputs_json: String) -> Result<()> {
        let inputs: Vec<String> = serde_json::from_str(&inputs_json)
            .map_err(|e| Error::from_reason(format!("invalid inputs: {e}")))?;
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker_async(tsfn, move |channel| async move {
            let mut results: Vec<Value> = Vec::new();
            for input in inputs {
                match channel.call_async("echo", json!({ "value": input })).await {
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

    /// PROOF entry — condition F for the async variant. Dispatch `n` concurrent
    /// `call_async` round-trips (each a tokio task on the single worker runtime,
    /// so they multiplex), then await all. The JS side resolves them **out of
    /// order**; correct `id -> channel` routing means each task still receives its
    /// own value. Completes with the in-index-order results — also proving that
    /// one worker thread awaits many JS promises at once (the async property the
    /// blocking variant cannot express).
    #[napi(js_name = "spikeConcurrent")]
    pub fn spike_concurrent(&self, dispatcher: JsFunction, n: u32) -> Result<()> {
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker_async(tsfn, move |channel| async move {
            let mut handles = Vec::new();
            for i in 0..n {
                let ch = channel.clone();
                // spawn_local: the futures are !Send-friendly and run cooperatively
                // on the current-thread runtime's LocalSet.
                handles.push(tokio::task::spawn_local(async move {
                    (
                        i,
                        ch.call_async("echoConcurrent", json!({ "index": i })).await,
                    )
                }));
            }
            let mut results: Vec<Value> = vec![Value::Null; n as usize];
            for h in handles {
                if let Ok((i, outcome)) = h.await {
                    results[i as usize] = match outcome {
                        Ok(v) => v,
                        _ => json!({ "index": i, "failed": true }),
                    };
                }
            }
            json!({ "results": results })
        });
        Ok(())
    }

    /// PROOF entry — condition B/I. Issue a single `call_async` whose JS handler
    /// never resolves; the JS side calls `abort()` while the worker awaits. The
    /// abort-drain wakes the awaiter with `Aborted`, so the run settles cleanly
    /// (never hangs) with `{ aborted: true }`.
    #[napi(js_name = "spikeAbort")]
    pub fn spike_abort(&self, dispatcher: JsFunction) -> Result<()> {
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker_async(tsfn, move |channel| async move {
            let outcome = channel.call_async("hang", json!({})).await;
            let aborted = matches!(
                outcome,
                Err(BridgeError::Aborted) | Err(BridgeError::Disconnected)
            );
            json!({ "aborted": aborted })
        });
        Ok(())
    }

    /// PROOF entry — the ONE real flip: file-mutation-queue via `call_async`
    /// (design note §5). `inputs_json` is a JSON array of `{ path }`. For each
    /// input (concurrently), the worker:
    ///   1. `call_async("fmqAcquire", { path })` — **await queue admission**. The
    ///      JS shim enqueues on `withFileMutationQueue(path, …)`, holds the slot
    ///      open in a pending promise, and `resolveBridge(id, id)` hands Rust a
    ///      release token (the id itself). The `.await` settles when the slot is
    ///      granted.
    ///   2. performs the Rust-owned "write" (here: record `{ path, order }`).
    ///   3. `emit("fmqRelease", { id: token })` — **fire-and-forget release** of
    ///      the held slot, letting the next same-path op in.
    ///
    /// Non-reentrant by construction: Rust never passes a closure into JS; the
    /// queue's own promise-chain logic stays in JS (proof-harness only, no native
    /// flip of the queue). Proves (b) end-to-end + the acquire/await/release
    /// contract with same-path serialization observable to the caller.
    #[napi(js_name = "spikeFmq")]
    pub fn spike_fmq(&self, dispatcher: JsFunction, inputs_json: String) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct FmqInput {
            path: String,
        }
        let inputs: Vec<FmqInput> = serde_json::from_str(&inputs_json)
            .map_err(|e| Error::from_reason(format!("invalid inputs: {e}")))?;
        let tsfn = make_tsfn(dispatcher)?;
        self.spawn_worker_async(tsfn, move |channel| async move {
            // A shared monotonically-increasing "write order" the worker stamps
            // as each admission is granted, so the JS caller can assert same-path
            // ops serialized (never interleaved) while distinct paths overlapped.
            let order = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let mut handles = Vec::new();
            for input in inputs {
                let ch = channel.clone();
                let order = order.clone();
                handles.push(tokio::task::spawn_local(async move {
                    let path = input.path;
                    match ch.call_async("fmqAcquire", json!({ "path": path })).await {
                        Ok(token) => {
                            // Rust owns the "write": stamp the admission order.
                            let stamp = order.fetch_add(1, Ordering::SeqCst);
                            // Fire-and-forget release of the held queue slot.
                            ch.emit("fmqRelease", json!({ "id": token }));
                            json!({ "path": path, "order": stamp, "aborted": false })
                        }
                        Err(_) => json!({ "path": path, "aborted": true }),
                    }
                }));
            }
            let mut results: Vec<Value> = Vec::new();
            for h in handles {
                if let Ok(v) = h.await {
                    results.push(v);
                }
            }
            json!({ "results": results })
        });
        Ok(())
    }

    /// Create the worker thread: it builds a **fresh current-thread tokio
    /// runtime** (never `block_on`ing an ambient one — the deadlock-avoidance
    /// invariant), captures its own ThreadId, `block_on`s the work future via a
    /// `LocalSet` (to host `spawn_local` tasks), delivers the completion envelope,
    /// then drops its TSFN clone so Node's event loop can drain and exit
    /// (conditions C/H). The join handle is stored for [`AsyncBridge::join`].
    fn spawn_worker_async<F, Fut>(
        &self,
        tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal>,
        work: F,
    ) where
        F: FnOnce(Arc<AsyncChannel>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Value> + 'static,
    {
        let shared = self.shared.clone();
        let js_thread = self.shared.js_thread;
        let handle = std::thread::spawn(move || {
            // Release hard-fail: never drive the worker runtime on the JS thread.
            assert_off_js_thread(js_thread, "spawn_worker_async");
            *shared.worker_thread.lock().unwrap() = Some(std::thread::current().id());

            // Fresh current-thread runtime + LocalSet, mirroring
            // pidgin-extensions/src/runtime.rs — OFF the Node thread and OFF any
            // ambient multi-thread runtime (no nested-block_on hazard).
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread tokio runtime for async bridge worker");
            let local = tokio::task::LocalSet::new();

            let channel = Arc::new(AsyncChannel { tsfn, shared });
            let result = local.block_on(&rt, work(channel.clone()));
            channel.complete(result);
            // `channel` (and its TSFN), then `rt`, drop here — releasing the JS
            // reference and the runtime so Node can finish and exit cleanly (H).
        });
        *self.shared.thread.lock().unwrap() = Some(handle);
    }
}

/// Extract the human-readable message from a `resolveBridgeError` payload. The
/// JS shim sends `{ "__bridge_error": "message" }`; fall back to the raw string
/// when it is a bare message rather than the tagged object. (Ported verbatim from
/// `agent_bridge.rs`.)
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

/// ITF trace-replay: prove the Quint model in `specs/bridge_async.qnt` is
/// LOAD-BEARING against this real registry code.
///
/// The formal model (Phase B/C of the code-derived verification pipeline; Phase
/// A = `hinzu model --emit quint`) exports a machine-checked happy-path trace to
/// `specs/traces/bridge_ok.itf.json`. This test replays that trace step by step
/// against the REAL [`BridgeShared`] — the same private `pending` registry,
/// [`BridgeShared::deliver`], and [`BridgeShared::abort`] the addon uses — and,
/// after EVERY step, asserts the real registry keyset equals the model's
/// `pending` set. If the model and the code ever disagree about what is pending,
/// this test fails. A minimal hand-rolled reader over `serde_json::Value` decodes
/// the ITF subset the spec emits (`{"#bigint":"N"}`, `{"#set":[...]}`, a string
/// `tag` for the `Event`), so no heavy new dependency is added.
#[cfg(test)]
mod itf_replay {
    use std::collections::{BTreeSet, HashMap};

    use serde_json::Value;
    use tokio::sync::oneshot;

    use super::{BridgeOutcome, BridgeShared};

    /// One replayable step distilled from an ITF state: the `lastEvent` tag, the
    /// `lastId` it acted on, and the model's `pending` keyset in that state.
    struct Step {
        event: String,
        id: i64,
        pending: BTreeSet<u64>,
    }

    /// Decode `{"#bigint":"N"}` (or a bare JSON number) to i64.
    fn bigint(v: &Value) -> i64 {
        if let Some(s) = v.get("#bigint").and_then(Value::as_str) {
            s.parse().expect("ITF #bigint must be an integer")
        } else if let Some(n) = v.as_i64() {
            n
        } else {
            panic!("not an ITF integer: {v}");
        }
    }

    /// Decode `{"#set":[...]}` of bigints into a set of ids.
    fn idset(v: &Value) -> BTreeSet<u64> {
        v.get("#set")
            .and_then(Value::as_array)
            .expect("ITF set must be {\"#set\":[...]}")
            .iter()
            .map(|e| bigint(e) as u64)
            .collect()
    }

    /// Parse the committed ITF trace into replayable steps.
    fn load_trace() -> Vec<Step> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../specs/traces/bridge_ok.itf.json"
        );
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read ITF trace at {path}: {e}"));
        let doc: Value = serde_json::from_str(&raw).expect("ITF must be valid JSON");
        doc["states"]
            .as_array()
            .expect("ITF must have a states array")
            .iter()
            .map(|st| Step {
                event: st["lastEvent"]["tag"]
                    .as_str()
                    .expect("lastEvent.tag must be a string")
                    .to_string(),
                id: bigint(&st["lastId"]),
                pending: idset(&st["pending"]),
            })
            .collect()
    }

    /// The real registry's current keyset, for the load-bearing equality check.
    fn real_pending(shared: &BridgeShared) -> BTreeSet<u64> {
        shared.pending.lock().unwrap().keys().copied().collect()
    }

    /// A short label for an unexpected `try_recv` outcome, for panic messages.
    fn describe(r: &Result<BridgeOutcome, oneshot::error::TryRecvError>) -> String {
        match r {
            Ok(BridgeOutcome::Value(_)) => "Ok(Value)".to_string(),
            Ok(BridgeOutcome::Error(_)) => "Ok(Error)".to_string(),
            Ok(BridgeOutcome::Aborted) => "Ok(Aborted)".to_string(),
            Err(e) => format!("Err({e:?})"),
        }
    }

    #[test]
    fn replays_bridge_ok_trace_against_real_registry() {
        let steps = load_trace();
        assert!(
            steps.len() >= 4,
            "trace is unexpectedly short: {}",
            steps.len()
        );

        // A real BridgeShared — the same struct the addon drives. `js_thread` is
        // this test thread; we never touch the JS-thread deadlock guard here.
        let shared = BridgeShared::new(std::thread::current().id());

        // The receiver half of each outstanding call, keyed by id — the worker
        // side that `call_async` would be awaiting.
        let mut rxs: HashMap<u64, oneshot::Receiver<BridgeOutcome>> = HashMap::new();

        for (i, step) in steps.iter().enumerate() {
            match step.event.as_str() {
                "EvInit" => {}
                "EvCall" => {
                    let id = step.id as u64;
                    // The model's `pending` for this state tells us whether this
                    // was a real registering call (callAsync) or the aborted
                    // fast-path (callWhenAborted, no insert).
                    if step.pending.contains(&id) {
                        // Replicate call_async's registry insert exactly.
                        let (tx, rx) = oneshot::channel::<BridgeOutcome>();
                        shared.pending.lock().unwrap().insert(id, tx);
                        rxs.insert(id, rx);
                    }
                }
                "EvResolveValue" => {
                    let id = step.id as u64;
                    // Drive the REAL deliver(); the awaiter must observe Value.
                    shared.deliver(id, BridgeOutcome::Value("\"ok\"".to_string()));
                    let mut rx = rxs.remove(&id).expect("resolve of an un-called id");
                    match rx.try_recv() {
                        Ok(BridgeOutcome::Value(s)) => assert_eq!(s, "\"ok\""),
                        other => panic!("expected Value, got {}", describe(&other)),
                    }
                }
                "EvResolveError" => {
                    let id = step.id as u64;
                    shared.deliver(id, BridgeOutcome::Error("boom".to_string()));
                    let mut rx = rxs.remove(&id).expect("resolve of an un-called id");
                    match rx.try_recv() {
                        Ok(BridgeOutcome::Error(_)) => {}
                        other => panic!("expected Error, got {}", describe(&other)),
                    }
                }
                "EvAbort" => {
                    // Drive the REAL abort(): every outstanding awaiter must wake
                    // with Aborted, and the registry must be emptied (drain).
                    shared.abort();
                    for (id, mut rx) in rxs.drain() {
                        match rx.try_recv() {
                            Ok(BridgeOutcome::Aborted) => {}
                            other => panic!(
                                "id {id}: abort must deliver Aborted, got {}",
                                describe(&other)
                            ),
                        }
                    }
                    assert!(
                        real_pending(&shared).is_empty(),
                        "abort must drain the registry"
                    );
                }
                "EvLoseReply" => {
                    // The worker drops the sender with no send: remove it from the
                    // registry and drop it. The awaiter observes a closed channel
                    // (-> BridgeError::Disconnected in call_async).
                    let id = step.id as u64;
                    let tx = shared.pending.lock().unwrap().remove(&id);
                    drop(tx);
                    let mut rx = rxs.remove(&id).expect("lose-reply of an un-called id");
                    assert!(
                        matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed)),
                        "a lost reply must close the channel (Disconnected)"
                    );
                }
                other => panic!("unknown ITF event: {other}"),
            }

            // THE load-bearing check: after every step the REAL registry keyset
            // must equal the model's `pending` set for that state.
            assert_eq!(
                real_pending(&shared),
                step.pending,
                "state {i} ({}): real registry keyset diverged from the model",
                step.event
            );
        }
    }
}
