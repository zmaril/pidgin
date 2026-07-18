<!-- straitjacket-allow-file[:duplication] — design note; illustrative napi/shim code sketches and Pro/Con scaffolding repeat intentionally -->

# Testing Strategy: Passing pi's Own Test Suite

**Status:** proposal · **Decision (user-confirmed):** build a **napi-rs bridge** that presents pi's exact TS module surface backed by the Rust core, and run pi's own suites against it. Node is a first-class target for atilla, so the bridge is a shipped deliverable, not test-only scaffolding. **Bar: literally pass all of pi's tests.**

## TL;DR

pi's suite is **319 test files / ~3,631 cases** across a 5-package ESM/TypeScript monorepo (vitest + `node:test`). **296/319 files (92.8%) deep-import pi's internal `../src/*` modules** by relative path — they are white-box, in-process tests. Only **4 files (15 cases)** are true black-box CLI tests; everything else needs an in-process bridge to reach a Rust core.

**Architecture: napi-rs shim packages + a src-module swap.**

1. **Shim packages (the deliverable).** Compile the Rust core with napi-rs into `.node` addons, wrapped in npm packages that reproduce pi's exact package names, exports, and `.d.ts` types (`@earendil-works/pi-ai`, `pi-agent-core`, `pi-coding-agent`, `pi-tui`).
2. **Src-module swap (the test seam).** Because 93% of tests import `../src/<module>.ts` by relative path — which bypasses package resolution — vendor pi's tests unmodified and generate a shim `src/**/*.ts` tree where each module re-exports its Rust-backed binding. Tests import `../src/...` exactly as before and hit Rust. A **module manifest** (`src-path → native | original`) is the swap map and the progress ledger.
3. **Black-box tier.** Repoint the 4 CLI tests (and the API-key-gated `rpc.test.ts`) at the `atilla` binary; reuse pi's fixtures and inline stdout/exit goldens unchanged.

**The honest constraint on "literally pass ALL":** ~58 `vi.mock`/`vi.spyOn` and ~68 `vi.stubGlobal("fetch")` tests inject mocks *between* internal modules or at the HTTP boundary. A monolithic Rust core does not honor a JS mock of one of its internal seams. Passing those requires **building matching injection seams into the Rust core** (an injectable provider, HTTP transport, and clock) or porting the specific tests. This is the load-bearing architectural cost of the 100% bar — it is real, and it shapes the Rust core's design (§5).

A coverage audit (§8) shows pi's own tests hit only ~74% lines / ~62% branches on `ai`/`agent` — and `ai`'s figure is inflated by 57% of tests skipping without provider keys, over exactly the provider-I/O and OAuth code — so even a green suite leaves the port under-specified in known places.

---

## 1. What pi's test suite actually looks like

pi (`pi-monorepo`) is an ESM/TypeScript monorepo — an interactive coding-agent CLI plus supporting libraries — in five workspaces:

| Package | npm name | Role | Test files |
|---|---|---|---|
| `ai` | `@earendil-works/pi-ai` | Multi-provider LLM API (Anthropic/OpenAI/Google/Bedrock/Mistral/xAI/…), streaming, OAuth, model catalog | **100** |
| `coding-agent` | `@earendil-works/pi-coding-agent` | Interactive coding agent CLI (`pi` bin) + RPC/SDK | **176** |
| `tui` | `@earendil-works/pi-tui` | Terminal UI library (differential render, editor, markdown) | **27** |
| `agent` | `@earendil-works/pi-agent-core` | Agent runtime: tool loop, sessions, compaction, storage | **16** |
| `orchestrator` | `@earendil-works/pi-orchestrator` | Multi-agent orchestration | **0** |

- **Runners:** Vitest for `ai`/`agent`/`coding-agent` (`globals: true`, `environment: node`, 30s timeout); **`node --test`** (node:test) for `tui`. Root `npm test` = `npm run test --workspaces`.
- **Counts:** 319 files, **~3,631 executed cases** (`it(`/`test(` grep upper bound ~4,896 before skips/gating).
- **Tests already run against source, not dist.** Each vitest config uses `resolve.alias` to point `@earendil-works/pi-ai` etc. at sibling `../<pkg>/src/index.ts`. This is the seam the bridge exploits.

### 1.1 How pi fakes an LLM

No msw, no nock, no recorded cassettes. Two mechanisms:

