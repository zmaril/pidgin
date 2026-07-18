//! Deferred port of pi's `core/tools/ls.ts`.
//!
//! The ls tool reads directory entries and stats each one through an
//! `ExecutionEnv`, then formats a sorted listing. The sorting/formatting is
//! thin and inseparable from the `readdir` + `stat` calls that produce it, so
//! it is Not yet ported: it depends on the filesystem directory-listing
//! environment.
