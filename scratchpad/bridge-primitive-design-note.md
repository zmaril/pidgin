# Design Note: Rust→JS Blocking-Callback Bridge Primitive (family)

Status: DESIGN-FIRST. No Rust source modified. Circulates to coordinator + steward.
Author scope: pidgin (`/workspace/pidgin`), napi edge.
Prior art read verbatim: `crates/pidgin-napi/src/agent_bridge.rs` (bridge slices 1–3),
`crates/pidgin-extensions/src/runtime.rs` + `dispatch.rs` (embedded-deno oneshot),
memory `atilla-agent-tier-port-status`, `exec-tools-async-vs-sync-agenttool`,
`operations-seam-locked-spec`, `ext-oauth-login-reentrant-primitive-parked`,
`native-count-honesty-no-nominal-flips`, `steward-flip-crew-model`.

> Naming note: memory pre-dates the atilla→pidgin rename (`atilla-renamed-to-pidgin`,
> PR #171). All `atilla-*` / `atilla_*` references there = `pidgin-*` / `pidgin_*` here.
> The "prior reuse-assessment" the task refers to is the **"Native flip needs a
> Rust→JS callback bridge (loop trio)"** section of `atilla-agent-tier-port-status.md`;
> there is no standalone `hard-async-trio-*` file — that content lives inline there.

---

## 0. What already exists (the base we build ON, not from scratch)

There are TWO working Rust→JS rendezvous implementations in-tree today. The primitive
family is a *generalization + hardening* of these, not a greenfield design.

**(1) The blocking variant — `agent_bridge.rs` (real Node / real V8).**
One `ThreadsafeFunction<String, ErrorStrategy::Fatal>` per run points at a single JS
*dispatcher* (`agent_bridge.rs:204-207`, `make_tsfn` :266-270). Every seam multiplexes
through it via a tagged JSON envelope `{ id, kind, payload, aborted }`
(`BridgeChannel::call` :211-237). A Rust-side `id -> std::sync::mpsc::SyncSender`
registry (`BridgeShared.pending` :133) lets the dispatcher deliver each result back via
`resolveBridge` / `resolveBridgeError` (`:294-305` → `deliver` :176-183). The blocking
`rx.recv()` runs on a **dedicated `std::thread`** spawned off any ambient runtime
(`spawn_worker` :703-716); `NonBlocking` TSFN mode is pure queue backpressure and never
waits for JS (`:227-228`, doc `:46-50`). No tokio → no nested-`block_on` hazard.

**(2) The async-oneshot variant — `extensions/runtime.rs` (embedded deno / owned loop).**
A `!Send` `JsRuntime` lives on a dedicated OS thread; callers submit `Command`s over an
`mpsc::UnboundedSender` and **`.await` a per-request `tokio::sync::oneshot`**
(`runtime.rs:63-103`, `invoke_stored` :245-263). The thread body is a current-thread
tokio runtime + `LocalSet` (`js_plane_thread` :280-361, comment :281-287) servicing the
command loop (`:310-359`). `InvokeStored` is documented as "the shared one-shot
invoke-stored-JS-function primitive" (`:90-100`); it is forward-only, JSON-in/JSON-out,
V8 handles never leave the thread (`:6-9`).

The family unifies these two shapes behind one envelope + one registry, differing only in
the **reply channel type** (std `SyncSender` = block a thread; tokio `oneshot` = `.await`).

---

## 1. The primitive's shape — a FAMILY of three seams

All three share the slice-1 envelope `{ id, kind, payload, aborted }` and the
`id -> reply` registry. They differ only in how the caller waits.

### (a) `call` — blocking call-from-dedicated-thread (EXISTS, `agent_bridge.rs:211`)

```rust
// Rust thread blocks on a channel recv until JS returns a value synchronously
// (or after awaiting an async JS closure). One id outstanding OR many (routed by id).
fn call(&self, kind: &str, payload: Value) -> Result<Value, BridgeError>;
//                                                    ^ Errored | Aborted | Disconnected (:120-127)
```
Reply channel: `std::sync::mpsc::sync_channel::<BridgeOutcome>(1)` (`:217`), `rx.recv()`
(`:230`). Used by every loop seam today (`streamFn`, `convertToLlm`, `toolExecute`, the
8 hooks). This is the loop-trio path and is already proven.

### (b) `call_async` — async-oneshot resolve variant (NEW; the file-mutation-queue seam)

```rust
// Rust `.await`s a JS promise resolution instead of blocking an OS thread.
// The worker runs inside a tokio runtime (NOT the Node thread); many concurrent
// tasks can each await their own JS promise while the single worker multiplexes.
async fn call_async(&self, kind: &str, payload: Value) -> Result<Value, BridgeError>;
```
Sketch (mirrors `call` but swaps the channel):
```rust
async fn call_async(&self, kind: &str, payload: Value) -> Result<Value, BridgeError> {
    if self.shared.signal.is_aborted() { return Err(BridgeError::Aborted); }   // :213-215
    let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);              // :216
    let (tx, rx) = tokio::sync::oneshot::channel::<BridgeOutcome>();
    self.shared.pending.lock().unwrap().insert(id, PendingReply::Async(tx));
    self.tsfn.call(envelope(id, kind, payload), NonBlocking);                  // :227-228
    match rx.await {                                                           // <-- .await, not recv()
        Ok(BridgeOutcome::Value(s)) => serde_json::from_str(&s).map_err(..),
        Ok(BridgeOutcome::Error(s)) => Err(BridgeError::Errored(..)),
        Ok(BridgeOutcome::Aborted)  => Err(BridgeError::Aborted),
        Err(_)                      => Err(BridgeError::Disconnected),         // sender dropped
    }
}
```
The **only registry change** is that `pending` holds an enum so `deliver`/`abort` can fan
out to either channel kind:
```rust
enum PendingReply { Sync(SyncSender<BridgeOutcome>), Async(oneshot::Sender<BridgeOutcome>) }
// BridgeShared.pending: Mutex<HashMap<u64, PendingReply>>   (was SyncSender, :133)
// deliver (:176-183) and abort-drain (:187-199) match on the variant; napi surface
// (resolveBridge/resolveBridgeError/abort, :294-311) is UNCHANGED — it only calls deliver.
```
The JS dispatcher (`_bridge/dispatcher.ts`) is **unchanged**: `resolveBridge(id, json)`
already settles "whatever is parked on id" — it does not care which channel type waits.

### (c) `emit` — fire-and-forget synchronous push (EXISTS as `dispatch_event`, `:240-249`)

```rust
// No id, no reply, no recv. NonBlocking TSFN enqueue only. Safe on ANY thread.
fn emit(&self, kind: &str, payload: Value);   // == dispatch_event / complete shape (:240-261)
```
This is the sync-emit event-bus seam and the tool `onUpdate` seam (`emit_tool_update`
:327-335). It cannot deadlock by construction (no round-trip) — see §2.

Worker entry points stay the `spawn_worker<F>` shape (`:703-716`). The async variant adds
a sibling `spawn_worker_async` that builds a **current-thread tokio runtime on the worker
std::thread** (exactly `runtime.rs:283-289`) and `block_on`s the `FnOnce -> impl Future`,
so `call_async` has a runtime to `.await` on without ever touching the Node thread.

---

## 2. How it avoids the JS-thread deadlock

The prior reuse-assessment's rule (from `atilla-agent-tier-port-status.md`): **a blocking
round-trip deadlocks iff the blocking recv runs ON the Node/JS/main thread** — because the
value can only arrive by Node running a microtask (`resolveBridge`), and a blocked JS
thread never runs microtasks. `agent_bridge.rs` avoids it by *construction*: the recv only
ever happens inside the `spawn_worker` closure, on a dedicated `std::thread` off any
runtime (doc `:46-50`). The primitive makes that invariant **explicit and enforced**:

1. **API shape (primary defense).** The blocking `call` / async `call_async` are reachable
   ONLY from inside the worker `FnOnce` passed to `spawn_worker[_async]`. No `#[napi]`
   method exposes a blocking recv — the napi surface is exclusively *non-blocking*
   (`resolveBridge`, `resolveBridgeError`, `abort`, `emitToolUpdate`, `isAborted`, `join`
   — all run on the Node thread and none of them recv). So a JS caller *cannot* invoke a
   blocking round-trip on its own thread; it can only kick off `run`/`spike*`, which spawn
   the worker thread first.

2. **Thread-id guard (belt-and-suspenders).** Capture the Node thread id at bridge
   construction (`AgentBridge::new`, the thread that owns the TSFN) into
   `BridgeShared.js_thread: ThreadId`. At the top of `call` / `call_async`:
   ```rust
   debug_assert_ne!(std::thread::current().id(), self.shared.js_thread,
       "blocking bridge recv on the JS thread would deadlock");
   ```
   Recommend also a **release-mode hard fail** returning `Err(BridgeError::Errored(
   "bridge misuse: blocking recv on JS thread"))` rather than only a debug assert, so a
   future misuse fails LOUD (mirrors the OAuth login "error-stub, not silent no-op"
   discipline in `ext-oauth-login-reentrant-primitive-parked`).

3. **`emit` is deadlock-immune.** The fire-and-forget seam does `tsfn.call(.., NonBlocking)`
   and returns — no id, no recv (`dispatch_event` :240-249; `emit_tool_update` documents
   exactly this at :318-326: "runs on the Node/JS thread ... never allocates a bridge id,
   never calls rx.recv()"). Therefore **sync-emit is safe even when emitted from the JS
   thread**, which is the specific case the prior assessment flagged as a deadlock for a
   *blocking* emit. The resolution: emit is void/fire-and-forget in pi
   (`event-bus.ts:4` `emit(channel, data): void`), so it maps to `emit`, never to `call`.

---

## 3. Safety conditions

`agent_bridge.rs` documents its hang-safety as three release paths + double-resolve safety
(doc `:52-65`) and the tests label the conditions **(A)…(G)**. Reproduced VERBATIM, then
extended for the async variant.

### Reused verbatim from `agent_bridge.rs` (doc comment `:52-65`)

> The loop thread blocks on `rx.recv()`, so **every** JS seam path must resolve the id
> exactly once. Three release paths exist:
> - `AgentBridge::resolve_bridge` — the normal success path.
> - `AgentBridge::resolve_bridge_error` — the JS exception / promise-rejection path.
>   Without it a thrown JS closure would park the Rust thread forever.
> - `AgentBridge::abort` — trips the cooperative signal **and drains every outstanding id**
>   with an aborted sentinel, so a mid-request abort unblocks the parked thread instead of
>   deadlocking it.
>
> Resolving an unknown or already-resolved id is a no-op (never a panic), so double-resolve
> races after an abort or error cannot crash the addon.

### The labeled conditions (A–G), verbatim from the test suite

- **(A)** rejection path: a throwing JS handler surfaces, never hangs
  — `agent-bridge-primitive.mjs:109-110`, loop `:8-9` "a JS streamFn that throws → the loop
  returns a terminal error message (clean surface, not a hang)". Impl: `resolve_bridge_error`
  → `BridgeOutcome::Error` (`:302-305`, `:233`).
- **(B)** abort mid-request unblocks the parked loop thread and the run settles
  — `agent-bridge-loop.mjs:10`, `agent-bridge-primitive.mjs:6-7` (paired with G: "the event
  loop is never starved"). Impl: `BridgeShared::abort` drains all pending (`:187-199`).
- **(C)** the process must still exit 0 on its own — no lingering handle
  — every test header (`primitive:8`, `loop:11`, `tools:16`, `hooks:20`). Impl: `join`
  reaps the thread (`:346-352`) and `spawn_worker` drops the TSFN clone so Node can exit
  (`:711-713`).
- **(E)** double-resolve / unknown id is a no-op, never a panic
  — `agent-bridge-primitive.mjs:125-139`. Impl: `deliver` removes-then-sends; a second
  resolve finds no entry (`:176-183`).
- **(F)** out-of-order concurrent resolution routes by id
  — `agent-bridge-primitive.mjs:141-168`, `spike_concurrent` (`:382-423`, doc "CONDITION-F
  proof" :382). Impl: per-id channel keyed in `pending`.
- **(G)** event loop keeps running while the Rust thread is parked (async JS work settles)
  — `agent-bridge-primitive.mjs:90-107` (setTimeout fires while Rust blocks). Impl:
  `NonBlocking` mode + off-thread recv (doc `:46-50`).
- **(D)** is not surfaced by name; the fourth error mode is `BridgeError::Disconnected`
  ("channel closed with no result (dispatcher gone) — treated as abort", `:125-126`, `:235`),
  the safe-fallback cousin of (B). Recommend the steward formally label it **(D) disconnect
  ⇒ abort-equivalent** when this note is folded into STEWARD.md.

### NEW conditions the async-oneshot variant adds

- **(H) no worker-runtime leak.** `spawn_worker_async` owns a current-thread tokio runtime
  on the worker std::thread (`runtime.rs:283-289` pattern); it must `block_on` to completion
  and drop the runtime before the thread exits, or condition (C) regresses. Test: the async
  proof harness must still exit 0.
- **(I) oneshot-drop ⇒ Disconnected, not panic.** `rx.await` returning `Err(_)` (sender
  dropped by an abort-drain that raced) maps to `BridgeError::Disconnected` (already the
  `call` contract `:235`); the async variant must preserve it — `abort` sends
  `BridgeOutcome::Aborted` into the oneshot *before* dropping, exactly as the sync drain does
  (`:187-199`), so the awaiter wakes with `Aborted` rather than a bare drop.
- **(J) single-resolution across channel kinds.** `PendingReply` is removed from the map on
  first `deliver` (`:177`) regardless of variant, so (E) double-resolve safety holds for
  async ids identically.

---

## 4. Coverage matrix

### UNLOCKS

| Blocked class | Seam used | Data crossing | Callback shape |
|---|---|---|---|
| **loop trio** (`agent-loop`/`agent`/`agent-harness`) | `call` (a) | AgentMessage[]/Model/Context JSON | blocking sync-return (streamFn, convertToLlm, 8 hooks, toolExecute) — **already served today** |
| **sync-emit event-bus** (`event-bus.ts`) | `emit` (c) | `{channel, data}` JSON | fire-and-forget `emit(channel,data):void` (`event-bus.ts:4`) — no round-trip |
| **file-mutation-queue** (`withFileMutationQueue`) | `call_async` (b) | `{path}` in, void-ack out | **await-a-promise** (queue admission) — the §5 proof case |
| **session-manager cluster** (`session`/`storage`/`repo`) | `call_async` (b) for the duck-typed async JS `FileSystem` (readFile/writeFile/readdir → Promise → `.await`); `call` (a) for the sync `entryTransform`/`projector` closures invoked in `buildContext`/`buildContextEntries` (`session-manager.ts:414`, method `:1205`); `emit` (c) for progress callbacks (`SessionListProgress`, `:703`) | Session JSONL entries, SessionEntry[], transformed context | mixed: async FileSystem + sync-return projector/transform + fire-and-forget progress |

Note the loop trio is listed for completeness — it is the pattern's *origin*, already
proven by `agent_bridge.rs`. The genuinely *new* unlocks are the async-oneshot variant
consumers: **file-mutation-queue** and the **session-manager FileSystem/`buildContext`**.

### DOES NOT UNLOCK (explicitly OUT OF SCOPE)

- **Live-Node-object cases — `ChildProcess` (bash spawn), `Readable`/`Writable` streams,
  live `EventEmitter`/`AbortSignal` objects, `Session` handles with method identity.**
  WHY: the bridge is strictly JSON-in/JSON-out; **V8 handles never cross the boundary**
  (identical rule to `extensions/runtime.rs:6-9` "only plain data … V8 handles never leave
  the owning thread"). A `ChildProcess` needs *live method dispatch* (`kill()`, `.stdout.on`)
  and *object identity* across many calls — that is a Rust-owned state machine + persistent
  dispatcher + live proxy (the **NativeAgent verdict**, `native-count-honesty-no-nominal-flips`),
  a separate deferred slice, not this JSON seam. Note bash/write/ls already have a *native*
  async ops backend (`operations-seam-locked-spec`, PR #157) — they do not need this bridge.
- **Reentrant / suspend-resume — extension OAuth `login`.** JS calls back INTO Rust
  mid-execution and AWAITS a Rust reply (`ext-oauth-login-reentrant-primitive-parked`).
  This primitive is forward-only (Rust→JS→Rust, one direction per id); reentrancy is a
  bidirectional rendezvous, a DIFFERENT primitive in the same parked decision packet.
- **Object-identity assertions.** `agent.test.ts` cases asserting `.toBe` on
  `state.model/tools/messages` (`native-count-honesty-no-nominal-flips`) — a JSON `state()`
  boundary cannot preserve JS reference identity. Out of scope by nature of JSON transport.

---

## 5. Proof plan — the ONE real flip: file-mutation-queue via `call_async`

**Why this one:** smallest genuine consumer of the *new* async-oneshot variant. pi's
`withFileMutationQueue<T>(filePath, fn): Promise<T>` (`file-mutation-queue.ts:33-62`) is a
pure JS promise-chain serializer: same-path ops serialize, different-path ops run in
parallel, and the slot releases in a `finally`. It exercises exactly the "Rust `.await`s a
JS promise resolution" contract with the least surrounding machinery.

**Non-reentrant framing (important — avoids the parked reentrant primitive).** Do NOT pass a
Rust closure into `withFileMutationQueue` (that would require JS→Rust reentrancy). Instead
split it into an **acquire (await) + release (fire-and-forget)** pair in a thin JS shim:

```ts
// _bridge shim, wraps pi's real withFileMutationQueue — no manifest row (helper)
const releasers = new Map<number, () => void>();
async function fmqAcquire(bridge, id, { path }) {
  await withFileMutationQueue(path, () => new Promise<void>((release) => {
    releasers.set(id, release);          // hold the slot open…
    bridge.resolveBridge(id, "null");    // …and tell Rust the slot is granted (await settles)
  }));                                    // this promise resolves only when Rust releases
}
function fmqRelease(_bridge, { id }) { releasers.get(id)?.(); releasers.delete(id); }
```

Rust side (write tool, on the async worker):
```rust
let token = channel.call_async("fmqAcquire", json!({ "path": p })).await?; // await queue admission
let out = do_native_write(&p, bytes);                                       // Rust owns the write
channel.emit("fmqRelease", json!({ "id": token_id }));                      // fire-and-forget release
```
Abort mid-wait ⇒ `BridgeOutcome::Aborted` wakes the awaiter (condition I), and a
`fmqRelease` for an unknown id is a no-op (condition E parity on the JS side).

**What it proves:** (b) `call_async` end-to-end; (G/H) event loop unstarved while Rust
awaits; (B/I) abort releases the awaiter; same-path serialization + different-path
parallelism observable exactly as `file-mutation-queue.test.ts` asserts. Ship as a
standalone `crates/pidgin-napi/__tests__/agent-bridge-async-oneshot.mjs`, mirroring
`agent-bridge-primitive.mjs`.

**Manifest row it touches:** at most a `tests[]` addition on the **write-tool row** for the
`file-mutation-queue.test.ts` same-path-serialization + aborted-in-flight cases (`L176`,
per `operations-seam-locked-spec`) — a crew-allowed ROW ADDITION only, never
`conformance.json` / `STEWARD.md` (`steward-flip-crew-model`). See §6 open-question on
whether this earns a `status:native` flip at all.

---

## 6. Open questions / decisions for coordinator + steward

1. **Native-count honesty tension (needs a steward ruling).** The §5 proof keeps pi's JS
   `withFileMutationQueue` as the source of truth and has *Rust await it* — the value is the
   reusable async-oneshot seam, not a native flip of the queue's own logic. Per
   `native-count-honesty-no-nominal-flips`, that must NOT be counted `status:native` (the
   queue logic still runs in JS). Meanwhile `operations-seam-locked-spec` says the queue is
   ALREADY served natively by the Rust `Deferred`. So: is file-mutation-queue a *primitive
   proof harness only* (no manifest flip), or does the steward want the write-tool row's
   `tests[]` extended? Recommend: **proof harness only**, no `status` change.

2. **Async worker runtime placement.** `spawn_worker_async` needs a tokio runtime on the
   worker thread. Confirm we build a fresh `new_current_thread` runtime per bridge (like
   `runtime.rs:283-289`) rather than reusing any ambient one — the whole point is to stay
   OFF the Node thread and OFF any ambient multi-thread runtime (`exec-tools-async-vs-sync-agenttool`
   nested-`block_on` hazard). Recommend: fresh current-thread runtime, matching extensions.

3. **Registry unification vs. two registries.** §1 folds sync + async into one
   `HashMap<u64, PendingReply>`. Alternative: two maps. The single-map enum keeps `deliver`
   / `abort` / double-resolve (E) uniform across variants — recommend the single map.

4. **Straitjacket ceiling.** `agent_bridge.rs` is 1046 lines with a top-of-file
   `straitjacket-allow-file:duplication` marker (`:67-71`) for the 8 near-identical hook
   seams. The async variant + `PendingReply` fan-out adds ~80–120 lines; still under the
   ~1500 convention but the coordinator should decide whether the async variant lives in
   `agent_bridge.rs` or a sibling `bridge_async.rs` module to keep the file comfortably
   under ceiling and to keep the loop-trio concerns separate from the fs/session concerns.

5. **Session-manager sequencing.** Session-manager needs BOTH variants (async FileSystem +
   sync projector) plus it is a large coupled subsystem (`session-manager.ts` 1623 lines).
   Confirm the intended order: prove `call_async` on file-mutation-queue FIRST (§5), then
   land session-manager as a separate slice consuming the now-proven seam — not both at once.

6. **Decision-packet linkage.** `ext-oauth-login-reentrant-primitive-parked` notes Zack's
   "novel reentrant/preemptible primitives" packet has NO memory note yet. THIS primitive is
   the *forward-only* sibling that is buildable now; the reentrant login primitive is NOT.
   Coordinator: should this note seed that packet's slug, with the reentrant/NativeAgent
   verdicts kept explicitly separate (future wave)?