1. **In-repo faux provider** — `registerFauxProvider()` from `pi-ai/compat`. A test queues canned assistant messages, then drives the *real* `stream`/`complete` code path (which simulates streaming deltas, token/cache accounting, aborts, errors). Primary mechanism for agent/coding-agent tests (~33 files); wired into the coding-agent suite harness (`test/suite/harness.ts`). This is a deliberate **injection seam** — the friendliest shape for the bridge.
   ```ts
   const registration = registerFauxProvider();
   registration.setResponses([
     fauxAssistantMessage(
       [fauxThinking("think"), fauxToolCall("echo", { text: "hi" }), fauxText("done")],
       { stopReason: "toolUse" },
     ),
   ]);
   const response = await complete(registration.getModel(), {
     messages: [{ role: "user", content: "hi", timestamp: Date.now() }],
   });
   ```
2. **Hand-built `Response`/SSE objects** fed straight to a provider's stream parser (no real HTTP):
   ```ts
   function createSseResponse(events) {
     const body = events.map(({ event, data }) => `event: ${event}\ndata: ${data}\n`).join("\n");
     return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
   }
   ```

`vi.mock` ~58×, `vi.stubGlobal("fetch", …)` ~68× (OAuth/token-refresh). Real-network e2e tests (~5 files plus smoke) self-skip without API keys; `test.sh` strips ~60 provider keys in CI.

### 1.2 Coupling — why the bridge must intercept relative imports

- **296 / 319 files (92.8%)** deep-import an internal module via `../src/<subpath>` (e.g. `../src/api/anthropic-messages.ts`, `../src/core/session-manager.ts`). 734 import lines reach into `../src`. Top targets: `../src/types.ts` (83), `../src/compat.ts` (65), `../src/core/session-manager.ts` (28).
- Only ~83 files import via the public `@earendil-works/*` name, and most of those *also* deep-import `../../src/core/*`.
- Tests assert on exact intermediate shapes: `Context`, `AssistantMessage`, event sequences like `["start","thinking_start",…,"done"]`.

**Consequence:** package-name aliasing alone reaches only a minority of tests. The bridge must intercept **relative deep imports**, which is what the src-module swap in §4 does.

### 1.3 Node-runtime coupling

`node:path` (81 files), `node:fs` (74), `node:os` (70, mostly `tmpdir()` sessions), `process.env` (69), `child_process` (17). `node:net` (5), `node:stream` (3), `worker_threads` (0 in tests; prod uses an image-resize worker). The agent storage layer is abstracted behind a `NodeExecutionEnv` / `readTextLines` interface that maps cleanly to a Rust trait. SSE parsing, JSONL session format, and token estimation (`Math.ceil(len/4)`) are pure-logic and portable.

### 1.4 Fixtures

No Vitest snapshots in practice (0 `__snapshots__`, 1 `toMatchSnapshot`); explicit `toEqual`. One fixture tree: `packages/coding-agent/test/fixtures/` — `before-compaction.jsonl` (1,003 lines) and `large-session.jsonl` (1,019 lines) session goldens, 16 SKILL.md skill fixtures (valid/invalid/collision/nested), empty-dir scaffolds, `assistant-message-with-thinking-code.json`. Plus `packages/ai/test/data/red-circle.png`.

---

## 2. Prior art

**No existing Rust port runs pi's own upstream suite.** `Dicklesworthstone/pi_agent_rust` (production-grade; *restrictive license — study, do not vendor*) runs Rust-native tests plus pi *extensions* inside embedded **QuickJS** as "extension conformance" — notably it declined to run pi's vitest suites even with a working embedded-TS runtime, a signal that pi's tests lean on Node/vitest harness features (globals, module resolution, mocking). `c4pt0r/pie` and `nktkt/pi` (both MIT) run only their own `cargo test`. Running pi's suite against Rust via a bridge is unclaimed territory.

---

## 3. Bridge architecture: napi-rs shim packages

The deliverable is a set of Node packages whose public surface is byte-for-byte pi's, backed by Rust:

```
crates/atilla-ai        (Rust core)      ─┐
crates/atilla-agent     (Rust core)       │  napi-rs  →  *.node addon + generated .d.ts
crates/atilla-napi      (#[napi] surface) ─┘
                                             │
        wrapped as npm packages that re-export the addon:
        @earendil-works/pi-ai        (index + ./compat + ./providers/* + ./api/* + ./oauth …)
        @earendil-works/pi-agent-core
        @earendil-works/pi-coding-agent
        @earendil-works/pi-tui
```

