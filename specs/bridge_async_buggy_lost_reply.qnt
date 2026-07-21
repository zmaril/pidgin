// bridge_async.qnt — a machine-checked model of pidgin's async-oneshot Rust->JS
// callback bridge (crates/pidgin-napi/src/bridge_async.rs).
//
// ============================================================================
// PROVENANCE — derived-by-tool vs. finished-by-agent
// ============================================================================
// This model is Phase B/C of the code-derived verification pipeline. Its SEED
// is specs/derived/bridge_async.derived.qnt — the raw `hinzu model --emit quint`
// skeleton lowered from the LIVE StableMIR MIR of bridge_async.rs (Phase A,
// hinzu #43). The seed contributed, and this file keeps in spirit:
//   * the state-variable framing (one registry keyset + per-id status), and
//   * the three per-function actions (deliver / abort / call_async), whose real
//     extracted CFGs the seed printed as `// AGENT-TODO: encode control flow`
//     holes (deliver: 19 blocks incl. the Option remove-then-send SwitchInt;
//     abort: 22 blocks incl. the drain loop).
// Everything the seed marked AGENT-TODO is FINISHED here by hand, because the
// registry/oneshot/abort-drain protocol is not numeric-local IR the tool can
// lower faithfully:
//   * DERIVED-FROM-SEED : the set of modeled operations (callAsync, deliver=
//     jsResolve*, abort) and that deliver is a remove-then-send on an Option.
//   * FINISHED-BY-AGENT : the protocol state abstraction (Set/Map registry),
//     the init/environment nondeterminism (loseReply), and the invariants.
//
// ============================================================================
// THE REAL SEMANTICS BEING MODELED (bridge_async.rs line ranges on main)
// ============================================================================
//  * Envelope {id,kind,payload,aborted}; id from next_id: AtomicU64 fetch_add
//    (call_async, ll. ~366-378).
//  * Registry pending: Mutex<HashMap<u64, oneshot::Sender<BridgeOutcome>>> on
//    BridgeShared (ll. ~232-247).
//  * call_async fast-path `if signal.is_aborted() -> Err(Aborted)` then alloc id
//    + `pending.insert(id, tx)` (ll. ~362-368) — modeled by callAsync /
//    callWhenAborted. The real check-then-insert is NON-atomic (a TOCTOU seam);
//    see the AGENT-TODO on callAsync.
//  * deliver(id,outcome): `pending.remove(&id)` THEN `tx.send(outcome)` (send
//    error ignored). Remove-before-send is the SINGLE-RESOLUTION guarantee: a
//    second deliver for the same id finds nothing and no-ops (ll. ~279-287,
//    condition E/J). Modeled by jsResolveValue/jsResolveError + jsResolveStale.
//  * abort(): trip signal; drain() ALL pending senders, send(Aborted) into each
//    before drop (ll. ~293-307, condition B/I). Modeled by abort.
//  * rx.await sender-dropped => Err(Disconnected) (ll. ~388-392, condition D/I).
//    Modeled by loseReply as an ENVIRONMENT action.
//
// Verify locally:
//   quint typecheck specs/bridge_async.qnt
//   quint run --invariant allInvariants --max-samples 200 specs/bridge_async.qnt
//   quint verify --invariant atMostOnceInv        specs/bridge_async.qnt
//   quint verify --invariant registryAccountingInv specs/bridge_async.qnt
//   quint verify --invariant abortDrainsInv        specs/bridge_async.qnt
// ============================================================================

module bridge_async {
  // !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
  // SEEDED BUG (lost reply on abort): abort() trips the signal but DOES NOT
  // drain the registry -- it leaves pending untouched and never sends Aborted.
  // The real abort() drains ALL pending senders. With the drain dropped, an
  // in-flight id is left parked in the registry after abort, so its awaiting
  // task never wakes (a lost reply) and `aborted && pending != {}` holds,
  // violating abortDrainsInv. `quint verify --invariant abortDrainsInv` must
  // return a counterexample.
  // !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
  // ---- Types ---------------------------------------------------------------

