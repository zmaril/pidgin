//! Shared filesystem-fixture helpers for the `deno`-gated resource-loader
//! acceptance tests. These generic path/write/symlink helpers now live in the
//! `pidgin-testkit` dev crate and are re-exported here so the moved extension
//! cases can keep referring to them as `common::{canonical, join, …}`.

// The test binary that includes this module uses a subset of these helpers, so
// per-binary `dead_code` (unused fns) and `unused_imports` (unused re-exports,
// e.g. a binary that pulls `common` but not `symlink_dir`/`write`) are both
// expected and allowed. Without the `unused_imports` allow the deno CI job's
// `-D warnings` denies the re-export line for the binaries that use fewer helpers.
#![allow(dead_code, unused_imports)]

pub use pidgin_testkit::{canonical, join, mkdir, symlink_dir, write};
