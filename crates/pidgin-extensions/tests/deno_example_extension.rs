//! Acceptance test that pidgin loads the shipped `examples/extensions/task-list`
//! extension — a real, user-style pi extension — through the same machinery a
//! pi user would exercise, and that its command, tool, and hooks all register.
//!
//! Two complementary loads:
//!
//!   1. `real_loader_loads_the_example_through_the_seam` drives the WHOLE
//!      pipeline: it injects [`RealExtensionLoader`] via the
//!      `DefaultResourceLoaderOptions.extension_loader` seam (exactly as
//!      `deno_resource_loader.rs` does), points the CLI-extension path at the
//!      example, `reload()`s, and asserts the resulting pidgin-coding
//!      [`Extension`] carries the `task` command and `list_tasks` tool. This is
//!      the "pidgin loads it through the merged ExtensionLoader" check. That
//!      lowered `Extension` surfaces tool/command names but NOT hooks, so:
//!   2. `example_registers_command_tool_and_hooks` loads the same file directly
//!      onto the plane via [`JsPlaneHandle::load_discovered`] — the identical
//!      entrypoint `RealExtensionLoader` calls internally — and inspects the full
//!      [`Inventory`], where the `session_start` and `tool_call` hooks are
//!      visible alongside the command and tool.
//!
//! The whole file is gated on the `deno` feature: it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox. Without the feature the file is empty.
#![cfg(feature = "deno")]

use std::path::PathBuf;

use pidgin_coding::core::extensions::discovery::{
    DiscoveredExtension, DiscoveryOrigin, ExtensionLanguage,
};
use pidgin_coding::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions,
};
use pidgin_extensions::{JsPlaneHandle, RealExtensionLoader};

mod common;
use common::{canonical, join, mkdir};

/// Absolute, canonical path to the shipped example extension entrypoint.
///
/// `CARGO_MANIFEST_DIR` is `crates/pidgin-extensions`; the repo root is two
/// levels up, so `../../examples/...` reaches the example. `canonical` resolves
/// the `..` segments (and requires the file to exist, which it must).
fn example_entrypoint() -> String {
    let raw = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/extensions/task-list/index.ts"
    );
    canonical(raw)
}

#[test]
fn real_loader_loads_the_example_through_the_seam() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = canonical(&tmp.path().to_string_lossy());
    let cwd = join(&root, &["project"]);
    let agent = join(&root, &["agent"]);
    let home = join(&root, &["home"]);
    mkdir(&cwd);
    mkdir(&agent);
    mkdir(&home);

    let example = example_entrypoint();
    let options = DefaultResourceLoaderOptions {
        cwd,
        agent_dir: agent,
        home_dir: Some(home),
        // The REAL loader, injected via the same seam the other deno tests use.
        extension_loader: Some(Box::new(RealExtensionLoader::spawn())),
        // Load the example as an explicit CLI (`-e`) extension.
        additional_extension_paths: vec![example],
        ..Default::default()
    };

    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    let result = loader.get_extensions();
    assert!(
        result.errors.is_empty(),
        "unexpected load errors: {:?}",
        result.errors
    );
    // The empty temp roots contribute nothing, so only the example loads.
    assert_eq!(result.extensions.len(), 1);

    let ext = &result.extensions[0];
    assert!(
        ext.commands.iter().any(|c| c == "task"),
        "expected `task` command, got {:?}",
        ext.commands
    );
    assert!(
        ext.tools.iter().any(|t| t == "list_tasks"),
        "expected `list_tasks` tool, got {:?}",
        ext.tools
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn example_registers_command_tool_and_hooks() {
    let entrypoint = PathBuf::from(example_entrypoint());
    let discovered = DiscoveredExtension {
        id: "task-list".to_string(),
        root: entrypoint
            .parent()
            .expect("entrypoint has a parent dir")
            .to_path_buf(),
        language: ExtensionLanguage::TypeScript,
        entrypoint_path: entrypoint,
        origin: DiscoveryOrigin::Configured,
    };

    let plane = JsPlaneHandle::spawn();
    let inv = plane
        .load_discovered(&discovered)
        .await
        .expect("the example extension loads");
    plane.shutdown().await;

    assert!(
        inv.commands.iter().any(|c| c.name == "task"),
        "expected `task` command, got {:?}",
        inv.commands.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        inv.tools.iter().any(|t| t.name == "list_tasks"),
        "expected `list_tasks` tool, got {:?}",
        inv.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // Both lifecycle hooks the example registers must be present.
    let events = inv.hook_events();
    assert!(
        events.iter().any(|e| e == "session_start"),
        "expected a `session_start` hook, got {events:?}"
    );
    assert!(
        events.iter().any(|e| e == "tool_call"),
        "expected a `tool_call` hook, got {events:?}"
    );
}
