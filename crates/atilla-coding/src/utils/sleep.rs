//! Abortable sleep helper.
//!
//! Ported from pi's `utils/sleep.ts`. pi's `sleep(ms, signal?)` wraps
//! `setTimeout` in a promise and lets an optional `AbortSignal` cancel the
//! wait. The Rust analog uses [`tokio::time::sleep`] plus a
//! [`tokio::sync::watch`] channel as the cancellation signal (a `bool` that
//! flips to `true` on abort — mirroring `AbortSignal.aborted` plus its `abort`
//! event).
//!
//! Behavior note vs. pi: pi's implementation *rejects* the promise with
//! `Error("Aborted")` when the signal fires. The coordinator's port spec calls
//! for the idiomatic Rust surface — an aborted sleep simply *returns early*
//! rather than surfacing an error. Callers that need to distinguish "slept
//! fully" from "aborted" can inspect their own signal after the call. This is
//! the one deliberate divergence from pi's observable behavior; the timing
//! semantics (fire after `ms`, or as soon as aborted) are otherwise identical.

use std::time::Duration;

use tokio::sync::watch;

/// Sleep for `ms` milliseconds, returning early if `signal` is aborted.
///
/// `signal` is an optional cancellation channel: when the watched value is (or
/// becomes) `true`, the sleep resolves immediately. If the signal's sender is
/// dropped, the sleep still honors the full duration (a dropped sender can no
/// longer abort). With `None`, this is a plain delay.
pub async fn sleep(ms: u64, signal: Option<&mut watch::Receiver<bool>>) {
    let duration = Duration::from_millis(ms);
    match signal {
        None => tokio::time::sleep(duration).await,
        Some(rx) => {
            // Already aborted before we started: resolve immediately, like pi's
            // `if (signal?.aborted)` fast path.
            if *rx.borrow() {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(duration) => {}
                _ = wait_for_abort(rx) => {}
            }
        }
    }
}

/// Resolve once the watched signal becomes `true`. If the sender is dropped
/// (the signal can never abort now), park forever so the timer wins the race.
async fn wait_for_abort(rx: &mut watch::Receiver<bool>) {
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn sleeps_full_duration_without_signal() {
        let start = Instant::now();
        sleep(20, None).await;
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn returns_immediately_when_already_aborted() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        let start = Instant::now();
        sleep(10_000, Some(&mut rx)).await;
        // Should not have waited anywhere near the requested 10s.
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn aborts_mid_sleep() {
        let (tx, mut rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let start = Instant::now();
            sleep(10_000, Some(&mut rx)).await;
            start.elapsed()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(true).unwrap();
        let elapsed = handle.await.unwrap();
        assert!(elapsed < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn completes_when_not_aborted() {
        let (_tx, mut rx) = watch::channel(false);
        let start = Instant::now();
        sleep(20, Some(&mut rx)).await;
        assert!(start.elapsed() >= Duration::from_millis(20));
    }
}
