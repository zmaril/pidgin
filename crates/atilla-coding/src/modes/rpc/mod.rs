//! `modes/rpc` — headless JSONL RPC entrypoint.
//!
//! Mirrors pi's `packages/coding-agent/src/modes/rpc/rpc-mode.ts`
//! (`runRpcMode(runtimeHost: AgentSessionRuntime): Promise<never>`).
//!
//! pi's RPC mode is a thin dispatch shell over a live `AgentSession` runtime.
//! That runtime is not yet ported to Rust, so the JSONL protocol layer here
//! runs against an in-memory [`session::RpcSession`]: the storage-backed and
//! in-memory command subset is served for real, and commands needing the
//! missing agent runtime return honest per-command errors (see [`dispatch`]).

pub mod dispatch;
pub mod jsonl;
pub mod session;
pub mod types;

use std::io::{BufReader, Write};

use serde_json::Value;

use session::RpcSession;
use types::{RpcCommand, RpcCommandEnvelope};

/// The command `type` strings pi recognizes. Used to tell an unknown command
/// type (→ `"Unknown command: <type>"`) apart from a known command whose
/// payload failed to deserialize (→ a per-command failure carrying the parse
/// message).
const KNOWN_COMMAND_TYPES: &[&str] = &[
    "prompt",
    "steer",
    "follow_up",
    "abort",
    "new_session",
    "get_state",
    "set_model",
    "cycle_model",
    "get_available_models",
    "set_thinking_level",
    "cycle_thinking_level",
    "set_steering_mode",
    "set_follow_up_mode",
    "compact",
    "set_auto_compaction",
    "set_auto_retry",
    "abort_retry",
    "bash",
    "abort_bash",
    "get_session_stats",
    "export_html",
    "switch_session",
    "fork",
    "clone",
    "get_fork_messages",
    "get_entries",
    "get_tree",
    "get_last_assistant_text",
    "set_session_name",
    "get_messages",
    "get_commands",
];

/// Write one framed JSONL record to real stdout (fd 1) and flush.
///
/// This bypasses the CLI's soft output guard (which routes chatter to stderr)
/// so only framed protocol JSON reaches stdout, mirroring pi's
/// `writeRawStdout(serializeJsonLine(obj))`.
fn write_line(value: &Value) {
    let line = jsonl::serialize_json_line(value);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.flush();
}

/// Handle a single input line: parse, route, dispatch, and emit the response.
fn handle_line(session: &mut RpcSession, line: &str) {
    // 1. Parse to a raw Value. A malformed line yields pi's parse-error shape
    //    with the `id` key absent.
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            write_line(&err_value(None, "parse", format!("Failed to parse command: {e}")));
            return;
        }
    };

    // 2. Extension UI responses are routed to pending requests. No extension
    //    system is ported, so they are accepted and ignored (no response).
    if value.get("type").and_then(Value::as_str) == Some("extension_ui_response") {
        return;
    }

    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let ty = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    // 3. Deserialize into a typed command.
    let command: RpcCommand = match serde_json::from_value::<RpcCommandEnvelope>(value) {
        Ok(env) => env.command,
        Err(e) => {
            // A recognized type with an invalid payload is a per-command
            // failure; an unrecognized type reproduces pi's exact message.
            if KNOWN_COMMAND_TYPES.contains(&ty.as_str()) {
                write_line(&err_value(id, &ty, e.to_string()));
            } else {
                write_line(&err_value(id, &ty, format!("Unknown command: {ty}")));
            }
            return;
        }
    };

    // 4. Dispatch and emit.
    let response = dispatch::handle_command(session, id, command);
    write_line(&response);
}

/// Serialize an error response to a `Value`.
fn err_value(id: Option<String>, command: &str, message: impl Into<String>) -> Value {
    serde_json::to_value(types::error(id, command, message)).expect("error serializes")
}

/// Entry point for `--mode rpc`.
///
/// Mirrors pi's `runRpcMode`: reads JSONL commands from stdin, dispatches them,
/// and writes framed responses to stdout. Returns `Ok(())` on a graceful stdin
/// EOF (pi shuts the process down at that point). SIGTERM/SIGHUP are left
/// unhandled so the process terminates by signal, which the parent observes as
/// exit code 143/129 — matching pi's explicit shutdown codes.
pub fn run_rpc_mode() -> anyhow::Result<()> {
    let mut session = RpcSession::new();
    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    jsonl::read_json_lines(reader, |line| handle_line(&mut session, line))?;
    Ok(())
}
