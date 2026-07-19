//! The `orchestrator` command-line binary, mirroring
//! `packages/orchestrator/src/cli.ts`.
//!
//! pi's `cli.ts` hand-parses `process.argv` into one of `serve | list | spawn |
//! status | stop | rpc | rpc-stream` (plus `--help`/`--version`). The `serve`
//! subcommand runs the server; every other subcommand is a one-shot IPC client
//! that connects to the orchestrator socket, sends a single request, and prints
//! the JSON response — except `rpc-stream`, which opens a bidirectional JSONL
//! bridge between stdin/stdout and the socket.
//!
//! This port keeps the same subcommands and behaviour but parses argv with
//! `clap` (the atilla workspace's chosen arg parser), so the subcommand/flag
//! surface is declared once and unit-testable at the parse layer. Client
//! responses are printed with `serde_json::to_string_pretty` — the 2-space
//! indentation of pi's `JSON.stringify(response, null, 2)`.
//!
//! # Binary
//!
//! Following the atilla CLI convention (the main crate `atilla-cli` declares a
//! `[[bin]]` with an explicit `name`/`path = "src/main.rs"`), this crate declares
//! a `[[bin]]` named `atilla-orchestrator` at `src/main.rs`; this module holds the
//! parsing and dispatch, invoked from that thin shell. `cli.ts` is not part of
//! pi's `index.ts` barrel, and this module is likewise binary-local (not a
//! library module).
//!
//! # Divergences from pi's hand-rolled parser
//!
//! `--help`/`--version` and missing-required-argument errors are formatted and
//! exit-coded by clap (clap's `-V`/`--version`, exit code 2 for a usage error)
//! rather than pi's bespoke help text and exit-1 usage lines; the successful
//! dispatch — the request sent and the pretty-printed JSON response — matches pi.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use atilla_orchestrator::config::{get_socket_path, VERSION};
use atilla_orchestrator::ipc::client::send_ipc_request;
use atilla_orchestrator::ipc::protocol::{
    encode_message, ListRequest, OrchestratorRequest, RpcRequest, RpcStreamRequest, SpawnRequest,
    StatusRequest, StopRequest,
};
use atilla_orchestrator::serve::serve;

/// The `orchestrator` CLI (pi's `cli.ts`).
#[derive(Debug, Parser)]
#[command(
    name = "atilla-orchestrator",
    version = VERSION,
    about = "Manage orchestrated coding-agent instances over the orchestrator socket.",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// The orchestrator subcommands (pi's `args[0]` dispatch).
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Start the orchestrator server.
    Serve,
    /// List all instances.
    List,
    /// Spawn a new instance.
    Spawn {
        /// Working directory for the new instance (defaults to the current dir).
        #[arg(long)]
        cwd: Option<String>,
        /// Optional human-readable label.
        #[arg(long)]
        label: Option<String>,
    },
    /// Show an instance's status.
    Status {
        /// The instance id to query.
        instance_id: String,
    },
    /// Stop an instance.
    Stop {
        /// The instance id to stop.
        instance_id: String,
    },
    /// Relay a single RPC command (a JSON object) to an instance.
    Rpc {
        /// The instance id to relay to.
        instance_id: String,
        /// The RPC command as a JSON string.
        command: String,
    },
    /// Open a bidirectional RPC stream to an instance.
    ///
    /// stdin expects JSONL `RpcCommand` or `extension_ui_response` messages; the
    /// instance's responses/events are written to stdout.
    #[command(name = "rpc-stream")]
    RpcStream {
        /// The instance id to stream with.
        instance_id: String,
    },
}

/// Parse argv and run the selected subcommand (pi's `main()`).
///
/// clap owns `--help`/`--version` and usage errors (printing and exiting on its
/// own); everything else dispatches here.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    dispatch(cli.command).await
}

