//! RPC child-process management mirroring
//! `packages/orchestrator/src/rpc-process.ts`.
//!
//! An [`RpcProcessInstance`] spawns a coding-agent child in RPC mode, speaks a
//! line-delimited JSON protocol over its stdio, and correlates responses back to
//! the requests that produced them. pi's implementation is event-driven on top
//! of Node streams; this port keeps the same behaviour on a tokio child process:
//! a background monitor task frames stdout into lines and dispatches them, while
//! [`RpcProcessInstance::send`] writes framed commands to stdin and awaits the
//! matching response by `id`.
//!
//! # Relay seam
//!
//! pi imports `RpcCommand`, `RpcResponse`, `AgentSessionEvent`,
//! `RpcExtensionUIRequest`, and `RpcExtensionUIResponse` from the coding-agent
//! package. The orchestrator never inspects these payloads; it only relays them.
//! Per the coordinator-approved seam decision they are modelled as
//! [`serde_json::Value`] (re-exported here from [`crate::ipc::protocol`]).
//!
//! # Streaming caveat
//!
//! Full `rpc_stream` / [`AgentSessionEvent`] streaming parity is deferred until
//! pidgin-coding gains a live agent runtime that emits session events. Until
//! then the event path here relays opaque JSON values faithfully, but no live
//! agent produces them (see the seam-decisions record).
//!
//! # Runtime adaptations
//!
//! * pi splits spawning between a Bun-compiled sibling binary
//!   (`pi --mode rpc`) and a Node interpreter running the resolved `rpc-entry`
//!   module. pidgin ships a single compiled binary, so both branches launch
//!   `pidgin --mode rpc` — the Bun branch as a sibling of the current
//!   executable, the non-Bun branch as the current executable itself.
//! * pi generates request ids with `crypto.randomUUID` (v4). pidgin has no
//!   `uuid` dependency; request ids use `pidgin_agent::harness::session::uuidv7`
//!   (v7), matching how the rest of pidgin mints ids. Only the id *format*
//!   differs; the `orchestrator_<seq>_<uuid>` structure is preserved.
//! * pi terminates the child with `SIGTERM`. tokio's kill sends `SIGKILL`;
//!   the observable effect (the child is torn down and reaped) is the same.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use pidgin_agent::harness::session::uuidv7;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use crate::config;
use crate::ipc::protocol::{
    AgentSessionEvent, RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse,
};

/// Error raised by an [`RpcProcessInstance`], mirroring the `Error` values pi
/// throws or rejects pending requests with.
#[derive(Debug, Clone)]
pub struct RpcProcessError {
    message: String,
}

impl RpcProcessError {
    /// Construct an error from a message string.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error message, mirroring `Error.message`.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RpcProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcProcessError {}

/// A listener invoked for each non-response, non-UI stdout frame. Mirrors pi's
/// `eventListeners` (`(event: AgentSessionEvent) => void`).
type EventListener = Box<dyn Fn(&AgentSessionEvent) + Send>;

/// A listener invoked once when the child process exits. Mirrors pi's
/// `exitListeners` (`(error?: Error) => void`).
type ExitListener = Box<dyn Fn(Option<&RpcProcessError>) + Send>;

/// The handler for `extension_ui_request` frames. Mirrors pi's
/// `uiRequestHandler` (`(request: RpcExtensionUIRequest) => void`).
type UiRequestHandler = Box<dyn Fn(&RpcExtensionUIRequest) + Send>;

/// The resolved child-process command and arguments. Mirrors the return of pi's
/// private `getSpawnCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnCommand {
    /// Executable to spawn.
    pub command: PathBuf,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
}

/// The coding-agent binary file name spawned in RPC mode (the pidgin analog of
/// pi's sibling `pi`/`pi.exe`).
fn agent_binary_file_name() -> &'static str {
    if cfg!(windows) {
        "pidgin.exe"
    } else {
        "pidgin"
    }
}

