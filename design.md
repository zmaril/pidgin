# atilla — design

The one-page picture of what this project is and the decisions that govern it. The research behind every statement here lives in `notes/startup/` (see its `index.md`); where an old note disagrees with this file, this file wins.

## What we are building

atilla is a continually updating Rust mirror of [pi](https://github.com/earendil-works/pi) — Mario Zechner's self-extensible coding agent and the libraries beneath it — re-exposed as first-class native extensions in the major host languages. The Rust core replaces pi's Node runtime; the languages get real native extensions (a PECL-style `.so` for PHP, napi-rs packages for Node, PyO3 wheels for Python, and so on), not subprocess wrappers or a C-ABI dlopen.

Upstream is pinned at commit `3da591ab` (v0.80.10) and tracked continuously: a machine-readable pin, a file-to-crate correspondence map, and a scheduled drift job that turns new upstream commits into tracked porting work.

## The bar: pi's own tests

Correctness is defined as passing pi's own test suite, literally — pi's ~3,600 test cases run unmodified against the Rust core.

The mechanism: napi-rs shim packages present pi's exact TypeScript module surface (pi's own `.d.ts` files front the Rust runtime exports, since types erase at runtime), and a generated `src`-tree swap intercepts the 93 percent of pi's test files that deep-import relative `../src/*` paths. A module manifest marks each pi module `native` (Rust-backed) or `original` (still pi's TS), and doubles as the porting ledger. pi's four black-box CLI tests repoint at the `atilla` binary. A conformance dashboard reports "N of M pi tests passing" and CI fails on regression.

We aim for 100 percent eventually; until then, the irreducibly-Node residue (worker_threads, clipboard, environment-shaped tests) lives on a documented, CI-tracked exclusion list that shrinks over time rather than being silently ignored.

Because roughly 58 of pi's test files mock internal collaborators and roughly 68 stub global fetch, the Rust core builds injection seams in from the start — injectable provider, HTTP transport, clock, and storage environment, as production-grade traits, not test hacks. This is the difference between passing most of the suite and passing all of it.

## Architecture

- **Workspace.** A Cargo workspace whose crates mirror pi's five packages (`ai`, `agent`, `coding-agent`, `tui`, `orchestrator`), funneled through one `atilla-core` façade crate. Every language binding depends only on the façade, which absorbs async-to-sync bridging, opaque handles, and error normalization once, so each binding stays a thin mechanical translation of the same surface. Module boundaries and naming stay deliberately close to pi's so upstream diffs map to tractable atilla diffs.
- **Rewrite mode.** AI-accelerated hand-rewrite: idiomatic-first, big-bang, no transpiler and no strangler-fig. pi's TypeScript and its test suite are the executable spec.
- **Providers.** Hand-rolled thin clients, one per wire dialect, on `reqwest` + `eventsource-stream`, all converging on pi's `AssistantMessageEvent` union. Order: Anthropic, then the OpenAI-compatible client (reused across compatible vendors), then Google, then Mistral, with Bedrock last (SigV4 via the AWS SDK). No multi-provider crate — we own the wire.
- **Sessions.** A byte-exact mirror of pi's version-3 JSONL session-tree format, read and write.
- **No MCP.** pi has none by design; the mirror reproduces that exactly.
- **Streaming across FFI.** The cross-language surface is a blocking `next_event()` over an opaque handle; the Node binding additionally re-presents pi's exact async-iterable stream API on top, because pi's tests require it.

## Extensions

One Rust extension registry (`Tool` / `Hook` / `Command`) is the successor to pi's `ExtensionAPI`, and everything lowers onto it.

- **pi's own TypeScript extensions** run unchanged on an embedded `deno_core` runtime with a Node-compat layer; passing pi's extension tests is part of the bar.
- **Host languages** get the same `(pi) => {}` shape: a handle with `registerTool` / `on` / `registerCommand` that wraps PHP/Python/JS closures as registry entries. Dispatch follows the two-flavor model in `notes/startup/deep-hooks.md`: a direct trampoline for hosts with a `Send` handle (Python under the GIL, Node via threadsafe functions), a thread-bound reentrant rendezvous pump for hosts without one (PHP, Ruby). Only JSON crosses the boundary; VM handles never enter the tokio world.
- **Hook exposure policy: implemented-only.** Each binding exposes exactly the hook events the core has actually implemented at that point — the surface grows with the port, and no binding advertises events that are stubs.
- **Discovery.** pi discovers extensions as TypeScript entrypoint files (project-level `.pi/extensions/*.ts` and configured paths), each default-exporting a factory `(pi) => void` that pi loads in-process via jiti and calls with the live API object — there is no separate manifest file in pi today; the filesystem convention is the manifest. atilla mirrors that convention for TS extensions, and extends it for host languages with a small per-extension declaration (language plus entrypoint) so a PHP or Python extension can be discovered the same way. The inventory of what is loaded — every registered tool, hook, and command, whatever language it came from — lives in Rust: the core registry is the single source of truth, and bindings query it rather than keeping their own lists.

## Languages

All major languages, eventually; ordering matters less than proving the model. PHP goes first because it is the weirdest host (synchronous, request-scoped, thread-bound) — if the façade survives PHP, easier hosts follow. Node is first-class from day one out of necessity: the test harness is a Node binding. Python follows; Ruby and others as demand shows.

## TUI

Shadow pi faithfully. pi's TUI is an inline line-diff renderer with a crash-on-mismatch width contract, both pinned by its tests — so atilla ports `tui.ts` and the width module exactly rather than rebuilding on another render model. crossterm serves as the ANSI sink and event source, ratatui-image handles images, and that is the extent of the ratatui footprint. No new surfaces are planned.

## Sequencing

1. The napi bridge harness first: shim packages, module manifest, codegen, and one `ai` test file green against Rust. The conformance mechanism precedes everything it gates.
2. `ai` bottom-up (Anthropic SSE parsing, request shaping, providers), flipping manifest modules to `native` as their tests pass; the dashboard and CI gate ship here.
3. `agent`, then `coding-agent` dependencies-first; the PHP binding surface grows in parallel once the façade exists.
4. `tui` (faithful port) and the extension plane per the porting order in `notes/startup/porting-map.md`; `orchestrator` last.

Distribution and packaging (PECL matrices, wheels, prebuilt binaries) are explicitly deferred until the core and first bindings are proven.
