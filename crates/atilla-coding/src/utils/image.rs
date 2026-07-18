//! Deferred port of pi's image utilities.
//!
//! Covers `image-convert.ts`, `image-process.ts`, `image-resize.ts`,
//! `image-resize-core.ts`, `image-resize-worker.ts`, and `photon.ts`. These
//! decode, convert, and resize images through the Photon WASM library and
//! dispatch work across `worker_threads`. Deferred: both Photon/WASM and the
//! worker-thread model require infrastructure not present in this crate yet.
