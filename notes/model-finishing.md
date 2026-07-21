# Finishing a `hinzu model --emit quint` skeleton

`hinzu model --emit quint` (Phase A of the code-derived verification pipeline,
hinzu #43) lowers the language-neutral body IR — the same `BodyFacts` the range
analysis consumes — into a **Quint model skeleton**: one `module derived { ... }`
whose faithfully-lowerable parts are real Quint inside
`// ---- BEGIN GENERATED ----` / `// ---- END GENERATED ----` regions, and whose
every judgment call is an explicit `// AGENT-TODO` hole.

The skeleton is honest by construction. It never invents a value or a control
flow it cannot derive from the IR: where a faithful lowering needs a modeling
decision, it leaves a comment saying so. Because the holes are `//` comments, the
generated document parses as valid Quint as-is (`quint parse`), so you can fill
the holes incrementally and keep a parsing model at every step.

This note is the **step-by-step checklist an agent follows to turn that skeleton
into a `quint verify`-checked model**. The async-oneshot bridge
(`specs/bridge_async.qnt`, seeded from `specs/derived/bridge_async.derived.qnt`)
is the worked reference throughout — every section points at the concrete lines
that did it.

---

## 0. The shape of the deliverable

You are turning ONE derived `module derived` skeleton into a finished module that:

1. `quint typecheck`s,
2. `quint run --backend typescript --invariant <all> --max-samples N` finds no
   violation,
3. `quint verify --invariant <each>` returns `The outcome is: NoError`, and
4. exports an ITF trace (`--out-itf`) that a test can replay against real code.

Keep the derived skeleton committed under `specs/derived/` as the SEED, and write
the finished model as a NEW file (`specs/<name>.qnt`). Do not hand-edit the
`BEGIN GENERATED` regions of the seed — they are regenerated from the IR.

---

## 1. Read the `hinzu model` output: generated regions vs. `AGENT-TODO` holes

### The generated regions (do not hand-edit — a re-run overwrites them)

- **state vars** — one module-level `var` per local of every function, typed by
  the local's numeric kind. Names are `<fnkey>_l<localid>` (`<fnkey>` = the
  function's symbol id with every non-alphanumeric char replaced by `_`). Local
  `0` is the return place; locals `1..=arg_count` are the parameters; the rest are
  temporaries. Each var carries the source function and `file:line`.
- **init** — every var set to a typed zero (`0` / `false`).
- **step** — `any { ... }` over the per-function actions.
- **per-function `action` blocks** — the straight-line statements of each
  function's entry block, lowered to Quint assignments; anything richer becomes a
  CFG summary + a hole.

### The holes (fill these)

| Hole marker | What it is asking for |
| --- | --- |
| `AGENT-TODO: Quint has no floats — choose an abstraction` | a `Float` local; pick fixed-point / uninterpreted / interval, or drop it if irrelevant to the property |
| `AGENT-TODO: unknown type — choose abstraction` | an `Other`-typed local; pick the abstraction the property needs |
| `AGENT-TODO: choose real initial state` | replace the typed-zero `init` with the real starting state (often `nondet` over parameter domains, or concrete constants) |
| `AGENT-TODO: encode control flow — this CFG needs a state-abstraction choice` | a multi-block CFG; the skeleton printed a CFG summary and lowered only the entry block — YOU choose how to encode the branches/loops (see §2) |
| `AGENT-TODO: environment nondeterminism — <var>' = <choose a value>` | an `Unknown` rvalue or a call destination: a value entering from outside the modeled code (external result, message, read). This is where message loss / races / aborts enter (see §3) |
| `AGENT-TODO: unsupported binop / unary op` | an operator outside the modeled set (bitwise, shifts); encode it with the matching Quint operator |
| `AGENT-TODO: add environment actions (message loss, races, aborts)` | `step` chooses only derived function actions; add the environment actions the real system exhibits (see §3) |
| `AGENT-TODO: state invariants` | the properties to prove — the whole point (see §4) |

**Worked read.** `specs/derived/bridge_async.derived.qnt` was lowered from the
LIVE StableMIR MIR of `bridge_async.rs`. It surfaced `deliver` as a 19-block CFG
(`block 5: SwitchInt [1=>6, 0=>8, ...]` = the `Option` remove-then-send match),
`abort` as a 22-block CFG (blocks 10..15 = the drain loop), and `call_async` — an
`async fn` — as a single environment-nondeterminism hole because its outer MIR
body is only the coroutine shell. Those holes told us exactly which protocol
behaviors we had to supply by hand.

---

## 2. Picking a STATE ABSTRACTION for a CFG hole

The seed refuses to invent a program-counter machine; it hands you the CFG
summary and asks you to choose. The decision tree:

- **Only one path matters to the property?** Inline that path as a single action
  with a guard conjunction; ignore the others. (Cheapest — start here.)
- **The branches are a genuine protocol choice?** Collapse the CFG to a
  **protocol-level state set / enum**, one action per protocol transition, and let
  the branch conditions become action guards. This is almost always right for
  concurrency/registry code. The bridge did exactly this: `deliver`'s
  Some/None SwitchInt is NOT modeled as a pc machine but as two actions —
  `jsResolveValue`/`jsResolveError` (the Some branch: a pending id) and
  `jsResolveStale` (the None branch: an absent id, a no-op). See
  `specs/bridge_async.qnt` `action jsResolveValue` … `action jsResolveStale`.
- **A real loop with a fixpoint (drain, retry)?** Model the loop's EFFECT, not its
  iterations: a set operation. `abort`'s drain loop (blocks 10..15) becomes one
  `fold` over `pending` — `status' = pending.fold(status, (acc,i) => acc.put(i,
  AbortedS))` — plus `pending' = Set()`. See `action abort`.
- **A `pc` variable per block** is the fallback when the control flow itself is the
  property (a state machine with reachability claims). Reserve it — it explodes
  the state space.

**Float / pointer / unknown-type holes** are abstraction choices too: model a
float as fixed-point or an uninterpreted value only if the property touches it;
otherwise drop the local. A pointer/`Other` local that only carries identity
(a channel handle, a lock) collapses to an `int` id or disappears into a `Set`.

---

## 3. Adding ENVIRONMENT actions (message loss, races, aborts)

The derived `step` chooses only among the code's own functions. Real systems also
take steps the code does not initiate: a message is dropped, two operations race,
a peer aborts. Add one action per such event and put it in `step`.

The bridge's worked patterns (`specs/bridge_async.qnt`):

- **abort** — a peer trips the signal and drains the registry. Guarded by nothing
  (always enabled); empties `pending`, marks every drained id `AbortedS`.
- **loseReply** — the ENVIRONMENT drops a reply sender with no send, so the
  awaiter wakes `Disconnected`. This is the message-loss action the derive step's
  `environment nondeterminism` hole asked for: `action loseReply(id)` removes the
  id from the registry and sets `status[id] = Disconnected` WITHOUT counting a
  delivery.
- Guard every `nondet id = someSet.oneOf()` with `someSet != Set()` inside the
  `step` disjunct, so the TypeScript simulator never picks from an empty set.

Also flag races you deliberately abstracted away rather than hiding them. The
bridge folds `call_async`'s real check-then-insert into one atomic action and
says so: see the `AGENT-TODO: refine — real code checks abort then inserts
NON-atomically` comment on `action callAsync`. An honest abstraction names the gap.

---

## 4. Translating a LOCKED PROSE CONTRACT into invariants

This is the core skill. A module's doc comment states guarantees in prose; each
becomes a `val <name>: bool` you can `quint verify`. Map them explicitly.

### Worked example (a): the async-oneshot bridge

The prose contract lives in `bridge_async.rs` (the "Hang-safety … conditions A–J"
section). Three guarantees, three invariants (`specs/bridge_async.qnt`):

| Prose contract (bridge_async.rs) | Invariant |
| --- | --- |
| "`deliver` does `pending.remove(&id)` THEN `tx.send` → a second deliver finds nothing and no-ops" (single resolution, condition E/J) | `val atMostOnceInv = deliveries.keys().forall(id => deliveries.get(id) <= 1)` |
| "`abort` drains ALL pending senders" (condition B/I) | `val abortDrainsInv = aborted implies (pending == Set())` |
| "no reply without a matching pending call; a resolved id leaves the registry" | `val registryAccountingInv = pending.forall(id => status.get(id) == Pending)` |

The mapping is mechanical once the state abstraction is right: single-resolution
is a per-id counter capped at 1; drain-on-abort is a postcondition on `pending`;
registry accounting is an agreement between the keyset and the per-id status.

**Prove the invariants are load-bearing.** Seed the exact bug each one forbids
into a COPY of the spec and check `quint verify` FAILS with a counterexample:

- `specs/bridge_async_buggy_double_resolve.qnt` drops the remove-before-send, so
  an id is delivered twice → `atMostOnceInv` counterexample.
- `specs/bridge_async_buggy_lost_reply.qnt` makes `abort` skip the drain, so a
  pending id survives an abort → `abortDrainsInv` counterexample.

A spec whose invariants no bug can violate is proving nothing.

### Worked example (b): the fluessig callback contract

The fluessig extension-runtime callback contract (see
`notes/fluessig-integration.md`, `crates/pidgin-extensions/src/runner/`) reads, in
prose: callbacks are **forward-only, synchronous, void, single-arg**,
`Box<dyn Fn(Args) + Send + Sync>`; they may run on **any thread** and must
**NEVER block the host runtime**; a subscription **register → returns an
unsubscribe handle whose drop removes the listener**.

You do not need to build the fluessig spec to see the translation. Sketch the
state — `subscribed: Set[int]` (live listener ids) and a flag
`invokedAfterUnsub: bool` — with actions `subscribe(id)`, `invoke(id)` (guarded
`subscribed.contains(id)`), and `unsubscribe(id)` (`subscribed' =
subscribed.exclude(Set(id))`). Then the prose guarantee maps directly:

| Prose contract (fluessig) | Invariant sketch |
| --- | --- |
| "after unsubscribe, the callback is never invoked again" (drop removes the listener) | model `invoke(id)` so it sets `invokedAfterUnsub' = true` when `not subscribed.contains(id)`, and prove `val unsubIsFinal = not invokedAfterUnsub` — no reachable state invokes a dropped listener |
| "callbacks must never block the host runtime" | a liveness/shape property, not a safety one over this state; encode as "no action both takes a callback step AND leaves the host action pending" — or keep it a documented non-goal of the model if the state does not carry runtime-occupancy |
| "single-arg, void, forward-only" | structural — enforced by the seam types, not a reachability invariant; note it as out of scope rather than faking a check |

The lesson the two examples share: a guarantee about **what can never happen**
(double resolve, invoke-after-unsub) becomes a safety `val` over a small state
abstraction; a guarantee about **shape/types** (single-arg, void) is enforced by
the seam and named out of scope, not faked.

---

## 5. Run, then verify, then export a trace

1. `quint typecheck specs/<name>.qnt`
2. `quint run --backend typescript --invariant allInvariants --max-samples 200
   --max-steps 12 specs/<name>.qnt` — fast randomized search; catches shallow
   bugs before the SMT solver. (The `--backend typescript` avoids the Rust
   evaluator download; the built-in evaluator is self-contained.)
3. `quint verify --invariant <each> --max-steps 5 specs/<name>.qnt` — Apalache
   (needs a JVM; it auto-downloads on first run). Bound `--max-steps` to keep the
   solve fast. **Run verify steps SEQUENTIALLY** — Apalache serves each over a
   gRPC server on a fixed port, so two concurrent `quint verify` invocations
   collide (RST_STREAM). CI enforces this by ordering the steps.
4. Export a replayable witness:
   `quint run --backend typescript --init initDet --step stepDet --max-steps 4
   --max-samples 1 --out-itf specs/traces/<name>_ok.itf.json <gen>.qnt`.
   Make the states carry a `lastEvent` / `lastId` label so the trace is directly
   replayable. The bridge uses a tiny deterministic driver
   (`specs/traces/bridge_ok_gen.qnt`) so the exported trace is reproducible with
   no random seed.

---

## 6. Close the loop against real code (optional but decisive)

Replay the exported ITF trace against the real implementation in a unit test and,
after each step, assert the real data structure matches the model's state. The
bridge does this in `crates/pidgin-napi/src/bridge_async.rs`
(`mod itf_replay`): it reads `specs/traces/bridge_ok.itf.json`, drives the REAL
`BridgeShared::deliver` / `abort`, and asserts the real `pending` keyset equals
the model's `pending` set at every step. That is what makes the spec
load-bearing rather than a diagram — if the model and the code drift, the test
goes red.

---

## Quick checklist

- [ ] Commit the `hinzu model` output under `specs/derived/` as the seed; document
      live-vs-`--bodies` provenance in its header.
- [ ] Write the finished model as a new `specs/<name>.qnt`; leave the seed's
      GENERATED regions alone.
- [ ] Pick a state abstraction for each CFG hole (§2); collapse to protocol
      states, model loops as set ops.
- [ ] Add environment actions for the nondeterminism holes (§3); flag abstracted
      races honestly.
- [ ] Translate each prose guarantee to a `val ...Inv` (§4); prove each is
      load-bearing with a seeded-bug copy that `quint verify` catches.
- [ ] `typecheck` → `run` → `verify` (sequential) → `--out-itf` (§5).
- [ ] Replay the trace against real code (§6).
