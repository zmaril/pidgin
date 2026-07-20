//! The clock seam: injectable wall-clock time and deterministic timer advance.
//!
//! # What this abstracts in pi
//!
//! pi's tests drive time through Vitest's fake-timer surface, and the
//! mock-seam inventory (`notes/mock-inventory.md`) found this is larger than it
//! first appears: 29 `vi.useFakeTimers` sites, 58 `vi.advanceTimers*` calls, and
//! 16 `vi.setSystemTime` calls. Roughly twelve of the `setSystemTime` sites bake
//! an injected wall-clock `now` into an assertion. The inventory enumerates five
//! `now`-injection patterns the seam must serve:
//!
//! 1. OAuth device-code and access-token poll timestamps asserted as
//!    `startTime + N * interval`.
//! 2. Token and credential expiry computed as `now + expires_in * 1000 - skew`
//!    (with a one-hour fallback when the field is absent).
//! 3. SSE retry and backoff delay derived from a `retry-after` header carrying
//!    an absolute future time.
//! 4. Elapsed-gated reconnect and session-lifetime logic (a test advances `now`
//!    by tens of minutes).
//! 5. The `uuidv7` embedded 48-bit timestamp, pinned and asserted inside the
//!    UUID string.
//!
//! So the seam must expose **both** a settable/queryable `now` (patterns 1-5)
//! **and** deterministic timer advance (the 58 `advanceTimers` sites). Those are
//! two distinct capabilities, and both are first-class here: [`Clock::now_ms`]
//! reports wall-clock time, while [`Timers::advance`] fires scheduled callbacks
//! deterministically without real elapsed time.
//!
//! # Implementations
//!
//! - [`SystemClock`] — the production clock, reading `SystemTime::now()` and
//!   scheduling real timers. This is what ships.
//! - [`FakeClock`] — the deterministic test clock. `now` is settable and
//!   advanceable; timers scheduled against it fire only when [`FakeClock::advance`]
//!   crosses their deadline, in deadline order, exactly reproducing Vitest's
//!   `advanceTimersByTime`.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of wall-clock time, in milliseconds since the Unix epoch.
///
/// Mirrors every pi read of `Date.now()`. Production code takes a `&dyn Clock`
/// (or a generic `C: Clock`) instead of calling `Date.now()` directly, so a
/// test can inject a [`FakeClock`] and pin the five `now`-injection patterns
/// documented on this module.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch — pi's `Date.now()`.
    fn now_ms(&self) -> i64;
}

/// Deterministic timer scheduling, the second half of the clock seam.
///
/// Separated from [`Clock`] because they are distinct capabilities: reading
/// `now` versus scheduling and firing delayed work. The production
/// [`SystemClock`] fulfils both; a [`FakeClock`] lets a test enqueue timers and
/// fire them by advancing virtual time, reproducing pi's `vi.advanceTimersByTime`.
pub trait Timers: Send + Sync {
    /// Register a timer to fire `delay_ms` in the future. Returns a
    /// [`TimerId`] that [`Timers::clear`] can cancel (pi's `clearTimeout`).
    fn set_timeout(&self, delay_ms: u64, callback: Box<dyn FnOnce() + Send>) -> TimerId;
    /// Cancel a pending timer, mirroring `clearTimeout`. A no-op if it already
    /// fired or never existed.
    fn clear(&self, id: TimerId);
}

/// An opaque handle to a scheduled timer, returned by [`Timers::set_timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(pub u64);

/// The production clock: real wall-clock time and real timers.
///
/// [`Clock::now_ms`] reads `SystemTime::now()`; [`Timers::set_timeout`] spawns a
/// thread that sleeps and fires. This is the implementation that ships in the
/// Node/host binding and every non-test caller.
#[derive(Debug, Default, Clone)]
pub struct SystemClock;

impl SystemClock {
    /// Construct the production clock.
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let now = SystemTime::now();
        match now.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            // Before the epoch (clock skewed backwards): negative offset.
            Err(e) => -(e.duration().as_millis() as i64),
        }
    }
}

