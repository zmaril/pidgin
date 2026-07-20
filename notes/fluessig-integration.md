# Fluessig integration plan

This note scopes connecting
[`fluessig`](https://github.com/zmaril/fluessig) and pidgin. The direction
is decided: **A, fluessig generates pidgin's bindings.** This note records
that decision, the concrete work it implies on each side, the milestone
that gates it, and the decisions and questions still open. It is grounded
in fluessig's code and commit history, not its README, which is stale (see
section 1a). Where this note and `design.md` disagree, `design.md` wins.

The starting premise needed correcting first. fluessig is **not** a PHP
application, so there is no `php.ini`, no `.so` to load, and no NTS/ZTS or
PHP-version match to worry about at fluessig's edge. fluessig is a Rust
plus Node build-time schema and code-generation tool with no runtime. The
integration is therefore not extension-loading; it is teaching fluessig to
generate the binding layer that pidgin writes by hand.

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
- It is Rust at the core. pidgin's façade crate `pidgin-core` is also
  Rust, so a Rust-to-Rust link needs no FFI binding at all. And with the
  front end going Rust-first, describing a model is itself becoming a
  Rust-derive exercise rather than a TypeSpec one.
- It already generates an agent-facing seam. `src/bindgen/mcp.rs` projects
  the op layer (`api.json`) into an MCP tool surface plus a generated Rust
  `dispatch()` module, wired into the CLI via `--mcp` and covered by
  `tests/mcp.rs`. Op shapes are `ctor | unary | stream | manual`.
- It has no PHP back-end. The back ends present are node, python, and ruby
  (plus the MCP projection); there is no `src/bindgen/php.rs`, and no
  "php" token anywhere in the tree. pidgin is PHP-first, so this gap is
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
per language." The difference is who writes the binding layer: pidgin
hand-writes it (`bindings/php` via ext-php-rs, `crates/pidgin-napi` via
napi-rs), and fluessig generates it from a schema. Direction A puts those
together: pidgin describes its façade surface once and fluessig emits the
per-language bindings, so the ext-php-rs and napi glue stop being
hand-maintained.

The alternative, B (pidgin as an agent driving a fluessig-described engine
such as entl or disponent), is not the path chosen. It is recorded only so
the decision is legible: B would need pidgin's agent loop (M3) and tool
plane (M6) plus an in-process bridge, since pidgin has no MCP client by
design. A is a build-time codegen relationship and can start against
today's surface.

---

## 3. Direction A: the concrete work

On the fluessig side, the changes this needs:

1. Add a PHP back-end. fluessig has `src/bindgen/{node,python,ruby}.rs`
   and no `php.rs`. A new `src/bindgen/php.rs` (ext-php-rs templates), a
   `php` language slug, and PHP type-map entries are the critical-path
   change, because pidgin is PHP-first.
2. Cover pidgin's op shapes. fluessig's op layer (`api.json`, the `Shape`
   enum `ctor | unary | stream | manual`) is entity and data-model
   centric. pidgin's façade is behavioral: `version()` is a plain unary
   call, `Session::open` returns an opaque handle (a `ctor`-shaped op),
   and agent runs emit a streaming event union (a `stream`-shaped op).
   Confirming, and where needed extending, the shape model and type map to
   carry opaque handles and event-union streams is a core design risk.
3. Reproduce pi's Node return shapes exactly. This is a large piece of the
   work and has its own section, because pidgin's Node binding is also its
   conformance harness (section 4).
4. Emit pi's package and module layout. fluessig produces one flat binding
   per catalog; pi's conformance surface is five packages with pi-exact
   names and deep module paths. Multi-package, multi-module output is a new
   fluessig capability (section 5).

On the pidgin side:

1. Provide a describable surface. Today the façade is only
   `pidgin_core::version()`; `Session::open` and the agent loop are not
   built yet, so the surface fluessig would generate from grows with
   pidgin's milestones (section 6).
2. Choose the source of truth. Because fluessig's front end is going
   Rust-first, the natural model is to annotate the façade types in
   `pidgin-core` with fluessig derives, or to keep a small schema crate
   that describes them. Either couples pidgin to a pinned fluessig ref;
   pick deliberately.
3. Retire the hand-written bindings as generation takes over. `bindings/php`
   and `crates/pidgin-napi` become generated output. The napi binding is
   also pidgin's conformance harness (it fronts pi's test suite), so a
   generated napi surface must stay a drop-in that keeps pi's tests
   passing; the swap cannot regress conformance.