  // The lifecycle of one request id. Mirrors the BridgeOutcome/BridgeError
  // outcomes a call_async awaiter can observe.
  type Status =
    | Pending        // in the registry, reply outstanding
    | ResolvedValue  // deliver(Value) -> Ok
    | ResolvedError  // deliver(Error) -> Err(Errored)
    | AbortedS       // abort-drained, or fast-path Err(Aborted)
    | Disconnected   // sender dropped with no send -> Err(Disconnected)

  // The event that produced a state — makes every trace self-describing and
  // directly replayable against the real Rust registry (see the ITF replay
  // test in bridge_async.rs).
  type Event =
    | EvInit
    | EvCall
    | EvResolveValue
    | EvResolveError
    | EvAbort
    | EvLoseReply

  // ---- State ---------------------------------------------------------------

  var nextId: int              // next_id: AtomicU64 (fetch_add allocator)
  var pending: Set[int]        // the registry keyset (pending.keys())
  var status: int -> Status    // per-id lifecycle
  var deliveries: int -> int   // per-id delivery count — the single-resolution ledger
  var aborted: bool            // signal.is_aborted()
  var jsInbox: Set[int]        // ids dispatched to JS with a reply still outstanding
  var lastEvent: Event         // trace label
  var lastId: int              // trace label (-1 when not id-specific)

  // ---- init ----------------------------------------------------------------

  action init = all {
    nextId' = 0,
    pending' = Set(),
    status' = Map(),
    deliveries' = Map(),
    aborted' = false,
    jsInbox' = Set(),
    lastEvent' = EvInit,
    lastId' = -1,
  }

  // ---- Actions -------------------------------------------------------------

  // call_async, the happy path: check `not aborted`, alloc an id, insert into
  // the registry, dispatch to JS. Modeled as ONE atomic action.
  //
  // AGENT-TODO: refine — real code checks abort then inserts NON-atomically
  // (call_async ll. ~362-368: `if signal.is_aborted()` ... later
  // `pending.insert(id, tx)`). An abort landing between the check and the insert
  // is a TOCTOU race this atomic abstraction folds away; the abort-drain still
  // catches such an id on the next abort, but a faithful race model would split
  // this into check / insert steps with an interleaved abort. Flagged honestly
  // rather than silently abstracted.
  action callAsync = all {
    not(aborted),
    nextId' = nextId + 1,
    pending' = pending.union(Set(nextId)),
    status' = status.put(nextId, Pending),
    deliveries' = deliveries.put(nextId, 0),
    jsInbox' = jsInbox.union(Set(nextId)),
    aborted' = aborted,
    lastEvent' = EvCall,
    lastId' = nextId,
  }

  // call_async fast-path when already aborted: `Err(Aborted)` with NO registry
  // insert (ll. ~362-364). Allocates an id, marks it AbortedS, touches neither
  // pending nor jsInbox.
  action callWhenAborted = all {
    aborted,
    nextId' = nextId + 1,
    pending' = pending,
    status' = status.put(nextId, AbortedS),
    deliveries' = deliveries.put(nextId, 0),
    jsInbox' = jsInbox,
    aborted' = aborted,
    lastEvent' = EvCall,
    lastId' = nextId,
  }

  // deliver(id, Value) for a still-pending id: remove-then-send. The removal is
  // what makes a second deliver a no-op (jsResolveStale below).
  action jsResolveValue(id: int): bool = all {
    jsInbox.contains(id),
    pending.contains(id),
    pending' = pending.exclude(Set(id)),
    jsInbox' = jsInbox.exclude(Set(id)),
    status' = status.put(id, ResolvedValue),
    deliveries' = deliveries.put(id, deliveries.get(id) + 1),
    nextId' = nextId,
    aborted' = aborted,
    lastEvent' = EvResolveValue,
    lastId' = id,
  }

  // deliver(id, Error) for a still-pending id: same shape, Err(Errored) outcome.
  action jsResolveError(id: int): bool = all {
    jsInbox.contains(id),
    pending.contains(id),
    pending' = pending.exclude(Set(id)),
    jsInbox' = jsInbox.exclude(Set(id)),
    status' = status.put(id, ResolvedError),
    deliveries' = deliveries.put(id, deliveries.get(id) + 1),
    nextId' = nextId,
    aborted' = aborted,
    lastEvent' = EvResolveError,
    lastId' = id,
  }

