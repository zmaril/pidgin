# node-hello — a native Node addon written in Rust via napi-rs (spike)

Throwaway spike for the **pi → Rust mirror** project. De-risks the napi-rs
foundation: can we build our own Node packages in Rust that present pi's exact
TypeScript module surface, so pi's own vitest suite can import them unmodified?

The strategy for the real harness: compile napi-rs packages that mirror pi's TS
modules, then resolve the tests' imports to those packages (via `resolve.alias`
or an `exports` map). This spike proves the mechanism end-to-end on a
hello-world surface, with extra weight on the **async / streaming** shapes
(pi's API is async/streaming-heavy) and on **TS-surface fidelity**.

## What it exposes (the representative shapes)

| Shape | Export | Notes |
|-------|--------|-------|
| plain sync | `piHello(name): string`, `piAdd(a, b): number` | trivial value-returning fns |
| async Promise | `piAsyncDouble(n): Promise<number>` | `#[napi] async fn`, tokio-backed |
| callback / streaming | `piStream(count, cb)` | emits N values via a `ThreadsafeFunction` from a background thread — mirrors pi's streaming API |
| class + method | `class PiGreeter { greet(name): string }` | class registration + `.d.ts` class |
| tag-typed object | `makeChunk(kind): StreamChunk` | `{ type, text?, code? }` — probes discriminated-union fidelity |

## Toolchain used (this spike, pinned)

| Component        | Version |
|------------------|---------|
| Node             | v22.22.2 |
| npm              | 10.9.7 |
| rustc / cargo    | 1.94.1 |
| `@napi-rs/cli`   | 2.18.4 |
| `napi` crate     | 2.16.17 |
| `napi-derive`    | 2.16.13 |
| `napi-build`     | 2.3.2 |
| `napi-sys`       | 2.4.0 |
| `tokio`          | 1.53.0 |
| vitest           | 2.1.9 |
| TypeScript (typecheck) | 7.0.2 |

We used the **napi-rs v2 line** (crate `napi` 2.x + `@napi-rs/cli` 2.x). v3 of
both is available but v2 is the well-documented, stable path; the macro surface
used here (`#[napi]`, `#[napi(object)]`, `#[napi(constructor)]`,
`ThreadsafeFunction`) is materially the same on v3.

## Build

```bash
cd throwaway/node-hello
npm install
npm run build          # napi build --platform --release
```

Outputs (all git-ignored):

- `node-hello.linux-x64-gnu.node` — the native addon (name/triple per platform)
- `index.js` — generated loader that `require()`s the right `.node`
- `index.d.ts` — generated TypeScript declarations

## Run the tests

Plain `node` + `assert` (exercises every shape, awaits the Promise, collects the
stream):

```bash
node test.js
```

```
PASS  piHello => 'Hello, Zack, from Rust!'
PASS  piAdd(19, 23) => 42
PASS  await piAsyncDouble(21) => 42 (real Promise, tokio-driven)
PASS  piStream(5, cb) => emitted [0,1,2,3,4] via ThreadsafeFunction
PASS  new PiGreeter('spike').greet('world') => 'spike: hello world (from Rust)'
PASS  makeChunk => {type:'text',...} | {type:'error',code:500}

ALL TESTS PASSED
```

Vitest through a **package alias** (`pi-core` → the addon; see
`vitest.config.ts`) — this is the exact drop-in mechanism the real harness will
use:

```bash
npx vitest run
```

```
 ✓ test/dropin.test.ts (5 tests) 19ms
 Test Files  1 passed (1)
      Tests  5 passed (5)
```

TypeScript type-checks the named imports/class/tag-object against the generated
`index.d.ts` (`tsc --noEmit --strict` exits 0).

---

## Findings (bounded)

### a. How tokio lives inside the Node process

- With the `tokio_rt` feature, napi-rs **creates one multi-threaded tokio
  runtime per process, lazily**, and hands its handle to the N-API layer. You
  do not construct or own it — `#[napi] async fn` bodies are spawned onto it.
- The bridge: napi drives the future on tokio worker threads and settles a JS
  `Promise` on the Node event-loop thread via an N-API `ThreadsafeFunction`
  under the hood. The Node event loop is **not** blocked while the future is
  pending — confirmed: `piAsyncDouble` returns a real `Promise` that resolves
  after a tokio `sleep`.
- Threading model: Node/JS stays single-threaded on the main thread; tokio adds
  background worker threads. Anything crossing back to JS must go through the
  N-API thread-safe path (which napi generates for `async fn`, and which we used
  explicitly via `ThreadsafeFunction` for `piStream`). No `worker_threads`
  needed.
- Relevant to pi: pi's async/streaming calls map naturally — an `async fn`
  becomes a `Promise`, and a streaming producer becomes a background thread /
  tokio task pushing through a `ThreadsafeFunction`. Both work here.

### b. The TS type-generation (`.d.ts`) story — how faithful is it?

napi-rs generates `index.d.ts` from the Rust signatures. Faithfulness for a
**drop-in of an existing TS module**:

**Maps cleanly (drop-in works):**

- **Named function exports** with correct primitive/`Promise`/optional types.
  `tsc --strict` resolves them and the vitest alias import compiles + runs.
- **Classes** — `export declare class PiGreeter { constructor(prefix: string); greet(name: string): string }`
  is emitted and `new`-able. Good fidelity for pi's class surface.
- **Interfaces** for `#[napi(object)]` structs, with `Option<T>` → `field?: T`.
- **Callbacks** typed as `(arg: number) => any`.

**Does NOT map cleanly (fidelity gaps — flagged, not solved):**

1. **Naming convention is forced.** napi auto-renames `snake_case` → camelCase
   (`pi_hello` → `piHello`) and struct fields likewise. To match an existing TS
   name exactly you must annotate every item (`#[napi(js_name = "...")]`). For a
   whole module surface that is per-symbol boilerplate, and any miss is a
   silent import mismatch. **Potential fidelity friction at scale.**
2. **No true discriminated unions.** `StreamChunk`'s tag comes out as
   `type: string`, never `type: "text" | "error"`, and napi cannot emit a
   union-of-object-types (`A | B`). pi's streaming chunks are exactly this
   pattern. The generated interface is a permissive superset (all payload fields
   optional), so `.d.ts` **structurally accepts** pi's usage but does NOT
   reproduce pi's narrowing — `switch (chunk.type)` won't narrow payload types.
   **→ FIDELITY BLOCKER for exact-type drop-in of union-typed APIs.**
3. **You get what napi generates, not an arbitrary hand-written `.d.ts`.** There
   is no supported way to make napi emit an existing module's exact declarations
   (generics, conditional types, overloads, branded types, literal unions,
   re-exported type aliases). You can only shape Rust signatures within napi's
   type-mapping. If pi's public types use anything beyond napi's vocabulary, the
   generated `.d.ts` will differ.
4. **Callback param typed `any`** loses type info vs a hand-written
   `(chunk: StreamChunk) => void`.

**Net:** for named functions + classes + plain object shapes, napi's `.d.ts` is
a faithful, `tsc`-clean drop-in. For **precise union/literal types and exact
hand-authored declarations, it is not** — the generated types are a looser
superset. Whether that matters depends on whether pi's vitest cases assert on
*runtime behavior* (fine — supersets pass) or lean on *compile-time type
narrowing* (problem).

### c. Gotchas for mirroring an existing TS API surface

- **camelCase auto-rename** (above) — the most pervasive gotcha; budget
  `js_name` annotations or a codegen step.
- **`.d.ts` is regenerated on every build** — don't hand-edit it; any manual
  type fixes must live in a separate `.d.ts` or wrapper module.
- **Default export / namespace shape.** napi emits **named exports** on a CJS
  module. If a pi module is consumed as `import pi from '...'` (default) or
  `import * as pi`, the alias target may need a thin `index.ts` shim
  re-exporting to match the exact import style.
- **Module resolution.** The drop-in worked via vitest `resolve.alias`; a real
  harness could instead use an `exports` map or workspace package. Either is
  viable — the alias is the lowest-friction and is what we validated.
- **Enums vs unions.** `#[napi(string_enum)]` yields a TS `enum`, not a
  string-literal union — another reason arbitrary union types don't round-trip.

### Fidelity blockers (flagged, one line each)

1. **Discriminated/literal union types cannot be generated** — pi's streaming
   chunk types degrade to permissive supersets; compile-time narrowing is lost.
2. **Cannot reproduce an arbitrary hand-written `.d.ts`** — only napi's own type
   vocabulary; generics/overloads/branded types won't match exactly.
3. **Forced camelCase renaming** — exact-name parity needs per-symbol
   `js_name`, an easy place to silently break an import.