---

## 4. Reproducing pi's Node return shapes

Direction A's harder half is not the PHP back-end; it is making fluessig's
Node back-end emit pi's Node API return shapes exactly, because pidgin's
Node binding is also its conformance harness and must pass pi's own test
suite unmodified. Two facts frame this:

- No Arrow, either side of the real surface. pi returns only plain JS
  objects and JSON-like values; there is no columnar, Arrow, or IPC
  representation anywhere in pi or in pidgin's design (session data is
  version-3 JSONL; cross-FFI events are a small struct or a JSON string).
  fluessig does use Arrow, but only for one thing: a DTO field typed
  `ArrowBatch` (its columnar data-plane carrier, for example entl's
  `ChangeBatch.ipc`), surfaced in Node as lazy Arrow-IPC `Buffer` bytes.
  So Arrow is a fluessig feature pidgin's surface must stay off: pidgin
  ops must avoid the `ArrowBatch` and `bytes` carriers and ride fluessig's
  plain-`#[napi(object)]` path. The Arrow question resolves to "do not use
  it here," not "wire it up."
- pi's own `.d.ts` fronts the surface. Conformance requires pi's exact
  TypeScript module surface: pi's hand-written `.d.ts` files front the
  Rust exports (napi's generated types cannot express pi's discriminated
  unions or string-literal tags), and a generated `src`-tree swap
  intercepts the deep `../src/*` imports pi's tests use. fluessig
  generating the `.node` addon is only the runtime-export half; the public
  typing half is pi-specific.

Where fluessig's Node back-end, as it stands, does not yet produce what pi
needs:

1. Exact export and field names. fluessig applies a fixed
   snake-case-to-camelCase transform on `#[napi(object)]` fields and
   derives enum wire tokens by lowercasing catalog names. pi's `.d.ts`
   dictates specific spellings (`contentIndex`, `toolCallId`, `stopReason`,
   `isError`); a near-miss is a silent import mismatch. fluessig needs a
   per-symbol name-pinning hook (emit `#[napi(js_name = ...)]` from the
   schema) rather than a fixed casing rule.
2. Discriminated unions as objects, not JSON strings. fluessig lowers a
   union return to a `String` carrying a `{"kind": tag, "payload": body}`
   envelope that the caller parses. pi's `AssistantMessageEvent` must be a
   real discriminated union of object types keyed on a literal `type` tag.
   fluessig would need to project unions as structured values and, because
   its generated `.d.ts` cannot express them, keep those types internal and
   let pi's hand-written `.d.ts` front them.
3. Async-iterable streams, not a poll cursor. fluessig's `stream` shape
   emits a `#[napi]` cursor class with `next(): Promise<T | null>`, a
   poll-based cursor. pi requires a real JS async-iterable
   (`Symbol.asyncIterator`) `AssistantMessageEventStream`. fluessig needs
   to generate the async-iterable adapter (a ThreadsafeFunction-backed
   surface) over the core's blocking `next_event()`, not merely expose the
   poll cursor.
4. The non-throwing in-stream error contract. After a stream starts, pi
   encodes failures as an `error` event in the stream, never thrown;
   non-streaming calls instead surface an error as a thrown JS exception.
   fluessig's Node back-end currently maps every `compute()` error to a
   thrown napi error uniformly. It would need the two-mode error model:
   errors-as-events for streams, exception for unary calls.

The parts that already fit: fluessig's default plain-object path (`unary`
returning `#[napi(object)]` as `Promise<T>`) matches pi's plain-object
returns for the simple cases, such as `Session::open` giving a message
list plus stats, or `calculateCost` giving numbers, provided the naming in
item 1 is controllable. So the Node work is targeted, not a rewrite: name
pinning, union projection with external `.d.ts` fronting, an async-iterable
stream variant, and a dual error model. The same union and naming levers
carry over to the PHP back-end when it lands.

---

## 5. Emitting pi's package and module layout