Each shim package must reproduce pi's **exact export names and TypeScript types** — the tests type-check against them. napi-rs emits `.d.ts` from `#[napi]` signatures, but pi's hand-written types are richer than napi's generated ones; expect a hand-maintained `.d.ts` layer on top that must stay in sync with pi's `src/types.ts` (83 tests import it).

Example binding + the faux-provider callback (the trickiest seam — a JS test must drive the Rust streaming loop):

```rust
// crates/atilla-napi/src/anthropic.rs
use napi_derive::napi;

#[napi(object)]
pub struct StreamEvent { pub kind: String, pub text: Option<String> }

/// Mirrors pi-ai src/api/anthropic-messages.ts: parse an SSE body into the
/// normalized event stream the tests assert on.
#[napi]
pub fn parse_anthropic_sse(body: String) -> napi::Result<Vec<StreamEvent>> {
    atilla_ai::anthropic::parse_sse(&body)
        .map(|evs| evs.into_iter().map(Into::into).collect())
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// stream() accepts a JS callback so registerFauxProvider() can supply queued
/// responses — the Rust loop calls back into JS for each faux message.
#[napi]
pub fn stream(
    model: JsModel, request: JsRequest,
    #[napi(ts_arg_type = "() => FauxResponse")] next_response: ThreadsafeFunction<()>,
) -> napi::Result<JsReadableStream> { /* call back into JS per queued faux message */ }
```

---

## 4. Import-resolution mechanism (the crux)

Two import shapes must both resolve to the bridge, and the dominant one is the harder one:

- **Package-name imports** (`@earendil-works/pi-ai`, ~83 files): resolved by pointing pi's existing `vitest resolve.alias` (and/or an npm workspace override) at our shim packages instead of `../ai/src/index.ts`. Trivial.
- **Relative deep imports** (`../src/api/foo.ts`, **296 files**): these bypass package resolution entirely. Intercept them by **replacing the `src/**/*.ts` files in the vendored pi tree** with generated shims:

  ```ts
  // packages/ai/src/api/anthropic-messages.ts  (generated from the module manifest)
  export { parseAnthropicSse as parseSse, stream } from "@earendil-works/pi-ai-native/anthropic-messages";
  ```

