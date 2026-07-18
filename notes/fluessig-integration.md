# Fluessig integration plan

This note scopes connecting
[`fluessig`](https://github.com/zmaril/fluessig) and atilla. The direction
is decided: **A, fluessig generates atilla's bindings.** This note records
that decision, the concrete work it implies on each side, the milestone
that gates it, and the questions still open. It is grounded in fluessig's
code and commit history, not its README, which is stale (see section 1a).
Where this note and `design.md` disagree, `design.md` wins.

The starting premise needed correcting first. fluessig is **not** a PHP
application, so there is no `php.ini`, no `.so` to load, and no NTS/ZTS or
PHP-version match to worry about at fluessig's edge. fluessig is a Rust
plus Node build-time schema and code-generation tool with no runtime. The
integration is therefore not extension-loading; it is teaching fluessig to
generate the binding layer that atilla writes by hand.

---

## 1. What fluessig actually is

fluessig is a build-time schema tool (Rust, edition 2021, MSRV 1.75). You
describe a typed entity graph once, and it projects that model everywhere:
SQL DDL, ORM models, and language bindings for Node (napi), Python (PyO3),
and Ruby (Magnus). The catalog contract (`catalog.json` plus `api.json`)
is the stable middle; the back ends read it.

The front end is mid-pivot. Historically the model was authored in
TypeSpec (`.tsp`) through a Node emitter. That is being retired. A Rust
derive front end (`crates/fluessig-derive`,
`crates/fluessig-derive-macros`, with `#[derive(Entity)]`,
`#[derive(Edge)]`, and a `catalog!` macro) is being built in slices to
replace TypeSpec outright. The recorded plan in
`notes/derive-front-end-decisions.md` is Rust-first and exclusively so,
with TypeSpec and Node removed from the toolchain once consumers port
over. The catalog contract and every back end stay unchanged; only the
front end moves.

Facts that matter for integration:

- No runtime. No Dockerfile, no server, no HTTP client, no LLM SDK, no
  async runtime, no queue. It runs at other projects' codegen time and is
  pinned by git ref. On this point the README is accurate.
- It is Rust at the core. atilla's façade crate `atilla-core` is also
  Rust, so a Rust-to-Rust link needs no FFI binding at all. And with the
  front end going Rust-first, describing a model is itself becoming a
  Rust-derive exercise rather than a TypeSpec one.
- It already generates an agent-facing seam. `src/bindgen/mcp.rs` projects
  the op layer (`api.json`) into an MCP tool surface plus a generated Rust
  `dispatch()` module, wired into the CLI via `--mcp` and covered by
  `tests/mcp.rs`. Op shapes are `ctor | unary | stream | manual`.
- It has no PHP back-end. The back ends present are node, python, and ruby
  (plus the MCP projection); there is no `src/bindgen/php.rs`, and no
  "php" token anywhere in the tree. atilla is PHP-first, so this gap is
  real and is the critical-path change (section 3).
- Its consumers are [`entl`](https://github.com/zmaril/entl) (the
  committed fixture) and `disponent` (a second consumer named in code and
  notes).

---

## 1a. What the README says versus what the code shows

The README predates recent work and understates the project. The
divergences, each confirmed against the code:

- The README presents the front end as TypeSpec-only. The code is
  mid-pivot to a Rust derive front end meant to replace TypeSpec
  (`crates/fluessig-derive*`, with design notes dated after the README).
- The README does not mention MCP or agent tooling. `src/bindgen/mcp.rs`
  is a real, CLI-wired, tested MCP tool-surface generator.
- The README names entl as the sole consumer. `disponent` is a second one
  in code and notes.
- The README frames fluessig as a general, language-agnostic schema tool.
  The notes retire that positioning for Rust-first, exclusively.
- No divergence on two points this plan leans on: fluessig is genuinely
  build-time only (no runtime, server, or LLM dependency), and there is
  genuinely no PHP back-end.

---

## 2. Why direction A

Both projects are built on "one Rust core, exposed as native extensions
per language." The difference is who writes the binding layer: atilla
hand-writes it (`bindings/php` via ext-php-rs, `crates/atilla-napi` via
napi-rs), and fluessig generates it from a schema. Direction A puts those
together: atilla describes its façade surface once and fluessig emits the
per-language bindings, so the ext-php-rs and napi glue stop being
hand-maintained.

The alternative, B (atilla as an agent driving a fluessig-described engine
such as entl or disponent), is not the path chosen. It is recorded only so
the decision is legible: B would need atilla's agent loop (M3) and tool
plane (M6) plus an in-process bridge, since atilla has no MCP client by
design. A is a build-time codegen relationship and can start against
today's surface.

---

## 3. Direction A: the concrete work

On the fluessig side, the changes this needs:

1. Add a PHP back-end. fluessig has `src/bindgen/{node,python,ruby}.rs`
   and no `php.rs`. A new `src/bindgen/php.rs` (ext-php-rs templates), a
   `php` language slug, and PHP type-map entries are the critical-path
   change, because atilla is PHP-first.
2. Cover atilla's op shapes. fluessig's op layer (`api.json`, the `Shape`
   enum `ctor | unary | stream | manual`) is entity and data-model
   centric. atilla's façade is behavioral: `version()` is a plain unary
   call, `Session::open` returns an opaque handle (a `ctor`-shaped op),
   and agent runs emit a streaming event union (a `stream`-shaped op).
   Confirming, and where needed extending, the shape model and type map to
   carry opaque handles and event-union streams is the main design risk
   this direction has to retire.

On the atilla side:

1. Provide a describable surface. Today the façade is only
   `atilla_core::version()`; `Session::open` and the agent loop are not
   built yet, so the surface fluessig would generate from grows with
   atilla's milestones (section 4).
2. Choose the source of truth. Because fluessig's front end is going
   Rust-first, the natural model is to annotate the façade types in
   `atilla-core` with fluessig derives, or to keep a small schema crate
   that describes them. Either couples atilla to a pinned fluessig ref;
   pick deliberately.
3. Retire the hand-written bindings as generation takes over. `bindings/php`
   and `crates/atilla-napi` become generated output. The napi binding is
   also atilla's conformance harness (it fronts pi's test suite), so a
   generated napi surface must stay a drop-in that keeps pi's tests
   passing; the swap cannot regress conformance.

