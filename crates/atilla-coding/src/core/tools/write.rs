//! Deferred port of pi's `core/tools/write.ts`.
//!
//! The write tool creates parent directories and writes file contents to disk
//! through a file-mutation queue. Its logic is dominated by filesystem
//! mutation; the only pure helpers (trailing-empty-line trimming, tab
//! replacement) already live in `render_utils`. Not yet ported: it is a
//! filesystem side-effect wrapper.
