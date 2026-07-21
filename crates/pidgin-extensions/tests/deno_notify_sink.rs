// straitjacket-allow-file:duplication
//! Integration test for the `ctx.ui.notify` DELIVERY seam.
//!
//! Proves the end-to-end path: JS `ctx.ui.notify(...)` on the off-thread deno
//! plane -> `op_notify` -> the host [`NotifySink`] bound via
//! [`DenoExtensionRunner::bind_notify_sink`] -> a [`NotifyReceiver`] the host
//! drains. The subject is the vendored `examples/extensions/pirate/index.ts`,
//! whose `/pirate` command handler calls
//! `ctx.ui.notify("Arrr! Pirate mode enabled!", "info")` on its first toggle.
//!
//! Before this seam `ctx.ui.notify` was a JS no-op that dropped the message;
//! here it is delivered into a recording sink, so the single dispatch both
//! returns `ok == true` AND yields exactly one [`Notification`].
//!
//! Gated on the `deno` feature — it compiles and runs ONLY in the dedicated
//! `deno runtime (V8)` CI job, since building `deno_core` needs the V8 blob that
//! 403s in-sandbox.
#![cfg(feature = "deno")]

use serde_json::json;

use pidgin_coding::core::extensions::notify::{notify_channel, Notification};
use pidgin_coding::core::extensions::runner::ExtensionRunner as RunnerTrait;
use pidgin_coding::core::extensions::types::NotifyLevel;

use pidgin_extensions::{DenoExtensionRunner, JsPlaneHandle, SourceLanguage};

use std::sync::Arc;

/// The vendored pirate extension (its `/pirate` handler is the notify subject).
const PIRATE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/extensions/pirate/index.ts"
);

/// pirate's exact first-toggle message.
const PIRATE_NOTIFY: &str = "Arrr! Pirate mode enabled!";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctx_ui_notify_is_delivered_to_a_bound_sink() {
    // 1. Boot the plane and load pirate onto it (its command handler stays live).
    let plane = Arc::new(JsPlaneHandle::spawn());
    let source = std::fs::read_to_string(PIRATE_PATH).expect("read vendored pirate extension");
    let inventory = plane
        .load_extension_source("pirate", source, SourceLanguage::TypeScript)
        .await
        .expect("vendored pirate extension loads clean");
    let loaded = vec![("examples/extensions/pirate/index.ts".to_string(), inventory)];

    // 2. Build the runner over the shared plane and bind a recording sink.
    let runner = DenoExtensionRunner::from_loaded(Arc::clone(&plane), loaded, "/project");
    let (sink, receiver) = notify_channel();
    runner.bind_notify_sink(sink);

    // 3. Invoke /pirate through the real one-shot invoke-stored primitive. On the
    //    first toggle pirate mode flips on and the handler fires
    //    `ctx.ui.notify("Arrr! Pirate mode enabled!", "info")`.
    let inv = plane
        .invoke_stored("command", "pirate", &json!([""]))
        .await
        .expect("the /pirate command dispatches through the plane and returns an envelope");
    assert!(
        inv.ok,
        "the /pirate command handler ran to completion: {:?}",
        inv.error
    );

    // 4. Draining the receiver yields exactly one delivered notification, with
    //    pirate's exact message at level Info.
    let mut delivered: Vec<Notification> = Vec::new();
    receiver.try_drain(|n| delivered.push(n));

    assert_eq!(
        delivered.len(),
        1,
        "exactly one notification was delivered (got {})",
        delivered.len()
    );
    assert_eq!(delivered[0].message, PIRATE_NOTIFY);
    assert_eq!(delivered[0].level, NotifyLevel::Info);

    // Drop the runner (releasing its shared plane handle) so this is the last
    // strong reference, then shut the plane thread down cleanly.
    drop(runner);
    if let Ok(plane) = Arc::try_unwrap(plane) {
        plane.shutdown().await;
    }
}