/// Dispatch a parsed [`Command`] (pi's `args[0]` switch body).
async fn dispatch(command: Command) -> ExitCode {
    match command {
        Command::Serve => match serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Command::List => send_and_print(&OrchestratorRequest::List(ListRequest {})).await,
        Command::Spawn { cwd, label } => send_and_print(&spawn_request(cwd, label)).await,
        Command::Status { instance_id } => {
            send_and_print(&OrchestratorRequest::Status(StatusRequest { instance_id })).await
        }
        Command::Stop { instance_id } => {
            send_and_print(&OrchestratorRequest::Stop(StopRequest { instance_id })).await
        }
        Command::Rpc {
            instance_id,
            command,
        } => match rpc_request(instance_id, &command) {
            Ok(request) => send_and_print(&request).await,
            // pi's `JSON.parse(commandJson)` throws on a bad command; surface it
            // and exit non-zero rather than sending a malformed request.
            Err(error) => {
                eprintln!("Invalid JSON command: {error}");
                ExitCode::FAILURE
            }
        },
        Command::RpcStream { instance_id } => rpc_stream(&instance_id).await,
    }
}

/// Build a `spawn` request, defaulting `cwd` to the current directory (pi's
/// `getFlagValue(args, "--cwd") ?? cwd()`).
fn spawn_request(cwd: Option<String>, label: Option<String>) -> OrchestratorRequest {
    let cwd = cwd.unwrap_or_else(current_dir_string);
    OrchestratorRequest::Spawn(SpawnRequest {
        cwd,
        label,
        provider: None,
        model: None,
    })
}

/// Build an `rpc` request, parsing `command` as JSON (pi's `JSON.parse`).
fn rpc_request(instance_id: String, command: &str) -> serde_json::Result<OrchestratorRequest> {
    let command = serde_json::from_str(command)?;
    Ok(OrchestratorRequest::Rpc(RpcRequest {
        instance_id,
        command,
    }))
}