There is a fifth fluessig gap the earlier draft did not name, and it is
structural rather than per-return. fluessig produces one flat binding per
catalog: its node emitter loops every interface into a single generated
file, the CLI writes one file per language, and the schema (`api.json`)
has no package, module, or scope field at all, with `deny_unknown_fields`
blocking any data-side addition. pi's conformance surface is the opposite.
pidgin mirrors pi's five packages (`ai`, `agent`, `coding-agent`, `tui`,
`orchestrator`) as crates funneled through the `pidgin-core` façade, and
pi's tests import by pi's exact package names (`@earendil-works/pi-ai`,
`pi-agent-core`, and so on) and by hundreds of deep `../src/*` module
paths, each an independently tracked shim in the conformance module
manifest. Names are load-bearing: a near-miss is a silent import mismatch.

So describing pidgin's whole façade as one fluessig catalog would collapse
it into one flat output and could not recreate pi's breakdown. Recreating
it needs three things fluessig lacks today: a schema-level package and
module grouping concept, an output fan-out in the emitter and CLI
(analogous to the language fan-out fluessig already does when rendering its
README), and caller-specified package names with nested module paths. This
is a distinct capability from the name pinning in section 4: that pins a
symbol's name, this pins where the symbol lives.

The `pidgin-core` façade being one Rust crate does not force the collapse.
The façade is the internal seam; the schema can tag each op and type with
its target pi package and module regardless of where the derive
definitions live. The grouping metadata is what matters, not whether the
schema sits in `pidgin-core` or a separate crate.

---

## 6. Milestone gate and a sequencing that de-risks A

The link itself is buildable today, and the payoff grows with pidgin's
façade:

| Step | pidgin surface | fluessig work | Gated at |
| --- | --- | --- | --- |
| Regenerate today's `Pidgin::version()` from a schema, byte-comparable to the hand-written binding | `pidgin_core::version()` | `src/bindgen/php.rs` MVP | M0 (today) |
| First non-trivial generated binding | `Session::open(path)` -> messages plus stats | handle plus struct lowering; Node name pinning (section 4); package and module targeting (section 5) | M1 |
| Generate over the agent surface | agent loop, event stream | union-as-object projection, async-iterable stream, dual error model (section 4) | M3 |
| Replace the hand-written napi harness with generated napi | napi conformance surface | node back-end parity with pi's `.d.ts` fronting, plus multi-package and multi-module output (section 5) | M7 |

Recommended first move, doable now: build the PHP back-end far enough to
regenerate the existing M0 `Pidgin::version()` binding and diff it against
the hand-written one. That proves the direction end-to-end against a
trivial surface before pidgin's API grows, and it is the concrete answer
to "what needs to happen with fluessig to enable pidgin": a `php.rs` back
end is step one, and the Node and package-layout work in sections 4 and 5
is the larger follow-on.

---

## 7. What pidgin exposes today versus what is needed

Today (M0, merged): `pidgin_core::version()` and PHP `Pidgin::version():
string` via ext-php-rs `=0.13.1` targeting PHP 8.4 NTS. `Session::open`
exists only as an M1 placeholder marker in `bindings/php/src/lib.rs`; the
mirror crates (`pidgin-agent`, `pidgin-ai`, `pidgin-coding`) are empty
scaffolds.

So any generated binding beyond a version call is blocked on pidgin
roadmap work, not on fluessig. The dependency list is short and
milestone-shaped: M1 for sessions, M3 for the agent surface, M7 for the
napi harness swap.

---

## 8. Decisions and open questions

Recorded decisions from the project owner, who owns both repos:

- Op model: fluessig's shape model and type map will likely need extending
  to carry pidgin's opaque handles and event unions (question 1 below).
- Node capabilities: the four section-4 gaps are grown into fluessig's
  generic back-end, not left behind the `@manual` escape hatch.
- Ownership: pidgin drives the fluessig-side work; a shared owner means the
  repo boundary is not a blocker.
- Harness swap: generated napi replaces the hand-written harness when it
  makes sense and can, not on a fixed milestone.
- Package layout: recreating pi's package breakdown is required and is a
  new fluessig capability (section 5), not recoverable by schema placement
  alone. "All in `pidgin-core`" is fine as the internal seam as long as the
  schema carries package and module grouping.

Still open:

1. Op-model fit: exactly which extensions the shape model and type map need
   to carry opaque session handles and streaming event unions.
2. Source of truth: do the fluessig derives live on the façade types in
   `pidgin-core`, or in a separate schema crate? Either supports the
   package grouping above; the trade is coupling `pidgin-core` to a
   fluessig ref versus carrying an extra crate.
3. Multi-package emission: the shape of the schema-level grouping concept
   and the emitter and CLI fan-out that section 5 requires is undecided.
