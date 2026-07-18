//! Deferred port of pi's `core/tools/file-mutation-queue.ts`.
//!
//! This is a per-file async mutex that serializes concurrent mutations to the
//! same path while allowing parallelism across distinct files. The queue keys
//! on a canonicalized (realpath) path, which is a filesystem probe, and it
//! coordinates `Promise`-based async tasks. Not yet ported: it needs the
//! async task runtime and realpath side of the mutation queue.
