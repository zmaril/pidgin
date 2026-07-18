# Fluessig integration plan

This note scopes what it would take to connect
[`fluessig`](https://github.com/zmaril/fluessig) and atilla. It is a
figuring-out document, not an implementation: it proposes paths and lists
dependencies. Where this note and `design.md` disagree, `design.md` wins.
This revision is grounded in fluessig's code and commit history rather
than its README; the README is stale and, as section 1a records,
understates the project.

The headline finding corrects the starting premise. fluessig is **not** a
PHP application, so there is no `php.ini`, no `.so` to load, and no
NTS/ZTS or PHP-version match to worry about. fluessig is a Rust plus Node
build-time schema and code-generation tool with no runtime and no
deployment. That reshapes the whole question: "enable atilla in fluessig"
is not an extension-loading problem, it is a question of how two projects
that both follow the same "one Rust core, native bindings per language"
shape should meet.

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
  `dispatch()` module (tool name plus JSON args -> trait call), wired into
  the CLI via `--mcp` and covered by `tests/mcp.rs`. Op shapes are
  `ctor | unary | stream | manual`.
- It has no PHP back-end. The back ends present are node, python, and ruby
  (plus the MCP projection); there is no `src/bindgen/php.rs`, and no
  "php" token anywhere in the tree. atilla is PHP-first, so this gap is
  real.
- Its consumers are [`entl`](https://github.com/zmaril/entl) (the
  committed fixture) and `disponent` (a second consumer named in code and
  notes).

---

## 1a. What the README says versus what the code shows

The README predates recent work and understates the project. The
divergences that matter here, each confirmed against the code:

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

Recording the gap is itself useful: the README understates rather than
overstates, so the integration surface is larger than the README implies
(an MCP seam and a Rust-first front end both help), while the two
constraints this plan depends on hold firm.

---

## 2. The two projects share one shape

Both atilla and fluessig are built on "one Rust core, exposed as native
extensions per language." The difference is who writes the binding layer:
atilla hand-writes it (`bindings/php` via ext-php-rs, `crates/atilla-napi`
via napi-rs), and fluessig generates it from a schema. That overlap is the
whole reason the two projects can meet, and it opens two genuinely
different integrations. They are not the same project, and the intended
one should be settled before any code is written (see section 6).

---

## 2a. Candidate A: fluessig generates atilla's bindings

Direction: fluessig serves atilla's build. atilla stops hand-writing
ext-php-rs and napi glue and instead describes its façade surface, and
fluessig emits the per-language bindings. With the front end going
Rust-first, that description is itself a set of Rust derives rather than a
TypeSpec file, which sits naturally beside atilla's Rust core.

What has to happen on the fluessig side:

- Add a PHP back-end `src/bindgen/php.rs` (ext-php-rs templates) plus a
  `php` language slug and type-map entries. This does not exist today, and
  atilla is PHP-first, so it is on the critical path for this direction.
- Confirm the op model fits. fluessig is entity and data-model centric
  (`@entity`, `@key`, `@edge`, Arrow data plane, SQL DDL). atilla's façade
  is a behavioral agent API. Only fluessig's op layer (`api.json`, the
  `Shape` enum) is relevant, not the entity or SQL projections. Whether
  atilla's streaming-event agent surface lowers cleanly onto
  `ctor | unary | stream | manual` is the open design risk, and the
  front-end pivot does not change it.

What this needs from atilla: a real surface worth generating. `version()`
alone is a toy. This direction only pays off once there is `Session::open`
(M1) and ideally the agent loop (M3), so the generated bindings cover
something real.

---

## 2b. Candidate B: atilla as an agent over a fluessig-described engine

Direction: atilla serves fluessig's consumers. A fluessig-described engine
(such as entl or disponent) gains an agentic capability by letting an
atilla agent drive its ops.

