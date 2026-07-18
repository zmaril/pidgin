//! Deferred port of pi's `utils/sleep.ts`.
//!
//! The abortable timer maps onto `tokio::time::sleep` plus a cancellation
//! token. Introducing an async runtime is out of scope for this PR, so the
//! helper is deferred.
