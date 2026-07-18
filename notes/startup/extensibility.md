# Cross-language extensions for the Rust rewrite

Decision-ready design for a single, deliberately narrow capability: letting
extension authors in other host languages call **more or less the same extension
API** that a JS/TS author calls in pi today.

**Scope box (explicit).** This is the only thing being added. The Rust core is a
faithful **mirror of upstream pi** — we are not adding new extension mechanisms
to pi, and we are not changing pi's own extension model. There is **no MCP, no
out-of-process JSON-RPC, no WASM, no embedded-scripting mechanism, and no
layered fallback story** in this document. The single addition is a native
binding layer that exposes pi's *existing* `ExtensionAPI` surface to PHP, then
Python, then Node, so an author in those languages registers tools, hooks, and
commands the same way a JS/TS author does.

Source material: `scratchpad/pi_extensibility.md` (pi, cited `file:line` against
`/workspace/pi` @ `3da591ab`). Verified currency is mid-2026.

**Relationship to `notes/startup/deep-hooks.md`.** This document defines the overall
cross-language extension API *surface* — the tools, hooks, and commands exposed
to host languages, and the one internal `Tool` / `Hook` / `Command` registry they
lower onto. The detailed *dispatch* mechanics for deep block/modify/replace hooks
across host-thread constraints — how a synchronous host closure acts as awaited
middleware on the async core without deadlocking — are specified in the merged
`notes/startup/deep-hooks.md`, which this document tracks. Section 4 summarizes that model
and cites it; deep-hooks.md is the authoritative detailed spec.

---

## 1. Scope and goal

**The one capability.** Today a pi extension is JS/TS. We want an author writing
PHP, Python, or Node to register tools, hooks, and commands against the same
agent, calling **more or less the same API** — a `pi`-like handle with
`registerTool` / `on(event, handler)` / `registerCommand`. Nothing more.

**Language order.** PHP first (via `ext-php-rs`), then Python (via PyO3), then
Node (via napi-rs).

**Mirror upstream pi.** The Rust core reproduces pi's extension model as-is; it
does not extend it. pi's own JS/TS extension path is part of the mirror and is
**not** touched by this work. However the Rust mirror chooses to run pi's own
JS/TS extensions is a separate, already-owned decision — this document does not
recommend or discuss a JS engine. The conformance bar for the JS/TS path remains
**passing pi's own test suite**, and that bar is unchanged here: this work only
adds *sibling* language bindings that expose the **same extension registry** the
JS/TS path registers against.

---

## 2. The API we are exposing

pi's current `ExtensionAPI` object is the contract every language binding must
reproduce. An extension is a **default-exported factory
`(pi: ExtensionAPI) => void | Promise<void>`** that receives the live `pi`
handle and registers things by calling methods on it (type at
`packages/coding-agent/src/core/extensions/types.ts:1167`; loaded in-process,
`.../extensions/loader.ts:403-419`; docs `docs/extensions.md:213-260`). Each
host-language binding must hand its author an equivalent `pi`-like handle with
the same registration methods.

The surface each binding must mirror:

| Surface | API | Contract to reproduce |
|---|---|---|
| **Tools** (LLM-callable) | `pi.registerTool(def)` (`types.ts:1220`, def `types.ts:439-486`) | `parameters` schema (TypeBox today); async `execute(id, params, signal, onUpdate, ctx)` → `AgentToolResult {content, details, addedToolNames?, terminate?}` (`agent/src/types.ts:349-361`). Streaming via `onUpdate`; cancel via `AbortSignal`; throw to signal error. |
| **Hooks** (~35 lifecycle events) | `pi.on(event, handler)` (diagram `docs/extensions.md:274-341`) | Handlers run in load order; many **block / modify / replace**. `tool_call` mutates `event.input` in place or returns `{block, reason}` (the permission gate); `tool_result` middleware-patches; `before_provider_request` replaces the outgoing payload; `context` mutates the message array; `before_agent_start` rewrites the system prompt (chained). |
| **Commands / CLI / UI** | `registerCommand`, `registerShortcut`, `registerFlag`, `registerMessageRenderer`, `registerEntryRenderer` (`types.ts:1229-1261`) | `/name` slash commands with completions; CLI flags; custom TUI rendering. |
| **Providers** | `registerProvider` (`types.ts:1382-1398`) | Register/override model providers dynamically. |