  // deliver(id, ..) for an id NOT in the registry: `pending.remove(&id)` finds
  // nothing, so the whole call no-ops (ll. ~279-287). This is the model of the
  // single-resolution guarantee: it must NOT touch status or deliveries.
  action jsResolveStale(id: int): bool = all {
    not(pending.contains(id)),
    pending' = pending,
    jsInbox' = jsInbox,
    status' = status,
    deliveries' = deliveries,
    nextId' = nextId,
    aborted' = aborted,
    lastEvent' = EvResolveValue,
    lastId' = id,
  }

  // abort(): trip the signal and DRAIN every pending sender, sending Aborted
  // into each exactly once before drop (ll. ~293-307). After abort the registry
  // is empty.
  action abort = all {
    aborted' = true,
    status' = status,        // BUG: no drain -- pending senders never get Aborted
    deliveries' = deliveries,
    jsInbox' = jsInbox,      // BUG: outstanding replies left parked
    pending' = pending,      // BUG: registry NOT emptied on abort
    nextId' = nextId,
    lastEvent' = EvAbort,
    lastId' = -1,
  }

  // ENVIRONMENT nondeterminism (the AGENT-TODO the seed asks for): the JS worker
  // drops the reply sender with no send. `rx.await` then returns Err(_) =>
  // Err(Disconnected) (ll. ~388-392, condition D/I). The id leaves the registry
  // WITHOUT a delivery counted — a lost reply, not a resolution.
  action loseReply(id: int): bool = all {
    jsInbox.contains(id),
    pending.contains(id),
    pending' = pending.exclude(Set(id)),
    jsInbox' = jsInbox.exclude(Set(id)),
    status' = status.put(id, Disconnected),
    deliveries' = deliveries,
    nextId' = nextId,
    aborted' = aborted,
    lastEvent' = EvLoseReply,
    lastId' = id,
  }

  // ---- step ----------------------------------------------------------------

  // Ids that can still receive a reply (dispatched + registered). Guarding the
  // nondet pickers on non-emptiness keeps `quint run` from picking from an empty
  // set.
  val liveIds: Set[int] = pending.intersect(jsInbox)
  // Allocated ids no longer in the registry — a stale deliver targets these.
  val staleIds: Set[int] = status.keys().exclude(pending)

  action step = any {
    callAsync,
    callWhenAborted,
    abort,
    all {
      liveIds != Set(),
      nondet id = liveIds.oneOf()
      jsResolveValue(id),
    },
    all {
      liveIds != Set(),
      nondet id = liveIds.oneOf()
      jsResolveError(id),
    },
    all {
      liveIds != Set(),
      nondet id = liveIds.oneOf()
      loseReply(id),
    },
    all {
      staleIds != Set(),
      nondet id = staleIds.oneOf()
      jsResolveStale(id),
    },
  }

  // ---- Invariants ----------------------------------------------------------

  // SINGLE RESOLUTION: every id is delivered at most once. This is the prose
  // contract "deliver removes-then-sends => a second deliver no-ops".
  val atMostOnceInv: bool =
    deliveries.keys().forall(id => deliveries.get(id) <= 1)

  // REGISTRY ACCOUNTING: an id in the registry has exactly Pending status; the
  // moment it resolves it leaves the registry. Prose: "no reply without a
  // matching pending call".
  val registryAccountingInv: bool =
    pending.forall(id => status.get(id) == Pending)

  // ABORT DRAINS: once aborted, the registry is empty (drain + fast-path).
  // Holds under the atomic-callAsync abstraction.
  val abortDrainsInv: bool =
    aborted implies (pending == Set())

  // Convenience conjunction for `quint run`.
  val allInvariants: bool = and {
    atMostOnceInv,
    registryAccountingInv,
    abortDrainsInv,
  }

  // ---- A meaningful, replayable run ---------------------------------------
  // call -> resolve -> call -> abort. Exported to ITF and replayed against the
  // real Rust registry (see the #[cfg(test)] module in bridge_async.rs).
  run bridgeHappyRun =
    init
      .then(callAsync)          // id 0 -> pending {0}
      .then(jsResolveValue(0))  // deliver(0, Value) -> resolved, registry empty
      .then(callAsync)          // id 1 -> pending {1}
      .then(abort)              // drain: id 1 -> AbortedS, registry empty
      .expect(allInvariants)
}
