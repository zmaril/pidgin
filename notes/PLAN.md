# atilla — Plan: a continually-updating Rust mirror of `pi`, with native language bindings

> Status: planning. This document is decision-ready but describes work not yet started.
> Upstream: [`earendil-works/pi`](https://github.com/earendil-works/pi) — MIT, studied at commit `3da591ab` (pkg version `0.80.10`).

## 1. What `pi` is (and what we are mirroring)

`pi` is an open-source, self-extendable **AI coding agent** (a Claude-Code / opencode-style CLI) plus the reusable libraries beneath it. It is a **TypeScript / Node.js ESM monorepo** of five npm packages totalling ~95.5k lines of non-test source (plus ~83k lines of tests), MIT-licensed, authored by Mario Zechner, under highly active development (4,986 commits, 303 release tags at time of study).

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
2. **The version-3 JSONL session file format** — a documented, append-only, tree-structured on-disk schema. atilla mirrors it **byte-for-byte**: an exact mirror of version-3 (same tree shape, field names, and ordering), not a re-designed equivalent.

Both are ideal cross-language conformance anchors. A third anchor is the `pi-ai` wire schema (`Message`, `AssistantMessageEvent`, `Tool`, `Model`, `Usage`) shared by every layer.

**License terms (they govern the rewrite):** MIT — maximally permissive, no copyleft. A Rust port (clean-room, direct, or derivative) is permitted including commercial/closed redistribution. The **only** obligation is to reproduce the MIT copyright notice + permission text (attributing Mario Zechner) in copies or substantial portions derived from `pi`'s source. A fully independent reimplementation of the *protocols/formats* carries no license obligation (formats/APIs aren't copyrightable), but any copied or closely-translated source keeps the MIT notice. Vendored third-party assets in `pi` (e.g. `marked`, `highlight.js`) carry their own MIT/BSD notices — handle per their terms only if reused. **Action: vendor upstream's `LICENSE` as `licenses/pi-MIT.txt` and attribute it in ours.**

## Prior art and related work

