//! Deferred port of pi's `utils/shell.ts`.
//!
//! Shell discovery, environment assembly, and process-tree termination all
//! depend on subprocess execution and OS process introspection. Only the pure
//! `sanitizeBinaryOutput` helper is portable; the module as a whole is deferred
//! until the process layer exists.
