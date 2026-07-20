# Deep hooks from any host language

This note extends the extension design in `notes/startup/extensibility.md` (open
draft PR #6, branch `docs/extensibility-research`). That design defines a
single internal trait registry — `Tool`, `Hook`, `Command` — onto which
every extension mechanism lowers, plus an `Affinity` enum that records
which thread a given extension is allowed to run on. This note answers the
question that design left open: how a synchronous host-language closure
(PHP, Python, Node, Ruby) registered through a native binding can act as
full block/modify/replace middleware on the Rust core's async hot path,
without deadlocking and without corrupting a host virtual machine that was
never built for foreign-thread reentry.

The design in PR #6 is not yet on `main` and not yet in code; the trait
signatures here are sketches that track that document, not compiled types.

---

## 1. The problem

pi exposes roughly 35 lifecycle hooks through `pi.on(event, handler)`.
Many are not observers. `tool_call` mutates `event.input` in place or
returns `{block, reason}` and is the permission gate. `tool_result`
patches a result. `before_provider_request` replaces the outgoing payload.
`context` rewrites the message array. The Rust successor models this as:

```rust
pub enum HookOutcome {
    Continue,                       // pure observation
    Modify(serde_json::Value),      // tool_call mutating event.input
    Replace(serde_json::Value),     // before_provider_request
    Block { reason: String },       // the permission gate
}

#[async_trait::async_trait]
pub trait Hook: Send + Sync {
    fn event(&self) -> HookEvent;
    async fn handle(&self, event: &mut HookEvent, ctx: &HookContext)
        -> HookOutcome;
}
```

The core drives an agent loop on a multi-threaded tokio runtime. A hook
fires on whatever worker thread reached the hook point, and the core
`.await`s its `HookOutcome` before continuing — the result gates, mutates,
or replaces the next step. A host closure has to behave as that awaited
middleware.

The obstacle is that none of the target host runtimes is freely callable
from an arbitrary tokio worker thread. Each owns its own thread and its own
lock, and calling in from the wrong thread is undefined behavior, not a
slow path. The rest of this note is about paying that cost correctly.

---

## 2. Threading reality per host

| Host | Concurrency model | Foreign thread may call in? | VM handle `Send`? | Reentry primitive |
|---|---|---|---|---|
| PHP (NTS) | One interpreter per request, one thread | No | No (`Zval` is thread-bound) | None — must run on the request thread |
| PHP (ZTS) | Thread-local interpreter via TSRM | Only a thread that owns a TSRM context | No | Bind interpreter to one OS thread |
| Python | GIL serializes all bytecode | Yes, after acquiring the GIL | Yes (`Py<T>: Send + Sync`) | `Python::with_gil`, `allow_threads` |
| Node | Single libuv event loop thread | No — enqueue onto the loop thread | Yes (`ThreadsafeFunction` is `Send`) | `napi_call_threadsafe_function` |
| Ruby | GVL serializes all execution | Only a VM-registered thread | No (`Value` is `!Send`) | `rb_thread_call_with_gvl` |
| deno_core | `JsRuntime` event loop, single thread | No | No (`JsRuntime` is `!Send`) | Poll on its own thread / `LocalSet` |

Reading across the table, three distinct classes fall out, and the
recommended dispatch differs per class.

**PHP.** The `php-hello` spike (`throwaway/php-hello`) already established
the shape: build one process-wide tokio runtime lazily after the php-fpm
fork, and have each PHP entry point `block_on` a core future. PHP never
runs on a tokio worker thread. NTS PHP is single-threaded per request; ZTS
adds thread-local storage through TSRM but still expects one interpreter to
stay pinned to one OS thread. Either way, a `Zval` or a callable captured
from PHP cannot cross to another thread. The closure has to run on the same
thread that entered the core.

**Python.** The GIL makes Python the most forgiving target. A tokio worker
thread can call `Python::with_gil(|py| callable.call1(py, args))` and get a
return value synchronously; the GIL serializes it against the rest of the
interpreter. `Py<PyAny>` is `Send + Sync`, so the closure handle can live
inside the `Hook` and travel to the worker thread. The one rule that makes
this safe is that whichever thread starts the core must release the GIL
first (`py.allow_threads(...)`), or a worker thread that tries to reacquire
it for a callback will wait forever. Free-threaded builds (3.13+, PEP 703)
relax the serialization but not the requirement to hold an attached thread
state, so the mechanism is unchanged.

**Node.** JavaScript runs only on the libuv loop thread. napi-rs wraps the
Node-API `napi_threadsafe_function` primitive as `ThreadsafeFunction`,
which is `Send` and can be called from any thread; the call is queued onto
the loop thread and drained there. To read a return value back, napi-rs can
await the JavaScript result (including a returned `Promise`) through a
per-call channel. The hard constraint is that the loop thread must stay
free: if JavaScript entered the core through a blocking synchronous call,
the queued hook callback sits behind that blocked frame and nothing drains
it. The Node binding therefore has to return the run as a `Promise`.

**Ruby.** The GVL behaves like the GIL, but magnus `Value` is `!Send` and a
foreign thread must be introduced to the VM before it may touch Ruby.
Running the core under `rb_thread_call_without_gvl` and reentering a
callback under `rb_thread_call_with_gvl` is possible, yet the `!Send`
values mean a closure handle cannot simply ride inside a `Hook` the way a
`Py<T>` can. Ruby lands closer to PHP: keep the closure on its owning
thread and hand work to it, rather than reaching in from a worker thread.
Ractors offer true isolation but only share a narrow set of shareable
objects, which is too restrictive for passing arbitrary hook payloads.

**deno_core.** The embedded JavaScript compatibility plane is its own case
and matters for coexistence (section 5). A `JsRuntime` is `!Send`, owns an
event loop, and is pinned to one thread — the same discipline as a host VM,
just one the core owns rather than one the host owns.

---

## 3. Candidate architectures

Four mechanisms are on the table. They are not mutually exclusive; the
recommendation in section 4 assigns each host class the one that fits.

### Option A: direct trampoline

The core, on a tokio worker thread, blocks the current task and dispatches
the closure onto the host thread, then resumes when the answer returns.

For a host with a thread-safe handle and a per-thread lock — Python, and
Node through its queue — this is direct and cheap. The worker thread
acquires the GIL, or enqueues onto the loop, runs the closure, and reads
the result. No extra thread, ordering preserved by awaiting one hook at a
time.

For PHP and Ruby it does not work at all: there is no thread-safe handle to
call, and the interpreter is not reentrant from a foreign thread. The
deadlock is concrete. If the host thread entered the core with a blocking
call and is parked inside `block_on`, and the core then tries to trampoline
back to that same thread, the thread is not listening. It is blocked on the
core, and the core is blocked on it.

### Option B: inversion / pull model

Flip ownership of the wait. The host thread does not block idly inside the
core; it runs a pump loop that pulls hook requests out of the core and
answers them. The core makes progress on background tokio threads; each
time it reaches a host hook, it parks that one task and surfaces a request
on a channel. The host thread — the only thread that may touch the VM —
receives the request, runs the closure, and sends the outcome back. The
core resumes.

This is the natural model for a synchronous single-threaded host. The core
only advances while the host is inside a core call, which is exactly PHP's
request lifetime. The host thread is, for the duration of a run, a hook
server for the core.

The cost is re-entrancy. A closure servicing hook A may call back into the
core (say, to invoke a tool). That inner core call can itself reach hook B,
which must land on the same host thread — but that thread is busy running
closure A and is not sitting in the pump. The fix is a reentrant pump: the
FFI call that reenters the core runs the same pull loop internally until
its sub-operation finishes, so nested hook requests are still serviced. The
pump is a stack, not a flat loop.

### Option C: dedicated dispatcher thread plus marshalling

Give each host VM one dedicated OS thread that owns the interpreter, and
route every hook call to it through a queue, marshalling arguments and
results as `serde_json::Value`. This is really Option B with an explicit owner
thread rather than borrowing the host's entry thread, and it is the honest
description of what a rendezvous is underneath. It buys a clean ownership
story at the cost of one parked thread per host VM and a marshalling hop on
every call. For Node it collapses into the loop thread the runtime already
owns; for PHP under php-fpm the "dedicated thread" is just the request
thread. It is most useful as the mental model, less so as a separate
implementation.

### Option D: timeouts and fallback policy

Independent of the transport, a host closure can hang or panic. Every hook
dispatch is wrapped in `tokio::time::timeout` with a per-hook fallback:
fail-open (`Continue`) for advisory hooks, fail-closed (`Block`) for the
permission gate. A closure that panics is caught at the boundary and mapped
to the same fallback. This is policy layered on top of A or B, not an
alternative to them.

### Deadlock and re-entrancy summary

| Concern | Trampoline (A) | Inversion (B) |
|---|---|---|
| Host thread parked in core | Deadlock for PHP / Ruby | Safe — host runs the pump |
| Closure calls back into core | Fine (Python GIL is reentrant) | Needs a reentrant pump |
| Ordering across hooks for one event | Preserved by awaiting one at a time | Preserved by the single pump |
| Extra threads | None | One owner thread per host VM |
| Works without a `Send` VM handle | No | Yes |

---

## 4. Recommended architecture per host class

There is one abstraction with two dispatch flavors. Every host closure is
an `impl Hook` behind the registry, so the core dispatch is uniform:
`hook.handle(&mut event, ctx).await`. What differs is what that `await`
does underneath, and the deciding factor is whether the host exposes a
`Send` handle callable from a worker thread.

**Flavor 1 — thread-safe handle (trampoline).** Python and Node. The
non-Rust handle (`Py<PyAny>`, or a napi `ThreadsafeFunction`) is itself
`Send` and lives inside the `Hook`. Dispatch calls it directly: acquire the
GIL for Python, enqueue onto the loop for Node. No extra thread, no
rendezvous.

**Flavor 2 — thread-bound rendezvous (inversion).** PHP and Ruby. The VM
handle is `!Send` and stays on its owning thread. The `Hook` carries only a
`Send + Sync` token — a closure id plus a channel to the owner thread. The
owner thread runs the reentrant pump.

Assignments:

- **Sync single-threaded (PHP NTS/ZTS, Ruby):** inversion / rendezvous. The
  host's entry thread is the hook server. Core runs on background tokio
  threads; hook requests arrive on a channel; the pump is reentrant so a
  closure may reenter the core. Timeouts still apply, but note the trap:
  because the same thread that answers hooks also drives the run, a hung
  closure stalls the whole run, so fallbacks here are about misbehavior,
  not concurrency.
- **GIL / GVL (Python, and Ruby when a native extension is willing to
  manage the GVL directly):** trampoline. Release the lock before running
  the core (`allow_threads`), reacquire per callback (`with_gil` /
  `with_gvl`). Python rides in flavor 1 directly because `Py<T>: Send`;
  Ruby can approximate it but its `!Send` values usually push it back to
  flavor 2.
- **Event loop (Node):** trampoline through `ThreadsafeFunction`. The core
  `run` returns a `Promise` so the loop thread stays free to drain the
  queued callbacks; each callback answers through a per-call channel.

The load-bearing invariant across both flavors: the `!Send` VM handle never
enters the tokio world. Only `serde_json::Value` request and response data
crosses the boundary. That is what lets `HostClosureHook` be `Send + Sync`
even when the language it fronts is not.

---

## 5. Coexistence with deno_core

deno_core is not a host binding; it is the embedded JavaScript plane that
runs pi's `(pi) => {}` extensions unchanged. Its extensions register on the
same `Hook` trait and carry `Affinity::OwnRuntime`. A `JsRuntime` is
`!Send` and pins to one thread with its own event loop, so it is dispatched
exactly like a flavor-2 host: the core awaits a task on the JavaScript
runtime thread (a `LocalSet` or a dedicated thread running
`run_event_loop`), and the answer comes back over a channel.

The process is hub and spoke. The tokio core is the hub. Each plane is a
spoke pinned to its own thread:

```
                     +------------------+
                     |  tokio core hub  |   (multi-thread runtime,
                     |  agent hot path  |    owns Hook dispatch)
                     +---------+--------+
                               | hook.handle(&mut event).await
        +----------------+-----+------+----------------+
        |                |            |                |
   deno_core        Python GIL     Node loop        PHP / Ruby
   JsRuntime        (in-process)   (Tsfn queue)     request thread
   OwnRuntime       AnyThread*     HostThreadOnly   HostThreadOnly
   (own thread)     (worker calls) (loop thread)    (rendezvous)
```

The three planes coexist because the `Affinity` enum tells the dispatcher
which route to take, and all three routes terminate in the same
`HookOutcome`. `OwnRuntime` and `HostThreadOnly` both mean thread-bound and
both use the rendezvous path; the only difference is who owns the thread —
the core owns the deno thread, the host owns the PHP or Ruby thread.
Python's `allow_threads` window is the one case where a worker thread does
the calling, which is why its affinity is closer to a controlled
`AnyThread` than to `HostThreadOnly`.

Ordering holds across planes because the core awaits one hook at a time per
event, in load order. Two extensions from two different languages listening
on the same event run in registration order, each on its own thread, one
after the other.

---

## 6. Rust API sketch

Sketches, not compiled code. They track the traits in `extensibility.md`.

A host closure becomes a `Hook` whose payload is transport, not a VM
handle:

```rust
/// A closure registered from a host language. Send + Sync because it holds
/// only a token and a channel — never the VM handle itself.
pub struct HostClosureHook {
    event: HookEvent,
    closure_id: ClosureId,
    dispatch: HostDispatch,   // clone of a channel sender, Send + Sync
}

/// What crosses the language boundary — plain data, both directions.
pub struct HookRequest {
    closure_id: ClosureId,
    event_json: serde_json::Value,          // snapshot of &mut HookEvent
    reply: oneshot::Sender<HookOutcome>,
}

#[async_trait::async_trait]
impl Hook for HostClosureHook {
    fn event(&self) -> HookEvent { self.event.clone() }

    async fn handle(&self, event: &mut HookEvent, ctx: &HookContext)
        -> HookOutcome
    {
        let (reply, answer) = oneshot::channel();
        let req = HookRequest {
            closure_id: self.closure_id,
            event_json: event.to_json(),     // serialize, do not lend &mut
            reply,
        };
        if self.dispatch.send(req).is_err() {
            return fallback_for(self.event);            // host gone
        }
        match tokio::time::timeout(ctx.hook_timeout, answer).await {
            Ok(Ok(outcome)) => { outcome.apply_to(event); outcome }
            _               => fallback_for(self.event), // timeout or drop
        }
    }
}
```

Two points do the heavy lifting. First, `event: &mut HookEvent` is never
lent across the boundary; it is serialized to `event_json`, the host
returns a `HookOutcome`, and the core applies that outcome to the real
`&mut` on the Rust side. A `Modify(v)` writes `v` back into the event; a
`Block` short-circuits the loop. Second, the async core awaits a
synchronous host answer through a `oneshot` channel, so the tokio task
parks cooperatively instead of blocking an OS thread, and the timeout is a
plain `select` against the channel.

`HostDispatch` has the two flavors from section 4:

```rust
enum HostDispatch {
    // Flavor 1: call the Send handle straight from the worker thread.
    Trampoline(Arc<dyn Fn(HookRequest) + Send + Sync>),
    // Flavor 2: hand the request to the owning thread's pump.
    Rendezvous(mpsc::Sender<HookRequest>),
}
```

**ext-php-rs glue (flavor 2, rendezvous).** The PHP entry point is the hook
server. It installs the receiver end, then pumps until the core future
completes, reentrantly:

```rust
#[php_function]
fn pidgin_run(session: Zval) -> Zval {
    let core = process_core();                 // lazy, post-fork
    let (tx, rx) = mpsc::channel::<HookRequest>();
    let handle = core.spawn_run(session.into(), HostDispatch::Rendezvous(tx));

    // This thread owns the PHP interpreter; it answers every hook here.
    pump_until_done(&rx, &handle)
}

fn pump_until_done(rx: &mpsc::Receiver<HookRequest>, handle: &RunHandle)
    -> Zval
{
    loop {
        if let Some(result) = handle.try_take_result() {
            return result.into_zval();
        }
        if let Ok(req) = rx.recv_timeout(POLL) {
            let cb: &ZendCallable = registry_lookup(req.closure_id);
            let arg = req.event_json.into_zval();
            let ret = cb.try_call(vec![&arg]);              // runs PHP here
            let _ = req.reply.send(parse_outcome(ret));     // may reenter
        }
    }
}
```

If `cb.try_call` reenters `pidgin_*` and that call reaches another hook,
the inner FFI call runs its own `pump_until_done`, so the nested request is
serviced on this same thread. The pump is a stack.

**napi-rs glue (flavor 1, trampoline).** Each closure is a
`ThreadsafeFunction`; the run is async so the loop stays free:

```rust
#[napi]
pub async fn run(session: Session, hooks: HashMap<String, JsFunction>)
    -> Result<RunResult>
{
    let mut host = ExtensionHost::new();
    for (event, f) in hooks {
        let tsfn: ThreadsafeFunction<serde_json::Value> =
            f.create_threadsafe_function(0, |cx| Ok(vec![cx.value]))?;
        host.register_hook(Arc::new(HostClosureHook::from_tsfn(event, tsfn)));
    }
    // core runs on tokio; each hook awaits the JS return over the tsfn.
    process_core().run(session, host).await
}
```

`HostClosureHook::from_tsfn` builds the `Trampoline` variant whose closure
calls `tsfn.call_async(event_json).await` and reads the JavaScript return
value (or awaited `Promise`) back as the `HookOutcome`.

**PyO3 and magnus, in brief.** Python registers `Py<PyAny>` closures; the
run releases the GIL with `py.allow_threads`, and dispatch calls
`Python::with_gil(|py| cb.call1(py, (event_json,)))`, turning the return
into a `HookOutcome`. An `async def` closure is driven on the asyncio loop
through `pyo3-async-runtimes` and awaited as a Rust future. Ruby wraps each
closure in `magnus::Opaque<Value>` bound to its owner thread and uses the
flavor-2 rendezvous, running the core under `rb_thread_call_without_gvl`
and each callback under `rb_thread_call_with_gvl`.

---

## 7. Timeouts, ordering, cancellation

- **Fallback policy.** Each hook declares fail-open or fail-closed. The
  permission gate (`tool_call`) is fail-closed: timeout or panic yields
  `Block`. Advisory hooks fail-open to `Continue`. The policy lives with
  the hook registration, not the transport.
- **Ordering.** Hooks for one event run in load order because the core
  awaits them one at a time. A rendezvous host serializes naturally on its
  single pump; a trampoline host serializes because the core issues the
  next `await` only after the previous `HookOutcome` returns.
- **Cancellation.** `HookContext` carries the session `CancellationToken`
  from `ToolContext`. Cancelling drops the `oneshot` receiver, the pump
  sees a closed reply channel, and an in-flight closure result is
  discarded rather than applied.
- **Backpressure.** One hook is in flight per host thread by construction,
  so there is no queue to bound beyond the reentrancy stack depth, which is
  the natural recursion depth of hooks calling tools calling hooks.

---

## 8. Sanity check against real systems

The mechanisms here are the documented ones for each runtime, not novel
inventions.

- **Node / napi-rs `ThreadsafeFunction`** is built for exactly this: call a
  JavaScript function from any native thread, queued onto the loop thread,
  with an option to await the return. See the napi-rs threadsafe function
  guide (<https://napi.rs/docs/concepts/threadsafe-function>) and the
  Node-API primitive it wraps
  (<https://nodejs.org/api/n-api.html#asynchronous-thread-safe-function-calls>).
  The `neon` binding solves the same problem with `Channel::send`, which
  also queues a closure onto the loop and returns a joinable handle
  (<https://docs.rs/neon>), confirming the queue-to-loop shape is the
  standard Node answer.
- **PyO3** documents `Python::with_gil` for acquiring the GIL on any thread
  and `Python::allow_threads` for releasing it around long native work,
  with `Py<T>: Send + Sync`
  (<https://pyo3.rs/v0.23.0/parallelism>). Awaiting Python coroutines from
  Rust and vice versa is what `pyo3-async-runtimes` (the successor to
  `pyo3-asyncio`) exists for
  (<https://docs.rs/pyo3-async-runtimes>). Free-threaded CPython is tracked
  by PEP 703.
- **deno_core** documents `JsRuntime` as single-threaded with its own event
  loop driven by `run_event_loop` (<https://docs.rs/deno_core>), which is
  why it takes the `OwnRuntime` rendezvous path rather than a trampoline.
- **Ruby** exposes `rb_thread_call_without_gvl` and
  `rb_thread_call_with_gvl` for exactly the release-and-reacquire pattern,
  and magnus documents `Value` as `!Send` and offers `Opaque` and `Ractor`
  for crossing threads (<https://docs.rs/magnus>).
- **PHP / ext-php-rs.** The `throwaway/php-hello` spike in this repo is the
  local evidence: one process-wide tokio runtime built lazily after the
  php-fpm fork, each PHP call doing `block_on`, PHP never touched from a
  worker thread. That is the rendezvous model with the pump collapsed onto
  the request thread. See ext-php-rs (<https://docs.rs/ext-php-rs>) and the
  spike's `README.md`.

---

## 9. Open questions

1. **Reentrancy depth.** A reentrant pump has no fixed bound; hooks calling
   tools calling hooks recurse on the host thread. Do we cap depth, and
   what is the failure mode when a closure recurses without terminating?
2. **ZTS versus NTS shipping.** The rendezvous model is identical for both,
   but distribution is not: a per-platform, per-PHP-version, NTS-or-ZTS
   `.so` matrix is heavy. Is a bundled static php-cli a better delivery
   vehicle than a PECL extension?
3. **Ruby placement.** Is Ruby worth the flavor-2 machinery, or should a
   Ruby binding wait until demand is proven?
4. **Async host closures.** Node promises and Python coroutines await
   cleanly. PHP has no in-language async a hook can await; a PHP closure is
   synchronous by nature. Is that a limitation to document or a constraint
   to enforce?
5. **Return-value coupling for the trampoline.** Awaiting a JavaScript
   `Promise` through a `ThreadsafeFunction` per hook adds a loop round-trip
   on the hot path. Is the latency acceptable for high-frequency hooks like
   `context`, or do those hooks need a fast synchronous subset?