The important constraint: atilla, mirroring pi, has **no MCP by design**.
So the natural-looking bridge (atilla speaks to fluessig's generated MCP
server) does not exist, because atilla has no MCP client. The real bridge
is in-process: an atilla extension (a Rust `Tool` in atilla's registry)
that calls fluessig's generated `dispatch()` or trait impls directly.

What this needs from atilla: the agent loop (M3) plus the extension and
tool plane (M6) to register such a `Tool`. `version()` and M1 are not
enough on their own.

What this needs from fluessig: little structurally. The generated
`dispatch()` already exists; a non-MCP entrypoint may be convenient. This
agent runs inside a consumer engine (entl or disponent), not inside
fluessig core, because fluessig has no runtime.

---

## 3. The minimal path, and what gates it

Because fluessig core is Rust and atilla-core is Rust, the smallest real
link is a direct Cargo dependency with no FFI:

1. Add `atilla-core = { git = "https://github.com/zmaril/atilla" }` to a
   fluessig crate's `Cargo.toml`.
2. Call `atilla::version()` and surface it. This is a handshake that
   proves the two builds link. It works **today** against M0.

Everything past the handshake needs more of atilla's façade and a process
to run it in:

| Capability fluessig would call | atilla surface needed | Milestone |
| --- | --- | --- |
| Version handshake | `atilla_core::version()` | M0 (merged) |
| Read a pi session file | `Session::open(path)` -> messages plus stats | M1 |
| Run an agent (faux provider) | agent loop plus tool execution | M3 |
| Run an agent (real provider) | Anthropic Messages provider | M5 |
| Register atilla tools over an engine | extension and tool plane | M6 |

The gating milestone depends on the target: a handshake is M0, reading
sessions is M1, and any actual **agent run** is gated on **M3** (faux
provider and agent loop), with live runs against a real model at **M5**.

---

## 4. fluessig-side changes needed regardless of direction

- Decide where the capability lives. fluessig itself is build-time only,
  with no runtime and no async. An agent run cannot execute "inside
  fluessig"; it runs inside a fluessig **consumer** engine (entl or
  disponent) that has a process and a tokio runtime. atilla's own note is
  that a tokio runtime must be created lazily, per process, after any
  fork.
- Add the dependency edge. Either a Cargo dep on `atilla-core` (Candidate
  B and the handshake) or a new bindgen back-end (Candidate A).
- For Candidate A only: add `src/bindgen/php.rs`, a `php` language slug,
  and type-map entries; then author atilla's façade surface as fluessig
  derives (the front end is going Rust-first).

---

## 5. What atilla exposes today versus what is needed

Today (M0, merged): `atilla_core::version()` and PHP `Atilla::version():
string` via ext-php-rs `=0.13.1` targeting PHP 8.4 NTS. `Session::open`
exists only as an M1 placeholder marker in `bindings/php/src/lib.rs`; the
mirror crates (`atilla-agent`, `atilla-ai`, `atilla-coding`) are empty
scaffolds.

So any integration beyond a version handshake is blocked on atilla
roadmap work, not on fluessig. The dependency list is short and
milestone-shaped: M1 for sessions, M3 for agent runs, M6 for a tool plane.

---

## 6. Open questions for the user

1. Which direction is intended: Candidate A (fluessig generates atilla's
   bindings) or Candidate B (atilla is an agent over a fluessig-described
   engine)? They are different projects with different critical paths.
2. Is the target really fluessig, or a consumer engine (entl or
   disponent)? fluessig has no runtime, so an agent run has to live in a
   consumer engine.
3. If Candidate A: is adding a PHP bindgen back-end to fluessig in scope?
   With the front end going Rust-first, atilla's surface would be authored
   as fluessig derives rather than TypeSpec, but the open risk is still
   whether atilla's behavioral façade fits fluessig's entity and op model.
4. atilla has no MCP by design, but fluessig's agent seam is its MCP
   generator. Confirm the intended bridge is in-process Rust, not MCP.