Handlers receive `ctx: ExtensionContext` — live stateful handles:
`sessionManager`, `modelRegistry`/`model`, `ui`, `cwd`, `mode`, `signal`.
Command handlers get a richer `ExtensionCommandContext` with
`newSession/fork/switchSession/reload`. **These context objects are the
coupling**: a JS extension receives and mutates live JS objects. A host-language
binding cannot hand out live JS objects; it hands out host-language proxies
backed by the same core state. Reproducing this handle faithfully in each
language is the substance of "the same API."

This section is the spec. Each binding in Sections 3–6 is measured against it.

---

## 3. Design: one Rust extension registry, many language bindings

The Rust core owns the extension registry and the agent runtime, and defines one
stable internal trait API — `Tool` / `Hook` / `Command` (plus providers and
renderers) — the faithful successor to pi's `ExtensionAPI` object. This is the
same registry the JS/TS mirror path registers against; the language bindings add
no second registry.

Each native binding (`ext-php-rs` / PyO3 / napi-rs) hands the host language a
`pi`-like handle whose methods (`register_tool`, `on`, `register_command`)
accept **host-language callables** and wrap each one as a registry entry
(`dyn Tool` / `dyn Hook` / `dyn Command`). A host-language extension therefore
has the same shape as pi's JS `(pi) => {}` factory: an entrypoint that receives
`pi` and registers things on it. The core never learns which language a
registration came from — it sees registered trait objects.

**Data mapping across the boundary** (the same in every language, expressed
idiomatically):

| pi concept | Rust registry | Host language |
|---|---|---|
| `parameters` schema (TypeBox) | JSON Schema on the descriptor | host dict/array literal, or a schema builder |
| tool args (`params`) | `serde_json::Value` | associative array / dict |
| tool result `{content, details, terminate}` | `ToolOutput` | host return value (array/dict/object) |
| `onUpdate(partial)` streaming | `ctx.emit(partial)` | callback/closure passed into `execute` |
| `AbortSignal` cancel | `CancellationToken` | host-visible cancel flag/token |
| hook event object | `&mut HookEvent` | host struct / associative array |
| hook `{block, reason}` / mutation | `HookOutcome` | host return value or in-place edit |

The binding's job is exactly this marshalling plus one hard constraint on *when*
and *on which thread* the host callable may be invoked — Section 4.

---

## 4. The core engineering challenge: invoking host-language code from Rust

This is the crux: a host-language closure (PHP, Python, Node, Ruby) has to act as
awaited block/modify/replace middleware on the Rust core's multi-threaded tokio
hot path, without deadlocking and without reentering a host virtual machine from
a thread it does not own. `notes/startup/deep-hooks.md` is the authoritative spec; the
model below is its summary — see deep-hooks.md §2 (the per-host threading table),
§4 (the two-flavor decision), §6 (the Rust sketch), and §7 (timeouts, ordering,
cancellation).

**Two dispatch flavors behind one uniform `Hook`.** Every host closure is an
`impl Hook`, so the core dispatch is always `hook.handle(&mut event, ctx).await`.
What that `await` does underneath is chosen by one test: does the host expose a
`Send` handle callable from a tokio worker thread?

- **Flavor 1 — trampoline (`Send` handle): Python (PyO3) and Node (napi-rs).**
  The handle — a `Py<PyAny>`, or a napi `ThreadsafeFunction` — is itself `Send`,
  lives inside the `Hook`, and is dispatched directly: acquire the GIL for Python
  (`Python::with_gil`), enqueue onto the libuv loop for Node. No extra thread, no
  rendezvous.
  - *Python rule:* whichever thread starts the core must release the GIL first
    (`py.allow_threads(...)`), or a worker thread reacquiring it for a callback
    deadlocks. `async def` closures are driven via `pyo3-async-runtimes`.
  - *Node hard constraint:* the binding's run must return a `Promise` (async). A
    blocking synchronous JS call into the core would leave queued hook callbacks
    undrained — a deadlock. Return values come back via `tsfn.call_async(...)
    .await`, including an awaited `Promise`.