/// The current working directory as a string, or `"."` if it cannot be read
/// (pi's `cwd()`).
fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// Send one request to the orchestrator and print the JSON response (pi's
/// `printResponse(await sendIpcRequest(...))`).
async fn send_and_print(request: &OrchestratorRequest) -> ExitCode {
    match send_ipc_request(request).await {
        Ok(response) => {
            print_response(&response);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Pretty-print a response as 2-space-indented JSON (pi's
/// `JSON.stringify(response, null, 2)`).
fn print_response<T: serde::Serialize>(response: &T) {
    match serde_json::to_string_pretty(response) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("{error}"),
    }
}

/// Bridge stdin/stdout with the orchestrator's `rpc_stream` socket (pi's
/// `rpcStream`).
///
/// Connect to the socket, send the `rpc_stream` open frame, then relay socket
/// bytes to stdout and JSONL stdin lines back to the socket until either side
/// closes.
async fn rpc_stream(instance_id: &str) -> ExitCode {
    let socket_path = get_socket_path();
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let (read_half, mut write_half) = stream.into_split();

    // pi: `socket.write(encodeMessage({ type: "rpc_stream", instanceId }))`.
    let open = OrchestratorRequest::RpcStream(RpcStreamRequest {
        instance_id: instance_id.to_string(),
    });
    if let Err(error) = write_half.write_all(encode_message(&open).as_bytes()).await {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    eprintln!(
        "connected to rpc stream {instance_id}; send JSONL RpcCommand or extension_ui_response on stdin"
    );

    // socket -> stdout: copy every byte through (pi's `socket.on("data")`).
    let to_stdout = tokio::spawn(async move {
        let mut reader = read_half;
        let mut stdout = tokio::io::stdout();
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer).await {
                // pi: `socket.on("end")` -> exit 0.
                Ok(0) => return 0i32,
                Ok(read) => {
                    if stdout.write_all(&buffer[..read]).await.is_err()
                        || stdout.flush().await.is_err()
                    {
                        return 0;
                    }
                }
                // pi: `socket.on("error")` -> print + exit 1.
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            }
        }
    });

    // stdin -> socket: parse each JSONL line and relay it framed (pi's
    // `process.stdin.on("data")` newline scan + `JSON.parse` + `socket.write`).
    let stdin_relay = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("{error}");
                    continue;
                }
            };
            if write_half
                .write_all(encode_message(&parsed).as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let exit_code = to_stdout.await.unwrap_or(1);
    stdin_relay.abort();
    if exit_code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse argv as the binary would (argv[0] is the program name).
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["atilla-orchestrator"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn parses_serve() {
        assert_eq!(parse(&["serve"]).unwrap().command, Command::Serve);
    }

    #[test]
    fn parses_list() {
        assert_eq!(parse(&["list"]).unwrap().command, Command::List);
    }

    #[test]
    fn parses_spawn_with_flags() {
        let cli = parse(&["spawn", "--cwd", "/work/project", "--label", "primary"]).unwrap();
        assert_eq!(
            cli.command,
            Command::Spawn {
                cwd: Some("/work/project".to_string()),
                label: Some("primary".to_string()),
            }
        );
    }

    #[test]
    fn parses_spawn_without_flags() {
        let cli = parse(&["spawn"]).unwrap();
        assert_eq!(
            cli.command,
            Command::Spawn {
                cwd: None,
                label: None,
            }
        );
    }

    #[test]
    fn parses_status_stop_with_instance_id() {
        assert_eq!(
            parse(&["status", "i-1"]).unwrap().command,
            Command::Status {
                instance_id: "i-1".to_string(),
            }
        );
        assert_eq!(
            parse(&["stop", "i-2"]).unwrap().command,
            Command::Stop {
                instance_id: "i-2".to_string(),
            }
        );
    }

    #[test]
    fn parses_rpc_with_instance_and_command() {
        let cli = parse(&["rpc", "i-1", "{\"type\":\"ping\"}"]).unwrap();
        assert_eq!(
            cli.command,
            Command::Rpc {
                instance_id: "i-1".to_string(),
                command: "{\"type\":\"ping\"}".to_string(),
            }
        );
    }

    #[test]
    fn parses_rpc_stream_hyphenated_subcommand() {
        assert_eq!(
            parse(&["rpc-stream", "i-9"]).unwrap().command,
            Command::RpcStream {
                instance_id: "i-9".to_string(),
            }
        );
    }

    #[test]
    fn missing_required_instance_id_is_a_usage_error() {
        // pi prints a usage line and exits 1; clap owns this as a usage error.
        let error = parse(&["status"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        // pi: `Unknown command: <x>` + help, exit 1; clap rejects the subcommand.
        let error = parse(&["teleport"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn no_subcommand_is_a_usage_error() {
        // pi prints help and exits 0 with no args; clap requires a subcommand.
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn version_flag_reports_the_package_version() {
        let error = parse(&["--version"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(VERSION));
    }

    // -- pure request construction ------------------------------------------

    #[test]
    fn spawn_request_defaults_cwd_to_current_dir() {
        let request = spawn_request(None, Some("l".to_string()));
        match request {
            OrchestratorRequest::Spawn(spawn) => {
                assert_eq!(spawn.cwd, current_dir_string());
                assert_eq!(spawn.label.as_deref(), Some("l"));
                assert!(spawn.provider.is_none());
                assert!(spawn.model.is_none());
            }
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    #[test]
    fn spawn_request_uses_explicit_cwd() {
        let request = spawn_request(Some("/explicit".to_string()), None);
        match request {
            OrchestratorRequest::Spawn(spawn) => {
                assert_eq!(spawn.cwd, "/explicit");
                assert!(spawn.label.is_none());
            }
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    #[test]
    fn rpc_request_parses_the_json_command() {
        let request =
            rpc_request("i-1".to_string(), "{\"type\":\"prompt\",\"text\":\"hi\"}").unwrap();
        match request {
            OrchestratorRequest::Rpc(rpc) => {
                assert_eq!(rpc.instance_id, "i-1");
                assert_eq!(rpc.command["type"], "prompt");
                assert_eq!(rpc.command["text"], "hi");
            }
            other => panic!("expected rpc, got {other:?}"),
        }
    }

    #[test]
    fn rpc_request_rejects_malformed_json() {
        assert!(rpc_request("i-1".to_string(), "{not json}").is_err());
    }
}
