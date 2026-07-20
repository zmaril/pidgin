//! Session-scoped resource cleanup — the Rust port of pi-ai's
//! `session-resources.ts` (`packages/ai/src/session-resources.ts`).
//!
//! pi keeps a module-level `Set<SessionResourceCleanup>` of cleanup callbacks
//! that providers register when they open a session-scoped resource (pi's
//! `openai-codex-responses.ts:857` registers a websocket-closer this way). When
//! the coding-agent tears down a session (`agent-session.ts:841`) it calls
//! [`cleanup_session_resources`] with the session id; every registered callback
//! runs, and any errors are collected and re-raised together.
//!
//! # Faithful adaptations
//!
//! - pi's `Set<SessionResourceCleanup>` becomes an insertion-ordered
//!   `BTreeMap<u64, SessionResourceCleanup>` keyed by a monotonic id. Rust
//!   closures are neither comparable nor hashable, so a `Set` of them cannot be
//!   deduped/removed by value; the id is what pi's returned unregister function
//!   closes over instead of the callback reference. Monotonic ids preserve the
//!   `Set`'s insertion iteration order.
//! - pi's callbacks are `void`-returning and signal failure by `throw`. A Rust
//!   `Fn` cannot throw, so the callback returns `Result<(), CleanupError>`; a
//!   returned `Err` is the faithful analog of a thrown exception, matching how
//!   [`crate::compat`] models pi's throws as returned values (a `panic!` would
//!   cross the napi boundary as an uncatchable abort instead of a catchable JS
//!   throw).
//! - pi's `throw new AggregateError(errors, "Failed to cleanup session
//!   resources")` becomes a returned [`Err`] carrying
//!   [`AggregateCleanupError`], which owns the collected errors and renders the
//!   same message.

// straitjacket-allow-file:duplication — a faithful transcription of pi-ai's
// `session-resources.ts`; its registry shape parallels `compat.rs` by design.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// A boxed error raised by a cleanup callback — the value analog of pi's
/// `unknown` caught from a thrown exception.
pub type CleanupError = Box<dyn Error + Send + Sync>;

/// A session-resource cleanup callback — pi's `SessionResourceCleanup`
/// (`session-resources.ts:1`, `(sessionId?: string) => void`).
///
/// The optional `session_id` mirrors pi's optional `sessionId`. The callback
/// returns `Result<(), CleanupError>` rather than `void`: a returned `Err` is
/// the faithful Rust analog of pi's callback `throw`.
pub type SessionResourceCleanup =
    Arc<dyn Fn(Option<&str>) -> Result<(), CleanupError> + Send + Sync>;

/// The aggregate error re-raised when one or more cleanup callbacks fail — pi's
/// `AggregateError(errors, "Failed to cleanup session resources")`
/// (`session-resources.ts:22`).
///
/// It owns the collected callback errors (pi's `errors` array) and renders pi's
/// aggregate message.
#[derive(Debug)]
pub struct AggregateCleanupError {
    /// The errors thrown by individual cleanup callbacks, in registration
    /// order — pi's `errors` array.
    pub errors: Vec<CleanupError>,
}

impl fmt::Display for AggregateCleanupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Failed to cleanup session resources")
    }
}

impl Error for AggregateCleanupError {}

fn registry() -> &'static Mutex<BTreeMap<u64, SessionResourceCleanup>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<u64, SessionResourceCleanup>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn next_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Register a session-resource `cleanup` callback and return a function that
/// unregisters it. pi's `registerSessionResourceCleanup`
/// (`session-resources.ts:5-11`).
///
/// The returned closure removes exactly the registered callback (pi's
/// `sessionResourceCleanups.delete(cleanup)`); calling it more than once is a
/// harmless no-op, matching pi's `Set.delete`.
pub fn register_session_resource_cleanup(
    cleanup: SessionResourceCleanup,
) -> impl Fn() + Send + Sync {
    let id = next_id();
    registry().lock().unwrap().insert(id, cleanup);
    move || {
        registry().lock().unwrap().remove(&id);
    }
}

