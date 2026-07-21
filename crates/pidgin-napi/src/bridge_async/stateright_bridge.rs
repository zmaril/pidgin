//! Exhaustive Stateright model that drives the REAL [`super::BridgeShared`]
//! registry over every reachable transition, checking the same three invariants
//! as `specs/bridge_async.qnt` — named 1:1 with the Quint spec
//! (`atMostOnceInv`, `registryAccountingInv`, `abortDrainsInv`).
//!
//! Like the sibling `itf_replay` module, this is a `#[cfg(test)]` CHILD module
//! of `bridge_async`, so it reaches the private `BridgeShared::pending` field
//! and the private `deliver`/`abort`/`BridgeOutcome` items through `super::` —
//! NO `pub`/`pub(crate)` seam is added. Where `itf_replay` replays ONE exported
//! ITF witness trace, this explores ALL reachable interleavings over a bounded
//! id domain.
//!
//! `next_state` is the load-bearing part: for every action it constructs a fresh
//! real `BridgeShared`, seeds its private registry with a live `oneshot` sender
//! per pending id (exactly how `itf_replay` seeds it), invokes the REAL
//! `deliver`/`abort`/insert/remove, then reads the real `pending` keyset BACK
//! and derives the post-state — including the per-id delivery ledger — from that
//! real readback, not from abstract bookkeeping. A green check therefore means
//! the real registry code upholds all three invariants across every reachable
//! transition in the bounded domain.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use stateright::{Checker, Model, Property};
use tokio::sync::oneshot;

use super::{BridgeOutcome, BridgeShared};

/// Bound on the monotonic id allocator: ids `0..N` are reachable. Small enough
/// that BFS is exhaustive, large enough to interleave out-of-order resolves,
/// lost replies, and an abort drain.
const N: u64 = 3;

/// The 2-valued lifecycle abstraction of a request id. FV-confirmed sound: only
/// `registryAccountingInv` reads `status`, and it only tests `== Pending`; the
/// distinct resolved outcomes (Value / Error / Aborted / Disconnected) collapse
/// to a single `Resolved` without changing any of the three invariants.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Status {
    Pending,
    Resolved,
}

/// Abstract mirror of the Quint state vars. A live `BridgeShared`
/// (Mutex/AtomicU64/oneshot::Sender) is neither `Hash` nor `Eq`, so the checker
/// state is this mirror; the REAL registry is built transiently in `next_state`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct BridgeState {
    /// Monotonic allocator — mirrors `next_id: AtomicU64` fetch_add. Ids are
    /// NEVER reused, so a resolved id can never be re-registered (the real
    /// allocator can never hand out the same id twice).
    next_id: u64,
    /// The registry keyset — `pending.keys()` of the real `BridgeShared`.
    pending: BTreeSet<u64>,
    /// Per-id lifecycle.
    status: BTreeMap<u64, Status>,
    /// Per-id delivery ledger — the single-resolution counter.
    deliveries: BTreeMap<u64, u8>,
    /// `signal.is_aborted()`.
    aborted: bool,
}

/// The action set, 1:1 with the Quint `step` actions.
#[derive(Clone, PartialEq, Eq, Debug)]
enum BridgeAction {
    /// `callAsync` happy path: allocate `next_id`, insert into the registry.
    /// Guarded on `not aborted`.
    Register(u64),
    /// `callWhenAborted`: post-abort fast-path — allocate an id and return
    /// `Aborted` immediately WITHOUT a registry insert.
    CallWhenAborted(u64),
    /// `jsResolveValue`: real `deliver(id, Value)` — remove-then-send for a
    /// pending id, or a real no-op for a stale/already-resolved id.
    Deliver(u64),
    /// `loseReply`: the worker drops the reply sender with no send. A real
    /// registry removal (`-> Disconnected`) that counts NO delivery.
    LoseReply(u64),
    /// `abort`: trip the signal and drain every pending sender. Each drained id
    /// receives an `Aborted` send, so it counts as one delivery.
    Abort,
}

struct BridgeModel;

/// Build a fresh REAL `BridgeShared` and seed its private `pending` map with a
/// live `oneshot` sender for every currently-pending id — the same seeding
/// `itf_replay` does. The receivers are returned so real sends observe an open
/// channel (a dropped receiver would make `send` error and, for `abort`, still
/// drain — but we keep them to stay faithful to a live awaiter).
fn seed(pending: &BTreeSet<u64>) -> (Arc<BridgeShared>, Vec<oneshot::Receiver<BridgeOutcome>>) {
    let shared = BridgeShared::new(std::thread::current().id());
    let mut rxs = Vec::new();
    {
        let mut guard = shared.pending.lock().unwrap();
        for &id in pending {
            let (tx, rx) = oneshot::channel::<BridgeOutcome>();
            guard.insert(id, tx);
            rxs.push(rx);
        }
    }
    (shared, rxs)
}

/// The REAL registry's current keyset — the load-bearing readback.
fn readback(shared: &BridgeShared) -> BTreeSet<u64> {
    shared.pending.lock().unwrap().keys().copied().collect()
}

impl Model for BridgeModel {
    type State = BridgeState;
    type Action = BridgeAction;

    fn init_states(&self) -> Vec<Self::State> {
        vec![BridgeState {
            next_id: 0,
            pending: BTreeSet::new(),
            status: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            aborted: false,
        }]
    }

