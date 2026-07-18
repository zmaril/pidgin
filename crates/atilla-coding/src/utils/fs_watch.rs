//! Deferred port of pi's `utils/fs-watch.ts`.
//!
//! Filesystem watching wraps Node's `fs.watch`. A Rust port would build on the
//! `notify` crate, which is outside the scope of this pure-utilities PR. Not
//! yet ported.