/// Resolve the spawn command, factored out of the constructor so it is testable
/// without spawning a process. Mirrors pi's `getSpawnCommand`.
///
/// pi branches on `isBunBinary`: a Bun-compiled build spawns the sibling
/// `pi`/`pi.exe` binary with `--mode rpc`, while a Node build spawns the
/// interpreter with the resolved `rpc-entry` module. pidgin is always a compiled
/// binary, so both branches launch `pidgin --mode rpc`: as a sibling of the
/// current executable in the Bun branch, and as the current executable itself
/// otherwise.
fn resolve_spawn_command(current_exe: &Path, is_bun: bool) -> SpawnCommand {
    let args = vec!["--mode".to_string(), "rpc".to_string()];
    if is_bun {
        let dir = current_exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        SpawnCommand {
            command: dir.join(agent_binary_file_name()),
            args,
        }
    } else {
        SpawnCommand {
            command: current_exe.to_path_buf(),
            args,
        }
    }
}

/// Format a generated request id, mirroring pi's
/// `` `orchestrator_${++nextRequestId}_${randomUUID()}` ``.
fn format_request_id(seq: u64, uuid: &str) -> String {
    format!("orchestrator_{seq}_{uuid}")
}

/// Extract a caller-supplied command id, mirroring pi's `command.id ?? …`.
///
/// pi keeps any non-nullish `command.id`; a missing or `null` id triggers
/// generation. Orchestrator commands always carry string ids, so a missing,
/// `null`, or non-string id yields `None` (a generated id) here — a documented
/// simplification of pi's nullish-coalescing that matches every real caller.
fn command_supplied_id(command: &RpcCommand) -> Option<String> {
    match command.get("id") {
        Some(Value::String(id)) => Some(id.clone()),
        _ => None,
    }
}

/// The pure, process-independent core of [`RpcProcessInstance`]: the pending
/// request table, listener registries, UI handler, and stdout frame dispatch.
///
/// Factoring this out of the child-process plumbing keeps the correlation and
/// framing logic unit-testable without spawning a real binary. It mirrors the
/// mutable fields pi keeps on the instance (`pendingRequests`, `eventListeners`,
/// `exitListeners`, `uiRequestHandler`, `nextRequestId`, `stderrBuffer`,
/// `exited`).
struct RpcDispatch {
    exited: bool,
    stderr_buffer: String,
    next_request_id: u64,
    next_listener_id: u64,
    pending: HashMap<String, oneshot::Sender<Result<RpcResponse, RpcProcessError>>>,
    event_listeners: HashMap<u64, EventListener>,
    exit_listeners: HashMap<u64, ExitListener>,
    ui_request_handler: Option<UiRequestHandler>,
}

impl RpcDispatch {
    fn new() -> Self {
        Self {
            exited: false,
            stderr_buffer: String::new(),
            next_request_id: 0,
            next_listener_id: 0,
            pending: HashMap::new(),
            event_listeners: HashMap::new(),
            exit_listeners: HashMap::new(),
            ui_request_handler: None,
        }
    }

    /// Allocate the next request sequence number, mirroring pi's
    /// `++this.nextRequestId` (pre-increment, so the first id uses `1`).
    fn allocate_request_id(&mut self) -> u64 {
        self.next_request_id += 1;
        self.next_request_id
    }

    /// Allocate a listener handle used to support unsubscription.
    fn allocate_listener_id(&mut self) -> u64 {
        self.next_listener_id += 1;
        self.next_listener_id
    }

    /// Register a pending request keyed by id (pi's `pendingRequests.set`).
    fn register_pending(
        &mut self,
        id: String,
        sender: oneshot::Sender<Result<RpcResponse, RpcProcessError>>,
    ) {
        self.pending.insert(id, sender);
    }