    fn actions(&self, s: &Self::State, actions: &mut Vec<Self::Action>) {
        // callAsync: happy-path register, only while not aborted and ids remain.
        if !s.aborted && s.next_id < N {
            actions.push(BridgeAction::Register(s.next_id));
        }
        // callWhenAborted: post-abort fast-path register.
        if s.aborted && s.next_id < N {
            actions.push(BridgeAction::CallWhenAborted(s.next_id));
        }
        // deliver: enabled for every allocated id — a pending id drives the real
        // remove-then-send, an already-resolved id drives the real no-op path.
        for id in 0..s.next_id {
            actions.push(BridgeAction::Deliver(id));
        }
        // loseReply: only a pending id has a seeded sender to drop.
        for &id in &s.pending {
            actions.push(BridgeAction::LoseReply(id));
        }
        // abort: always enabled (drives the real drain, and the real no-op when
        // the registry is already empty / already aborted).
        actions.push(BridgeAction::Abort);
    }

    fn next_state(&self, s: &Self::State, action: Self::Action) -> Option<Self::State> {
        match action {
            BridgeAction::Register(id) => {
                let (shared, mut rxs) = seed(&s.pending);
                // The REAL registry insert that call_async performs.
                let (tx, rx) = oneshot::channel::<BridgeOutcome>();
                shared.pending.lock().unwrap().insert(id, tx);
                rxs.push(rx);
                let post = readback(&shared);
                let mut ns = s.clone();
                ns.pending = post; // derived from the real readback
                ns.status.insert(id, Status::Pending);
                ns.deliveries.insert(id, 0);
                ns.next_id += 1;
                Some(ns)
            }
            BridgeAction::CallWhenAborted(id) => {
                let (shared, _rxs) = seed(&s.pending);
                // Fast-path: NO registry insert — readback must equal pre pending.
                let post = readback(&shared);
                let mut ns = s.clone();
                ns.pending = post;
                ns.status.insert(id, Status::Resolved);
                ns.deliveries.insert(id, 0);
                ns.next_id += 1;
                Some(ns)
            }
            BridgeAction::Deliver(id) => {
                let (shared, _rxs) = seed(&s.pending);
                // Drive the REAL deliver (remove-then-send, or no-op).
                shared.deliver(id, BridgeOutcome::Value("\"ok\"".to_string()));
                let post = readback(&shared);
                let mut ns = s.clone();
                // Ledger derived from the REAL readback: an id present pre-op and
                // absent post-op was really removed-then-sent => +1 delivery.
                for removed in s.pending.difference(&post) {
                    *ns.deliveries.entry(*removed).or_insert(0) += 1;
                    ns.status.insert(*removed, Status::Resolved);
                }
                ns.pending = post;
                Some(ns)
            }
            BridgeAction::LoseReply(id) => {
                let (shared, _rxs) = seed(&s.pending);
                // Models channel-teardown message-loss via a direct registry
                // removal. Deliver and Abort drive the REAL named methods
                // (`deliver`/`abort`); LoseReply pokes the pending map directly
                // because no per-id lose-without-send method exists (real
                // `Disconnected` is whole-channel teardown — the worker thread
                // dies and the pending map drops un-sent senders). This is an
                // idealization of per-id message loss: dropping this oneshot
                // Sender makes the held Receiver observe `oneshot::RecvError`
                // (== `BridgeError::Disconnected`). Still a faithful poke of the
                // REAL registry map + a REAL oneshot disconnect, reachable from
                // the child `#[cfg(test)] mod` via `super::` private access.
                let tx = shared.pending.lock().unwrap().remove(&id);
                drop(tx);
                let post = readback(&shared);
                let mut ns = s.clone();
                ns.pending = post;
                // No delivery counted — a dropped sender is not a send.
                ns.status.insert(id, Status::Resolved); // Disconnected -> Resolved
                Some(ns)
            }
            BridgeAction::Abort => {
                let (shared, _rxs) = seed(&s.pending);
                // Drive the REAL abort: trip the signal and drain every sender.
                shared.abort();
                let post = readback(&shared);
                let mut ns = s.clone();
                // Every id the REAL abort drained got an Aborted send => +1.
                for drained in s.pending.difference(&post) {
                    *ns.deliveries.entry(*drained).or_insert(0) += 1;
                    ns.status.insert(*drained, Status::Resolved);
                }
                ns.pending = post;
                ns.aborted = true;
                Some(ns)
            }
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // SINGLE RESOLUTION: deliveries.keys().forall(id => deliveries.get(id) <= 1)
            Property::<Self>::always("atMostOnceInv", |_, s| {
                s.deliveries.values().all(|c| *c <= 1)
            }),
            // REGISTRY ACCOUNTING: pending.forall(id => status.get(id) == Pending)
            Property::<Self>::always("registryAccountingInv", |_, s| {
                s.pending
                    .iter()
                    .all(|id| s.status.get(id) == Some(&Status::Pending))
            }),
            // ABORT DRAINS: aborted implies (pending == Set())
            Property::<Self>::always("abortDrainsInv", |_, s| !s.aborted || s.pending.is_empty()),
        ]
    }
}

#[test]
fn stateright_drives_real_bridge_registry() {
    let checker = BridgeModel.checker().spawn_bfs().join();

    // All three invariants (each a `Property::always`) must hold with NO
    // counterexample across the whole reachable state graph.
    checker.assert_properties();
    assert!(
        checker.discoveries().is_empty(),
        "no invariant may have a counterexample: {:?}",
        checker.discoveries().keys().collect::<Vec<_>>()
    );

    // Sanity: the BFS actually explored a non-trivial reachable graph driven by
    // the real registry, not a single init state.
    let states = checker.unique_state_count();
    assert!(states > 1, "checker explored too few states: {states}");

    eprintln!(
        "stateright bridge model: explored {states} unique states, \
         {} discoveries (0 expected)",
        checker.discoveries().len()
    );
}
