# TypeScript → Rust: Transpilation Assessment for the pi Rewrite

*Decision-ready research for rewriting [`earendil-works/pi`](https://github.com/earendil-works/pi) — a ~100k-src-LOC TypeScript coding-agent CLI — into an idiomatic Rust core exposed via native extensions (ext-php-rs and friends). Dated 2026-07-18.*

## TL;DR

- **Existing tools won't save you.** There is no production-grade general TS→Rust transpiler in 2026. The direct-transpiler repos are 1–15 star toys; the most serious enabler — a native-Rust TS type-checker (`stc`) — was officially abandoned; and most search hits are either the wrong direction (`ts-rs` generates TS *from* Rust) or JS→WASM engines (Javy, Porffor, AssemblyScript, Boa) that never emit Rust source.
- **A custom transpiler for pi is not worth building.** pi's type system is unusually Rust-friendly (strict mode, discriminated unions, no decorators/reflection/eval), but ~half its ~95k non-generated LOC is Node-runtime-coupled (≈2,900 closures, ≈2,500 async/streaming sites, an event-driven from-scratch TUI, 142 `node:` imports), and it has a hard blocker: a `jiti`-based runtime-TypeScript **extension engine with no Rust equivalent**. A transpiler would clean-map maybe 40–50% and emit non-idiomatic goo on the rest — poisoning the "idiomatic Rust core" goal.
- **Recommended path: AI-accelerated hand-rewrite, using the TS as executable spec.** Port module-by-module, idiomatically, with the TypeScript compiler API / oxc as a type oracle and LLMs as a per-function accelerator inside a compile-and-test loop — not a push-button transpile. Decide the extension-system question *before* estimating scope.

---

## Decision (2026-07-18)

Correction (later the same day): the "no napi bridge" consequence below is superseded. The project now builds napi-rs shim packages that present pi's exact module surface and runs pi's own unit suites against them — see notes/startup/testing-strategy.md and the repo-root design.md. The idiomatic-first, big-bang, no-strangler-fig decision itself stands.

The project owner has chosen: **idiomatic-first, big-bang, no strangler-fig.** A rewrite that fails a ported test fails honestly; a test that passes does so because the behavior genuinely works in Rust, not because a Node shim made it pass. This supersedes the napi-rs strangler-fig fallback below — that fallback is recorded for completeness but is not the chosen path.

Two consequences that follow from pi's actual code and tests:

- **Port surface is ~95,500 LOC.** Excluding all tests and ~5,200 LOC of generated model-catalog files (which become a serde/JSON data table, effectively free), the source to rewrite is ~95,478 LOC across 362 files, dominated by `coding-agent` (54,662), then `ai` (18,424 non-generated), `tui` (12,166), `agent` (8,244), `orchestrator` (1,982).
- **pi's existing tests are almost all unusable as a runnable oracle for a bridge-free Rust rewrite.** Of 319 test files / ~82,800 test LOC / ~3,631 cases, ~99.6% are in-process unit tests that `import` directly from `src/` (vitest, plus node:test in `tui`) and are welded to the JS module graph. Without an FFI/napi bridge (which the no-strangler-fig decision rules out) they cannot execute against a Rust binary and become a **spec to read, not a suite to run.** Only 4 files / ~15 cases (`stdout-cleanliness`, `session-file-invalid`, `session-id-readonly`, `startup-session-name` in `coding-agent/test/`) are black-box tests that spawn the CLI; they are reusable by repointing the spawn target from `node src/cli.ts` to the compiled Rust binary (~2 lines each). To get a runnable oracle consistent with "pass = works in Rust," expect to build a thin black-box CLI/e2e harness that drives the Rust `pi` binary and reuses pi's fixtures and assertions — that is the only test layer that validates Rust behavior rather than Node's.

---

## 1. State of existing TS/JS → Rust tooling (2026)

### 1a. Direct TS→Rust transpilers — a graveyard

| Tool | Direction | What it does | Maturity | Idiomatic Rust output? | License | Alive? |
|---|---|---|---|---|---|---|
| [vedantroy/ts2rust](https://github.com/vedantroy/ts2rust) | TS→Rust | Aspirational transpiler | 1 commit, toy | — (nonfunctional) | unstated | **Dead** |
| [coltonoscopy/ts2rust](https://github.com/coltonoscopy/ts2rust) | TS→Rust | PoC transpiler in Clojure | 3 commits, toy | No | unclear | Dormant PoC |
| [mcmah309/ts2rs](https://github.com/mcmah309/ts2rs) | TS→Rust (**types only**) | TS type decls → Rust structs + serde | v0.1.x, early (Feb 2026) | Types only, no logic | unstated | Alive |
| [j4ger/ts2rs](https://github.com/j4ger/ts2rs) | TS→Rust (**types only**) | Import TS interfaces via proc-macro | small | Types only | MIT-ish | Low activity |
| **[ts-rs (Aleph-Alpha)](https://github.com/Aleph-Alpha/ts-rs)** | **Rust→TS (WRONG WAY)** | Derives TS bindings from Rust structs | Mature, ~3k stars | — (reverse direction) | MIT/Apache | Actively maintained |
| CodeConvert / FavTutor / Syntha / CodingFleet | TS/JS→Rust | LLM-wrapper web converters, ~25k-char cap | snippet-only | per-snippet, needs rewrite | commercial | Alive |

**Read:** every genuine direct-transpiler attempt is a single-author experiment. The "types-only" tools (`mcmah309/ts2rs`, `j4ger/ts2rs`) are real but convert *type declarations* to serde structs for a shared TS/Rust boundary — they do not translate logic. **Name-collision trap:** `ts-rs` (the popular one) goes Rust→TS and is useless here; don't confuse it with the `ts2rs`/`ts2rust` experiments.

### 1b. Parsing / type infrastructure — parsing solved, type-checking abandoned

| Tool | Role | Maturity | Gives typed AST? | License | Alive? |
|---|---|---|---|---|---|
| [oxc](https://oxc.rs/) | Rust JS/TS parser, arena AST (~3× swc) | Production | **No** — syntax only; type-aware lint delegates to tsc | MIT | Actively maintained |
| [swc](https://swc.rs/) | Rust JS/TS parser + transforms (powers Next.js) | Production | No — syntax only | MIT/Apache | Alive |
| TypeScript compiler / **tsgo (TS 7)** | The **only** fully type-resolved AST + `TypeChecker` | Production; Go port (Project Corsa) ~GA Jul 2026, ~10× faster | **Yes** (the type oracle you need) | Apache-2.0 | Actively maintained |
| [stc](https://github.com/dudykr/stc) | TS type-checker **in Rust** | **Officially abandoned** | — | Apache-2.0 | **Dead** |
| [tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript) | Incremental untyped CST | Mature | No (untyped, error-tolerant) | MIT | Alive |

**Read:** parsing TypeScript in Rust is a solved, excellent problem (oxc/swc). *Type-checking* it in Rust is not — `stc` gave up, and Microsoft chose **Go**, not Rust, for the tsc rewrite. Faithful TS→Rust translation needs resolved types, so any custom pipeline must shell out to `tsc`/`tsgo` as an out-of-process type oracle and write the entire semantic-mapping layer by hand.

### 1c. WASM/runtime-adjacent — confused with transpilation, but none emit Rust

[Javy](https://github.com/bytecodealliance/javy) (embeds QuickJS in WASM), [Porffor](https://porffor.dev/) (AOT JS/TS→WASM/C, "not for serious use", subset only), [AssemblyScript](https://www.assemblyscript.org/) (a TS-*like* language → WASM, you must write to its subset), [Boa](https://boajs.dev/) / rquickjs / quickjs-rs (JS *engines* in/for Rust). All of these either run your JS or compile a JS subset to WASM. **None produce maintainable Rust source.** "Run my JS inside Rust/WASM" is a different project from "turn my TS into idiomatic Rust"; only the latter is a rewrite.

### 1d. AI-assisted translation — the only thing that attempts real logic

This is where the honest action is, with sobering numbers:

- **Repository-level benchmark ([RustRepoTrans](https://arxiv.org/html/2411.13990v6), 375 tasks):** best single-shot Pass@1 ~45–51% (Claude-3.5 45.3%, DeepSeek-R1 ~51.5%); with one compiler-feedback repair round, ~62%. So **a third to a half of tasks still fail** even at function granularity with dependencies provided.
- **Google's at-scale LLM migrations ([arxiv 2501.06972](https://arxiv.org/pdf/2501.06972)):** ~50% developer-effort reduction, LLMs generating ~74% of changes — but they *decompose* migrations into discrete steps, combine LLMs with deterministic AST transforms, and keep engineers reviewing. This is the realistic model: LLM-in-the-loop, not point-and-shoot.
- JS→Rust is *harder* than the C→Rust pair most published tooling targets (GC + structural typing vs. Rust ownership + nominal sum types).

**Bottom line (§1):** Budget automated translation as an **AI-accelerated manual rewrite** (~50% effort reduction at best, per-function assist inside a compile-and-test harness), not a push-button transpile. Use oxc/tsgo as parsing/type oracles; expect no tool to hand you idiomatic Rust.

---

## 2. pi codebase characterization

### What pi is

A **coding-agent CLI + interactive TUI** (npm `@earendil-works/pi-coding-agent`, binary `pi`), a TypeScript ESM monorepo of five workspace packages targeting Node ≥22.19 (with a Bun entrypoint). It ships a multi-provider LLM client (`pi-ai`: OpenAI, Anthropic, Google, Bedrock, Mistral + ~30 more via generated catalogs), an agent runtime with tool-calling and session state (`pi-agent`), an interactive coding agent with a from-scratch differential-rendering TUI (`pi-coding-agent` + `pi-tui`), and a thin orchestrator. Its defining feature is **self-extensibility**: users drop TypeScript extension files loaded and executed *at runtime* via `jiti`. That capability is a JS-runtime feature, not transpilable logic — and it drives the whole assessment.

### LOC (`.ts` only, excl. node_modules/dist; 850 files, ~205k total)

| Package | Purpose | src LOC | test LOC | src files | Notes |
|---|---|---:|---:|---:|---|
| `packages/ai` | Multi-provider LLM API | 23,590 | 25,901 | 269 | **~5,913 LOC generated** (model catalogs) |
| `packages/agent` | Agent runtime / tool loop | 8,244 | 5,680 | 47 | Cleanest, most self-contained |
| `packages/coding-agent` | CLI + interactive mode + tools | 54,662 | 41,018 | 459 | Largest; heaviest runtime coupling |
| `packages/tui` | Differential-render terminal UI | 12,166 | 13,562 | 62 | Includes 2 native C `.node` addons |
| `packages/orchestrator` | Thin coordinator | 1,982 | 0 | 13 | No tests |
| **Total src** | | **~100,644** | **~86,161** | **~850** | test:src ≈ 0.86 (strong coverage) |

**Real logic to port ≈ 94,700 src LOC** (after subtracting ~5,900 generated data lines, which become a serde/JSON data file, not ported code).

### Dynamism scorecard — type system is Rust-friendly (LOW dynamism)

`tsconfig`: **strict: true**, `erasableSyntaxOnly: true`, ES2022. Highlights across src:

- Green 646 `interface` + 859 `type` decls; **523 discriminant literal fields** (`type:`/`kind:`/`role:`) → discriminated-union-first style → clean Rust `enum`s; 84 `switch` → `match`; 0 `enum` decls (string-literal unions instead); 0 mapped types; 2 conditional types.
- Green **0 decorators, 0 `Reflect`/metadata, 0 `eval`, 1 `Proxy`, 1 `.prototype`, 2 `defineProperty`** — essentially no metaprogramming.
- Yellow 152 `any` (low for 100k LOC, concentrated at provider/JSON edges), 496 `unknown` (mostly IO/JSON boundaries → `serde_json::Value`), 887 `as` casts (inflated by 125 `as const`), 13 `as unknown as`, 8 non-null `!`.
- Yellow 141 classes / 65 `implements` / shallow inheritance → traits+structs; 20 `Object.assign` → explicit struct updates; 40 `keyof`.

**Verdict:** about as Rust-friendly as a large TS codebase gets.

### Runtime-coupling scorecard — this is where the cost is (HIGH)

- Red **2,922 arrow fns / 880 callback-typed params** → Rust ownership makes stored/escaping closures costly (`Box<dyn Fn>`, `Arc`, sometimes redesign).
- Yellow 870 `async` fns / 1,385 `await` / 1,097 `Promise` → `tokio` + `async fn`; mechanical but everywhere.
- Red 142 `node:` imports (fs 65, path 54, os 21, child_process/spawn/exec 50) → `std`/`tokio`, but 210+ call sites to rewrite; 102 `process.env`; 18 `__dirname` / 23 `import.meta`.
- Yellow 16 async generators / 18 `yield` — **streaming LLM responses** → Rust `Stream`/`async-stream`; 8 EventEmitter + 85 `.on(` (event-driven TUI/agent) → channels.
- Yellow 61 `JSON.parse` / 90 `JSON.stringify` → serde, but JSON is treated as loosely-typed objects at LLM edges.
- Yellow 2 native C `.node` addons (darwin modifiers, win32 console) — reimplement directly in Rust (easier than in TS).

### The showstopper: the extension system

`packages/coding-agent/src/core/extensions/loader.ts` uses `jiti` to **compile and execute user-supplied TypeScript at runtime**, with documented reliance on JS semantics (cross-loader duck-typing because `instanceof` fails across jiti caches; `globalThis` to share theme state). **Rust cannot JIT arbitrary TypeScript.** A Rust port must (i) drop runtime extensibility, (ii) switch to WASM/dylib plugins (breaks every existing extension), or (iii) embed a JS engine (Deno core / QuickJS) — reintroducing a JS runtime and defeating much of the point. **Decide this before any LOC/effort estimate.**

### Dependency table (runtime deps; workspace-internal omitted)

| Dependency | Purpose | Rust equivalent | Maturity |
|---|---|---|---|
| `@anthropic-ai/sdk` | Anthropic client | hand-roll on `reqwest` (community crates exist) | Yellow |
| `openai` | OpenAI-compatible client | `async-openai` | Green |
| `@google/genai` | Gemini client | hand-roll on `reqwest` (no first-party crate) | Red |
| `@aws-sdk/client-bedrock-runtime` | Bedrock | `aws-sdk-bedrockruntime` | Green |
| `@mistralai/mistralai` | Mistral client | hand-roll on `reqwest` | Red |
| `@opentelemetry/api` | Tracing | `opentelemetry` + `tracing` | Green |
| `undici` / proxy-agents | HTTP/fetch/proxy | `reqwest`/`hyper` (built-in proxy) | Green |
| `typebox` | Runtime schema + JSON Schema (tool params) | `serde` + `schemars` (+ `jsonschema`) | Green mature but **load-bearing & pervasive** → real re-modeling |
| `partial-json` | Parse incomplete streaming JSON | none — hand-roll | Red essential to streaming tool-calls |
| `yaml` | Config | `serde_yaml` | Green |
| `ignore` | .gitignore matching | `ignore` (ripgrep author) | Green |
| `glob` / `minimatch` | Globbing | `glob` / `globset` | Green |
| `chalk` | Terminal color | `owo-colors` / `nu-ansi-term` | Green |
| `diff` | Text diffing | `similar` | Green |
| `highlight.js` | Syntax highlighting | `syntect` | Green |
| `marked` | Markdown (TUI) | `pulldown-cmark` | Green |
| `get-east-asian-width` | Char width | `unicode-width` | Green |
| `cross-spawn` | Subprocess | `std`/`tokio::process` | Green |
| `proper-lockfile` | File locking | `fs4` / `fd-lock` | Green |
| `hosted-git-info` | Parse git host URLs | `git-url-parse` | Yellow |
| `semver` | Version ranges | `semver` | Green |
| `@silvia-odwyer/photon-node` | Image processing | `image` | Green |
| `@mariozechner/clipboard` | Clipboard | `arboard` | Green |
| **`jiti`** | **Runtime TS loader (extensions)** | **none** | Red **architectural blocker** |

**Dependency verdict:** ~85% of deps have mature Rust equivalents (several *better*: `ignore`, `globset`, `syntect`, `image`, AWS SDK). Genuine gaps forcing hand work: **`jiti` (blocker)**, **Google/Mistral clients** (no maintained crates), **`typebox`** (mature target but pervasive), **`partial-json`** (small but essential).

### Clean-transpile vs hand-rewrite

- Green **Clean (port largely mechanically):** `packages/agent/src` (~8.2k LOC, discriminated-union heavy, self-contained, well-tested — best starting point); the generated model catalogs (~5.9k → serde data file); `pi-ai` type/message-shape logic; pure tool algorithms in `coding-agent/src/core/tools/` (edit-diff, path-utils, glob/grep); utilities (diff, truncation, width, semver/glob wrappers).
- Yellow **Portable with real redesign:** `pi-ai` streaming layer (`openai-codex-responses.ts` 1,573, `anthropic-messages.ts` 1,313, `openai-completions.ts` 1,355 → `Stream` + custom incremental parser); `pi-tui` (12k LOC differential renderer, `editor.ts` 2,333, `keys.ts` 1,401 → crossterm/ratatui-style redesign; C addons reimplement cleanly); `coding-agent` session/settings managers (heavy async + env + fs).
- Red **Hand-rewrite / decision required:** the **extension system** (`loader.ts`, `runner.ts` 1,214, `types.ts` 1,682 + jiti); `interactive-mode.ts` (**6,008 LOC**, deeply event-driven, cross-loader duck-typing); `package-manager.ts` (2,650, npm/child_process orchestration); Google & Mistral clients; Bun entrypoints and native `.node` loading.

---

## 3. Three paths, with honest effort estimates

**Effort scale:** relative, assuming a small team fluent in both Rust and TS. "Unit" ≈ the baseline hand-rewrite effort. These are directional, not calendar commitments.

### Path A — Custom purpose-built transpiler (swc/oxc + tsc oracle → Rust)
Build a TS-subset transpiler mapping interfaces→structs/traits, unions→enums, async/await→tokio, closures→`Box<dyn Fn>`/redesign, using oxc for parse + tsgo for types.

- **Up-front cost:** substantial. You write the entire semantic-mapping layer (ownership inference, closure capture, structural→nominal typing, async lowering) that every abandoned project in §1a died on. `stc`'s abandonment is the warning.
- **Coverage:** the clean ~40–50% of pi transpiles; the Yellow/Red half (closures, streaming, TUI, Node builtins, extensions) either fails or emits non-idiomatic Rust you'd rewrite anyway.
- **Output quality:** machine-goo on the hard half — `Rc<RefCell<…>>` soup, cloned-everywhere, non-idiomatic error handling. **Directly poisons the "idiomatic Rust core for native extensions" goal.**
- **Estimate:** ~2–3× the hand-rewrite for a *worse* result on the parts that matter. **Not recommended.**

### Path B — Continuous transpilation (keep TS as source of truth, regenerate Rust)
Only viable if the generated Rust is never hand-edited. But pi's Rust would need heavy hand-idiomatization (ownership, traits, native-extension surface), so generated output can't be the artifact. Also inherits every Path-A cost plus a permanent maintenance burden, and the `jiti` extension model can't be regenerated at all. **Not viable for pi.**

### Path C — AI-accelerated hand-rewrite, TS as executable spec (RECOMMENDED)
Port module-by-module into idiomatic Rust, reading the TS (and its ~86k LOC of tests) as the spec. Use tsc/oxc as a type oracle to resolve tricky inferred types; use LLMs as a per-function accelerator inside a compile-and-test loop (the Google model); port pi's existing tests alongside each module as the correctness gate.

- **Coverage:** everything, because a human decides the Rust shape (enums, traits, channels, `Stream`s) rather than mechanically mirroring JS.
- **Output quality:** idiomatic by construction — the only path compatible with a clean native-extension surface.
- **Order:** `pi-agent` core → `pi-ai` types + model-catalog data → `pi-ai` streaming → tools → session/settings → TUI → interactive mode. Extension system decided separately (below).
- **Estimate:** baseline **1 unit**; AI-in-the-loop realistically shaves ~30–50% off the pure-manual figure on the clean/portable ~two-thirds, less on the Red redesign parts. The generated model catalogs (~5.9k LOC) are near-free (data transform).

---

## 4. Recommendation

**Take Path C: an AI-accelerated hand-rewrite that treats pi's TypeScript (and its tests) as the spec.** Rationale:

1. **The end goal forbids machine-translation.** An idiomatic Rust core exposed via ext-php-rs demands hand-shaped ownership, traits, and error types. Any transpiler (Path A/B) emits non-idiomatic Rust on exactly the closure/async/streaming half that dominates pi — you'd rewrite it anyway, having paid twice.
2. **The tooling isn't there (§1).** No mature transpiler exists, the native-Rust type-checker is dead, and AI translation tops out around ~half of tasks unaided. The market has effectively voted that this is a guided-rewrite problem.
3. **pi is well-suited to a clean rewrite.** Strict types, discriminated unions, no metaprogramming, ~85% of deps covered by mature (often better) Rust crates, and a strong existing test suite to port as the correctness gate.

**Sequencing:** start with `packages/agent` (cleanest, self-contained, well-tested) to establish Rust patterns and prove the test-porting workflow, then `pi-ai` types + the generated catalogs, then the streaming layer, then tools, then the TUI, and finally interactive mode.

**Decide the extension system first.** It is the single largest scope risk and has no transpile path. Recommended default: **redefine extensions as a stable Rust plugin/host contract** (trait-based dylib or WASM component), accepting that existing `jiti` TS extensions won't port 1:1 — surface this as a product decision before committing to a rewrite budget. If runtime *TypeScript* extensibility must survive verbatim, the only option is embedding a JS engine (Deno core / QuickJS) for the extension sandbox while the core is Rust — a hybrid, not a pure port.

**Fallback:** if a full rewrite proves too large to justify at once, do a **strangler-fig hybrid** — extract the hottest, cleanest, most CPU-bound cores first (agent tool loop, streaming JSON/diff, TUI renderer) into a Rust library exposed to the existing Node/TS app via napi-rs, and migrate outward from there. This banks idiomatic-Rust wins incrementally without a big-bang cutover and keeps the `jiti` extension host on the TS side until the plugin contract is settled. Note (2026-07-18): superseded by the idiomatic-first / no-strangler-fig decision recorded at the top of this document; retained here only as analysis of the alternative.

---

### One-paragraph summary

In 2026 there is still no mature general-purpose TypeScript→Rust transpiler: the direct-transpiler repos are abandoned toys, the one serious native-Rust TS type-checker (`stc`) is dead, most search hits are the wrong direction (`ts-rs`: Rust→TS) or JS→WASM engines that never emit Rust source, and even LLM-assisted translation tops out near ~50–62% task success with JS→Rust being harder than the C→Rust pair most tooling targets. A custom transpiler is **not** worth building for pi specifically — its type system is genuinely Rust-friendly (strict mode, discriminated unions, no decorators/reflection/eval, low `any`), but ~half of its ~95k non-generated LOC is Node-runtime-coupled (≈2,900 closures, ≈2,500 async/streaming sites, an event-driven TUI, 142 `node:` imports) and it has a hard blocker in its `jiti`-based runtime-TypeScript extension system, so a transpiler would emit non-idiomatic goo on exactly the parts that matter and poison the idiomatic-Rust-core goal. **Recommended path: an AI-accelerated hand-rewrite using the TypeScript and its strong test suite as the executable spec**, ported module-by-module (start with `pi-agent`) with tsc/oxc as a type oracle and LLMs as a per-function accelerator inside a compile-and-test loop — with the extension-system contract decided up front, and a napi-rs strangler-fig hybrid as the fallback if a big-bang rewrite is too large.
