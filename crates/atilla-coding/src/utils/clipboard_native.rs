//! Deferred port of pi's `utils/clipboard-native.ts`.
//!
//! This module loads the `@mariozechner/clipboard` native Node addon through
//! `createRequire`. There is no equivalent to port until a Rust-native
//! clipboard backend is selected, so it stays deferred.
