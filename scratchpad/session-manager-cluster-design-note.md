# Design Note: Flipping the session-manager cluster to Rust on the async-oneshot bridge

Status: DESIGN-FIRST. No Rust/TS/manifest/conformance changes. Circulates to coordinator + steward.
Author scope: pidgin (`/workspace/pidgin`), napi + flip-crew edge.
Builds on the AS-BUILT bridge family: `crates/pidgin-napi/src/agent_bridge.rs` (blocking
`AgentBridge`/`BridgeChannel::call` + `emit`/`dispatch_event`/`emit_tool_update`) and
`crates/pidgin-napi/src/bridge_async.rs` (`AsyncBridge`/`AsyncChannel::call_async` + `emit`).
Naming: older memory says `atilla_*` = `pidgin_*` (`atilla-renamed-to-pidgin`, PR #171).

---

## 0. Headline finding (read first — it reshapes the task)

**There are TWO distinct "session" subsystems in pi, and the task premise conflates them.**
The bridge note's §4 row ("session-manager cluster … async JS FileSystem → `call_async`; sync
`entryTransform`/`projector` in `buildContextEntries` (session-manager.ts:414) → `call`;
SessionListProgress:703 → `emit`") mixes symbols that live in **two different files in two
different packages** — and, checked against real pi test source, **the async-FileSystem
`call_async` seam is not actually exercised by any session test.** The genuinely bridge-blocked
surface is far thinner than the note implies. Concretely:

| Symbol the task cites | Actually lives in | Reality |
|---|---|---|
| `buildContextEntries` @ `session-manager.ts:414` | **coding-agent** `core/session-manager.ts` | Pure compute, **no callbacks**; takes `(entries, leafId?, byId?)`. Already Rust-ported. |
| `SessionListProgress` @ `:703` (`(loaded,total)=>void`) | **coding-agent** `core/session-manager.ts` | Fire-and-forget `emit`; optional arg, rarely asserted. |
| `entryTransforms` / `entryProjectors`, injected `FileSystem` | **agent-core** `packages/agent/src/harness/session/session.ts` (`:32`,`:33`,`:82`,`:171`) + `jsonl-storage.ts`/`jsonl-repo.ts` | The real bridge-relevant callbacks — but the injected `FileSystem` in tests is a **concrete native env, not a JS closure** (see §5). |

So this note treats the "session-manager cluster" as **two clusters** and is honest about which
members are genuine bridge consumers vs. already-native crew flips vs. unbridgeable.

---

## 1. Cluster inventory

### Cluster A — agent-core `packages/agent/src/harness/session/` (the real bridge cluster)

| Module (pi src) | Role | Rust port | manifest status |
|---|---|---|---|
| `session.ts` | `Session` class + `buildContextEntries`/`buildSessionContext` + transform/projector types | **Ported** `crates/pidgin-agent/src/harness/session/session.rs` (460 L; `build_context_entries` :116, method :246; `ContextEntryTransform` :21, `CustomEntryProjector` :25 as Rust `Box<dyn Fn>`) | `original` (manifest L85) |
| `jsonl-storage.ts` | JSONL-backed `SessionStorage` over async `FileSystem` | **Ported** `session/jsonl_storage.rs` (439 L) | `original` (L65) |
| `jsonl-repo.ts` | `SessionRepo`; `open()`/`create()` **return live `Session` handles** | **Ported** `session/repo.rs` (342 L) | `original` (L60) |
| `memory-storage.ts` | in-memory `SessionStorage` | **Ported** `session/storage.rs` (289 L, in-mem) | `original` (L75) |
| `memory-repo.ts` | in-memory `SessionRepo` | Ported (repo.rs) | `original` (L70) |
| `repo-utils.ts` | `getFileSystemResultOrThrow` Result→throw helper | trivial (inlined) | `original` (L80) |
| `uuid.ts` | `uuidv7` | Ported `session/uuid.rs` (120 L) | `original` (L90) — **flip-blocked** (`uuid-flip-blocked-clock-rng-injection`, injected clock/RNG) |
| `../types.ts` (`FileSystem`, `SessionStorage`, `ContextEntryTransform`, projector) | shared interfaces | trait `FileSystem` @ `harness/env.rs:353` — **SYNCHRONOUS** | (types row) |

Ports already exist and are largely complete; the cluster is `original` because the **flip** (run
pi's TS tests against Rust via napi) is blocked on callback boundaries, not because Rust is missing.

### Cluster B — coding-agent `core/session-manager.ts` (already-native crew flip, ~not~ a bridge consumer)

| Module (pi src) | Role | Rust port | manifest status |
|---|---|---|---|
| `session-manager.ts` (1623 L) | canonical CLI `SessionManager` (create/open/list/append/rewrite), direct Node `fs` | **Ported** `crates/pidgin-coding/src/core/session_manager.rs` (+ `io.rs`, `discovery.rs`); CLI uses it, PR #101, CLI conformance 15/15 (`atilla-cli-binary-and-session-mirror`) | `original` (L1261) |

**What Cluster B gates (the steward's "session-manager alone gates 8+ modules"):** importers of
`session-manager.ts` in coding-agent — `agent-session.ts`, `agent-session-runtime.ts`,
`agent-session-services.ts`, `cache-stats.ts`, `compaction/compaction.ts` +
`compaction/branch-summarization.ts`, `sdk.ts`, `export-html/index.ts`, `migrations.ts`,
`cli/session-picker.ts`, interactive `session-selector*.ts`/`tree-selector.ts`. Coding-agent
compaction is already Rust-ported and was "blocked on unported session-manager"
(`pi-compaction-two-copies`) — that block is gone; only the **manifest flip** gates the count.

---

## 2. Per-module bridge-seam mapping

The callback boundaries that block the flip today, and which AS-BUILT seam resolves each:

| Boundary (file:line) | Callback shape | Seam | Notes |
|---|---|---|---|
| `session.ts:32` `entryTransforms?: ContextEntryTransform[]`, invoked sync in `buildContextEntries` (`session.ts:82`/`:171`, method `session.ts:126` test path) | sync `(entries)=>entries` | **`call`** (blocking sync-return, `AgentChannel::call`) | Only exercised by 1 case (`session.test.ts:126` `entryTransforms:[dropCompaction]`). |
| `session.ts:33` `entryProjectors`, invoked in `sessionEntryToContextMessages` (`session.ts:159-161`) | sync `(entry,i,entries)=>messages` | **`call`** | Only 1 case (`session.test.ts:107-124` `entryProjectors:{…}`). |
| `jsonl-storage.ts:6` / `jsonl-repo.ts:19` duck-typed `Pick<FileSystem, readTextFile\|readTextLines\|writeFile\|appendFile\|exists\|readdir>` — **async, Promise-returning** | `.await` a JS promise | **`call_async`** *in principle* | **Not exercised as a JS closure by any test** — tests inject concrete `NodeExecutionEnv`/`InMemorySessionStorage` (§5). So `call_async` here is a **0-signal/nominal** path today. Rust `FileSystem` (`env.rs:353`) is *sync*, so a JS-async impl would also hit the sync/async impedance (`exec-tools-async-vs-sync-agenttool`). |
| coding `session-manager.ts:703` `SessionListProgress=(loaded,total)=>void`, called `:766` | fire-and-forget void | **`emit`** | Optional param; rarely asserted. |
| `jsonl-repo.ts` `open()/create()` return **live `Session` handle**; `repo.test.ts:16` `expect(await repo.open(m)).toBe(session)` | V8 object identity across calls | **NONE — unbridgeable** (§5) | JSON boundary can't preserve `.toBe`. |

Net: the **only genuine bridge consumers in the whole cluster are the 2 `session.test.ts` cases
that inject JS `entryTransforms`/`entryProjectors` → `call`.** Everything else is either
already-native (concrete env), pure compute, emit-optional, or unbridgeable.

---

## 3. Flip order / slices (one PR per slice)

1. **Slice 1 — coding-agent `session-manager.ts` native (Cluster B).** No bridge. Shim over the
   existing `pidgin_coding::core::session_manager` (+ `io`/`discovery`). Optional `emit` for
   `SessionListProgress`. Biggest count payoff (unblocks the 8+ gated modules). **Independent.**
2. **Slice 2 — agent-core `storage` native (`jsonl-storage.ts` + `memory-storage.ts`).** No
   bridge. Shim over `jsonl_storage.rs`/`storage.rs` driving the **already-native**
   `NodeExecutionEnvCore` (`storage.test.ts` injects `NodeExecutionEnv`). **Independent of Slice 1.**
3. **Slice 3 — agent-core `session.ts` native.** Depends on Slice 2 (Session wraps a Storage).
   12/14 `session.test.ts` cases flip with pure-Rust `build_context_entries` + native storage;
   the **2 injected-closure cases** consume the `call` seam (blocking) via a thin dispatcher
   shim. **Must serialize after Slice 2.**
4. **Slice 4 — agent-core `repo` (`jsonl-repo.ts` + `memory-repo.ts`).** Depends on Slice 3.
   3/4 cases flip; the `.toBe(session)` live-handle case stays delegated/original (§5). **Serialize.**

Slices 1 and 2 are truly independent and can land in parallel. 3→4 serialize (both build on the
Session/Storage Rust types and share `lib.rs` mod-list + `Cargo.lock`). `uuid.ts` is **not** in
the flip set (stays `original`, injected clock/RNG blocker).

---

## 4. Honest bar (measured signal per flip)

| Slice / module | Test file (module-under-test) | ~cases | Earns a native row? |
|---|---|---|---|
| 1 — coding `session-manager.ts` | `test/session-manager/{tree-traversal(30),file-operations(21),build-context(16),custom-session-id(12),labels(9),migration(2),save-entry(1)}` | ~91 | **YES — genuine.** Real create/open/list/rewrite/tree logic runs in Rust. `emit` (if wired) is plumbing, not the native claim. |
| 2 — agent `jsonl-storage.ts` | `storage.test.ts` | 18 (+ mem cases) | **YES — genuine.** JSONL parse/serialize/append/rewrite executes in Rust over native env. |
| 2 — agent `memory-storage.ts` | `storage.test.ts` (in-mem suite) | subset | **YES — genuine** (in-mem store logic in Rust). |
| 3 — agent `session.ts` | `session.test.ts` | 14 (12 native + 2 via `call`) | **YES — genuine.** `buildContext`/tree/model-derivation is Rust. HYBRID (`rust-backed-majority-native-threshold`): majority-native, 2 injected-closure cases delegate through `call`. |
| 4 — agent `jsonl-repo.ts` | `repo.test.ts` | 4 (3 native, 1 unbridgeable) | **YES — genuine but minority-risk.** Confirm ≥ majority native; the `.toBe` case delegates. |
| — `repo-utils.ts` | (no dedicated test) | 0 | **NO — do NOT flip.** Trivial Result→throw helper, 0-signal; leave `original`. |
| — `uuid.ts` | `session-uuid.test.ts` | 1 | **NO.** Injected clock/RNG blocker (`uuid-flip-blocked-clock-rng-injection`). |
| — the `call_async` FileSystem seam | (none) | 0 | **NO native attribution.** No test injects a JS-async FileSystem; wiring `call_async` here would be **bridge plumbing only** — must not earn a `status:native` claim (`native-count-honesty-no-nominal-flips`). |

**Bridge-plumbing-only vs. earns-a-row:** the only place the bridge does real work is the 2
`session.ts` injected-closure cases through **`call`** — and even there the module earns its native
row on the 12 pure-Rust cases, not on the bridge. The `call_async` async-FileSystem seam earns
**nothing** in this cluster (no signal); it stays proven by the file-mutation-queue harness
(`spikeFmq`, bridge note §5), not by a session flip.

---

## 5. Unbridgeable cases (OUT OF SCOPE — document + skip, don't force)

- **`jsonl-repo.ts` live `Session` handle identity** — `repo.test.ts:16`
  `expect(await repo.open(metadata)).toBe(session)`. `open()`/`create()` return a **live `Session`
  object** whose method identity must survive across calls; a JSON `state()` boundary can't
  preserve `.toBe` (same class as `agent.test.ts` cases 2 & 10, `native-count-honesty-no-nominal-flips`;
  same V8-handle rule as `bridge_async.rs:120-142`). That one case stays delegated/original.
- **The async JS `FileSystem` as a live duck-typed object** — if a caller ever injected a *live*
  JS `FileSystem` closure literal (as the `Pick<FileSystem,…>` type permits) with method identity
  / streaming, that would be a live-Node-object case, not a JSON round-trip. Pi's own tests don't
  do this (they inject `NodeExecutionEnv`), so it's moot for the flip — but do not build a
  `call_async` FileSystem proxy speculatively to "support" it.
- **No reentrant/suspend-resume case exists in this cluster.** Session ops are forward-only; none
  of the parked reentrant primitive (`ext-oauth-login-reentrant-primitive-parked`) is needed here.

---

## 6. Steward coordination

- **Single-writer.** `conformance/manifest.json` rows, the `conformance.json` baseline, and
  `STEWARD.md` merge sequencing are steward-only (`steward-flip-crew-model`). Crew slice PRs may
  touch ONLY `crates/pidgin-napi` additions, shim files, and manifest **row additions** — never
  `conformance.json`/`STEWARD.md`. Baseline regens are creds-stripped, done by the steward at
  merge time (`conformance-offline-regen-unset-aws-creds`).
- **Shared surfaces across these PRs:** `crates/pidgin-napi/src/lib.rs` mod-list + `Cargo.lock`
  (and, if a new `session_bridge.rs`/shim module is added, its `pub mod` line). These are trivial
  rebases; **serialize the merge moment** so the mod-list/lockfile don't thrash. Napi class merge
  sequencing (new `#[napi]` shim exports) goes through the steward.
- **Straitjacket:** any faithful-port `.rs` still needs its `// straitjacket-allow-file[:duplication]`
  marker (`straitjacket-faithful-port-dup-allow`); the ports already exist, so this is mostly a
  shim concern.

---

## 7. Open questions / decisions

**For coordinator:**
1. **Confirm the reframing.** Is the intended target Cluster A (agent-core `session/*`, the real
   bridge cluster) or Cluster B (coding `session-manager.ts`, already-native crew flip) — or
   both as sequenced here? The task text describes a blended module that doesn't exist as one file.
2. **`call_async` has no session consumer.** Given no session test injects a JS-async FileSystem,
   do we drop the async-FileSystem seam from the session plan entirely (recommended) and let
   file-mutation-queue (`spikeFmq`) remain the sole `call_async` proof? Or is there an off-test
   consumer (SDK / RPC mode) that would inject a JS FileSystem and needs it?
3. **Is the thin `call` win worth a bridge dependency?** Only 2 `session.test.ts` cases need
   `call`. Option (a): flip `session.ts` as HYBRID with those 2 delegated to `__pi_original__`
   (no bridge, ships now, still majority-native); option (b): wire `call` for them. Recommend (a)
   first, (b) as a follow-up only if the count honesty rule prefers it.

**For steward:**
4. **Slice 1 (coding `session-manager.ts`) native-count ruling** — its Rust port already backs the
   CLI; confirm the manifest flip + `tests[]` attribution for the 7 `test/session-manager/*` files
   and that the ~8 transitively-gated modules (agent-session, compaction, sdk, export-html…) are
   NOT auto-flipped nominally on its back (`native-count-honesty-no-nominal-flips`) — each still
   needs its own genuine flip.
5. **Slice 4 `repo` majority check** — with the `.toBe` case delegated, does `repo.test.ts` (4
   cases, 3 native) clear the majority-native bar for a `tests[]` entry, or stay honest-0?
6. **Merge order** of Slices 1/2 (independent) vs 3→4 (serialized), and who holds the
   `lib.rs`/`Cargo.lock` pen during the window.