    /// Dispatch a single decoded stdout line. Mirrors pi's `handleLine`.
    ///
    /// A `response` frame resolves the pending request whose id matches, and is
    /// otherwise ignored (missing/unknown id — pi returns early). An
    /// `extension_ui_request` frame is delivered to the UI handler. Every other
    /// frame is broadcast to the event listeners. Malformed JSON yields an
    /// error (pi's unguarded `JSON.parse` throws); the monitor drops such lines.
    fn handle_line(&mut self, line: &str) -> serde_json::Result<()> {
        let parsed: Value = serde_json::from_str(line)?;
        match parsed.get("type").and_then(Value::as_str) {
            Some("response") => {
                let id = match parsed
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => id.to_string(),
                    None => return Ok(()),
                };
                if let Some(sender) = self.pending.remove(&id) {
                    // Ignore send errors: the receiver may have been dropped if
                    // the caller stopped awaiting.
                    let _ = sender.send(Ok(parsed));
                }
            }
            Some("extension_ui_request") => {
                if let Some(handler) = &self.ui_request_handler {
                    handler(&parsed);
                }
            }
            _ => {
                for listener in self.event_listeners.values() {
                    listener(&parsed);
                }
            }
        }
        Ok(())
    }

    /// Reject every pending request with `error` (pi's `rejectAllPending`).
    fn reject_all_pending(&mut self, error: &RpcProcessError) {
        for (_id, sender) in self.pending.drain() {
            let _ = sender.send(Err(error.clone()));
        }
    }

    /// Notify every exit listener (pi's `notifyExit`).
    fn notify_exit(&self, error: Option<&RpcProcessError>) {
        for listener in self.exit_listeners.values() {
            listener(error);
        }
    }
}

/// Options for constructing an [`RpcProcessInstance`]. Mirrors pi's
/// `{ cwd: string }`.
#[derive(Debug, Clone)]
pub struct RpcProcessOptions {
    /// Working directory for the spawned child.
    pub cwd: PathBuf,
}

/// A subscription handle returned by [`RpcProcessInstance::on_event`] and
/// [`RpcProcessInstance::on_exit`]. Calling it unsubscribes the listener,
/// mirroring the `() => void` disposer pi returns. Unsubscribing twice is a
/// no-op (pi's `Set.delete`).
pub struct Unsubscribe {
    remove: Box<dyn Fn() + Send>,
}

impl Unsubscribe {
    /// Build an unsubscribe handle from a removal closure.
    ///
    /// Used by alternative [`crate::supervisor::RpcProcess`] implementations (test
    /// fakes) that need to return the same handle type as [`RpcProcessInstance`].
    pub fn from_fn(remove: impl Fn() + Send + 'static) -> Self {
        Self {
            remove: Box::new(remove),
        }
    }

    /// Remove the associated listener.
    pub fn unsubscribe(&self) {
        (self.remove)();
    }
}

/// A running coding-agent RPC child process. Mirrors pi's `RpcProcessInstance`.
pub struct RpcProcessInstance {
    shared: Arc<Mutex<RpcDispatch>>,
    stdin: AsyncMutex<Option<ChildStdin>>,
    kill_tx: mpsc::Sender<()>,
    exited_rx: watch::Receiver<bool>,
    /// The monitor task handle. Kept alive for the instance's lifetime; the task
    /// is detached and continues if dropped.
    _monitor: JoinHandle<()>,
}

