# Mock-seam inventory: sizing the true cost of the 100% bar

This inventory answers the open question in `notes/startup/testing-strategy.md` §10: of pi's mock-based tests, how many mock a *whole module under test* (fine — the napi shim replaces it wholesale) versus a *collaborator* (which only passes if the Rust core exposes a matching injectable seam, or the test is ported). The count sizes the real cost of literally passing all of pi's suite.

## Method

- Source: pi submodule at `vendor/pi`, pinned commit `3da591ab74ab9ab407e72ed882600b2c851fae21` (v0.80.10).
- Scanned every `.test.ts` file under `vendor/pi/packages/*/test` (319 files: ai=100, coding-agent=176, tui=27, agent=16, orchestrator=0).
- Frameworks: `agent`, `ai`, `coding-agent` use Vitest 4.1.9; `tui` uses `node:test`; `orchestrator` has no tests. All mocking is the Vitest `vi.*` API — no jest, no bun, no bare `spyOn`.
- Enumerated `vi.mock` (39 call sites / 29 files), `vi.spyOn` (151 call sites), and `vi.stubGlobal` (68 sites), then read each mocking test to identify the module under test versus what is mocked. The `tui` package contributes zero `vi.*` usages (it is `node:test`), so it does not appear in the tables below.
- Every classified call site is listed in the machine-usable sidecar `notes/mock-inventory.json`.

Classification key:
- **a — whole-module-under-test.** The mock replaces the exact module or object that is the subject of the test. The Rust-backed shim satisfies the test's intent by construction. No seam needed.
- **b — collaborator.** The mock steers or observes a *different* internal module that the subject calls. When the subject is Rust, a JS mock of the collaborator has no effect, so this passes only via a Rust injection seam or a ported test. Observation-only spies on an internal collaborator method count here too: once that method lives in Rust, the JS spy never fires.
- **c — global stub.** `vi.stubGlobal` of a runtime global (fetch, WebSocket, crypto). Maps to a transport-style seam.
- **d — irrelevant.** Mocks of node builtins, env, or console/logging, and spy-for-assertion on a boundary the shim or runtime already owns. No seam needed.

## Counts per category per package

| Package | Total | a whole-module | b collaborator | c global stub | d irrelevant |
|---|---:|---:|---:|---:|---:|
| ai | 87 | 0 | 29 | 57 | 1 |
| coding-agent | 169 | 0 | 87 | 10 | 72 |
| agent | 2 | 0 | 1 | 1 | 0 |
| tui | 0 | 0 | 0 | 0 | 0 |
| orchestrator | 0 | 0 | 0 | 0 | 0 |
| **Total** | **258** | **0** | **117** | **68** | **73** |

The headline is that category a is empty: not one pi test mocks the whole module under test in a way the shim alone satisfies. Every mock either steers a collaborator, stubs a global, or is incidental. So the shim-swap mechanism buys correctness for the 73 category-d sites (already owned by the shim or runtime) and nothing more on its own — the rest ride on seams.

## Per-seam coverage rollup

Categories b and c both map to seams. The four seams already planned in `design.md` (provider, HTTP transport, clock, storage env) are marked; everything below the subtotal is work beyond them.

| Seam | b | c | total | in the four planned seams |
|---|---:|---:|---:|:--|
| provider | 22 | 0 | 22 | yes |
| HTTP transport (fetch + WebSocket) | 13 | 67 | 80 | yes |
| clock | 3 | 0 | 3 | yes |
| storage env | 3 | 0 | 3 | yes |
| **subtotal — four planned seams** | **41** | **67** | **108** | — |
| subprocess / command runner | 44 | 0 | 44 | no (new seam) |
| clipboard | 8 | 0 | 8 | no (new seam or exclusion) |
| stdio transport | 4 | 0 | 4 | no (new seam) |
| RNG / crypto | 0 | 1 | 1 | no (new seam) |
| image processing | 1 | 0 | 1 | no (new seam) |
| port-only (private-method and internal-transform spies) | 19 | 0 | 19 | no (port the test) |
| **Total** | **117** | **68** | **185** | — |

The 185 seam-relevant sites are categories b plus c; the 73 category-d sites need nothing.

