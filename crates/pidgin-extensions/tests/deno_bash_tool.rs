// straitjacket-allow-file:duplication
//! Integration test for the `createBashTool` HOST seam on the deno plane.
//!
//! Proves the end-to-end path: JS `createBashTool(...).execute(...)` on the
//! off-thread deno plane -> the `op_run_bash` op -> the host
//! [`BashToolHost`](pidgin_coding::core::tools::bash_host::BashToolHost) bound via
//! [`DenoExtensionRunner::bind_bash_host`] -> a real
//! [`RealBashToolHost`](pidgin_coding::core::tools::bash_host::RealBashToolHost)
//! that runs the command through `create_bash_tool(...).execute()` and returns a
//! synchronous outcome.
//!
//! It asserts pi's exact shape at the boundary:
//! - **success (exit 0):** `execute` RESOLVES with `{ content }` carrying output.
//! - **non-zero exit:** `execute` THROWS, and the `Command exited with code N`
//!   footer rides in the thrown message (NOT in any resolved output).
//! - **bad cwd:** `execute` THROWS with the pi-exact working-directory message.
//!
//! Gated on the `deno` feature — it compiles and runs ONLY in the dedicated
//! `deno runtime (V8)` CI job, since building `deno_core` needs the V8 blob that
//! 403s in-sandbox.
#![cfg(feature = "deno")]

use std::sync::Arc;

use serde_json::Value;

use pidgin_coding::core::tools::bash_host::{BashToolHost, RealBashToolHost};
use pidgin_extensions::{DenoExtensionRunner, JsPlaneHandle};

/// Build a script that awaits `createBashTool().execute(...)` and returns a plain
/// object, catching any throw into `{ threw, message }` so `eval` always resolves
/// (never surfaces as an `Err`) and the test can assert on the thrown message.
fn probe_script(command: &str, cwd: &str) -> String {
    // JSON-encode both values so they cross into the script as safe string
    // literals regardless of their contents.
    let command = serde_json::to_string(command).unwrap();
    let cwd = serde_json::to_string(cwd).unwrap();
    format!(
        r#"(async () => {{
            try {{
                const tool = createBashTool();
                const result = await tool.execute("call-1", {{ command: {command}, cwd: {cwd} }});
                return {{ threw: false, content: result.content, message: "" }};
            }} catch (e) {{
                return {{ threw: true, content: "", message: e.message }};
            }}
        }})()"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_bash_tool_runs_through_the_bound_host() {
    // The host drives `execute` on THIS test's multi-thread runtime handle (what a
    // real caller passes). `RealBashToolHost::run` blocks a dedicated std::thread
    // on `Handle::block_on`, off the async context, so the drive is legal; and
    // there is no separately-owned runtime to drop at test end (dropping a tokio
    // runtime from within an async context panics).
    let handle = tokio::runtime::Handle::current();

    // Boot the plane and build the runner over it; bind a real host.
    let plane = Arc::new(JsPlaneHandle::spawn());
    let runner = DenoExtensionRunner::from_loaded(Arc::clone(&plane), Vec::new(), "/project");
    let host: Arc<dyn BashToolHost> = Arc::new(RealBashToolHost::new(handle));
    runner.bind_bash_host(host);

    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir path is valid UTF-8");

    // 1. Success (exit 0): execute RESOLVES with the output content.
    let ok = plane
        .eval(probe_script("echo hi", cwd))
        .await
        .expect("the success probe resolves");
    assert_eq!(
        ok["threw"],
        Value::Bool(false),
        "a zero-exit command must not throw: {ok:?}"
    );
    let content = ok["content"].as_str().expect("content is a string");
    assert!(
        content.contains("hi"),
        "resolved content must carry the output, got {content:?}"
    );

    // 2. Non-zero exit: execute THROWS, and the footer is in the THROWN message
    //    (the error path) — never in a resolved output.
    let nonzero = plane
        .eval(probe_script("exit 7", cwd))
        .await
        .expect("the non-zero probe resolves (the throw is caught in JS)");
    assert_eq!(
        nonzero["threw"],
        Value::Bool(true),
        "a non-zero exit must surface as a throw: {nonzero:?}"
    );
    let message = nonzero["message"].as_str().expect("message is a string");
    assert!(
        message.contains("Command exited with code 7"),
        "the thrown message must carry the exit footer, got {message:?}"
    );

    // 3. Bad cwd: execute THROWS with the pi-exact working-directory message.
    let bad_cwd = plane
        .eval(probe_script("echo hi", "/nonexistent/does/not/exist"))
        .await
        .expect("the bad-cwd probe resolves (the throw is caught in JS)");
    assert_eq!(
        bad_cwd["threw"],
        Value::Bool(true),
        "a bad cwd must surface as a throw: {bad_cwd:?}"
    );
    let message = bad_cwd["message"].as_str().expect("message is a string");
    assert_eq!(
        message,
        "Working directory does not exist: /nonexistent/does/not/exist\nCannot execute bash commands."
    );

    // Drop the runner (releasing its shared plane handle), then shut the plane
    // down cleanly as its sole remaining owner.
    drop(runner);
    if let Ok(plane) = Arc::try_unwrap(plane) {
        plane.shutdown().await;
    }
}

/// With NO host bound, `createBashTool().execute()` THROWS the "not bound"
/// message rather than hanging — the op's unbound fallback, faithful to pi's
/// error path. Proven on the plane directly (no runner, no host binding).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbound_host_makes_execute_throw() {
    let plane = JsPlaneHandle::spawn();

    let out = plane
        .eval(probe_script("echo hi", "/tmp"))
        .await
        .expect("the unbound probe resolves (the throw is caught in JS)");
    assert_eq!(
        out["threw"],
        Value::Bool(true),
        "an unbound host must make execute throw: {out:?}"
    );
    let message = out["message"].as_str().expect("message is a string");
    assert!(
        message.contains("bash host is not bound"),
        "the thrown message must be the unbound fallback, got {message:?}"
    );

    plane.shutdown().await;
}
