# atilla — Plan: a continually-updating Rust mirror of `pi`, with native language bindings

> Status: planning. This document is decision-ready but describes work not yet started.
> Upstream: [`earendil-works/pi`](https://github.com/earendil-works/pi) — MIT, studied at commit `3da591ab` (pkg version `0.80.10`).

## 1. What `pi` is (and what we are mirroring)

`pi` is an open-source, self-extensible **AI coding agent** (a Claude-Code / opencode-style CLI) plus the reusable libraries beneath it. It is a **TypeScript / Node.js ESM monorepo** of five npm packages totalling ~95.5k lines of non-test source (plus ~83k lines of tests), MIT-licensed, authored by Mario Zechner, under very active development (4,986 commits, 303 release tags at time of study).

The five upstream packages, in dependency order:

| Upstream package | npm name | src LOC | Role |
|---|---|--:|---|
| `tui` | `@earendil-works/pi-tui` | 12.2k | Differential-rendering terminal UI toolkit |
| `ai` | `@earendil-works/pi-ai` | 23.6k | Unified multi-provider LLM API (~40 providers) |
| `agent` | `@earendil-works/pi-agent-core` | 8.2k | Agent loop + tool calling + JSONL session tree + compaction |
| `coding-agent` | `@earendil-works/pi-coding-agent` | 54.7k | The `pi` CLI: tools, run modes (interactive/print/rpc), extensions, SDK |
| `orchestrator` | `@earendil-works/pi-orchestrator` | 2.0k | Experimental daemon supervising many agent subprocesses |

**How users invoke it:** primarily the `pi` CLI (interactive TUI, `-p` print mode, and `--mode rpc` headless JSONL-over-stdio); secondarily an in-process TypeScript SDK (`createAgentSession`); and as independently-consumable libraries.

**Two stable, documented, language-agnostic boundaries** matter most for a mirror:
1. **The RPC protocol** (`pi --mode rpc`) — a fully documented JSONL request/response/event protocol over stdio. It is the recommended non-Node integration path today.
2. **The version-3 JSONL session file format** — a documented, append-only, tree-structured on-disk schema.

Both are ideal cross-language conformance anchors. A third anchor is the `pi-ai` wire schema (`Message`, `AssistantMessageEvent`, `Tool`, `Model`, `Usage`) shared by every layer.

**License terms (they govern the rewrite):** MIT — maximally permissive, no copyleft. A Rust port (clean-room, direct, or derivative) is permitted including commercial/closed redistribution. The **only** obligation is to reproduce the MIT copyright notice + permission text (attributing Mario Zechner) in copies or substantial portions derived from `pi`'s source. A fully independent reimplementation of the *protocols/formats* carries no license obligation (formats/APIs aren't copyrightable), but any copied or closely-translated source keeps the MIT notice. Vendored third-party assets in `pi` (e.g. `marked`, `highlight.js`) carry their own MIT/BSD notices — handle per their terms only if reused. **Action: vendor upstream's `LICENSE` as `licenses/pi-MIT.txt` and attribute it in ours.**

## 2. Goal and guiding constraints

The goal is **not a one-time port** but a **continually-updating mirror**: as upstream `pi` evolves at its high release cadence, atilla tracks it. Two hard constraints shape every decision:

- **C1 — Conformance is defined as passing pi's own test suite.** atilla must be exercised by (a Rust-adapted form of) upstream's existing tests, not only by bespoke tests. A sibling workstream (**"Design: pass pi's own test suite"**) owns that harness design; this plan references it and does not duplicate it. See §6.
- **C2 — Bindings are native extensions per language, decided.** PHP gets a real native extension (ext-php-rs, PECL-style `.so` loaded via `php.ini`), not a C-ABI/FFI-loaded library. Future languages likewise get first-class native extensions (PyO3, napi-rs, magnus, …). See §5.

A corollary of C1 + the mirror goal (C3): **atilla's module structure stays deliberately close to pi's**, so that an upstream diff maps to a tractable atilla diff. We optimise for *diff-portability*, not for the most idiomatic-from-scratch Rust layout.

