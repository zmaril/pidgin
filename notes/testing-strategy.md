<!-- straitjacket-allow-file[:duplication] — design note; illustrative napi/SSE code sketches and Pro/Con scaffolding repeat intentionally -->

# Testing Strategy: Passing pi's Own Test Suite

**Status:** proposal · **Scope:** how the atilla Rust mirror proves conformance against [earendil-works/pi](https://github.com/earendil-works/pi) · **Conformance bar:** *take pi's own test suite and pass all of it.*

## TL;DR

pi's test suite is **319 test files / ~3,600 cases** across a 5-package TypeScript monorepo, and **~93% of test files deep-import pi's internal `../src/*` modules** — they are white-box tests that assert on internal function signatures and exact intermediate data structures, not a portable black-box spec. No existing Rust port (including the production-grade `pi_agent_rust`) even attempts to run pi's own suite; we'd be first.

**Recommendation: a tiered "vendor-and-swap" harness.**

1. **Tier 1 — napi-rs module swap (primary engine).** Vendor pi's test tree *unmodified*. Port pi's internals to Rust module-by-module, expose each via a napi-rs binding, and replace the corresponding pi `src/*.ts` file with a thin JS shim that re-exports the Rust binding. Tests import `../src/...` exactly as before and never change. Each ported module lights up its tests green.
2. **Tier 2 — RPC/CLI drop-in.** Make the `atilla` binary and its JSON-line RPC protocol wire-compatible with `pi`, so coding-agent's subprocess/RPC integration tests run unmodified against the real Rust binary.
3. **Tier 3 — golden vectors + hand-port.** Extract input→expected vectors from pi-ai's pure protocol tests into JSON, replayed in `cargo test` (Node-free CI insurance). Hand-port the handful of tests too married to Node/JS runtime identity (worker_threads, clipboard, some TUI internals).

A **conformance dashboard** parses the vitest/node:test JSON reporter into `N of M passing`, pinned to a specific upstream pi SHA; a scheduled job pulls new upstream tests, which expand the denominator and *lower* the score until ported — exactly the drift signal we want.

**Estimated "run unmodified and pass" coverage:** ~30–40% of cases near-term (dominated by the pi-ai protocol layer + agent core once those are ported), with a long-term ceiling around ~90% (a small tail is intrinsically Node-specific and gets ported or excluded).

**How good a spec is pi's suite?** A coverage audit (§7) shows pi's own tests hit only ~74% lines / ~62% branches on the `ai` and `agent` packages — and `ai`'s figure is inflated by 57% of tests skipping without live provider keys, exactly over the provider-I/O and OAuth code. So the suite is a useful but not airtight spec, which independently argues for the incremental tiered port above over a Bun-style big-bang.

---

## 1. What pi's test suite actually looks like

pi (`pi-monorepo`) is an ESM/TypeScript monorepo — an interactive coding-agent CLI plus supporting libraries — with five workspaces:

| Package | npm name | Role | Test files |
|---|---|---|---|
| `ai` | `@earendil-works/pi-ai` | Multi-provider LLM API (Anthropic/OpenAI/Google/Bedrock/Mistral/xAI/…), streaming, OAuth, model catalog | **100** |
| `coding-agent` | `@earendil-works/pi-coding-agent` | Interactive coding agent CLI (`pi` bin) + RPC/SDK | **176** |
| `tui` | `@earendil-works/pi-tui` | Terminal UI library (differential render, editor, markdown) | **27** |
| `agent` | `@earendil-works/pi-agent-core` | Agent runtime: tool loop, sessions, compaction, storage | **16** |
| `orchestrator` | `@earendil-works/pi-orchestrator` | Multi-agent orchestration | **0** |

- **Runners:** Vitest for `ai`/`agent`/`coding-agent` (`globals: true`, `environment: node`, 30s timeout); **`node --test`** (node:test) for `tui`. Root `npm test` = `npm run test --workspaces`.
- **Counts:** 319 files, **~3,600 leaf cases** (`it(` ≈ 2,989, `test(` ≈ 491) in ~270 `describe` blocks.
- **Tests run against source, not dist.** Vitest configs use `resolve.alias` to redirect `@earendil-works/pi-ai` etc. to sibling `../<pkg>/src/index.ts`. This alias hook is the seam we exploit in Tier 1.

### 1.1 How pi fakes an LLM (critical for the port)

There is **no msw, no nock, and no recorded HTTP cassettes**. LLMs are faked two ways:

1. **In-repo faux provider** — `registerFauxProvider()` from `pi-ai/compat`. A test queues canned assistant messages and then drives the *real* `stream`/`complete` code path, which simulates streaming deltas, token/cache accounting, aborts, and errors. This is the primary mechanism for agent/coding-agent tests (~33 files) and is wired into the coding-agent suite harness (`test/suite/harness.ts`).
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
2. **Hand-built `Response`/SSE objects at the fetch boundary** — pi-ai protocol tests construct a `text/event-stream` `Response` and feed it straight to the provider's stream parser:
   ```ts
   function createSseResponse(events) {
     const body = events.map(({ event, data }) => `event: ${event}\ndata: ${data}\n`).join("\n");
     return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
   }
   ```

`vi.mock` appears ~58×, `vi.stubGlobal("fetch", …)` ~68× (OAuth/token-refresh). Real-network e2e tests (~5 `*-e2e` files plus `stream`/`images`/smoke) **self-skip when API-key env vars are absent**; `test.sh` strips ~60 provider keys in CI, so the network tier is green-as-skipped by default.

> **Design constraint that falls out of this:** the faux provider is a *stateful JS registry* that the streaming code path calls into. For Tier 1, the Rust `stream`/`complete` binding must accept a provider callback (a napi threadsafe function) so a JS test's queued responses can drive the Rust streaming loop. This is the single trickiest binding in the whole harness — get it right early.

### 1.2 Coupling — why "unmodified black-box tests" is the wrong mental model

- **296 / 319 files (~93%)** deep-import at least one internal module via `../src/<subpath>` — e.g. `../src/api/anthropic-messages.ts`, `../src/core/session-manager.ts`, `../src/cli/args.ts`. 734 import lines reach into `../src`. Top targets: `../src/types.ts` (83), `../src/compat.ts` (65), `../src/core/session-manager.ts` (28), `../src/core/auth-storage.ts` (23).
- Only ~83 files import via the public `@earendil-works/*` name, and most of those *also* deep-import `../../src/core/*`. The public API is a thin barrel over a large internal surface.
- Tests assert on exact intermediate shapes: `Context`, `AssistantMessage`, and event sequences like `["start","thinking_start",…,"done"]`.

**Consequence:** you cannot treat pi's suite as a portable spec you feed a black box. The unit of conformance is the *internal module*. That is fine — it just dictates the harness shape (swap implementations behind the tests, don't rewrite the tests).