/// Run every registered cleanup callback, collecting any errors and re-raising
/// them together. pi's `cleanupSessionResources` (`session-resources.ts:13-24`).
///
/// Callbacks are snapshotted under the lock and invoked with the lock released:
/// pi runs them single-threaded with no lock held, and a callback may itself
/// register or clean up resources, which would deadlock a non-reentrant `Mutex`
/// held across the call. Every callback runs even if an earlier one fails (pi's
/// `try`/`catch` per iteration); the collected errors are returned as an
/// [`AggregateCleanupError`], mirroring pi's `throw new AggregateError(...)`.
pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<(), AggregateCleanupError> {
    let cleanups: Vec<SessionResourceCleanup> =
        registry().lock().unwrap().values().cloned().collect();
    let mut errors: Vec<CleanupError> = Vec::new();
    for cleanup in cleanups {
        if let Err(error) = cleanup(session_id) {
            errors.push(error);
        }
    }
    if !errors.is_empty() {
        return Err(AggregateCleanupError { errors });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::MutexGuard;

    // The cleanup registry is process-global (pi's module-level `Set`), so these
    // tests must not run concurrently. Each takes this lock and starts from a
    // cleared registry; the lock is poison-tolerant so a panicking test does not
    // wedge the others.
    fn serialized() -> MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry().lock().unwrap().clear();
        guard
    }

    // Registration runs every callback with the session id, and the returned
    // unregister function removes exactly its callback — pi's register/unregister
    // and `cleanupSessionResources` forwarding of `sessionId`.
    #[test]
    fn registers_runs_and_unregisters() {
        let _guard = serialized();

        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_a = seen.clone();
        let unregister_a = register_session_resource_cleanup(Arc::new(move |sid| {
            seen_a.lock().unwrap().push(sid.map(str::to_string));
            Ok(())
        }));
        let calls_b = Arc::new(AtomicUsize::new(0));
        let calls_b_cb = calls_b.clone();
        let _unregister_b = register_session_resource_cleanup(Arc::new(move |_sid| {
            calls_b_cb.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }));

        assert!(cleanup_session_resources(Some("session-1")).is_ok());
        assert_eq!(seen.lock().unwrap().as_slice(), &[Some("session-1".into())]);
        assert_eq!(calls_b.load(Ordering::Relaxed), 1);

        // Unregistering removes exactly one callback; the other still runs.
        unregister_a();
        assert!(cleanup_session_resources(None).is_ok());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(calls_b.load(Ordering::Relaxed), 2);

        // Calling the unregister function again is a harmless no-op.
        unregister_a();
    }

    // A failing callback does not short-circuit the others, and every error is
    // collected into the aggregate — pi's per-iteration `try`/`catch` plus
    // `throw new AggregateError(errors, "Failed to cleanup session resources")`.
    #[test]
    fn collects_all_errors_into_aggregate() {
        let _guard = serialized();

        let last_ran = Arc::new(AtomicUsize::new(0));
        let last_ran_cb = last_ran.clone();

        let _u1 = register_session_resource_cleanup(Arc::new(|_sid| {
            Err(Box::<dyn Error + Send + Sync>::from("boom-1"))
        }));
        let _u2 = register_session_resource_cleanup(Arc::new(|_sid| {
            Err(Box::<dyn Error + Send + Sync>::from("boom-2"))
        }));
        let _u3 = register_session_resource_cleanup(Arc::new(move |_sid| {
            last_ran_cb.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }));

        let error = cleanup_session_resources(Some("s")).unwrap_err();
        assert_eq!(error.errors.len(), 2);
        assert_eq!(error.errors[0].to_string(), "boom-1");
        assert_eq!(error.errors[1].to_string(), "boom-2");
        assert_eq!(error.to_string(), "Failed to cleanup session resources");
        // The callback after the failing ones still ran.
        assert_eq!(last_ran.load(Ordering::Relaxed), 1);
    }

    // An empty registry cleans up successfully — pi returns without throwing.
    #[test]
    fn empty_registry_is_ok() {
        let _guard = serialized();
        assert!(cleanup_session_resources(Some("s")).is_ok());
    }
}
