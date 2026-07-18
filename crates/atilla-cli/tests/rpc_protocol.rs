//! Keyless black-box test of the JSONL-RPC protocol.
//!
//! Spawns the `atilla` binary in `--mode rpc`, feeds JSONL commands over stdin,
//! and asserts the framed responses. This is an atilla-owned test (pi's own
//! `rpc.test.ts` is API-key-gated and hard-codes `--provider anthropic`, so it
//! cannot run in CI without a key). It exercises the protocol layer and the
//! implementable-now command subset deterministically, with no model.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

/// Run the binary in RPC mode, sending `commands` (one JSON object per element)
/// and returning the parsed stdout response lines.
fn run_rpc(commands: &[&str]) -> Vec<Value> {
    let exe = env!("CARGO_BIN_EXE_atilla");
    let session_dir = std::env::temp_dir().join(format!("atilla-rpc-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&session_dir);

    let mut child = Command::new(exe)
        .arg("--mode")
        .arg("rpc")
        // Isolate session storage so the test never touches real sessions.
        .env("PI_CODING_AGENT_SESSION_DIR", &session_dir)
        .env("PI_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn atilla --mode rpc");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        for cmd in commands {
            writeln!(stdin, "{cmd}").expect("write command");
        }
        // Dropping stdin closes it -> EOF -> graceful shutdown.
    }

    let stdout = child.stdout.take().expect("stdout");
    let reader = BufReader::new(stdout);
    let mut responses = Vec::new();
    for line in reader.lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        responses.push(serde_json::from_str(&line).expect("stdout line is valid JSON"));
    }

    let status = child.wait().expect("wait for child");
    assert!(status.success(), "rpc process exited non-zero: {status:?}");

    let _ = std::fs::remove_dir_all(&session_dir);
    responses
}

#[test]
fn framing_round_trips_every_command() {
    // Six commands in, six response lines out, in order.
    let responses = run_rpc(&[
        r#"{"id":"1","type":"get_state"}"#,
        r#"{"id":"2","type":"set_thinking_level","level":"high"}"#,
        r#"{"id":"3","type":"cycle_thinking_level"}"#,
        r#"{"id":"4","type":"set_steering_mode","mode":"all"}"#,
        r#"{"id":"5","type":"set_auto_retry","enabled":true}"#,
        r#"{"id":"6","type":"get_entries"}"#,
    ]);
    assert_eq!(responses.len(), 6);
    for (i, r) in responses.iter().enumerate() {
        assert_eq!(r["type"], "response");
        assert_eq!(r["success"], true, "command {} should succeed: {r}", i + 1);
    }
    // ids echoed back in order.
    let ids: Vec<&str> = responses.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["1", "2", "3", "4", "5", "6"]);
}

#[test]
fn parse_error_yields_parse_failure_shape() {
    let responses = run_rpc(&["this is not json"]);
    assert_eq!(responses.len(), 1);
    let r = &responses[0];
    assert_eq!(r["type"], "response");
    assert_eq!(r["command"], "parse");
    assert_eq!(r["success"], false);
    assert!(r["error"].as_str().unwrap().starts_with("Failed to parse command:"));
    // The parse-error line carries no `id`.
    assert!(r.get("id").is_none());
}

#[test]
fn unknown_command_yields_exact_message() {
    let responses = run_rpc(&[r#"{"id":"z","type":"frobnicate"}"#]);
    assert_eq!(responses.len(), 1);
    let r = &responses[0];
    assert_eq!(r["command"], "frobnicate");
    assert_eq!(r["success"], false);
    assert_eq!(r["error"], "Unknown command: frobnicate");
    assert_eq!(r["id"], "z");
}

#[test]
fn get_state_omits_absent_optional_fields() {
    let responses = run_rpc(&[r#"{"id":"1","type":"get_state"}"#]);
    let data = &responses[0]["data"];
    // sessionName/model/sessionFile must be *absent*, not null (the client
    // asserts `sessionName === undefined`).
    assert!(data.get("sessionName").is_none());
    assert!(data.get("model").is_none());
    assert!(data.get("sessionFile").is_none());
    // Present scalar fields with pi's defaults.
    assert_eq!(data["thinkingLevel"], "medium");
    assert_eq!(data["steeringMode"], "one-at-a-time");
    assert_eq!(data["followUpMode"], "one-at-a-time");
    assert_eq!(data["autoCompactionEnabled"], true);
    assert_eq!(data["isStreaming"], false);
    assert_eq!(data["isCompacting"], false);
    assert_eq!(data["messageCount"], 0);
    assert_eq!(data["pendingMessageCount"], 0);
    assert!(data["sessionId"].is_string());
}

#[test]
fn bash_returns_bash_result_shape() {
    let responses = run_rpc(&[r#"{"type":"bash","command":"echo hello"}"#]);
    let r = &responses[0];
    assert_eq!(r["command"], "bash");
    assert_eq!(r["success"], true);
    // No id on either side.
    assert!(r.get("id").is_none());
    let data = &r["data"];
    assert_eq!(data["output"].as_str().unwrap().trim(), "hello");
    assert_eq!(data["exitCode"], 0);
    assert_eq!(data["cancelled"], false);
    assert_eq!(data["truncated"], false);
}

#[test]
fn get_entries_empty_session_shape() {
    let responses = run_rpc(&[r#"{"id":"1","type":"get_entries"}"#]);
    let data = &responses[0]["data"];
    assert_eq!(data["entries"].as_array().unwrap().len(), 0);
    // leafId present as null on an empty session.
    assert!(data.get("leafId").is_some());
    assert!(data["leafId"].is_null());
}

#[test]
fn get_entries_unknown_since_errors() {
    let responses = run_rpc(&[r#"{"id":"1","type":"get_entries","since":"missing"}"#]);
    let r = &responses[0];
    assert_eq!(r["success"], false);
    assert_eq!(r["command"], "get_entries");
    assert_eq!(r["error"], "Entry not found: missing");
}

#[test]
fn set_session_name_empty_errors_and_reflects_in_state() {
    let responses = run_rpc(&[
        r#"{"id":"1","type":"set_session_name","name":"   "}"#,
        r#"{"id":"2","type":"set_session_name","name":"my session"}"#,
        r#"{"id":"3","type":"get_state"}"#,
    ]);
    assert_eq!(responses[0]["success"], false);
    assert_eq!(responses[0]["error"], "Session name cannot be empty");
    assert_eq!(responses[1]["success"], true);
    assert!(responses[1].get("data").is_none());
    assert_eq!(responses[2]["data"]["sessionName"], "my session");
}

#[test]
fn stub_command_returns_honest_failure() {
    let responses = run_rpc(&[r#"{"id":"1","type":"prompt","message":"hi"}"#]);
    let r = &responses[0];
    assert_eq!(r["command"], "prompt");
    assert_eq!(r["success"], false);
    let error = r["error"].as_str().unwrap();
    assert!(error.contains("not implemented"), "error: {error}");
    assert!(error.contains("AgentSession"), "error: {error}");
    // No `data` key on an error response.
    assert!(r.get("data").is_none());
}