---

## 4. Milestone gate and a sequencing that de-risks A

The link itself is buildable today, and the payoff grows with atilla's
façade:

| Step | atilla surface | fluessig work | Gated at |
| --- | --- | --- | --- |
| Regenerate today's `Atilla::version()` from a schema, byte-comparable to the hand-written binding | `atilla_core::version()` | `src/bindgen/php.rs` MVP | M0 (today) |
| First non-trivial generated binding | `Session::open(path)` -> messages plus stats | handle plus struct lowering | M1 |
| Generate over the agent surface | agent loop, event stream | stream-shape lowering | M3 |
| Replace the hand-written napi harness with generated napi | napi conformance surface | node back-end parity | M7 |

Recommended first move, doable now: build the PHP back-end far enough to
regenerate the existing M0 `Atilla::version()` binding and diff it against
the hand-written one. That proves the whole direction end-to-end against a
trivial surface before atilla's API grows, and it is the concrete answer
to "what needs to happen with fluessig to enable atilla": a `php.rs` back
end is step one.

---

## 5. What atilla exposes today versus what is needed

Today (M0, merged): `atilla_core::version()` and PHP `Atilla::version():
string` via ext-php-rs `=0.13.1` targeting PHP 8.4 NTS. `Session::open`
exists only as an M1 placeholder marker in `bindings/php/src/lib.rs`; the
mirror crates (`atilla-agent`, `atilla-ai`, `atilla-coding`) are empty
scaffolds.

So any generated binding beyond a version call is blocked on atilla
roadmap work, not on fluessig. The dependency list is short and
milestone-shaped: M1 for sessions, M3 for the agent surface, M7 for the
napi harness swap.

---

## 6. Open questions now that A is chosen

1. Source of truth: does atilla describe its façade with fluessig derives
   inside `atilla-core`, or in a separate schema crate? The first is
   tighter but couples the core to a fluessig ref.
2. Op-model fit: can fluessig's `ctor | unary | stream | manual` shapes
   and type map carry atilla's opaque session handles and streaming event
   unions as they stand, or does the op model need extending first? This
   is the key design risk.
3. Ownership: the PHP back-end lives in the fluessig repo. Does atilla
   drive that work upstream in fluessig, and on whose milestone?
4. Harness swap: `atilla-napi` is the conformance harness. At which
   milestone does generated napi replace the hand-written harness without
   regressing pi's test suite, before or after M7?