impl Timers for SystemClock {
    fn set_timeout(&self, delay_ms: u64, callback: Box<dyn FnOnce() + Send>) -> TimerId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = TimerId(NEXT.fetch_add(1, AtomicOrdering::Relaxed));
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            callback();
        });
        id
    }

    fn clear(&self, _id: TimerId) {
        // Real timers run on detached threads; cancellation is best-effort and
        // unused by the ported seams, so this is intentionally a no-op. The
        // deterministic [`FakeClock`] honors `clear` exactly.
    }
}

struct ScheduledTimer {
    deadline_ms: u64,
    seq: u64,
    id: TimerId,
    callback: Box<dyn FnOnce() + Send>,
}

// The heap orders by earliest deadline, then insertion order, so ties fire in
// scheduling order exactly like a real event loop.
impl PartialEq for ScheduledTimer {
    fn eq(&self, other: &Self) -> bool {
        self.deadline_ms == other.deadline_ms && self.seq == other.seq
    }
}
impl Eq for ScheduledTimer {}
impl Ord for ScheduledTimer {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` is a max-heap, we want the earliest deadline on
        // top, and the lowest sequence number to break ties.
        other
            .deadline_ms
            .cmp(&self.deadline_ms)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for ScheduledTimer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct FakeClockState {
    now_ms: i64,
    virtual_ms: u64,
    next_seq: u64,
    next_id: u64,
    timers: BinaryHeap<ScheduledTimer>,
}

/// The deterministic test clock: a settable `now` plus advanceable timers.
///
/// Cheap to clone — clones share the same underlying state, so a clock handed to
/// production code and the one a test drives are the same clock. This mirrors
/// how a Vitest test both sets the system time and advances timers on one global
/// fake-timer instance.
///
/// ```
/// use pidgin_ai::seams::clock::{Clock, FakeClock, Timers};
/// use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
///
/// let clock = FakeClock::new(1_700_000_000_000);
/// assert_eq!(clock.now_ms(), 1_700_000_000_000);
///
/// // Pattern 1/2/4: pin now, assert an elapsed offset.
/// clock.set_now_ms(1_700_000_000_000 + 30 * 60 * 1000);
/// assert_eq!(clock.now_ms(), 1_700_000_000_000 + 1_800_000);
///
/// // Deterministic timer advance (the 58 advanceTimers sites).
/// let fired = Arc::new(AtomicBool::new(false));
/// let f = fired.clone();
/// clock.set_timeout(500, Box::new(move || f.store(true, Ordering::SeqCst)));
/// clock.advance(499);
/// assert!(!fired.load(Ordering::SeqCst));
/// clock.advance(1);
/// assert!(fired.load(Ordering::SeqCst));
/// ```
#[derive(Clone)]
pub struct FakeClock {
    state: Arc<Mutex<FakeClockState>>,
}

impl FakeClock {
    /// Create a fake clock whose `now` starts at `start_ms` (Unix ms).
    pub fn new(start_ms: i64) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeClockState {
                now_ms: start_ms,
                ..FakeClockState::default()
            })),
        }
    }

    /// Pin the wall clock to `now_ms`, mirroring `vi.setSystemTime`. Does not
    /// fire timers (matching Vitest, where `setSystemTime` and `advanceTimers`
    /// are separate operations).
    pub fn set_now_ms(&self, now_ms: i64) {
        self.state.lock().unwrap().now_ms = now_ms;
    }

    /// Advance virtual time by `delta_ms`, firing every timer whose deadline is
    /// crossed in deadline (then scheduling) order, and moving `now` forward by
    /// the same amount. This is `vi.advanceTimersByTime`.
    pub fn advance(&self, delta_ms: u64) {
        let target = {
            let mut state = self.state.lock().unwrap();
            state.now_ms += delta_ms as i64;
            state.virtual_ms + delta_ms
        };
        loop {
            let due = {
                let mut state = self.state.lock().unwrap();
                match state.timers.peek() {
                    Some(t) if t.deadline_ms <= target => state.timers.pop(),
                    _ => None,
                }
            };
            match due {
                Some(timer) => {
                    // Move virtual time to the timer's deadline before firing, so
                    // a callback that schedules another timer measures its delay
                    // from the correct instant.
                    self.state.lock().unwrap().virtual_ms = timer.deadline_ms;
                    (timer.callback)();
                }
                None => break,
            }
        }
        self.state.lock().unwrap().virtual_ms = target;
    }

    /// Number of timers still pending. Test-introspection helper with no pi
    /// analog; handy for asserting all scheduled work fired.
    pub fn pending_timers(&self) -> usize {
        self.state.lock().unwrap().timers.len()
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.state.lock().unwrap().now_ms
    }
}