## 3. Proposed repository / workspace layout

Single Cargo **workspace**. `main` already establishes the workspace with `crates/atilla-core` + `crates/atilla-cli` (root `Cargo.toml` lists them as the two members, edition 2021, MIT); we **extend those existing crates and add new members** rather than inventing a fresh layout. Crates mirror pi's package boundaries so upstream changes localise to the corresponding crate.

```
atilla/                          # repo root (zmaril/atilla)
├── Cargo.toml                   # [workspace] — members below (extends the existing 2-member workspace)
├── crates/
│   ├── atilla-ai/               # ⇔ pi-ai      : wire types, provider abstraction, per-provider APIs (NEW)
│   ├── atilla-agent/            # ⇔ pi-agent   : agent loop, tools, ExecutionEnv, sessions, compaction (NEW)
│   ├── atilla-coding/           # ⇔ pi-coding-agent core: built-in tools, SessionManager, RPC, SDK surface (NEW)
│   ├── atilla-core/             # EXISTS on main — grows into the FACADE crate: the single binding-facing API
│   │                            #   (re-exports the above, in a binding-friendly shape). All native bindings depend ONLY on this.
│   ├── atilla-cli/              # EXISTS on main — ⇔ the `pi` binary: argument parsing + run modes (print, rpc; TUI later)
│   └── atilla-tui/              # ⇔ pi-tui (LATER; terminal UI — deferred, see §4/§8) (NEW)
├── bindings/
│   ├── php/                     # atilla-php  : ext-php-rs native extension (cdylib), PECL/composer packaging
│   ├── python/                  # atilla-py   : PyO3 + maturin (LATER)
│   ├── node/                    # atilla-node : napi-rs (LATER)
│   └── ruby/                    # atilla-rb   : magnus (LATER)
├── conformance/                 # shared, language-agnostic test vectors (JSON) + adapters (see §6)
├── notes/                       # all research/planning/design docs live here
│   ├── ts-to-rust.md            # upstream TS→Rust porting notes (relocated by the transpilation workstream)
│   ├── upstream-tracking.md     # the mirror/diff-porting playbook (see §7)
│   ├── architecture.md          # crate boundaries + the facade contract
│   └── PLAN.md                  # this document
└── licenses/pi-MIT.txt          # upstream MIT notice (attribution)
```

**Why a workspace of layered crates, not one big crate:** the layering (a) matches pi's package split so upstream diffs are localisable, (b) lets `atilla-ai` and `atilla-agent` be published/consumed independently on crates.io, (c) keeps compile units small, and (d) isolates the facade so binding crates never reach into internals.

**Why a single `atilla-core` facade:** every native binding crate (§5) depends *only* on `atilla-core`. This is the mechanism that keeps N binding crates **thin and consistent** — the facade absorbs all the async→sync bridging, handle management, and error-model normalisation once, so each language glue crate is a mechanical translation of the *same* surface. If a language needs a shape the facade doesn't expose, we add it to the facade, not to one binding.

## 4. What we mirror first, and what we defer

Not all 95k lines are equally load-bearing across the binding boundary. Priority follows the two stable boundaries (§1):

**Mirror early (the interoperable core, ~33k upstream LOC of portable logic):**
- `pi-ai` wire types + `Usage`/cost math + the `AssistantMessageEvent` streaming union.
- The version-3 JSONL **session format** (parse, tree-walk, append, migrate) — pure, deterministic, fixture-rich.
- The **agent loop**, tool abstraction, `ExecutionEnv` (already `Result`-returning and Rust-shaped), compaction.
- Built-in **tools** (bash/read/write/edit/grep/find/ls) and the **RPC protocol**.
- Provider APIs, starting with **Anthropic Messages** (best-documented), then OpenAI, Google.

**Defer (terminal- or JS-specific, orthogonal to the binding boundary):**
- The **interactive TUI** (~16.7k upstream LOC; grapheme-width/ANSI-diff contract is a correctness minefield). The RPC boundary lets a host build its own UI.
- The **jiti-based in-process JS extension system** — architecturally the single biggest blocker (§8). v1 has no in-process JS plugins; extensibility is revisited via an out-of-process protocol.
- The **orchestrator** daemon.
- Exotic providers, OAuth loopback flows, Bedrock SigV4 (until a later provider milestone).