- **Flavor 2 — thread-bound rendezvous / reentrant pump (`!Send` handle): PHP
  (ext-php-rs) and Ruby.** The VM handle is `!Send` and stays on its owning
  thread; the `Hook` carries only a `Send + Sync` token — a closure id plus a
  channel to the owner thread — which runs a *reentrant* pump (`pump_until_done`,
  a stack, not a flat loop) so a closure that reenters the core to call a tool is
  still serviced on the same thread. PHP builds one process-wide tokio runtime
  lazily *after* the php-fpm fork; each PHP call does `block_on`; PHP never
  touches a tokio worker thread. PHP has no in-language async — a PHP closure is
  synchronous by nature (a stated limitation).

**The load-bearing invariant.** The `!Send` VM handle never enters the tokio
world; only `serde_json::Value` request/response data crosses the boundary. That
is what lets `HostClosureHook` (and `HostClosureTool`) be `Send + Sync` even when
the language it fronts is not. The `&mut HookEvent` is never lent across FFI: it
is serialized to `event_json`, the host returns a `HookOutcome`, and the core
applies that outcome to the real `&mut` on the Rust side. The async core awaits
the synchronous host answer through a `oneshot` channel, so the tokio task parks
cooperatively rather than blocking an OS thread.

**Ordering, timeouts, cancellation** (deep-hooks.md §7). Hooks for one event run
in registration order — the core awaits one at a time. Every dispatch is wrapped
in `tokio::time::timeout`: the `tool_call` permission gate is **fail-closed**
(`Block`), advisory hooks **fail-open** (`Continue`), and a boundary panic maps
to the same fallback. Cancellation flows through the session `CancellationToken`
in `HookContext`. This is the honest part of "the same API in another language":
the API *shape* is reproducible everywhere, but concurrency fidelity is not — PHP
is synchronous and request-scoped, Node comes closest to pi's own behavior, and
Python sits between.

---

## 5. Core Rust API sketch

Illustrative (compilable-looking, not literal). One object-safe trait registry —
the successor to pi's `ExtensionAPI` — plus the single bridge that matters here:
a host-language closure wrapped as a registry entry, tagged with its thread
affinity (Section 4).

```rust
use std::sync::Arc;

// ---- The one internal contract (successor to pi's ExtensionAPI object) ----

/// Successor to pi's ToolDefinition (types.ts:439). JSON Schema replaces TypeBox.
pub struct ToolDescriptor {
    pub name: String,                    // LLM-facing tool name
    pub label: String,                   // UI label
    pub description: String,             // shown to the model
    pub input_schema: serde_json::Value, // JSON Schema (TypeBox lowers to this)
    pub affinity: Affinity,              // which threads may run this extension
}

/// Successor to AgentToolResult (agent/src/types.ts:349).
pub struct ToolOutput {
    pub content: Vec<Content>,        // returned to the model (text/image)
    pub details: serde_json::Value,   // structured data for UI + state
    pub terminate: bool,              // stop after batch if all terminate
}

/// Successor to pi's execute(id, params, signal, onUpdate, ctx).
pub struct ToolContext {
    pub cancel: CancellationToken,        // == AbortSignal (cooperative)
    pub session: Arc<dyn SessionHandle>,  // == ctx.sessionManager, live state
    emit: Box<dyn Fn(ToolOutput) + Send>, // == onUpdate streaming
}
impl ToolContext {
    pub fn emit(&self, partial: ToolOutput) { (self.emit)(partial) }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext)
        -> anyhow::Result<ToolOutput>;
}

/// Successor to pi's ~35 pi.on(event, handler) hooks with block/modify/replace.
pub enum HookOutcome {
    Continue,                       // pure observation
    Modify(serde_json::Value),      // e.g. tool_call mutating event.input
    Replace(serde_json::Value),     // e.g. before_provider_request
    Block { reason: String },       // the permission gate (tool_call -> {block})
}

#[async_trait::async_trait]
pub trait Hook: Send + Sync {
    fn event(&self) -> HookEvent;   // ToolCall | ToolResult | Context | ...
    async fn handle(&self, event: &mut HookEvent, ctx: &HookContext) -> HookOutcome;
}

#[async_trait::async_trait]
pub trait Command: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, args: Vec<String>, ctx: &CommandContext) -> anyhow::Result<()>;
}

/// Where an extension is allowed to run. Matches notes/startup/deep-hooks.md.
#[derive(Clone, Copy)]
pub enum Affinity {
    AnyThread,      // Python: a worker thread calls it under the GIL (trampoline)
    HostThreadOnly, // Node, PHP, Ruby: pinned to the host's owning thread
    OwnRuntime,     // the embedded deno_core JS plane (pi's own JS/TS extensions)
}

/// The `pi` object equivalent: the registry extensions lower onto. Each binding
/// hands the host language a proxy whose methods call these.
pub trait ExtensionHost {
    fn register_tool(&mut self, tool: Arc<dyn Tool>);
    fn register_hook(&mut self, hook: Arc<dyn Hook>);
    fn register_command(&mut self, cmd: Arc<dyn Command>);
}

// ---- The one bridge: a host-language callable wrapped as a registry entry ----

/// Wraps a host-language callable as a registry entry. `Send + Sync` because it
/// holds only a token + channel (flavor 2) or a `Send` handle (flavor 1) — never
/// the VM handle itself; only `serde_json::Value` crosses. See deep-hooks.md §4, §6.
pub struct HostClosureTool { callable: HostCallable, desc: ToolDescriptor }

#[async_trait::async_trait]
impl Tool for HostClosureTool {
    fn descriptor(&self) -> &ToolDescriptor { &self.desc }
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext)
        -> anyhow::Result<ToolOutput> {
        // The scheduler guarantees this resolves on the host thread. `ctx.emit`
        // is passed through so the host callable can stream partial updates
        // inline; `ctx.cancel` is polled cooperatively by the host.
        self.callable.invoke_on_host_thread(args, ctx) // sync; request-scoped
    }
}
// HostClosureHook / HostClosureCommand are the same pattern over Hook / Command.
```