Roughly ten prior pi-to-Rust ports already exist; none is a model we build on. The full survey is [`notes/prior-art.md`](notes/prior-art.md) (PR #4) — read it before writing code. Three findings feed this plan. First, mature and permissively-licensed Rust building blocks are worth reusing rather than re-inventing: **[`codex-rs`](https://github.com/openai/codex)** (Apache-2.0) is a solid architecture reference for a Rust agent CLI. (**[`rust-genai`](https://github.com/jeremychone/rust-genai)** was weighed for the provider layer and rejected in favour of hand-rolled thin per-provider clients — see §4, §8, M5, and [`notes/communications.md`](notes/communications.md).) Second, ports that pin a specific upstream version — **[`c4pt0r/pie`](https://github.com/c4pt0r/pie)** and **[`nktkt/pi`](https://github.com/nktkt/pi)** — are precedent for the version-pinning mirror strategy (§7). Third, upstream has no Rust plans (a "rewrite in Rust" issue was opened and closed as a joke), so atilla is an independent effort.

## 2. Goal and guiding constraints

The goal is **not a one-time port** but a **continually-updating mirror** that exposes `pi` as **native extensions in as many host languages as possible** while tracking upstream at its high release cadence. **PHP is the first target, and Node compatibility is maintained as a first-class target** — not a later add-on. Two hard constraints shape every decision:

- **C1 — Conformance is defined as passing pi's own tests, literally.** pi's existing unit-test suites run **unmodified** against napi-rs Node packages that present pi's exact module surface (backed by the Rust core), and pi's black-box CLI tests run against the `atilla` binary. Bespoke tests supplement but never replace this bar. See §6.
- **C2 — Bindings are native extensions per language, decided.** PHP gets a real native extension (ext-php-rs, a `.so` loaded via `php.ini`), not a C-ABI/FFI-loaded library, and Node is maintained as a first-class native extension (napi-rs). Further languages likewise get first-class native extensions (PyO3, magnus, …). See §5.

A corollary of C1 + the mirror goal (C3): **atilla's module structure stays deliberately close to pi's**, so that an upstream diff maps to a tractable atilla diff. We optimise for *diff-portability*, not for the most idiomatic-from-scratch Rust layout.

**Runtime and UI choices (decided, our own call).** atilla uses **tokio** for the async runtime and, if and when a terminal UI is built, **ratatui** with **crossterm** — deliberately not a hand-rolled async runtime or renderer. This keeps the maintenance surface small and stays on the mainstream Rust agent stack.

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
│   └── atilla-tui/              # ⇔ pi-tui (terminal UI — tracked as an open sibling workstream, see §4/§8) (NEW)
├── bindings/
│   ├── php/                     # atilla-php  : ext-php-rs native extension (cdylib), PECL/composer packaging
│   ├── node/                    # atilla-node : napi-rs (first-class, with PHP)
│   ├── python/                  # atilla-py   : PyO3 + maturin (LATER)
│   └── ruby/                    # atilla-rb   : magnus (LATER)
├── conformance/                 # shared, language-agnostic test vectors (JSON) + adapters (see §6)
├── notes/                       # all research/planning/design docs live here
│   ├── ts-to-rust.md            # upstream TS→Rust porting notes (relocated by the transpilation workstream)
│   ├── upstream-tracking.md     # the mirror/diff-porting playbook (see §7)
│   ├── architecture.md          # crate boundaries + the façade contract
│   └── PLAN.md                  # this document
└── licenses/pi-MIT.txt          # upstream MIT notice (attribution)
```

**Why a workspace of layered crates, not one big crate:** the layering matches pi's package split so upstream diffs are localisable; lets `atilla-ai` and `atilla-agent` be published and consumed independently on crates.io; keeps compile units small; and isolates the façade so binding crates never reach into internals.

**Why a single `atilla-core` façade:** every native binding crate (§5) depends *only* on `atilla-core`. This is the mechanism that keeps N binding crates **thin and consistent** — the façade absorbs all the async→sync bridging, handle management, and error-model normalisation once, so each language glue crate is a mechanical translation of the *same* surface. If a language needs a shape the façade doesn't expose, we add it to the façade, not to one binding.

## 4. What we mirror first, and what we defer

Not all 95k lines are equally load-bearing across the binding boundary. Priority follows the two stable boundaries (§1):

**Mirror early (the interoperable core, ~33k upstream LOC of portable logic):**
- `pi-ai` wire types + `Usage`/cost math + the `AssistantMessageEvent` streaming union.
- The version-3 JSONL **session format** (parse, tree-walk, append, migrate) — an **exact byte-compatible mirror** of pi's on-disk format, pure, deterministic, fixture-rich.
- The **agent loop**, tool abstraction, `ExecutionEnv` (already `Result`-returning and Rust-shaped), compaction.
- Built-in **tools** (bash/read/write/edit/grep/find/ls) and the **RPC protocol**.
- The **JS/TS extension plane** — pi's own TypeScript extensions run inside atilla on an **embedded `deno_core`** runtime (§8). Passing pi's extension tests is a hard requirement (§6), so this is in scope, not deferred.
- Provider APIs — **hand-rolled thin per-provider HTTP + SSE clients** (`reqwest` + `eventsource-stream`), one per wire dialect, owning our own wire rather than inheriting a third-party provider model across the FFI boundary (rationale in [`notes/communications.md`](notes/communications.md)). Start with **Anthropic Messages** (best-documented), then OpenAI, Google, Mistral, Bedrock (§8).

**No MCP.** pi ships **no MCP integration**; atilla adds none. We mirror upstream's surface exactly and do not introduce Model Context Protocol support (confirmed in [`notes/communications.md`](notes/communications.md)).

**Defer (terminal-specific or orthogonal to the binding boundary):**
- The **interactive TUI** (~16.7k upstream LOC; grapheme-width/ANSI-diff contract is a correctness minefield) — an **open sibling workstream** is investigating a ratatui recreation (§8); this plan targets the RPC boundary so a host can build its own UI in the meantime.
- The **orchestrator** daemon.
- Exotic providers, OAuth loopback flows, Bedrock SigV4 (until a later provider milestone).

## 5. Binding architecture (native extensions — decided)

**Decision (C2): each language gets a first-class native extension, all built on the single `atilla-core` façade.** We are *not* shipping a C-ABI `.so` that every language loads via its FFI; we ship real, idiomatic extensions.

```
          ┌─────────────────────────────────────────────────────┐
          │  atilla-ai   atilla-agent   atilla-coding            │  (layered Rust crates)
          └───────────────────────┬─────────────────────────────┘
                                   │ re-exported, reshaped
                          ┌────────▼─────────┐
                          │   atilla-core    │  ← the ONE façade every binding targets
                          │  (async→sync,    │
                          │   handles, error │
                          │   normalisation) │
                          └───┬───┬───┬───┬──┘
              ext-php-rs ─────┘   │   │   └───── magnus (Ruby)
                 (PHP)           PyO3 napi-rs
                              (Python) (Node)
```

**How the façade keeps binding crates thin and consistent (the core design problem):**
- **One surface, N mechanical translations.** `atilla-core` exposes a deliberately small, binding-friendly API: opaque handle types (`Session`, `AgentSession`, `ModelRuntime`), value types (the wire schema), and functions that return `Result<T, AtillaError>`. Each binding crate maps: handles → the language's resource object (PHP class, Python class, JS class), value types → native structs/arrays/assoc-arrays, `AtillaError` → the language's exception type. That mapping is the *only* code a binding contains.
- **Async is bridged once, in the façade.** pi is streaming-async throughout (`AssistantMessageEventStream`, `EventStream<Event,Result>`). The façade owns a Tokio runtime and exposes each stream **two ways**: (a) a **blocking iterator** (`next_event() -> Option<Event>`) for synchronous hosts like classic PHP, and (b) a **callback/channel** form for hosts with async or event loops (Node). Bindings pick the shape their language wants; neither re-implements the bridge.
- **Callbacks flow inward as registered handles.** pi's config carries 9+ host callbacks (`convertToLlm`, `beforeToolCall`, tool `execute`/`onUpdate`, …), several contractually "must not throw." The façade defines these as Rust traits with a registration API; each binding wraps a language closure and **enforces the non-throwing invariant on the foreign side** (catch → convert to `AtillaError`, never unwind across FFI).
- **Ownership at the boundary is explicit.** Large payloads (base64 images inline in messages, spilled bash output) are copied at the boundary by value; long-lived state lives behind handles with explicit `dispose()`/`Drop`. No borrowed pointers cross the boundary.

**Per-language toolchain:**
| Language | Framework | Package channel | Notes |
|---|---|---|---|
| **PHP** (first) | **ext-php-rs** → `cdylib` | **PECL** + composer metapackage | Real `.so` in `php.ini`. Classic PHP is synchronous → uses the façade's blocking-iterator form. |
| Python | PyO3 + maturin | PyPI wheels (`abi3`) | Blocking API + optional `asyncio` bridge over the callback form. |
| **Node** (first-class, with PHP) | napi-rs | npm packages | Uses the callback/channel form + N-API ThreadsafeFunction; maps naturally to async iterators. Also the natural host for pi's JS/TS extension conformance. |
| Ruby | magnus | RubyGems (precompiled) | Blocking form. |

**Cost we accept (and track):** native extensions add **per-language maintenance cost** — each new façade method must be surfaced in every binding, and each language has its own build/ABI/packaging matrix (PHP's ZTS/NTS × versions, Python abi3, Node N-API versions). We mitigate with the thin-by-construction façade rule, a shared conformance suite every binding must pass (§6), and codegen where practical (a manifest of façade methods → generated binding stubs is an open option, see §11).

**Rejected:** a single C-ABI + per-language FFI wrapper. It would minimise Rust-side work but yields non-idiomatic, hand-marshalled APIs in each language, loses type safety at the boundary, and is exactly what the user ruled out. Native extensions cost more to maintain but deliver first-class ergonomics and are the decided path.

## 6. Conformance: pass pi's own tests (the literal bar)

**The bar (C1): pi's own tests pass, unmodified, against atilla.** This is a concrete mechanism, not an aspiration:

- **Drop-in Node packages.** atilla compiles **Node packages that present pi's exact TypeScript module surface** — napi-rs packages that re-export pi's public API shape, backed by the Rust core. pi's existing **unit-test suites run unmodified against these packages**, as a drop-in replacement for the real `@earendil-works/pi-*` packages. This is the literal conformance bar: pi's own tests must pass against the Rust-backed packages, extension tests included.
- **The Node bridge does double duty.** This napi-rs Node bridge is a **deliverable in its own right** — Node is a first-class target language (§2, §5) — so a shipping binding and the conformance harness are the same artifact.
- **Black-box CLI tests repointed.** pi's **4 black-box CLI / end-to-end tests** are repointed to run against the Rust **`atilla` binary** in place of the Node `pi` binary.
- **Byte-compatible faux provider.** pi ships a scripted, deterministic `fauxProvider` (`ai/src/providers/faux.ts`) that replays predefined content blocks / tool calls / stop reasons through the real event protocol — no API keys. atilla-ai implements a **byte-compatible faux provider**, so the same scripted scenarios drive identical agent-loop runs and pi's tests pass deterministically.
- **Exact-mirror session format.** atilla's session file format is a **byte-compatible exact mirror** of pi's version-3 JSONL — same tree shape, field names, and ordering — not a re-designed equivalent (§1). Golden vectors run identical faux-driven scenarios in pi and atilla and diff the resulting version-3 JSONL trees.
- **Shared `conformance/` vectors for the non-Node bindings.** The Node bridge carries the primary bar; a complementary, language-agnostic layer keeps JSON test vectors in `conformance/`, consumed by every non-Node binding's test suite (PHP, later Python and Ruby) — so each proves *identical* observable behaviour, not just that it loads. A binding is "done" only when it passes the shared vectors (and, where the language hosts them, pi's own tests). We keep atilla's module structure close to pi's (C3) so these test intents map over with minimal translation.

## 7. Upstream-tracking / mirror strategy

Keeping atilla a *living* mirror of a fast-moving upstream is a first-class concern, not an afterthought.

- **Pin the upstream commit.** `notes/upstream-tracking.md` records the exact upstream commit atilla currently mirrors (start: `3da591ab`, `v0.80.10`). Every port PR that advances the mirror updates this pin. A `UPSTREAM_COMMIT` file at repo root is the machine-readable source of truth. Prior ports `c4pt0r/pie` and `nktkt/pi` pin a specific upstream version too — precedent for this approach.
- **Structural correspondence map.** Maintain a table mapping each upstream file/dir → its atilla crate/module (e.g. `ai/src/types.ts` → `atilla-ai/src/wire.rs`). This is what makes an upstream diff *portable*: given `git diff <old>..<new>` upstream, the map tells you which atilla modules must change.
- **Automated drift detection.** A scheduled CI job (weekly) fetches upstream, computes `git diff <pinned>..upstream/main`, filters to the paths we mirror (via the correspondence map), and opens/updates a tracking issue: "N upstream commits, M touch mirrored paths, here are the diffs." This turns "notice upstream changed" from manual vigilance into a standing signal. (A follow-on could have an agent draft the port PR from that diff.)
- **Diff-portability by construction (C3).** We keep function/module boundaries and even naming close to pi's, and keep the interactive-TUI surface out of the ported set — the JS/TS extension surface runs on the embedded `deno_core` plane (§8) rather than being ported line by line — so the mirrored surface is the stable, diff-friendly one. Idiomatic-Rust refactors that would scramble the correspondence map are avoided in mirrored code.
- **Protocol/format versioning.** The RPC protocol and session `version: 3` are the contract. atilla asserts the same `version` constant; when upstream bumps it, that's a high-priority tracked change with its own milestone.
- **Conformance gates the mirror.** A mirror advance is only "landed" when the conformance suite (§6) still passes against the new pinned commit's vectors — so tracking and conformance are the same loop.

## 8. Hardest problems (surfaced early, from the study)

| Risk | Why hard | Plan |
|---|---|---|
| **JS/TS extension system** | Extensions are TypeScript modules run **in-process** via `jiti`, mutating agent control flow through a ~30-event API. | **Decided: embed `deno_core`** as the JS/TS compatibility plane so pi's own extensions run inside atilla. **Passing pi's extension tests is a hard requirement (§6).** A separate sibling session researches how deep these in-process hooks reach across host languages (how they map when the host is PHP/Python rather than Node), feeding the extension design. The open part here is scoping and sequencing the ~30-event surface (§11). |
| **Streaming async iterators** | Everything is `AssistantMessageEventStream` / `EventStream`; ordering (interleaved content-block deltas keyed by `contentIndex`, parallel tool completion order) must be bit-for-bit. | Façade owns the Tokio runtime; exposes blocking-iterator + callback forms; conformance vectors assert exact event ordering. |
| **Provider SDKs** | pi leans on 5 official SDKs; **Bedrock SigV4 + AWS credential chain** is the worst to replicate. | **Hand-roll thin per-provider clients**: one `reqwest` + `eventsource-stream` driver per wire dialect (`anthropic-messages`, `openai-completions`/`-responses`, `google-generative-ai`, `mistral-conversations`, `bedrock-converse-stream`), each decoding into pi's `AssistantMessageEvent` union. Own the wire; don't inherit a third-party provider model across the FFI boundary ([`notes/communications.md`](notes/communications.md)). Anthropic first; Bedrock SigV4 late. `codex-rs` (Apache-2.0) is an architecture reference. |
| **TUI grapheme-width contract** | Rendered line width must exactly equal terminal grapheme width or the renderer aborts; JS `get-east-asian-width` vs Rust parity is fragile. | The TUI is an **open sibling workstream** — a separate session is investigating recreating pi's TUI with **ratatui** — referenced here, not owned by this plan. The grapheme-width contract (`get-east-asian-width` vs Rust `unicode-width` parity, with golden render tests) is the hard part that workstream carries. This plan targets the RPC boundary so hosts can build their own UI regardless. |
| **`typebox` / `partial-json`** | JSON-Schema tool params + tolerant streaming-JSON parsing of partial tool args. | Rust equivalents exist (`schemars`/`jsonschema`, a tolerant JSON parser); conformance vectors cover partial-parse edge cases. |
| **Native `rg`/`fd` download** | grep/find shell out to ripgrep/fd auto-downloaded from GitHub releases. | Use Rust-native `grep`/`ignore` crates (same authors as ripgrep) — removes the download+subprocess dependency. |

## 9. Milestone roadmap

Small, independently verifiable milestones. Each has a concrete **Done** check. The first is a true vertical slice: one small piece of pi's API working end-to-end from Rust through a PHP native extension.

**M0 — Toolchain skeleton (native path proven).**
Extend the workspace (`atilla-core` façade crate + `bindings/php` ext-php-rs crate). PHP extension exposes one trivial call, e.g. `Atilla::version(): string`.
*Done:* `cargo build -p atilla-php` produces a `.so`; loaded via `php.ini`, a PHP script prints the version; CI builds the extension on Linux for one PHP version.

**M1 — Vertical slice: session-format read, Rust → PHP.**
Implement version-3 JSONL session parsing + tree-walk + "build context messages" in `atilla-agent`, surface it through `atilla-core`, expose in PHP as `Session::open(path)` → message list + stats.
*Done:* given a pi-produced `.jsonl` fixture, atilla (Rust unit test **and** the PHP extension) returns a message list identical to pi's `buildSessionContext` output for the same file; result checked into `conformance/` as the first shared vector, consumed by both the Rust and PHP test suites.

**M2 — Wire schema + cost math.**
Port `pi-ai` `Message`/`AssistantMessage`/`Usage`/`Tool`/`Model` types and `calculateCost` (tiered pricing incl. Anthropic 1h cache) into `atilla-ai`; expose via façade + PHP.
*Done:* shared JSON vectors of (usage, model) → cost match pi exactly across Rust and PHP.

**M3 — Faux provider + agent loop.**
Implement the byte-compatible faux provider and the agent loop + tool execution (bash/read/write/edit) over `ExecutionEnv`, producing session JSONL.
*Done:* a scripted faux scenario run in pi and in atilla yields identical session JSONL trees and identical event sequences (the §6 golden-vector diff passes).

**M4 — RPC mode.**
Implement `atilla --mode rpc` (a subset of commands: prompt/steer/abort/get_state/get_messages/get_session_stats + the event stream).
*Done:* an identical RPC command script produces byte-identical event streams from `pi` and `atilla` for faux-driven runs.

**M5 — First real provider (Anthropic Messages).**
Hand-roll a thin `reqwest` + `eventsource-stream` Anthropic `messages` driver that decodes SSE into pi's `AssistantMessageEvent` union — the pattern every later provider follows ([`notes/communications.md`](notes/communications.md)).
*Done:* live smoke test streams a real completion; SSE→event decoding matches pi's decoder on captured-fixture SSE streams (no key needed for the fixture test).

**M6 — Extension plane (embedded `deno_core`).**
Embed `deno_core` so pi's own TypeScript extensions run inside atilla, driving the agent through the ~30-event extension API.
*Done:* pi's own extension test suite passes against atilla's embedded `deno_core` plane — the §6 hard requirement.

**M7 — Node binding (napi-rs), first-class alongside PHP, and the primary conformance harness.**
Stand up `bindings/node` over the *same* `atilla-core` façade (callback/async-iterator form), shaped as napi-rs packages that present pi's exact module surface as a drop-in for `@earendil-works/pi-*`. It must pass the *same* `conformance/` vectors as PHP **and** run pi's own unit-test suites unmodified (§6).
*Done:* Node passes M1–M4 shared vectors unchanged, and pi's own unit tests pass against the Rust-backed packages — proving the façade keeps bindings thin, that Node is a maintained first-class target, and that the literal conformance bar (§6) holds.

**M8 — Upstream-tracking automation live.**
`UPSTREAM_COMMIT` pin + correspondence map + scheduled drift-detection CI job.
*Done:* the weekly job runs, produces a diff report against a newer upstream commit, and opens a tracking issue.

**M9+ — Breadth:** more languages (Python/PyO3, Ruby/magnus), more providers (OpenAI, Google, Mistral, Bedrock). The TUI is not sequenced here — it's an open sibling workstream (ratatui recreation, §8).

## 10. CI strategy (correctness)

- **Rust core.** `cargo fmt` + `cargo clippy -D warnings` + `cargo test` + the shared `conformance/` vectors gate every PR.
- **Conformance in CI (the cross-cutting gate):** the shared `conformance/` vectors run in *every* binding's CI job, so no binding ships that diverges from the core. A binding is "done" only when it passes them.
- **Prose.** `vale` lints the planning and design docs; PR titles follow conventional commits.
- **Upstream-tracking job:** scheduled workflow (§7) opens tracking issues; does not block PRs.
- **Distribution and packaging is deferred.** Release channels and the prebuilt-artifact matrix (PECL and prebuilt `.so`, Python wheels, npm packages, Ruby gems, `cargo-dist` CLI binaries, trusted-publish/OIDC) are future work, noted here and planned once the core and first bindings are proven.

## 11. Risks and open questions for the user

1. **Extension system — deno_core confirmed, two open threads.** pi's power comes largely from in-process JS/TS extensions (custom providers, tools, UI, control-flow hooks). **Decision:** embed `deno_core` as the JS/TS compatibility plane so pi's own extensions run inside atilla, and **passing pi's extension tests is a hard requirement** (§6, §8). Open: (a) how much of the ~30-event extension API to cover in the first cut and how to sequence it against provider and RPC work; (b) **how deep these in-process hooks reach across host languages** — a separate sibling session is researching how pi's hooks map when the host is PHP/Python rather than Node, feeding the extension design.
2. **Interactive TUI — ownership and timing.** An **open sibling workstream** is investigating a ratatui recreation of pi's TUI (§8); it is referenced here, not owned by this plan. Open: whether and when atilla should absorb that work, given the fragile grapheme-width contract, or keep exposing only RPC and let hosts build their own UIs.
3. **Provider breadth vs. depth.** ~40 providers upstream, each a hand-rolled thin client (decided, §8). Which handful must the mirror support first? *Recommendation: Anthropic → OpenAI → Google → Mistral; Bedrock later due to SigV4.*
4. **Language priority.** PHP is first and Node is maintained first-class alongside it (§2, M7); Python and Ruby follow (M9+). Confirm this ordering, or reprioritise.
5. **Binding codegen.** Should we invest early in generating binding stubs from a façade-method manifest to cut per-language maintenance, or hand-write bindings until the surface stabilises? *Recommendation: hand-write through M7, then evaluate codegen.*
6. **Upstream TS→Rust porting notes.** The upstream TS→Rust porting notes are being relocated into `notes/` (as `notes/ts-to-rust.md`) by the transpilation workstream; reconcile this plan with them once landed.
7. **Two upstream session-tree implementations** (`agent-core` `Session` vs `coding-agent` `SessionManager`) both claim version-3; field-for-field parity is unverified. We'll diff them before treating them as one schema for shared vectors.