impl Timers for FakeClock {
    fn set_timeout(&self, delay_ms: u64, callback: Box<dyn FnOnce() + Send>) -> TimerId {
        let mut state = self.state.lock().unwrap();
        let deadline_ms = state.virtual_ms + delay_ms;
        state.next_seq += 1;
        state.next_id += 1;
        let seq = state.next_seq;
        let id = TimerId(state.next_id);
        state.timers.push(ScheduledTimer {
            deadline_ms,
            seq,
            id,
            callback,
        });
        id
    }

    fn clear(&self, id: TimerId) {
        let mut state = self.state.lock().unwrap();
        // Rebuild the heap without the cleared timer. Timer volumes in tests are
        // tiny, so the O(n) rebuild is irrelevant.
        let kept: Vec<ScheduledTimer> = std::mem::take(&mut state.timers)
            .into_vec()
            .into_iter()
            .filter(|t| t.id != id)
            .collect();
        state.timers = BinaryHeap::from(kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as O};

    #[test]
    fn system_clock_now_is_after_2020() {
        // 2020-01-01 in ms; a sanity floor that does not flake.
        assert!(SystemClock::new().now_ms() > 1_577_836_800_000);
    }

    #[test]
    fn fake_clock_now_is_settable_and_advanceable() {
        let clock = FakeClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.set_now_ms(5_000);
        assert_eq!(clock.now_ms(), 5_000);
        clock.advance(250);
        assert_eq!(clock.now_ms(), 5_250);
    }

    #[test]
    fn fake_timers_fire_in_deadline_order() {
        let clock = FakeClock::new(0);
        let log = Arc::new(Mutex::new(Vec::new()));
        for (delay, tag) in [(300u64, "c"), (100, "a"), (200, "b")] {
            let log = log.clone();
            clock.set_timeout(delay, Box::new(move || log.lock().unwrap().push(tag)));
        }
        assert_eq!(clock.pending_timers(), 3);
        clock.advance(250);
        assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
        clock.advance(100);
        assert_eq!(*log.lock().unwrap(), vec!["a", "b", "c"]);
        assert_eq!(clock.pending_timers(), 0);
    }

    #[test]
    fn cleared_timer_does_not_fire() {
        let clock = FakeClock::new(0);
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let id = clock.set_timeout(
            100,
            Box::new(move || {
                c.fetch_add(1, O::SeqCst);
            }),
        );
        clock.clear(id);
        clock.advance(1_000);
        assert_eq!(count.load(O::SeqCst), 0);
    }

    #[test]
    fn nested_timer_measures_delay_from_its_deadline() {
        let clock = FakeClock::new(0);
        let clock2 = clock.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        clock.set_timeout(
            100,
            Box::new(move || {
                // Scheduled at virtual t=100; its 50ms child fires at t=150.
                let f = f.clone();
                clock2.set_timeout(50, Box::new(move || f.store(true, O::SeqCst)));
            }),
        );
        clock.advance(140);
        assert!(!fired.load(O::SeqCst));
        clock.advance(10);
        assert!(fired.load(O::SeqCst));
    }
}