### 1.3 Node-runtime coupling

`node:path` (81 files), `node:fs` (74), `node:os` (70, mostly `tmpdir()` sessions), `process.env` (69), `child_process` (17 — RPC-client tests `spawn` the built CLI). `node:net` (5), `node:stream` (3), `worker_threads` (0 in tests). The agent storage layer is abstracted behind a `NodeExecutionEnv` / `readTextLines` interface that maps cleanly to a Rust trait. SSE parsing, JSONL session format, and token estimation (`Math.ceil(len/4)`) are pure-logic and portable; clipboard, TUI ANSI width, and subprocess/RPC are the hardest to mirror.

### 1.4 Fixtures

No Vitest snapshots in practice (0 `__snapshots__`, 1 `toMatchSnapshot`); assertions are explicit `toEqual`. One fixture tree: `packages/coding-agent/test/fixtures/` (SKILL.md skill fixtures, `.jsonl` session fixtures, a thinking-message JSON) plus `packages/ai/test/data/red-circle.png`. Shared helpers (`test/suite/harness.ts`, `virtual-terminal.ts`) live alongside tests.

---

## 2. Prior art: how existing Rust ports handle conformance

**None of the surveyed ports run pi's own upstream TS test suite.**

- **`Dicklesworthstone/pi_agent_rust`** (production-grade; *non-standard/restrictive license — study, do not vendor*). Vendors pi's TS source for reference but its own harness is entirely Rust-native (`tests/*.rs`, `conformance/`, `certification/`, `golden_corpus/`, `snapshots/`). What it calls "conformance" is **extension conformance**: it runs ~224 vendored pi *extensions* inside an embedded **QuickJS** runtime against the release binary and asserts they behave through Rust capability-gated host shims (e.g. `123/123 must-pass extensions passed`). Notably, even with a working embedded-TS runtime that *could* host pi's tests, they chose extension-behavior fixtures over running pi's vitest suites — a signal that pi's tests assume Node/vitest harness features (globals, module resolution, mocking) that QuickJS+shims don't fully provide.
- **`c4pt0r/pie`** (MIT): plain `cargo test --workspace` + clippy on its own tests. No cross-check against upstream.
- **`nktkt/pi`** (MIT): `cargo test` (10 passing, no network). No parity harness.

