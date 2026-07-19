# pi → pidgin Porting Map

> **Pinned upstream commit:** [`3da591ab74ab9ab407e72ed882600b2c851fae21`](https://github.com/earendil-works/pi/commit/3da591ab74ab9ab407e72ed882600b2c851fae21)
> (`earendil-works/pi`, 2026-07-17 17:13:39 +0200 — "feat(coding-agent): add Hugging Face llama search")
>
> This map inventories pi at that exact commit. When re-syncing upstream, bump this hash and diff against it. pidgin base at time of writing: `20603e2` (empty 2-crate scaffold).

## Overview

pi is an npm-workspaces monorepo (pure ESM, Node >=22.19.0, TypeScript built with `tsgo`, linted with biome). It contains **five packages** totaling **~122,420 source LOC + ~82,841 test LOC**. pidgin is a from-scratch Rust port: the target is a continually-updating Rust mirror exposed via native extensions per language, with pi's own test suite as the conformance bar and pi's TS as the spec.

The internal dependency DAG is a clean chain — `ai` is the leaf everything rests on, `orchestrator` is the root that pulls in everything:

```
ai ──▶ agent ──┐
 │             ├──▶ coding-agent ──▶ orchestrator
 └────▶ tui ───┘
```

- **ai** — provider/LLM layer (Anthropic, OpenAI, Google, Bedrock, Mistral), OAuth, streaming, model catalog. No internal deps → **the leaf, port first.**
- **agent** — agent-core loop; depends only on `ai`. Lightly node-coupled → **easy second.**
- **tui** — custom differential ANSI terminal renderer + native C modifier addons. No internal deps but **HARD** (native code) → defer.
- **coding-agent** — the 72K-LOC bulk: tools, the jiti TS-extension engine, compaction, the interactive TUI app, RPC mode. Depends on ai + agent + tui → **the hairball, port incrementally after its deps.**
- **orchestrator** — thin socket/IPC layer over coding-agent. Depends on coding-agent → **last, but tiny.**

## Package inventory

| Package | npm name | Src LOC (files) | Test LOC (files) | Test fw | Internal deps | Node/runtime coupling |
|---|---|---|---|---|---|---|
| **ai** | `@earendil-works/pi-ai` | 26,674 (167) | 25,407 (100) | vitest | — | **Low–Med** — 4 files touch node builtins (http/https/zlib/os/fs/readline); bulk is provider HTTP glue |
| **agent** | `@earendil-works/pi-agent-core` | 8,498 (31) | 5,475 (16) | vitest | ai | **Low** — 1 file touches node builtins (readline/os/fs/child_process); `.` + `./node` entrypoint split |
| **tui** | `@earendil-works/pi-tui` | 12,843 (35) | 12,892 (27) | node:test | — | **High** — custom ANSI renderer + **native C addons** (`darwin-modifiers.c`, `win32-console-mode.c` + prebuilt `.node`) |
| **coding-agent** | `@earendil-works/pi-coding-agent` | 72,423 (282) | 39,067 (176) | vitest | ai, agent, tui | **High** — 69 files touch node builtins (fs×39, child_process×17, os×15, worker_threads×2), **jiti** runtime-TS extensions, `bun build --compile` binary |
| **orchestrator** | `@earendil-works/pi-orchestrator` | 1,982 (13) | 0 (0) | none | coding-agent | **Med** — 9 files (node:net×3 socket/IPC server, fs×5, child_process×1) |

### ai — internal breakdown (port first)
| Module | Src LOC | Purpose | Coupling | Notes |
|---|---|---|---|---|
| `src/api/*` | 9,800 | Per-provider request/stream codecs: anthropic-messages, openai-completions, google-generative-ai, bedrock-converse-stream, pi-messages | Med (http) | Each provider is a fairly independent unit → good slice boundaries |
| `src/providers/*` | 6,314 | Provider registry, capability/config, routing | Low | Depends on api |
| `src/auth/*` | 2,714 | OAuth flows | Med | http + local token storage |
| `src/utils/*` | 1,428 | Shared helpers | Low | Leaf-ish |
| `src/compat` | 45 | Compat shims | Low | Trivial |
| `scripts/generate-models.ts` | — | Build-time model-catalog codegen | Low | Reproduce as a Rust build step / checked-in data |

### agent — internal breakdown (port second)
~8,498 LOC across 31 files. Agent loop + tool orchestration built on `ai`. Only 1 file is node-coupled; exports split into `.` (portable) and `./node` (node-specific) — the `.` surface is the priority port target.

### tui — internal breakdown (DEFER — hard)
| Module | Purpose | Coupling |
|---|---|---|
| `terminal.ts`, `terminal-colors.ts`, `terminal-image.ts`, `tui.ts` | Raw-ANSI differential renderer | High |
| `editor-component.ts`, `autocomplete.ts`, `fuzzy.ts`, `keybindings.ts`, `undo-stack.ts`, `kill-ring.ts` | Editor/input widgets | Med |
| `native/darwin-modifiers.c`, `native/win32-console-mode.c` (+ `.node` prebuilds) | Platform key-modifier detection | **Native C** |

Not ink/blessed — a bespoke renderer. `@xterm/headless` is a test-only harness. Tests use `node:test`. Porting faithfully means re-implementing the renderer in Rust and replacing the C addons with equivalent Rust platform code (or `crossterm`-style crates), then matching behavior against the node:test suite.

### coding-agent — internal breakdown (the hairball)
| Module | Src LOC | Purpose | Coupling | Order |
|---|---|---|---|---|
| `src/core/tools/*` | 4,072 | Filesystem/exec tools: bash, edit, edit-diff, read, write, ls, grep, find, path-utils, file-mutation-queue, output-accumulator, truncate, render-utils | High (fs/exec) | Early within coding-agent — mechanical but voluminous |
| `src/core/` (root) | ~17,000 | Core agent wiring, config, session, message handling | High | Mid |
| `src/core/extensions/*` | 3,846 | **jiti runtime-TS extension engine** (`loader.ts` uses `jiti/static`) | **Extreme** | **LAST — needs architecture decision** |
| `src/core/compaction/*` | 1,420 | Context compaction | Med | Mid |
| `src/core/export-html` | 746 | HTML transcript export | Low | Anytime |
| `src/modes/interactive/*` | 16,663 | The interactive TUI app (depends on `tui`) | High | **After tui** |
| `src/modes/rpc/*` | 1,726 | Headless RPC entrypoint (`dist/rpc-entry.js`) | Med | Good early headless target |
| `src/utils/*` | 3,236 | Shared utilities | Low–Med | Early |
| `src/cli/*` | 1,043 | argv shell, `pi` bin | Med | Maps to pidgin-cli |
| `src/bun` | 55 | `bun build --compile` glue | Low | Skip (Rust binary is native) |

### orchestrator (port last, tiny)
~1,982 LOC, 13 files, no tests. A socket/IPC (`node:net`) layer that supervises coding-agent processes. Small; port once coding-agent has a stable surface.

## Three hard problems (all in coding-agent / tui)

1. **jiti runtime-TS extension engine** (`coding-agent/src/core/extensions/loader.ts`, ~3,846 LOC subsystem, used in 4 files). It loads TypeScript extension modules at runtime via `createJiti` from `jiti/static`, statically bundling pi-agent-core / pi-ai / pi-tui / pi-coding-agent / typebox as "virtualModules" exposed to extensions. **No direct Rust analog.** Requires an explicit architecture decision — options: embed a JS/TS runtime (e.g. deno_core / boa / quickjs), WASM plugins, dynamic libs, or a subprocess extension protocol. In-repo extensions to conform against: `.pi/extensions/{prompt-url-widget,tps,import-repro,redraws}.ts`.
2. **Custom native-addon TUI** (`tui` package). Bespoke differential ANSI renderer + C addons for macOS/Windows key modifiers. Hardest to reproduce faithfully.
3. **Heavy fs/child_process/worker_threads coupling** in coding-agent (69 files). Mostly mechanical translation to Rust `std::fs` / `std::process` / threads, but voluminous and pervasive.

Secondary: the `ai` package is largely provider-SDK glue (`@anthropic-ai/sdk`, `openai`, `@google/genai`, bedrock, mistral) + OAuth + a build-time model-catalog generator. The SDKs don't port 1:1 — in Rust they become hand-written HTTP clients against each provider's wire API.

## Recommended porting order

1. **`ai`** (leaf; everything depends on it). Within ai: `utils` → `api` (one provider at a time) → `providers` → `auth`.
2. **`agent`** (depends only on ai; the portable `.` entrypoint first, `./node` after).
3. **`coding-agent` — incrementally**, deps-first: `utils` → `core/tools` → `core` root + `compaction` → `modes/rpc` (headless) → `modes/interactive` (needs tui) → `core/extensions` (needs the jiti decision).
4. **`tui`** (no internal deps, so it *can* start anytime, but it's hard and only `modes/interactive` needs it — schedule it to land just before interactive mode).
5. **`orchestrator`** (last; thin, depends on coding-agent).

### Recommended first vertical slice
**`ai`: types + a single provider (Anthropic `anthropic-messages`) end-to-end — request build, streaming decode, response shape — enough to pass a targeted subset of `ai`'s vitest suite.** Rationale: `ai` is the leaf every other package needs; a thin vertical through one provider de-risks the whole port, establishes the crate layout and the test-conformance harness, and gives the native-extension exposure something real to call. Explicitly **defer** `tui` and the jiti extension engine to the end.

## Porting ledger

Status legend: `[ ] not started` · `[~] in progress` · `[x] ported` · `[T] passing upstream tests`. Future sessions update the Status column and link the pidgin PR/crate as work lands.

| # | Module | Package | Src LOC | Upstream tests | Coupling | Status | pidgin crate / PR |
|---|---|---|---|---|---|---|---|
| 1 | ai/utils | ai | 1,428 | packages/ai/test/** | Low | [x] ported | #52, #80 (also #71 compat) |
| 2 | ai/api (anthropic-messages) — **first slice** | ai | ~part of 9,800 | packages/ai/test/** | Med | [x] ported | native per manifest (first-slice + seam series) |
| 3 | ai/api (openai, google, bedrock, mistral, pi) | ai | ~rest of 9,800 | packages/ai/test/** | Med | [x] ported | #54 (Google/Vertex), #117 (Bedrock); openai/mistral/pi via registry #47 |
| 4 | ai/providers | ai | 6,314 | packages/ai/test/** | Low | [x] ported | #47 (+#131 dup marker) |
| 5 | ai/auth | ai | 2,714 | packages/ai/test/** | Med | [x] ported | #57, #87, #118 |
| 6 | ai/model-catalog codegen | ai | — | — | Low | [~] in progress | #65, #128 (runtime/registry/store landed; build-time codegen step pending) |
| 7 | agent (`.` entrypoint) | agent | ~part of 8,498 | packages/agent/test/** | Low | [x] ported | #46, #97, #109, #142 |
| 8 | agent (`./node` entrypoint) | agent | ~rest of 8,498 | packages/agent/test/** | Med | [x] ported | #99, #120 |
| 9 | coding-agent/utils | coding-agent | 3,236 | packages/coding-agent/test/** | Low–Med | [x] ported | #68 (core-glue/utils series) |
| 10 | coding-agent/core/tools | coding-agent | 4,072 | packages/coding-agent/test/** | High | [x] ported | #48, #94, #81, #139, #130 |
| 11 | coding-agent/core (root) | coding-agent | ~17,000 | packages/coding-agent/test/** | High | [x] ported | #68, #45, #102, #104 |
| 12 | coding-agent/core/compaction | coding-agent | 1,420 | packages/coding-agent/test/** | Med | [x] ported | #83 |
| 13 | coding-agent/core/export-html | coding-agent | 746 | packages/coding-agent/test/** | Low | [ ] not started | — |
| 14 | coding-agent/modes/rpc | coding-agent | 1,726 | packages/coding-agent/test/** | Med | [ ] not started | — |
| 15 | coding-agent/cli | coding-agent | 1,043 | packages/coding-agent/test/** | Med | [x] ported | #73 (CLI conformance), #101 (shared SessionManager) |
| 16 | tui (renderer + widgets) | tui | 12,843 | packages/tui/test/** (node:test) | High | [x] ported | #91, #86, #95, #88, #106, #124, #133, #105 |
| 17 | tui native (darwin/win32 modifiers) | tui | (C addons) | — | Native C | [x] ported | #78 (crossterm + native replacements) |
| 18 | coding-agent/modes/interactive | coding-agent | 16,663 | packages/coding-agent/test/** | High | [~] in progress | — (needs tui; in progress) |
| 19 | coding-agent/core/extensions (jiti) | coding-agent | 3,846 | packages/coding-agent/test/** | Extreme | [~] in progress | #108, #112, #122, #125, #144 (bootstrap + seams; full jiti engine pending) |
| 20 | orchestrator | orchestrator | 1,982 | none | Med | [~] in progress | #107, #110, #116, #127, #135 |

## Re-sync checklist (for future upstream bumps)
- [ ] `git -C <pi-clone> fetch && git rev-parse origin/main` → compare to pinned hash above.
- [ ] `git diff 3da591ab..NEW --stat -- packages/` to see which modules moved.
- [ ] Update the pinned hash, package LOC table, and any ledger rows whose upstream surface changed.
