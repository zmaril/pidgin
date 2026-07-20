// straitjacket-allow-file:duplication -- the subprocess-spawn harness
// (CARGO_BIN_EXE_pidgin + Command::output) parallels black_box_cli.rs by design;
// this file is the deno-gated end-to-end probe and stays independent of it.
//! End-to-end `deno`-gated probe: `pidgin -e <ext.ts>` drives the REAL extension
//! loader and reports the loaded command/tool to stderr.
//!
//! pidgin-cli is a binary-only crate (no lib target), so an integration test
//! cannot call `load_and_report_extensions` directly — it drives the compiled
//! binary instead and asserts the observable stderr report. The in-process
//! assertion against the `ExtensionsReport` struct lives in the crate's own
//! `cli::extensions` unit tests (also `deno`-gated). Both run under the
//! dedicated V8 CI job (`cargo test -p pidgin-cli --features deno`).
//!
//! The whole file is gated on the `deno` feature — without it, it is empty.
#![cfg(feature = "deno")]

use std::process::Command;

/// Drive the binary with `-e <fixture>` and a `--model missing-model` so model
/// resolution deterministically fails (exit 1) with no network — the extension
/// report is emitted to stderr first, before model resolution.
#[test]
fn cli_loads_task_list_extension_and_reports_to_stderr() {
    let fixture = format!("{}/tests/fixtures/task-list.ts", env!("CARGO_MANIFEST_DIR"));

    let out = Command::new(env!("CARGO_BIN_EXE_pidgin"))
        .args(["-p", "hi", "--model", "missing-model", "-e", &fixture])
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .output()
        .expect("failed to spawn pidgin binary");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The extension report is on stderr and names the registered command/tool.
    assert!(
        stderr.contains("task-list"),
        "stderr missing extension name:\n{stderr}"
    );
    assert!(
        stderr.contains("task"),
        "stderr missing command `task`:\n{stderr}"
    );
    assert!(
        stderr.contains("list_tasks"),
        "stderr missing tool `list_tasks`:\n{stderr}"
    );
    // Stdout must stay clean of the extension report (stdout-cleanliness).
    assert!(
        !stdout.contains("list_tasks"),
        "extension report leaked to stdout:\n{stdout}"
    );
}