impl RpcProcessInstance {
    /// Spawn the RPC child process. Mirrors pi's constructor.
    ///
    /// Must be called within a tokio runtime (the monitor task is spawned and
    /// the child is a tokio process). pi surfaces spawn failures asynchronously
    /// via the `error` event; tokio surfaces them synchronously, so a failed
    /// spawn returns an [`RpcProcessError`] here.
    pub fn new(options: RpcProcessOptions) -> Result<Self, RpcProcessError> {
        let current_exe = std::env::current_exe().map_err(|e| {
            RpcProcessError::new(format!("Failed to resolve current executable: {e}"))
        })?;
        let spawn = resolve_spawn_command(&current_exe, config::is_bun_binary());

        let mut command = Command::new(&spawn.command);
        command
            .args(&spawn.args)
            .current_dir(&options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Not in pi: reap the child if the instance is dropped without an
            // explicit dispose, so a forgotten instance cannot orphan a process.
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| RpcProcessError::new(format!("RPC process error: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RpcProcessError::new("Failed to create RPC process stdio"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcProcessError::new("Failed to create RPC process stdio"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RpcProcessError::new("Failed to create RPC process stdio"))?;

        let shared = Arc::new(Mutex::new(RpcDispatch::new()));
        let (kill_tx, kill_rx) = mpsc::channel(1);
        let (exited_tx, exited_rx) = watch::channel(false);

        let monitor = tokio::spawn(run_monitor(
            shared.clone(),
            child,
            stdout,
            stderr,
            kill_rx,
            exited_tx,
        ));

        Ok(Self {
            shared,
            stdin: AsyncMutex::new(Some(stdin)),
            kill_tx,
            exited_rx,
            _monitor: monitor,
        })
    }

    /// Send a command and await the correlated response. Mirrors pi's `send`.
    ///
    /// The command is stamped with an id (the caller's `command.id` if present,
    /// otherwise a generated `orchestrator_<seq>_<uuid>` id), registered as a
    /// pending request, framed, and written to stdin. The returned future
    /// resolves when a `response` frame with the matching id arrives, or rejects
    /// if the process exits, is disposed, or the write fails.
    pub async fn send(&self, command: RpcCommand) -> Result<RpcResponse, RpcProcessError> {
        let (id, framed, receiver) = {
            let mut dispatch = self.shared.lock().expect("rpc dispatch poisoned");
            if dispatch.exited {
                return Err(RpcProcessError::new(format!(
                    "RPC process is not running. Stderr: {}",
                    dispatch.stderr_buffer
                )));
            }
            let id = command_supplied_id(&command)
                .unwrap_or_else(|| format_request_id(dispatch.allocate_request_id(), &uuidv7()));

            let mut full_command = command;
            if let Some(object) = full_command.as_object_mut() {
                object.insert("id".to_string(), Value::String(id.clone()));
            }
            let framed = format!(
                "{}\n",
                serde_json::to_string(&full_command)
                    .map_err(|e| RpcProcessError::new(e.to_string()))?
            );

            let (sender, receiver) = oneshot::channel();
            dispatch.register_pending(id.clone(), sender);
            (id, framed, receiver)
        };

        {
            let mut guard = self.stdin.lock().await;
            let write_result = match guard.as_mut() {
                Some(stdin) => stdin.write_all(framed.as_bytes()).await,
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "RPC process stdin closed",
                )),
            };
            if let Err(error) = write_result {
                self.shared
                    .lock()
                    .expect("rpc dispatch poisoned")
                    .pending
                    .remove(&id);
                return Err(RpcProcessError::new(error.to_string()));
            }
            if let Some(stdin) = guard.as_mut() {
                let _ = stdin.flush().await;
            }
        }

        match receiver.await {
            Ok(response) => response,
            // The sender was dropped without resolving (the dispatch was cleared
            // without a recorded error); surface it as a disposed process.
            Err(_) => Err(RpcProcessError::new("RPC process disposed")),
        }
    }

    /// Write a UI response back to the child. Mirrors pi's `handleUiResponse`
    /// (fire-and-forget; ignored once the process has exited).
    pub async fn handle_ui_response(&self, response: RpcExtensionUIResponse) {
        if self.shared.lock().expect("rpc dispatch poisoned").exited {
            return;
        }
        let Ok(framed) = serde_json::to_string(&response) else {
            return;
        };
        let framed = format!("{framed}\n");
        let mut guard = self.stdin.lock().await;
        if let Some(stdin) = guard.as_mut() {
            let _ = stdin.write_all(framed.as_bytes()).await;
            let _ = stdin.flush().await;
        }
    }

    /// Set (or clear with `None`) the extension-UI request handler. Mirrors pi's
    /// `setUiRequestHandler`.
    pub fn set_ui_request_handler(&self, handler: Option<UiRequestHandler>) {
        self.shared
            .lock()
            .expect("rpc dispatch poisoned")
            .ui_request_handler = handler;
    }

    /// Build the [`Unsubscribe`] handle that runs `remove` (which deletes the
    /// listener from its registry by id) against the shared dispatch. Shared by
    /// [`Self::on_event`] and [`Self::on_exit`], whose only difference is which
    /// registry the listener lives in.
    fn unsubscribe(&self, remove: impl Fn(&mut RpcDispatch) + Send + 'static) -> Unsubscribe {
        let shared = self.shared.clone();
        Unsubscribe {
            remove: Box::new(move || {
                remove(&mut shared.lock().expect("rpc dispatch poisoned"));
            }),
        }
    }

    /// Register an event listener, returning a handle that unsubscribes it.
    /// Mirrors pi's `onEvent`.
    pub fn on_event<F>(&self, listener: F) -> Unsubscribe
    where
        F: Fn(&AgentSessionEvent) + Send + 'static,
    {
        let id = {
            let mut dispatch = self.shared.lock().expect("rpc dispatch poisoned");
            let id = dispatch.allocate_listener_id();
            dispatch.event_listeners.insert(id, Box::new(listener));
            id
        };
        self.unsubscribe(move |dispatch| {
            dispatch.event_listeners.remove(&id);
        })
    }

    /// Register an exit listener, returning a handle that unsubscribes it.
    /// Mirrors pi's `onExit`.
    pub fn on_exit<F>(&self, listener: F) -> Unsubscribe
    where
        F: Fn(Option<&RpcProcessError>) + Send + 'static,
    {
        let id = {
            let mut dispatch = self.shared.lock().expect("rpc dispatch poisoned");
            let id = dispatch.allocate_listener_id();
            dispatch.exit_listeners.insert(id, Box::new(listener));
            id
        };
        self.unsubscribe(move |dispatch| {
            dispatch.exit_listeners.remove(&id);
        })
    }

    /// Terminate the child and reject all pending requests. Mirrors pi's
    /// `dispose`.
    ///
    /// Clears the UI handler and rejects pending requests with an
    /// `RPC process disposed` error. If the process has already exited this
    /// returns immediately; otherwise it signals the monitor to kill the child
    /// and awaits its exit.
    pub async fn dispose(&self) {
        {
            let mut dispatch = self.shared.lock().expect("rpc dispatch poisoned");
            dispatch.ui_request_handler = None;
            dispatch.reject_all_pending(&RpcProcessError::new("RPC process disposed"));
            if dispatch.exited {
                return;
            }
        }
        let _ = self.kill_tx.send(()).await;
        let mut exited_rx = self.exited_rx.clone();
        if !*exited_rx.borrow() {
            while exited_rx.changed().await.is_ok() {
                if *exited_rx.borrow() {
                    break;
                }
            }
        }
    }
}

/// Factory mirroring pi's `createRpcProcessInstance`.
pub fn create_rpc_process_instance(
    options: RpcProcessOptions,
) -> Result<RpcProcessInstance, RpcProcessError> {
    RpcProcessInstance::new(options)
}

/// Render an [`std::process::ExitStatus`] signal for the exit message, mirroring
/// pi's `signal` template value (`null` when the process exited normally).
fn signal_string(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return signal.to_string();
        }
    }
    let _ = status;
    "null".to_string()
}