## 5. Binding architecture (native extensions — decided)

**Decision (C2): each language gets a first-class native extension, all built on the single `atilla-core` facade.** We are *not* shipping a C-ABI `.so` that every language loads via its FFI; we ship real, idiomatic extensions.

```
          ┌─────────────────────────────────────────────────────┐
          │  atilla-ai   atilla-agent   atilla-coding            │  (layered Rust crates)
          └───────────────────────┬─────────────────────────────┘
                                   │ re-exported, reshaped
                          ┌────────▼─────────┐
                          │   atilla-core    │  ← the ONE facade every binding targets
                          │  (async→sync,    │
                          │   handles, error │
                          │   normalisation) │
                          └───┬───┬───┬───┬──┘
              ext-php-rs ─────┘   │   │   └───── magnus (Ruby)
                 (PHP)           PyO3 napi-rs
                              (Python) (Node)
```

**How the facade keeps binding crates thin and consistent (the core design problem):**
- **One surface, N mechanical translations.** `atilla-core` exposes a deliberately small, binding-friendly API: opaque handle types (`Session`, `AgentSession`, `ModelRuntime`), value types (the wire schema), and functions that return `Result<T, AtillaError>`. Each binding crate maps: handles → the language's resource object (PHP class, Python class, JS class), value types → native structs/arrays/assoc-arrays, `AtillaError` → the language's exception type. That mapping is the *only* code a binding contains.
- **Async is bridged once, in the facade.** pi is streaming-async throughout (`AssistantMessageEventStream`, `EventStream<Event,Result>`). The facade owns a Tokio runtime and exposes each stream **two ways**: (a) a **blocking iterator** (`next_event() -> Option<Event>`) for synchronous hosts like classic PHP, and (b) a **callback/channel** form for hosts with async or event loops (Node). Bindings pick the shape their language wants; neither re-implements the bridge.
- **Callbacks flow inward as registered handles.** pi's config carries 9+ host callbacks (`convertToLlm`, `beforeToolCall`, tool `execute`/`onUpdate`, …), several contractually "must not throw." The facade defines these as Rust traits with a registration API; each binding wraps a language closure and **enforces the non-throwing invariant on the foreign side** (catch → convert to `AtillaError`, never unwind across FFI).
- **Ownership at the boundary is explicit.** Large payloads (base64 images inline in messages, spilled bash output) are copied at the boundary by value; long-lived state lives behind handles with explicit `dispose()`/`Drop`. No borrowed pointers cross the boundary.

**Per-language toolchain:**
| Language | Framework | Package channel | Notes |
|---|---|---|---|
| **PHP** (first) | **ext-php-rs** → `cdylib` | **PECL** + composer metapackage | Real `.so` in `php.ini`. Classic PHP is synchronous → uses the facade's blocking-iterator form. |
| Python | PyO3 + maturin | PyPI wheels (`abi3`) | Blocking API + optional `asyncio` bridge over the callback form. |
| Node | napi-rs | npm prebuilds | Uses the callback/channel form + N-API ThreadsafeFunction; maps naturally to async iterators. |
| Ruby | magnus | RubyGems (precompiled) | Blocking form. |

