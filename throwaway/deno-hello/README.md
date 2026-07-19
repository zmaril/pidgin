# deno-hello — the deno_core "extension plane" shape for pidgin (spike)

Throwaway spike proving that pi-style TypeScript extensions can run on an
embedded `deno_core` `JsRuntime`, register into a Rust-side registry through
ops, and be driven back from Rust — including async tool execution and the
off-thread rendezvous described in `notes/startup/deep-hooks.md` §5
(`Affinity::OwnRuntime`).

The `JsRuntime` is `!Send`, so it lives on its own OS thread with its own
current-thread tokio runtime. The "core hub" thread (a normal multi-thread
tokio runtime, standing in for pidgin's tokio core) talks to it over channels:
commands in, JSON results out. Only JSON crosses the Rust <-> JS boundary; JS
closures (a tool's `execute`, hook handlers) stay inside the runtime, keyed by
name. This mirrors pi's loader, where VM handles never cross.

## Versions used

Determined from crates.io on 2026-07-18 and pinned exactly in `Cargo.toml`.

| Component  | Version    |
|------------|------------|
| `deno_core`| `=0.408.0` |
| `deno_ast` | `=0.53.3` (feature `transpiling`) |
| `v8` (transitive) | `149.4.0` (prebuilt static lib) |
| `serde_v8` (transitive) | `0.317.0` |
| rustc / cargo | `1.94.1` |

## Build and run

```sh
cargo run      # runs steps 1-6 and prints PASS/FAIL for each
cargo test     # asserts the full loop off-thread, plus a transpile unit test
```

### First-build V8 download

`deno_core` pulls in the `v8` crate, which downloads a ~38 MB prebuilt static
V8 library on first build (`librusty_v8_*.a.gz`) from GitHub release assets.
The first `cargo build` is therefore slow and needs network. If your
environment cannot reach `github.com/denoland/rusty_v8/releases`, download the
matching archive out of band and point the build at it:

```sh
export RUSTY_V8_ARCHIVE=/path/to/librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz
cargo build
```

`RUSTY_V8_MIRROR` (a base URL ending in `.../releases/download`) is the other
override the `v8` build script honors.

## What the demo proves (steps 1-6)

1. Start the JS plane thread (prints its thread id).
2. From the hub thread (different id), load the transpiled TS extension, then
   print the Rust registry — proving `pi.registerTool` / `pi.on` crossed
   JS -> Rust through the ops.
3. Invoke tool `greet` with `{"name":"world"}` -> `{"content":"Hello, world!"}`.
   The tool `execute` is genuinely async (a `setTimeout` macrotask), so this is
   the proof that Rust awaits a JS promise through `run_event_loop`, off-thread.
4. Fire hook `tool_call` with an allowed input -> `block:false` and the event's
   `input.audited` set to `true` (modify).
5. Fire hook `tool_call` with `{"danger":true}` -> `block:true` with a reason.
6. Shut down cleanly (the hub joins the JS plane thread).

### Verbatim `cargo run` output

```
[hub] core hub thread: ThreadId(1)

[js-plane] runtime thread started: ThreadId(4)

[hub] Rust registry after load (proves JS -> Rust):
{
  "tools": {
    "greet": {
      "name": "greet",
      "description": "Greets a person asynchronously"
    }
  },
  "hooks": {
    "tool_call": [
      "tool_call#0"
    ]
  }
}
PASS  step2 register-from-JS: tool `greet` + hook `tool_call` registered into Rust

PASS  step3 async invoke: {"content":"Hello, world!"}
PASS  step4 hook modify: {"block":false,"event":{"input":{"cmd":"ls","audited":true}}}
PASS  step5 hook block: {"block":true,"reason":"blocked dangerous call"}

[js-plane] shutting down on ThreadId(4)
PASS  step6 shutdown: js plane joined

SUMMARY: 5 passed, 0 failed
```

The hub runs on `ThreadId(1)`; every `JsRuntime` operation runs on
`ThreadId(4)`. That is the off-thread rendezvous, visible in the output.

### Verbatim `cargo test` output

```
running 2 tests
test tests::transpile_strips_types ... ok
test tests::full_loop_off_thread ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

## What worked / what did not

Everything in the task brief worked, with no fallbacks:

- TypeScript, not JavaScript. The sample extension is real `.ts` with type
  annotations, transpiled to JS in Rust with `deno_ast` (`parse_module` then
  `transpile`). The `transpile_strips_types` unit test confirms the strip.
- A genuine macrotask, not a microtask. The tool awaits
  `new Promise((r) => setTimeout(r, 10))`. Bare `deno_core` has no
  web-standard `setTimeout`, so the bootstrap shims one over
  `Deno.core.createTimer`, which schedules on the real timer queue. Rust has to
  keep pumping `run_event_loop` for the promise to resolve.
- Off-thread, not same-thread. The `JsRuntime` is created inside its owning
  thread and never leaves it; the hub only ever sends `Command`s and awaits
  `oneshot` replies.

### Module loading shortcut (noted on purpose)

The extension is an ES module (`export default <factory>`). Rather than juggle
v8 module-namespace handles, the loader does a one-shot string replace of the
single `export default ` into an assignment onto a global, runs the result as a
classic script, then calls a shared JS loader that invokes the factory with
`pi`. This is a spike shortcut and only safe because the sample has no other
`import` / `export` statements. A production loader should use the ES-module
path (`load_main_es_module_from_code` / `mod_evaluate` / `get_module_namespace`)
together with a real module loader (next section).

## Findings

### TS-at-runtime story (what jiti does that we must replicate)

pi's loader (`vendor/pi/packages/coding-agent/src/core/extensions/loader.ts`)
uses jiti to do two things at runtime:

1. **Transpile** the `.ts` extension to JS and import its default export as the
   factory `(pi) => void`. This spike replicates that with `deno_ast`
   (type-strip only, no type-check — same as jiti and Deno's `--no-check`).
2. **Module resolution** for the specifiers a real extension imports. This
   spike does NOT do this: the sample has no imports. Real pi extensions import
   `@earendil-works/pi-*` (also aliased `@mariozechner/pi-*`) and `typebox`.
   jiti satisfies these two ways: `virtualModules` (the `VIRTUAL_MODULES` map in
   the loader, used by the compiled Bun binary) and `alias` into `node_modules`
   (development). pidgin's `deno_core` loader will need the equivalent: a
   `ModuleLoader` that maps those bare specifiers to bundled JS
   implementations, because `deno_core` has no bare-specifier resolver of its
   own.

### Minimal Node-compat surface a real pi extension needs

Beyond this hello-world, a real extension reaches for:

- **Timers.** `setTimeout` / `setInterval` are not globals in bare `deno_core`;
  only `Deno.core.createTimer` exists. We shimmed `setTimeout`. A real host
  needs the full web/Node timer surface, or to bundle a shim.
- **Module imports**, including `node:*` builtins and the
  `@earendil-works/pi-*` virtual modules that pi's loader exposes (see the
  `VIRTUAL_MODULES` map in `loader.ts`). None of these resolve in bare
  `deno_core`.
- **TypeBox** (`typebox`, `@sinclair/typebox`, and the `/compile` and `/value`
  subpaths) for tool `parameters` schemas.
- **Richer tool `execute` signature.** Real pi tools receive streaming
  (`onUpdate`) callbacks and an `AbortSignal`, not just an args object. Those
  are additional values that would have to cross the boundary (a callback id
  plus an abort channel), on top of the JSON argument.

### Threading / event-loop integration gotchas actually hit

- `JsRuntime` is `!Send`: it must be constructed inside the thread that owns it.
  The `JsPlaneHandle` is `Send` only because it holds a channel, never the
  runtime.
- Awaiting a JS promise from Rust means pumping the event loop: build the call
  with `execute_script`, then `resolve` the returned value and drive it with
  `with_event_loop_promise` (which polls `run_event_loop` concurrently). Simply
  calling `execute_script` is not enough for an async result.
- The owning thread runs a **current-thread** tokio runtime plus a `LocalSet`;
  `deno_core` spawns `!Send` local tasks (timers, async ops) that need a
  `LocalSet` to host them. A multi-thread runtime on that thread does not work.
- The rendezvous itself: `tokio::sync::mpsc` for commands, a per-command
  `tokio::sync::oneshot` for the reply. The hub `.await`s the oneshot, so from
  the core's perspective a JS hook is just another awaited `HookOutcome`.

### deno_core API churn caveat

`deno_core`'s API changes across minor versions. The specific surfaces this
spike depends on that are most likely to churn:

- The `#[op2]` / `#[op2(fast)]` macro and its parameter attributes
  (`#[string]`, `&mut OpState`), plus the `extension!` macro and `::init()`.
- Promise handling: `resolve`, `resolve_value` (already deprecated in 0.408 in
  favor of `resolve`), `with_event_loop_promise`, `run_event_loop`, and
  `PollEventLoopOptions`.
- The `scope!` macro and `serde_v8::from_v8` for extracting values.
- `Deno.core.ops.op_*` as the JS-side op access path, and `Deno.core.createTimer`
  as the only built-in timer primitive.
- `deno_ast` transpile: `parse_module` / `ParseParams` / `transpile` taking
  three separate options structs (`TranspileOptions`, `TranspileModuleOptions`,
  `EmitOptions`).

## Layout

```
deno-hello/
  Cargo.toml          standalone crate (empty [workspace] table; not in root workspace)
  src/lib.rs          registry, ops, transpile, JS plane thread + handle, tests
  src/main.rs         the steps 1-6 demo driver
  extensions/hello.ts the sample pi-style TypeScript extension
```