**Per-binding lowering of `HostCallable::invoke_on_host_thread`:**

- **PHP (`ext-php-rs`):** the callable is a PHP closure held as a `Zval`;
  `invoke` calls it on the request thread. No background dispatch exists, so the
  affinity marker is not a restriction so much as the only possibility.
- **Python (PyO3):** the callable is a `Py<PyAny>`; `invoke` does
  `Python::with_gil(|py| callable.call1(py, args))`, marshalling the JSON args
  to Python objects and the return value back to `ToolOutput`.
- **Node (napi-rs):** the callable is a JS function. On the main thread it is
  called directly; if the core ever needs to reach it from another thread the
  binding routes through a `ThreadsafeFunction`, which posts to the event loop.

The `Affinity` marker is the linchpin: the core scheduler consults it to pick a
dispatch flavor — `AnyThread` trampolines Python under the GIL, `HostThreadOnly`
pins Node/PHP/Ruby to the owning host thread (trampoline for Node's `Send`
handle, rendezvous for PHP/Ruby's `!Send` handle), and `OwnRuntime` routes the
embedded deno_core JS plane on its own thread. This structurally enforces the
Section 4 constraint rather than relying on discipline. Full dispatch mechanics
are in `notes/startup/deep-hooks.md` §4 and §6.

---

## 6. Rollout order and per-language notes

PHP, Python, and Node are the first-class trio; Ruby is a marginal, lowest-
priority target (its `magnus::Value` is `!Send`, so it takes the flavor-2
rendezvous path — deep-hooks.md §2, §4). Two spikes already ground this: the PHP
hello spike (`throwaway/php-hello`, PR #9) and the Node spike (PR #13).

**1. PHP — `ext-php-rs` (first).** The first binding target and the most
concretely grounded. Caveats: the crate is 0.x with no backward-compatibility
guarantee; PHP is single-threaded per request; callbacks are tied to the request
lifecycle; there is **no in-language async** — a PHP closure is synchronous by
nature and long work blocks the request thread. Dispatch is flavor-2 rendezvous
with the pump collapsed onto the request thread. Idiomatically, "more or less the
same API" is a `Pi` object whose `$pi->registerTool([...])` takes an
associative-array descriptor and a PHP closure for `execute`.

**2. Python — PyO3 (second).** Dispatch is flavor-1 trampoline: the core runs
under `py.allow_threads`, and each callback reacquires the GIL via
`Python::with_gil`. `Py<PyAny>` closures are `Send`, and `async def` closures are
driven through `pyo3-async-runtimes`. Idiomatically, a `pi` object with
`pi.register_tool(...)` taking a dict descriptor and either a plain function or a
coroutine for `execute`, with `on(event, handler)` for hooks.

**3. Node — napi-rs (third).** Dispatch is flavor-1 trampoline through a
`ThreadsafeFunction`; the binding's run returns a `Promise` so the loop thread
stays free to drain queued callbacks. Closest to pi's own Node semantics — native
async through the event loop. Idiomatically the binding is nearly the literal
`(pi) => { pi.registerTool(...) }` shape pi authors already use, because the host
language *is* JS — registration just reaches the Rust registry through the native
binding rather than pi's own loader.

**Ruby (marginal).** Lowest priority. `magnus::Value` is `!Send`, so Ruby takes
the same flavor-2 rendezvous as PHP; whether it earns that machinery before
demand is proven is an open question in `notes/startup/deep-hooks.md` §9.

---

## 7. Open questions

Scoped to this narrow goal. The *dispatch* open questions — reentrancy depth,
ZTS-vs-NTS shipping, Ruby placement, async-host-closure policy, trampoline
return-value latency, and deep hooks over IPC — live in `notes/startup/deep-hooks.md` §9
and are not repeated here. What remains specific to the API surface:

1. **How literal must "the same API" be?** One shared method vocabulary across
   all languages, or idiomatic-per-language handles (array vs dict vs object
   descriptors, method naming conventions)?
2. **How are host-language extensions discovered and loaded?** Presumably a
   manifest pointing at a PHP/Python/Node entrypoint, mirroring pi's existing
   extension discovery — what does that manifest look like?
3. **Which hooks are exposed to host-language extensions?** All ~35 events
   including the deep block/modify/replace middleware, or a subset — with the
   rest JS-only? (The dispatch for whichever set is exposed is settled in
   deep-hooks.md; this is a surface-scoping question, not a mechanics one.)
4. **Language priority after PHP — Python or Node next?** PHP is firmly first
   and Ruby last; the ordering of the middle two is unsettled.

The threading and async-per-language mechanics that earlier drafts listed here —
whether PHP's synchronous execution is acceptable, whether cancellation and
streaming fidelity suffice per language — are now **decided** in
`notes/startup/deep-hooks.md` (two-flavor dispatch, per-hook timeout/fallback,
cooperative cancellation); reference them as settled rather than open.

---

## 8. Summary

**The single capability.** Extension authors in other host languages — PHP
first, then Python, then Node — can register tools, hooks, and commands against
the same agent, calling more or less the same extension API a JS/TS author calls
in pi today. Nothing else is added: the Rust core mirrors pi as-is, with no MCP,
no out-of-process transport, no WASM, and no new extension mechanisms.

**The approach.** The Rust core owns one extension registry — the successor to
pi's `ExtensionAPI` object, the same registry the JS/TS mirror path uses. Each
native binding (`ext-php-rs`, then PyO3, then napi-rs) hands the host language a
`pi`-like handle whose `register_tool` / `on` / `register_command` methods wrap
host-language callables as registry entries, so a host-language extension has the
same `(pi) => {}` factory shape. JSON-Schema params, `{content, details,
terminate}` results, and hook event objects marshal to and from host arrays,
dicts, and structs.

**The main constraint.** A host closure must act as awaited middleware on the
async core without reentering a host VM from a thread it does not own. This
resolves into two dispatch flavors (Section 4, specified in
`notes/startup/deep-hooks.md`): a **trampoline** for hosts with a `Send` handle (Python
under the GIL, Node via a `ThreadsafeFunction` with a `Promise`-returning run),
and a **thread-bound rendezvous** with a reentrant pump for `!Send` hosts (PHP,
Ruby). Only `serde_json::Value` crosses the boundary, so `HostClosureHook` stays
`Send + Sync` regardless of the language it fronts. PHP is synchronous and
request-scoped, Node comes closest to pi's own semantics, Python sits between,
and Ruby is a marginal flavor-2 target. The open questions (Section 7) are how
literal the API must be, how extensions are discovered, and which language comes
after PHP.