**Cost we accept (and track):** native extensions add **per-language maintenance cost** — each new facade method must be surfaced in every binding, and each language has its own build/ABI/packaging matrix (PHP's ZTS/NTS × versions, Python abi3, Node N-API versions). We mitigate with (a) the thin-by-construction facade rule, (b) a shared conformance suite every binding must pass (§6), and (c) codegen where practical (a manifest of facade methods → generated binding stubs is an open option, see §9).

**Rejected:** a single C-ABI + per-language FFI wrapper. It would minimise Rust-side work but yields non-idiomatic, hand-marshalled APIs in each language, loses type safety at the boundary, and is exactly what the user ruled out. Native extensions cost more to maintain but deliver first-class ergonomics and are the decided path.

## 6. Conformance: pass pi's own test suite (referenced, not duplicated)

**The bar (C1): atilla must pass pi's own tests.** The dedicated sibling workstream *"Design: pass pi's own test suite"* owns the harness; this plan defers to its output (expected in `notes/` / `conformance/`) and only records how atilla is *built to be testable* by it:

- **Faux-provider parity.** pi ships a scripted, deterministic `fauxProvider` (`ai/src/providers/faux.ts`) that replays predefined content blocks / tool calls / stop reasons through the real event protocol — no API keys. atilla-ai implements a **byte-compatible faux provider** so the same scripted scenarios drive identical agent-loop runs. This is the single most valuable conformance mechanism.
- **Golden vectors from two stable boundaries.** (1) **Session JSONL**: run identical faux-driven scenarios in pi and atilla; diff the resulting version-3 JSONL trees. (2) **RPC**: drive `pi --mode rpc` and `atilla --mode rpc` with identical command sequences; diff the event streams line-for-line. Both are deterministic and language-agnostic.
- **Shared `conformance/` vectors.** Test vectors live as JSON in `conformance/` and are consumed by (a) the Rust core tests and (b) **every native binding's test suite** — so PHP, Python, etc. each prove *identical* observable behaviour, not just that they load. A binding is "done" only when it passes the shared vectors.
- **Adapters over upstream tests.** Where pi's vitest tests encode behaviour we must match (stream decoding, session-manager edge cases, tool semantics, compaction cut-points), the sibling harness defines how those assertions are re-expressed against atilla (via the RPC/JSONL boundary or a thin test shim). We keep atilla's module structure close to pi's (C3) specifically so these test intents map over with minimal translation.

**Open dependency:** the exact re-execution mechanism (port vitest cases → Rust `#[test]` vs. black-box RPC diffing vs. a Node harness driving the atilla binary) is the sibling's call. Flagged in §9.

## 7. Upstream-tracking / mirror strategy

Keeping atilla a *living* mirror of a fast-moving upstream is a first-class concern, not an afterthought.

- **Pin the upstream commit.** `notes/upstream-tracking.md` records the exact upstream commit atilla currently mirrors (start: `3da591ab`, `v0.80.10`). Every port PR that advances the mirror updates this pin. A `UPSTREAM_COMMIT` file at repo root is the machine-readable source of truth.
- **Structural correspondence map.** Maintain a table mapping each upstream file/dir → its atilla crate/module (e.g. `ai/src/types.ts` → `atilla-ai/src/wire.rs`). This is what makes an upstream diff *portable*: given `git diff <old>..<new>` upstream, the map tells you which atilla modules must change.
- **Automated drift detection.** A scheduled CI job (weekly) fetches upstream, computes `git diff <pinned>..upstream/main`, filters to the paths we mirror (via the correspondence map), and opens/updates a tracking issue: "N upstream commits, M touch mirrored paths, here are the diffs." This turns "notice upstream changed" from manual vigilance into a standing signal. (A follow-on could have an agent draft the port PR from that diff.)
- **Diff-portability by construction (C3).** We keep function/module boundaries and even naming close to pi's, and keep the interactive-TUI and JS-extension surfaces *out* of scope, so the mirrored surface is the stable, diff-friendly one. Idiomatic-Rust refactors that would scramble the correspondence map are avoided in mirrored code.
- **Protocol/format versioning.** The RPC protocol and session `version: 3` are the contract. atilla asserts the same `version` constant; when upstream bumps it, that's a high-priority tracked change with its own milestone.
- **Conformance gates the mirror.** A mirror advance is only "landed" when the conformance suite (§6) still passes against the new pinned commit's vectors — so tracking and conformance are the same loop.

## 8. Hardest problems (surfaced early, from the study)

| Risk | Why hard | Plan |
|---|---|---|
| **JS extension system** | Extensions are TypeScript modules run **in-process** via `jiti`, mutating agent control flow through a ~30-event API. A Rust core can't run them. | **Out of scope for v1.** Revisit as an out-of-process extension protocol (the RPC extension-UI sub-protocol is a partial precedent). Biggest single design decision — flagged for the user (§9). |
| **Streaming async iterators** | Everything is `AssistantMessageEventStream` / `EventStream`; ordering (interleaved content-block deltas keyed by `contentIndex`, parallel tool completion order) must be bit-for-bit. | Facade owns the Tokio runtime; exposes blocking-iterator + callback forms; conformance vectors assert exact event ordering. |
| **Provider SDKs** | pi leans on 5 official SDKs; **Bedrock SigV4 + AWS credential chain** is the worst to replicate. | Port raw HTTP+SSE per provider (feasible — pi mostly builds params then re-parses events). Anthropic first; Bedrock late. |
| **TUI grapheme-width contract** | Rendered line width must exactly equal terminal grapheme width or the renderer aborts; JS `get-east-asian-width` vs Rust parity is fragile. | Defer the TUI entirely; target the RPC boundary. Revisit with `unicode-width` + golden render tests only if we rebuild the UI. |
| **`typebox` / `partial-json`** | JSON-Schema tool params + tolerant streaming-JSON parsing of partial tool args. | Rust equivalents exist (`schemars`/`jsonschema`, a tolerant JSON parser); conformance vectors cover partial-parse edge cases. |
| **Native `rg`/`fd` download** | grep/find shell out to ripgrep/fd auto-downloaded from GitHub releases. | Use Rust-native `grep`/`ignore` crates (same authors as ripgrep) — removes the download+subprocess dependency. |

## 9. Milestone roadmap

Small, independently verifiable milestones. Each has a concrete **Done** check. The first is a true vertical slice: one small piece of pi's API working end-to-end from Rust through a PHP native extension.

**M0 — Toolchain skeleton (native path proven).**
Extend the workspace (`atilla-core` facade crate + `bindings/php` ext-php-rs crate). PHP extension exposes one trivial call, e.g. `Atilla::version(): string`.
*Done:* `cargo build -p atilla-php` produces a `.so`; loaded via `php.ini`, a PHP script prints the version; CI builds the extension on Linux for one PHP version.

**M1 — Vertical slice: session-format read, Rust → PHP.**
Implement version-3 JSONL session parsing + tree-walk + "build context messages" in `atilla-agent`, surface it through `atilla-core`, expose in PHP as `Session::open(path)` → message list + stats.
*Done:* given a pi-produced `.jsonl` fixture, atilla (Rust unit test **and** the PHP extension) returns a message list identical to pi's `buildSessionContext` output for the same file; result checked into `conformance/` as the first shared vector, consumed by both the Rust and PHP test suites.

**M2 — Wire schema + cost math.**
Port `pi-ai` `Message`/`AssistantMessage`/`Usage`/`Tool`/`Model` types and `calculateCost` (tiered pricing incl. Anthropic 1h cache) into `atilla-ai`; expose via facade + PHP.
*Done:* shared JSON vectors of (usage, model) → cost match pi exactly across Rust and PHP.

**M3 — Faux provider + agent loop.**
Implement the byte-compatible faux provider and the agent loop + tool execution (bash/read/write/edit) over `ExecutionEnv`, producing session JSONL.
*Done:* a scripted faux scenario run in pi and in atilla yields identical session JSONL trees and identical event sequences (the §6 golden-vector diff passes).

**M4 — RPC mode.**
Implement `atilla --mode rpc` (a subset of commands: prompt/steer/abort/get_state/get_messages/get_session_stats + the event stream).
*Done:* an identical RPC command script produces byte-identical event streams from `pi` and `atilla` for faux-driven runs.

**M5 — First real provider (Anthropic Messages).**
Raw HTTP+SSE Anthropic provider decoded into the `AssistantMessageEvent` union.
*Done:* live smoke test streams a real completion; SSE→event decoding matches pi's decoder on captured-fixture SSE streams (no key needed for the fixture test).

**M6 — Second language proves the facade (Python via PyO3).**
Stand up `bindings/python` over the *same* `atilla-core` facade; it must pass the *same* `conformance/` vectors as PHP.
*Done:* `pip install` the wheel; Python passes M1–M4 shared vectors unchanged — proving the facade keeps bindings thin.

**M7 — Upstream-tracking automation live.**
`UPSTREAM_COMMIT` pin + correspondence map + scheduled drift-detection CI job.
*Done:* the weekly job runs, produces a diff report against a newer upstream commit, and opens a tracking issue.

**M8+ — Breadth:** more providers (OpenAI, Google, Bedrock), more languages (Node/napi-rs, Ruby/magnus), then the deferred surfaces (extension protocol, TUI) as separate epics.

## 10. CI / packaging strategy

- **Rust core → crates.io.** Publish `atilla-ai`, `atilla-agent`, `atilla-coding`, `atilla-core` as versioned crates. `cargo test` + `cargo clippy` + the `conformance/` vectors gate every PR. `cargo-dist` produces prebuilt `atilla` CLI binaries per platform.
- **PHP → PECL + composer.** Build the ext-php-rs `.so` across the PHP support matrix (8.1–8.4, NTS + ZTS) on Linux/macOS/Windows; publish to **PECL**; ship a composer metapackage that pulls the right prebuilt or falls back to source build. Prebuilt binaries attached to GitHub releases.
- **Python → PyPI.** maturin builds `abi3` wheels (manylinux/macos/windows) via cibuildwheel; publish to PyPI.
- **Node → npm.** napi-rs prebuild matrix; publish platform packages + a loader package.
- **Ruby → RubyGems.** magnus + rb-sys precompiled gems.
- **Conformance in CI (the cross-cutting gate):** the shared `conformance/` vectors run in *every* binding's CI job, so no binding ships that diverges from the core. Trusted-publish (OIDC) mirrors pi's own release hardening; pin dependencies, `min-release-age`, lockfiles committed.
- **Upstream-tracking job:** scheduled workflow (§7) opens tracking issues; does not block PRs.

## 11. Risks and open questions for the user

1. **Extension system — the big one.** pi's power comes largely from in-process JS/TS extensions (custom providers, tools, UI, control-flow hooks). A native Rust mirror cannot run them in-process. **Question:** for the mirror, is it acceptable to (a) drop extensibility in v1, (b) design a new *out-of-process* extension protocol, or (c) embed a JS engine (QuickJS/Deno) for extension compatibility? This shapes the whole architecture. *Recommendation: (a) for v1, (b) as the durable answer.*
2. **How literal must "pass pi's own test suite" be?** Re-execute upstream's vitest cases directly (needs a Node harness driving atilla), or match behaviour via black-box RPC/JSONL golden diffs generated *from* those tests? The sibling harness workstream owns this; **the user should confirm the intended strictness** (bit-identical outputs vs. behavioural equivalence).
3. **Interactive TUI — mirror it or not?** It's ~17k LOC of terminal-specific code with a fragile grapheme-width contract, orthogonal to bindings. *Recommendation: defer; expose RPC and let hosts build UIs.* Confirm this is acceptable for the product vision.
4. **Provider breadth vs. depth.** ~40 providers upstream. Which handful must the mirror support first? *Recommendation: Anthropic → OpenAI → Google; Bedrock later due to SigV4.*
5. **Language priority after PHP.** Plan assumes Python next (best proves the facade). Confirm, or reprioritise (Node? Ruby?).
6. **Binding codegen.** Should we invest early in generating binding stubs from a facade-method manifest to cut per-language maintenance, or hand-write bindings until the surface stabilises? *Recommendation: hand-write through M6, then evaluate codegen.*
7. **Upstream TS→Rust porting notes.** The upstream TS→Rust porting notes are being relocated into `notes/` (as `notes/ts-to-rust.md`) by the transpilation workstream; reconcile this plan with them once landed.
8. **Two upstream session-tree implementations** (`agent-core` `Session` vs `coding-agent` `SessionManager`) both claim version-3; field-for-field parity is unverified. We'll diff them before treating them as one schema for shared vectors.