**Recommended mechanism:**
1. Vendor pi as a **git submodule pinned to a SHA** (clean tree, explicit drift).
2. A **module manifest** lists every pi `src` module and marks it `native` (Rust-backed shim) or `original` (kept as pi's TS until ported). Unported modules stay as pi's own source so the import graph still resolves and their tests run on pi's code — they just don't count toward Rust conformance yet.
3. A codegen step writes the shim `src` files from the manifest before `npm test`. Tests are **never edited**.
4. `napi build --platform` produces the addon + `.d.ts`; the hand-maintained type layer is applied.

This yields "run pi's tests unmodified," with per-module conformance that grows monotonically as the manifest flips modules `native`.

Alternative considered: a custom vitest resolver plugin that rewrites `../src/*` specifiers at load time (no file writes). Rejected as primary — relative-specifier rewriting is fragile across vitest/Vite versions and hides what's swapped; the manifest + generated files are inspectable and diff-able. Keep the resolver plugin as a fallback if file generation proves unwieldy for `node:test` (tui), which doesn't use Vite resolution.

---

## 5. What will be hardest to pass through the shim

"Literally pass all" is bounded by tests that assume a JS runtime *inside* the module under test. Ranked by difficulty:

1. **Module-internal mocks/spies (`vi.mock`/`vi.spyOn`, ~58 files) — the hard blocker.** When a test mocks internal module B to control module-A-under-test, and A is Rust, mocking the JS B has no effect on Rust A. Passing these requires the Rust core to expose the *same* seam as an injectable dependency (so the test injects via a JS callback the Rust side honors), or porting the test. Tests that `vi.mock` the *whole module under test* are fine (the shim is replaced wholesale); tests that mock a *collaborator* to steer real logic are not. **Action: enumerate the vi.mock targets early and classify collaborator-mocks vs whole-module-mocks — this set sizes the real 100% cost.**
2. **`vi.stubGlobal("fetch")` (~68 usages; OAuth/token-refresh ~6 files).** Rust making its own HTTP won't see a JS fetch stub. The bridge's HTTP layer must accept an **injectable transport** (a JS-provided fetch the Rust side calls) so these tests keep working; otherwise port them. Most `ai` protocol tests avoid this (they feed a hand-built `Response` to a parser directly — fine), so the exposure is concentrated in OAuth.
3. **Fake timers (`vi.useFakeTimers`) and `Date.now()`.** Retry/backoff/timeout/abort tests and any timestamped output won't respond to JS fake timers if Rust owns the clock. The Rust core needs an **injectable clock** to stay controllable; timestamp assertions need the bridge to let JS supply `now`.
4. **Streaming event order/timing across the FFI boundary.** The faux provider is a designed seam, but the exact async ordering of emitted events (`["start", …, "done"]`) must be reproduced deterministically through the threadsafe-function callback.
5. **Type-level expectations.** Tests type-check against pi's rich `.d.ts`; the shim's generated types must match pi's `src/types.ts` exactly or `tsc`/vitest type errors fail files wholesale.
6. **`node:test` / tui (27 files).** Different runner, no Vite resolution — needs the file-swap mechanism (not resolve.alias) and Rust equivalents for ANSI width/wrapping/editor. Lower priority.
7. **Irreducibly Node-bound:** worker_threads (image resize), clipboard, real subprocess/signal handling. Port or document an explicit skip with reason.

**Design implication:** build the Rust core with the same injection points pi exposes to its tests — provider, HTTP transport, clock, and storage env — as first-class trait objects. This is aligned with shipping Node as a target anyway (those seams are useful in production), and it is the difference between ~80% and ~100% of the suite passing.

---

## 6. Black-box tier: the 4 CLI tests + gated RPC

Four files (15 cases) drive the program purely through stdin/stdout/exit/files and repoint trivially at the `atilla` binary (they currently `spawn(process.execPath, [src/cli.ts, …])` via tsx; change the target to the Rust binary):

- `stdout-cleanliness.test.ts` (5) — `--version` matches `/^\d+\.\d+\.\d+/`, stderr empty; `--help` prints `Usage:` to stdout; in `--mode json`/`-p` the Usage text and npm chatter go to **stderr** and stdout stays empty. (Tests stream routing / stdout cleanliness.)
- `session-id-readonly.test.ts` (7) — session-id reservation, warnings ("No project session found with id …", "Session already exists with id …", "Session id must be non-empty"), exit codes, no stack-trace leakage.
- `startup-session-name.test.ts` (2) — `--name` trimming, name written to the session `.jsonl` before model validation aborts, `--name "   "` → "requires a non-empty value".
- `session-file-invalid.test.ts` (1) — invalid session file → exit 1, `Error: Session file is not a valid pi session: <path>`, no stack trace, file left byte-identical.

Plus **`rpc.test.ts`** (API-key-gated) drives the built `dist/cli.js` over a JSONL RPC protocol via a `src`-imported `RpcClient`; repointable only once the `atilla` binary implements that protocol (commands: `prompt`, `get_state`, `new_session`, `fork`, `compact`, `export_html`, `set_model`, `bash`, `skill`, `extension`, …). These tests define the **CLI/RPC contract** the Rust binary must satisfy; reuse pi's fixtures and the inline stdout/exit goldens verbatim.

---

## 7. Conformance dashboard & upstream tracking

- **Runner** (`scripts/conformance.sh`): checkout pinned pi submodule → apply the module manifest (generate shim src) → `npm test -- --reporter=json` (vitest) + node:test JSON → parse per-file/per-case `pass | fail | skip`.
- **Output:** `conformance.json` (`{ pi_sha, total, passing, skipped, by_package, by_file, manifest_native_modules }`) + a rendered HTML dashboard published as a CI artifact: `N of M pi tests literally passing`, per-package bars, and a "modules still on pi's TS" list.
- **Upstream drift as a first-class signal:** a weekly cron bumps the pi submodule to upstream HEAD and re-runs. New upstream tests default **red** and expand the denominator, so the score drops until ported. Diff the test-file set between SHAs to auto-file "new pi tests to claim"; diff pi's `src` module list against the manifest to flag new modules needing shims.
- **CI gate:** atilla CI fails if the passing count regresses below the committed baseline in `conformance.json` — the mirror only moves forward.

Because the bridge runs the *actual* tests, "N of M" is now a real, non-subjective conformance number — the whole point of taking on the bridge.

---

## 8. Coverage audit — where the spec is thin

We ran pi's suite under v8 coverage (Node v22.22, keys unset so network tests skip).

| package | statements | branches | functions | lines | tests |
|---|---|---|---|---|---|
| **ai** | 72.5% | 62.9% | 82.6% | 74.1% | 556 pass / **738 skipped** / 1294 |
| **agent** | 72.5% | 61.2% | 85.1% | 74.3% | 180 pass / 0 skip |
| coding-agent | not scored¹ | — | — | — | 1491 pass / 87 fail / 47 skip |
| tui | n/a² | — | — | — | node:test, not vitest |

¹ 87 tests fail in a bare sandbox (extension discovery, fswatch, stdout-cleanliness — environment-shaped) and v8 suppresses the report while any test fails. ² tui uses `node:test`; no v8 summary.

**Even a fully green suite leaves blind spots:**
- **`ai`'s 74% is inflated by skips.** 738/1294 tests (57%) skip without provider keys — over precisely the least-covered code: `bedrock-converse-stream.ts` 52%, `mistral-conversations.ts` 30%, `google-vertex.ts` 50%, `oauth/*.ts` 11–57%. Passing the offline suite proves little about those adapters; they need live-key runs or recorded golden transcripts.
- **`agent`** (0 skips) still has `proxy.ts` at **0%** (104 lines) and the 440-line core `agent-harness.ts` at 71% line / **50% branch**. Session/storage is well covered (session 99%, compaction 98%).
- **`coding-agent` / `tui`** unmeasured.

So the suite is a strong gate for the well-covered core but must be supplemented with atilla's own Rust tests (and golden LLM transcripts) for provider-I/O, OAuth, and the agent-harness branches the offline suite never exercises.

**Two environment gotchas** (both block a from-scratch `npm test`): `packages/ai` devDepends on native `canvas` (needs `libcairo2`/`pango`/`jpeg`/`gif`/`rsvg`), and `packages/ai/src/providers/data/*.json` is **generated, not committed** (`npm run generate-models` fetches catalogs over HTTPS). The conformance harness must install the system libs and run the model-catalog generation (or vendor the JSON).

---

## 9. Recommendation & sequencing

1. **Prove the seam on one file end-to-end.** Add `crates/atilla-napi`, vendor pi as a pinned submodule, build the manifest + shim codegen, and get `packages/ai/test/anthropic-sse-parsing.test.ts` green against Rust. De-risks the whole approach.
2. **Build the injection seams into the Rust core up front** (§5): injectable provider, HTTP transport, and clock, plus the `NodeExecutionEnv`-equivalent storage trait. Retrofitting these later is expensive and they gate the 100% bar.
3. **Solve the faux-provider threadsafe callback early** — it unblocks the entire agent/coding-agent tier.
4. **Port `pi-ai` bottom-up** (SSE/request-shaping/token/model-catalog) — mostly pure logic, best ROI, ~100 files; flip manifest modules to `native` as they pass.
5. **Repoint the 4 black-box CLI tests** at the `atilla` binary as soon as it has `--version`/`--help`/`--session*` (cheap early wins, real end-to-end signal).
6. **Enumerate and classify the ~58 `vi.mock` targets** (collaborator vs whole-module) to get a true estimate of the 100% cost before committing to it.
7. **Ship the dashboard + CI gate** once step 1 is green, so N-of-M is visible and monotonic.

**Realistic reach:** the shim + injection seams should carry the large majority of the ~3,631 cases; the residual (collaborator-mocks, fetch-stub OAuth, fake-timer, node:test/tui, worker_threads/clipboard) is where "all" gets expensive and some tests may be ported rather than run. Target that residual explicitly rather than assuming the shim is free.

---

## 10. Open questions

- **Collaborator-mock inventory:** how many of the ~58 `vi.mock` tests mock an internal *collaborator* (unpassable without a Rust seam) vs the *whole module under test* (fine)? This number sets the true 100% cost. (Step 6.)
- **Injectable transport/clock:** are we willing to make the Rust core's HTTP and time fully injectable from JS for test control, as production-grade seams? (Recommended yes.)
- **Type parity:** how much hand-maintained `.d.ts` is needed on top of napi-generated types to satisfy pi's type-level assertions, and how do we keep it synced to `src/types.ts` on upstream drift?
- **Submodule vs vendored subtree** for pinning pi (leaning submodule for clean drift tracking).
- **node:test/tui mechanism:** confirm the file-swap works without Vite resolution for the 27 tui files, or scope tui later.
- **License:** confirm pi's license permits vendoring its test files into our conformance harness (study `pi_agent_rust` only; never vendor it).
- **Exclusion policy:** which irreducibly-Node tests (worker_threads, clipboard) get a documented skip vs a port, and how is that list kept honest against drift?