**Takeaway:** "pass pi's own suite" is an unclaimed strategy. QuickJS-embedding is a real alternative host, but the ports that have it declined to run pi's tests through it — reinforcing that a Node-hosted harness (Tier 1/Tier 2) is the realistic path, with QuickJS a possible fallback host only if we later want to drop the Node dependency.

---

## 3. Strategy evaluation

### 3a. napi-rs module swap — **PRIMARY**

Vendor pi's test tree unmodified. For each pi `src/*.ts` implementation file whose Rust equivalent exists, replace it with a generated JS shim that re-exports the napi binding:

```
packages/ai/src/api/anthropic-messages.ts   →   export * from "@atilla/napi/anthropic-messages";
```

The test file `packages/ai/test/anthropic-sse-parsing.test.ts` still does `import { stream } from "../src/api/anthropic-messages.ts"` — unchanged — and now exercises Rust. A **swap map** (`src-path → ported | original`) is the source of truth; unported modules keep pi's original TS so the rest of the graph still resolves and those tests pass on pi's own code (they just don't count toward Rust conformance yet).

- **Pro:** tests are byte-for-byte unmodified; conformance grows monotonically per module; the swap map *is* the progress ledger; works with pi's existing vitest/node:test configs.
- **Con:** requires mirroring pi's internal module decomposition (function names, return shapes) in Rust — the surface is large. The faux-provider callback (§1.1) must round-trip JS→Rust→JS. TS-only constructs (`vi.mock` of a *JS module*, type-level assertions) can't be swapped and fall to Tier 3.
- **Coverage:** ceiling is high per package but earned incrementally.

### 3b. RPC / CLI driver — **COMPLEMENTARY (integration tier)**

pi already exposes an RPC entry (`pi-coding-agent/rpc-entry`) and coding-agent's `child_process`-based tests `spawn` the `pi` binary and speak its JSON-line protocol. If `atilla` ships a wire-compatible binary + protocol, those integration/RPC/SDK tests run **unmodified against the real Rust binary** — no napi needed. This is the right vehicle for the ~17 subprocess-coupled files and the `test/suite/` end-to-end tests.

- **Pro:** exercises the real shipped artifact; no per-module bindings; naturally covers the Node-subprocess tier.
- **Con:** requires exact RPC protocol parity (message framing, tool-call schema, event ordering); slower; failures are coarser-grained (whole-session, not per-function).

### 3c. Port to Rust / golden vectors — **FALLBACK (Node-free insurance + irreducible tail)**

Two uses: (1) dump `(input, expected)` from pi-ai's pure protocol tests (SSE parse, request shaping, token count, model catalog) into JSON vectors replayed in `cargo test` — gives fast Node-free CI and pins wire behavior independent of the napi layer; (2) hand-port the small set of tests bound to JS/Node runtime identity (worker_threads, clipboard, deep `vi.mock` of JS modules, some TUI ANSI internals) that can never run "unmodified."

- **Pro:** fast, Node-free, no binding fragility. **Con:** golden vectors freeze behavior at extraction time (must be regenerated on upstream drift) and hand-ported tests are maintenance debt divorced from upstream.

### Per-package coverage estimate (run unmodified via Tier 1/2, and pass)

| Package | Files | Near-term | Long-term ceiling | Primary tier | Notes |
|---|---|---|---|---|---|
| `ai` | 100 | ~60% | ~90% | 1 (+3 vectors) | Pure protocol logic; ~5 e2e are key-gated skips. Highest ROI — **start here.** |
| `agent` | 16 | ~50% | ~85% | 1 | Faux provider + `NodeExecutionEnv` trait port cleanly. |
| `coding-agent` | 176 | ~30% | ~85% | 1 + 2 | Config/args/session-manager logic via napi; RPC/subprocess/interactive via CLI drop-in. |
| `tui` | 27 | ~10% | ~70% | 1 + 3 | node:test; width/wrap/editor portable but a big self-contained port. Low priority. |
| `orchestrator` | 0 | — | — | — | No tests. |

**Blended near-term ~30–40% of ~3,600 cases** once the pi-ai layer + agent core are ported; ceiling ~90%. The residual tail (worker_threads, clipboard, real signal handling, JS-module `vi.mock`) is ported or explicitly excluded with a documented reason.

---

## 4. napi-rs shim sketch

A dedicated `crates/atilla-napi` crate exposes ported Rust internals to the vendored TS tests. Example for the Anthropic SSE parser:

```rust
// crates/atilla-napi/src/anthropic.rs
use napi_derive::napi;

/// Mirrors pi-ai `src/api/anthropic-messages.ts` : parse an SSE body into the
/// normalized event stream the tests assert on.
#[napi(object)]
pub struct StreamEvent {
    pub kind: String,           // "start" | "thinking_start" | "text_delta" | "done" | ...
    pub text: Option<String>,
}

#[napi]
pub fn parse_anthropic_sse(body: String) -> napi::Result<Vec<StreamEvent>> {
    atilla_core::ai::anthropic::parse_sse(&body)
        .map(|evs| evs.into_iter().map(Into::into).collect())
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
```

The faux-provider callback (the hard part — a JS test drives the Rust streaming loop):

```rust
// The Rust stream() accepts a JS callback so registerFauxProvider() can supply responses.
#[napi]
pub fn stream(
    model: JsModel,
    request: JsRequest,
    #[napi(ts_arg_type = "() => FauxResponse")] next_response: ThreadsafeFunction<()>,
) -> napi::Result<JsReadableStream> { /* ... call back into JS for each queued faux message ... */ }
```

Generated JS shim that replaces the pi source file (tests import this unchanged):

```ts
// packages/ai/src/api/anthropic-messages.ts  (generated — swap-map entry)
export { parseAnthropicSse as parseSse, stream } from "@atilla/napi/anthropic-messages";
```

Build: napi-rs `napi build --platform` emits the `.node` addon + `.d.ts`; a small codegen step reads the swap map and writes the shim files before `npm test`. Keep the vendored pi checkout as a **git submodule pinned to a SHA** so swaps are applied to a clean tree and drift is explicit.

---

## 5. Conformance dashboard (continuous N-of-M)

- **Harness runner** (`scripts/conformance.sh`): checkout pinned pi submodule → apply swap map → `npm test -- --reporter=json` (vitest) and node:test JSON → parse into per-file / per-case `pass | fail | skip`.
- **Output:** machine-readable `conformance.json` (`{ pi_sha, total, passing, skipped, by_package, by_file }`) plus a rendered HTML dashboard (published as a CI artifact) showing `N of M pi tests passing` with per-package bars.
- **Upstream drift as a first-class signal:** a scheduled job (weekly cron) bumps the pi submodule to upstream HEAD and re-runs. **Newly added upstream tests default to red** (unclaimed) and expand the denominator, so the score *drops* until we port — the exact pressure we want. Diff the test-file set between SHAs to auto-file "new pi tests to claim."
- **CI gate:** atilla CI runs the conformance harness on every PR and fails if the passing count regresses below the committed baseline in `conformance.json`, so the mirror can only move forward.

---

## 6. Recommendation & sequencing

1. **Stand up the seam first.** Add `crates/atilla-napi`, vendor pi as a pinned submodule, build the swap-map codegen, and get *one* pi-ai protocol test file (e.g. `anthropic-sse-parsing.test.ts`) green end-to-end. This de-risks the whole approach before bulk porting.
2. **Solve the faux-provider callback early** (§1.1) — it unblocks the entire agent/coding-agent tier.
3. **Port pi-ai bottom-up** (SSE/request-shaping/token/model-catalog) — best ROI, ~100 files of mostly pure logic.
4. **Bring up the CLI/RPC drop-in** (Tier 2) for coding-agent integration tests in parallel.
5. **Extract golden vectors** from the green pi-ai files for Node-free `cargo test` insurance.
6. **Ship the dashboard + CI gate** as soon as step 1 is green, so progress is visible and monotonic.

---

## 7. How good a spec is pi's suite? (upstream code-coverage audit)

The sibling "Bun-in-Rust" finding raises the real question: Bun big-banged ~535k lines using an unchanged TS test suite *as the spec*. Whether we can be that aggressive hinges on one number — how much of pi's code its own tests actually exercise. We ran pi's suite under v8 coverage (Node v22.22, provider keys unset so network tests skip, per `test.sh`).

| package | statements | branches | functions | lines | tests |
|---|---|---|---|---|---|
| **ai** | 72.5% | 62.9% | 82.6% | 74.1% | 556 pass / **738 skipped** / 1294 |
| **agent** | 72.5% | 61.2% | 85.1% | 74.3% | 180 pass / 0 skip |
| coding-agent | not scored¹ | — | — | — | 1491 pass / 87 fail / 47 skip |
| tui | n/a² | — | — | — | node:test, not vitest |

¹ 87 tests fail in a bare sandbox (extension discovery, fswatch, stdout-cleanliness — environment/filesystem-shaped, not code bugs) and v8 suppresses the report while any test fails. ² tui uses `node:test`; no v8 summary emitted.

**Verdict: usable spec, but not spec-grade — lean incremental, not big-bang.**

- **`ai`'s 74% is inflated by skips.** 738 of 1294 tests (57%) self-skip without live provider keys — and they cover precisely the least-covered code: provider streaming and OAuth. `bedrock-converse-stream.ts` 52%, `mistral-conversations.ts` 30%, `google-vertex.ts` 50%, `oauth/*.ts` 11–57%. Porting those adapters from the offline suite alone would fly blind; they need live-key runs or recorded golden transcripts.
- **`agent` is more trustworthy** (0 skips, 180 passing) but has hard gaps: `proxy.ts` is **0%** (104 lines untested) and the 440-line core `agent-harness.ts` sits at 71% line / **50% branch**. The session/storage layer is well covered (session 99%, compaction 98%, jsonl/memory repos 88–100%).
- **`coding-agent` and `tui` are unmeasured** — treat their conformance as unknown until a cleaner-environment run.

**Implication for the harness:** a faithful port can lean on pi's tests for the well-covered core (session/storage, SSE parsing, request shaping) but must add *its own* Rust tests — and likely record golden LLM transcripts — for the provider-I/O and OAuth adapters and the agent-harness branches the offline suite never touches. This is a second, independent argument for the tiered/incremental approach in §3 over a Bun-style big-bang: the spec has holes exactly where the port is riskiest.

**Two porting gotchas surfaced while measuring** (both block a from-scratch `npm test`):

- `packages/ai` devDepends on `canvas` (a native module needing `libcairo2`/`pango`/`jpeg`/`gif`/`rsvg` system libs) for image tests — the conformance environment must install these.
- `packages/ai/src/providers/data/*.json` is **generated, not committed** — `npm run generate-models` fetches model catalogs over HTTPS (models.dev, openrouter). Without it, both `ai` and `agent` fail at import. **The Rust port (and its conformance harness) must reproduce this model-catalog generation step or vendor the JSON.**

## 8. Open questions

- **Faux-provider round-trip:** can napi threadsafe-function callbacks reproduce the exact streaming event *ordering and timing* the tests assert on, or do we need the faux provider to stay JS and only swap the code *below* it? (Prototype in step 1.)
- **RPC protocol stability:** is pi's RPC/line protocol versioned and stable enough to target, or does it churn? Determines Tier 2 viability.
- **Submodule vs vendored copy:** submodule (clean drift tracking) vs vendored subtree (simpler CI). Leaning submodule.
- **Where does the vendored-pi + Node toolchain live** — in atilla's repo/CI, or a separate `atilla-conformance` repo so the Rust crates stay Node-free? Leaning separate directory (`conformance/`) in-repo, Node isolated there.
- **License hygiene:** confirm pi's license permits vendoring its test files into our conformance harness (study `pi_agent_rust` only; never vendor it).
- **Exclusion policy:** which irreducibly-Node tests (worker_threads, clipboard) get an explicit documented skip vs a hand-port, and how do we keep that list honest against upstream drift?
