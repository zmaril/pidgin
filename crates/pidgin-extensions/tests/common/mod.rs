//! Shared filesystem-fixture helpers for the `deno`-gated resource-loader
//! acceptance tests. These generic path/write/symlink helpers now live in the
//! `pidgin-testkit` dev crate and are re-exported here so the moved extension
//! cases can keep referring to them as `common::{canonical, join, …}`.

// The test binary that includes this module uses a subset of these helpers, so
// per-binary `dead_code` is expected and allowed.
#![allow(dead_code)]

pub use pidgin_testkit::{canonical, join, mkdir, symlink_dir, write};