Notes on the beyond-four buckets:
- **Subprocess / command runner (44).** 43 sites are the `package-manager.test.ts` suite spying `DefaultPackageManager`'s private command runners (`runCommand`, `runCommandCapture`, `runCommandSync`, git and npm helpers) to steer or assert argv on the subprocess boundary; one more mocks `child_process` for a git `symbolic-ref` branch lookup. A single injectable command-runner seam collapses all of them.
- **Clipboard (8).** The `clipboard*` tests mock `child_process`/`os`/the native binding to drive read, write, and BMP-conversion paths. `design.md` already lists clipboard as irreducibly-Node residue, so these are seam-or-exclusion, not free.
- **stdio transport (4).** `runRpcMode` tests mock the raw-stdout guard and the JSONL line reader/serializer.
- **RNG / crypto (1).** `session-uuid.test.ts` stubs `crypto.getRandomValues` to feed deterministic bytes into `uuidv7`.
- **image processing (1).** A read-tool test stubs `resizeImage` to exercise the fallback path.
- **Port-only (19).** These spy the unit-under-test's *own* private methods or an internal transform, so no external seam helps: auto-compaction internals (`_runAutoCompaction`, queue checks) 12, `Agent.continue` orchestration 2, platform shell-config detection 4, and one internal SSE event-stream transform. Each needs the test restructured or the internal logic re-exposed as a deliberate seam.

## Clock seam: fake timers and timestamp assertions

The clock bucket is small by category-b count (3 direct collaborator sites) but the fake-timer surface that gates its design is larger:

- `vi.useFakeTimers`: 29 sites across 9 files (5 in ai, 4 in coding-agent).
- `vi.advanceTimers*`: 58 sites — the clock seam must expose deterministic timer *advance*, not just a settable now.
- `vi.setSystemTime`: 16 sites across 5 files (all in ai); roughly 12 bake an injected wall-clock `now` into an assertion.

The timestamp-assertion patterns that require the shim to inject `now`:
1. OAuth device-code and access-token poll timestamps asserted as `startTime + N * interval`.
2. Token and credential expiry computed as `now + expires_in * 1000 - skew` (with a one-hour fallback when the field is absent).
3. SSE retry and backoff delay derived from a `retry-after` header carrying an absolute future time.
4. Elapsed-gated reconnect and session-lifetime logic (a test advances `now` by tens of minutes).
5. The `uuidv7` embedded 48-bit timestamp, pinned via `spyOn(Date, 'now')` and asserted inside the UUID string.

Pervasive `timestamp: Date.now()` on message construction is test-internal bookkeeping and is *not* asserted, so it needs no injected clock. The clock seam therefore must offer both a settable `now` and deterministic timer advance, and the shim must let a test inject `now` for the five patterns above.

## The true 100% cost

Of the 258 mock, spy, and stub call sites across pi's suite:

- **73** (category d) need nothing beyond the shim and runtime that already exist.
- **108** pass through the four seams already committed in `design.md`: provider (22), HTTP transport including WebSocket (80), clock (3), storage env (3). This is 58 percent of the 185 seam-relevant sites.
- **77** need work beyond the four planned seams. Of those:
  - **44** collapse under one additional seam: an injectable subprocess / command runner. This is the single highest-leverage addition, and 43 of the 44 are one coherent suite (`package-manager.test.ts`). Recommendation: adopt a command-runner seam as a fifth first-class trait.
  - **14** map to narrow additional seams or the documented Node-exclusion list: clipboard (8, already flagged irreducible in `design.md`), stdio transport (4), RNG (1), image processing (1).
  - **19** are irreducible port-the-test cases: they spy the unit-under-test's own private methods or an internal transform, so no external seam reaches them. Each is a test to restructure or a piece of internal logic to re-expose as a deliberate seam.

Bottom line: the four planned seams are necessary and get the suite most of the way, but they are not sufficient for 100 percent. A fifth subprocess seam is strongly warranted (it alone unlocks 44 sites). After all five seams plus clipboard/stdio/RNG/image handling, the irreducible residue that can only be closed by porting or re-exposing internals is roughly 19 sites, concentrated in auto-compaction internals and a handful of orchestration and platform-detection spies. None of pi's mock-based tests are free by shim-swap alone.

## Appendix

Every classified call site — file, line, pattern, target, category, seam, port flag, and a one-phrase note — is in the machine-usable sidecar [`notes/mock-inventory.json`](./mock-inventory.json), sorted by file and line, for the porting work to consume directly.