/// Background monitor: frames stdout into lines and dispatches them, buffers
/// stderr, and — once both pipes reach EOF — reaps the child and reports its
/// exit. Mirrors the work pi does in its `stdout`/`stderr`/`exit`/`error`
/// stream handlers.
async fn run_monitor(
    shared: Arc<Mutex<RpcDispatch>>,
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    mut kill_rx: mpsc::Receiver<()>,
    exited_tx: watch::Sender<bool>,
) {
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr);
    let mut stderr_chunk = [0u8; 4096];

    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut kill_done = false;

    while !(stdout_done && stderr_done) {
        tokio::select! {
            biased;

            kill = kill_rx.recv(), if !kill_done => {
                kill_done = true;
                if kill.is_some() {
                    let _ = child.start_kill();
                }
            }

            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            // Malformed frames are dropped (pi's unguarded parse
                            // would throw inside the stream handler).
                            let _ = shared
                                .lock()
                                .expect("rpc dispatch poisoned")
                                .handle_line(trimmed);
                        }
                    }
                    Ok(None) | Err(_) => stdout_done = true,
                }
            }

            read = stderr_reader.read(&mut stderr_chunk), if !stderr_done => {
                match read {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&stderr_chunk[..n]);
                        shared
                            .lock()
                            .expect("rpc dispatch poisoned")
                            .stderr_buffer
                            .push_str(&text);
                    }
                }
            }
        }
    }

    let status = child.wait().await;
    let (code, signal) = match &status {
        Ok(status) => (
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string()),
            signal_string(status),
        ),
        Err(_) => ("null".to_string(), "null".to_string()),
    };

    let stderr_buffer = shared
        .lock()
        .expect("rpc dispatch poisoned")
        .stderr_buffer
        .clone();
    let error = RpcProcessError::new(format!(
        "RPC process exited (code={code} signal={signal}). Stderr: {stderr_buffer}"
    ));

    {
        let mut dispatch = shared.lock().expect("rpc dispatch poisoned");
        dispatch.exited = true;
        dispatch.reject_all_pending(&error);
        dispatch.notify_exit(Some(&error));
    }
    let _ = exited_tx.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn dispatch() -> RpcDispatch {
        RpcDispatch::new()
    }

    /// A dispatch with a single pending request registered under `id`, plus the
    /// receiver that the request would resolve through.
    #[allow(clippy::type_complexity)]
    fn dispatch_with_pending(
        id: &str,
    ) -> (
        RpcDispatch,
        oneshot::Receiver<Result<RpcResponse, RpcProcessError>>,
    ) {
        let mut dispatch = dispatch();
        let (tx, rx) = oneshot::channel();
        dispatch.register_pending(id.to_string(), tx);
        (dispatch, rx)
    }

    // ---- spawn command resolution -----------------------------------------

    #[test]
    fn spawn_command_bun_branch_uses_sibling_agent_binary() {
        let exe = Path::new("/opt/pidgin/bin/orchestrator");
        let spawn = resolve_spawn_command(exe, true);
        assert_eq!(
            spawn.command,
            PathBuf::from("/opt/pidgin/bin").join(agent_binary_file_name())
        );
        assert_eq!(spawn.args, vec!["--mode".to_string(), "rpc".to_string()]);
    }

    #[test]
    fn spawn_command_non_bun_branch_uses_current_exe() {
        let exe = Path::new("/usr/local/bin/pidgin");
        let spawn = resolve_spawn_command(exe, false);
        assert_eq!(spawn.command, PathBuf::from("/usr/local/bin/pidgin"));
        assert_eq!(spawn.args, vec!["--mode".to_string(), "rpc".to_string()]);
    }

    // ---- id helpers -------------------------------------------------------

    #[test]
    fn format_request_id_matches_pi_shape() {
        assert_eq!(
            format_request_id(1, "abc-123"),
            "orchestrator_1_abc-123".to_string()
        );
    }

    #[test]
    fn allocate_request_id_pre_increments_from_one() {
        let mut dispatch = dispatch();
        assert_eq!(dispatch.allocate_request_id(), 1);
        assert_eq!(dispatch.allocate_request_id(), 2);
        assert_eq!(dispatch.allocate_request_id(), 3);
    }

    #[test]
    fn command_supplied_id_prefers_string_id_else_none() {
        assert_eq!(
            command_supplied_id(&json!({ "id": "caller-id", "method": "ping" })),
            Some("caller-id".to_string())
        );
        assert_eq!(command_supplied_id(&json!({ "method": "ping" })), None);
        assert_eq!(command_supplied_id(&json!({ "id": null })), None);
        // Non-string ids are not honored (documented simplification).
        assert_eq!(command_supplied_id(&json!({ "id": 7 })), None);
    }

    // ---- response correlation ---------------------------------------------

    #[test]
    fn handle_line_resolves_matching_pending_request() {
        let (mut dispatch, mut rx) = dispatch_with_pending("req-1");

        dispatch
            .handle_line(r#"{"type":"response","id":"req-1","result":{"ok":true}}"#)
            .unwrap();

        let resolved = rx.try_recv().expect("request should be resolved").unwrap();
        assert_eq!(
            resolved,
            json!({ "type": "response", "id": "req-1", "result": { "ok": true } })
        );
        assert!(dispatch.pending.is_empty(), "pending entry consumed");
    }

    #[test]
    fn handle_line_ignores_response_with_mismatched_id() {
        let (mut dispatch, mut rx) = dispatch_with_pending("req-1");

        dispatch
            .handle_line(r#"{"type":"response","id":"other","result":1}"#)
            .unwrap();

        // The registered request is untouched; nothing was delivered.
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(dispatch.pending.contains_key("req-1"));
    }

    #[test]
    fn handle_line_ignores_response_without_id() {
        let (mut dispatch, mut rx) = dispatch_with_pending("req-1");

        dispatch
            .handle_line(r#"{"type":"response","result":1}"#)
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        // An empty-string id is treated as absent (pi's falsy check).
        dispatch
            .handle_line(r#"{"type":"response","id":"","result":1}"#)
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(dispatch.pending.contains_key("req-1"));
    }

    // ---- event + UI dispatch ----------------------------------------------

    #[test]
    fn handle_line_broadcasts_non_response_frames_to_event_listeners() {
        let mut dispatch = dispatch();
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = seen.clone();
        let id = dispatch.allocate_listener_id();
        dispatch.event_listeners.insert(
            id,
            Box::new(move |event: &Value| sink.lock().unwrap().push(event.clone())),
        );

        dispatch
            .handle_line(r#"{"type":"agent_event","phase":"start"}"#)
            .unwrap();
        // A frame with no `type` is also an event (pi's default branch).
        dispatch.handle_line(r#"{"delta":"hi"}"#).unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], json!({ "type": "agent_event", "phase": "start" }));
        assert_eq!(seen[1], json!({ "delta": "hi" }));
    }

    #[test]
    fn handle_line_routes_extension_ui_requests_to_the_handler() {
        let mut dispatch = dispatch();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        dispatch.ui_request_handler = Some(Box::new(move |_request: &Value| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        dispatch
            .handle_line(r#"{"type":"extension_ui_request","id":"u1"}"#)
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A UI request must not reach the event listeners.
        let events = Arc::new(AtomicUsize::new(0));
        let sink = events.clone();
        let listener_id = dispatch.allocate_listener_id();
        dispatch.event_listeners.insert(
            listener_id,
            Box::new(move |_event: &Value| {
                sink.fetch_add(1, Ordering::SeqCst);
            }),
        );
        dispatch
            .handle_line(r#"{"type":"extension_ui_request","id":"u2"}"#)
            .unwrap();
        assert_eq!(events.load(Ordering::SeqCst), 0);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn removing_an_event_listener_stops_delivery() {
        let mut dispatch = dispatch();
        let count = Arc::new(AtomicUsize::new(0));
        let sink = count.clone();
        let id = dispatch.allocate_listener_id();
        dispatch.event_listeners.insert(
            id,
            Box::new(move |_event: &Value| {
                sink.fetch_add(1, Ordering::SeqCst);
            }),
        );

        dispatch.handle_line(r#"{"type":"e"}"#).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        dispatch.event_listeners.remove(&id);
        dispatch.handle_line(r#"{"type":"e"}"#).unwrap();
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "listener no longer invoked"
        );
    }

    // ---- malformed input --------------------------------------------------

    #[test]
    fn handle_line_reports_malformed_json() {
        let mut dispatch = dispatch();
        assert!(dispatch.handle_line("{not json").is_err());
        assert!(dispatch.handle_line("").is_err());
    }

    // ---- rejection + exit notification ------------------------------------

    #[test]
    fn reject_all_pending_rejects_every_registered_request() {
        let mut dispatch = dispatch();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        dispatch.register_pending("a".to_string(), tx1);
        dispatch.register_pending("b".to_string(), tx2);

        dispatch.reject_all_pending(&RpcProcessError::new("boom"));

        let err1 = rx1.try_recv().unwrap().unwrap_err();
        let err2 = rx2.try_recv().unwrap().unwrap_err();
        assert_eq!(err1.message(), "boom");
        assert_eq!(err2.message(), "boom");
        assert!(dispatch.pending.is_empty());
    }

    #[test]
    fn notify_exit_invokes_exit_listeners_with_the_error() {
        let mut dispatch = dispatch();
        let messages = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let sink = messages.clone();
        let id = dispatch.allocate_listener_id();
        dispatch.exit_listeners.insert(
            id,
            Box::new(move |error: Option<&RpcProcessError>| {
                sink.lock()
                    .unwrap()
                    .push(error.map(|e| e.message().to_string()));
            }),
        );

        dispatch.notify_exit(Some(&RpcProcessError::new("exit-1")));
        dispatch.notify_exit(None);

        let messages = messages.lock().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], Some("exit-1".to_string()));
        assert_eq!(messages[1], None);
    }

    #[test]
    fn rpc_process_error_displays_its_message() {
        let error = RpcProcessError::new("something failed");
        assert_eq!(error.to_string(), "something failed");
        assert_eq!(error.message(), "something failed");
    }
}
