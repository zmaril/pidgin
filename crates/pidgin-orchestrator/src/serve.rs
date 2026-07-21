//! The orchestrator serve entrypoint, mirroring
//! `packages/orchestrator/src/serve.ts`.
//!
//! pi's `serve()` creates the socket directory, starts the IPC server bound to
//! `handleIpcRequest` (with `openRpcStream` attached), recovers supervised
//! instances left over from a previous run, starts radius presence (or reports it
//! disabled), and then installs `SIGINT`/`SIGTERM` handlers so a signal drives a
//! graceful shutdown (close the server, shut the supervisor down, stop radius,
//! and unlink the socket) before the process exits.
//!
//! # Production seams
//!
//! Where pi reaches for module-global singletons (`supervisor`, `radiusPresence`)
//! and a live `node:net` server, this port wires the *production* seam
//! implementations the earlier stages introduced:
//!
//! * the IPC server binds a real [`tokio::net::UnixListener`] through
//!   [`start_ipc_server`] (which also runs the stale-socket
//!   [`UnixSocketProbe`](crate::ipc::transport::UnixSocketProbe));
//! * the supervisor spawns real children through [`RealRpcProcessSpawner`];
//! * radius presence is a real [`RadiusPresence`] over the [`SystemRadiusClock`],
//!   reading stored credentials on demand from the coding-agent's `auth.json`
//!   via [`pidgin_coding::core::auth::read_stored_credential`];
//! * `SIGINT`/`SIGTERM` are handled with tokio's `signal` feature.
//!
//! # Radius HTTP transport
//!
//! Radius registration/heartbeat is an HTTP round-trip. The standalone
//! `orchestrator` binary has no host `fetch` to delegate to, so it performs
//! radius HTTP directly through the native [`ReqwestTransport`], which honors the
//! ambient proxy environment (`HTTPS_PROXY`/`NO_PROXY`) like pi's undici client.
//! HTTP is only exercised when radius is *enabled* (credentials present); with
//! radius disabled — the default — `serve()` performs no HTTP at all.

use std::io;
use std::path::Path;
use std::sync::Arc;

use pidgin_ai::seams::ReqwestTransport;

use crate::config::get_socket_path;
use crate::handler::OrchestratorHandler;
use crate::ipc::server::{start_ipc_server, IpcServer};
use crate::radius::{get_radius_orchestrator_base_url, RadiusPresence, SystemRadiusClock};
use crate::supervisor::{OrchestratorSupervisor, RealRpcProcessSpawner, SupervisorError};

/// Run the orchestrator server until a signal or fatal error triggers shutdown.
///
/// Mirrors pi's `serve()`: make the socket directory, start the IPC server,
/// recover supervised instances, start radius presence, then block on the
/// shutdown signal. On a startup failure (recover/radius) the server is stopped
/// and the socket unlinked before the error propagates, exactly as pi's
/// `try/catch` does.
pub async fn serve() -> io::Result<()> {
    let socket_path = get_socket_path();
    // pi: `mkdirSync(dirname(socketPath), { recursive: true })`.
    if let Some(parent) = socket_parent_dir(&socket_path) {
        std::fs::create_dir_all(parent)?;
    }

    let supervisor = build_production_supervisor();
    let handler = Arc::new(OrchestratorHandler::new(supervisor.clone()));
    let server = start_ipc_server(handler).await?;

    // pi wraps recover + radius start in a `try`; on failure it closes the
    // server, unlinks the socket, and rethrows.
    match prepare_runtime(&supervisor, &socket_path).await {
        Ok(startup_lines) => {
            for line in startup_lines {
                println!("{line}");
            }
        }
        Err(error) => {
            server.abort();
            remove_socket_if_present(&socket_path);
            return Err(io::Error::other(error.to_string()));
        }
    }

    println!("orchestrator listening on {}", socket_path.display());

    // pi installs SIGINT/SIGTERM handlers and awaits a never-resolving promise to
    // keep the process alive; a signal runs `shutdown(0)`.
    wait_for_shutdown_signal().await;
    graceful_shutdown(&server, &supervisor, &socket_path).await;
    Ok(())
}

/// Build the production supervisor: a real radius presence (system clock, with
/// credentials read from the coding-agent's `auth.json`), the real RPC-child
/// spawner, and the system ISO clock.
///
/// Mirrors how pi's module graph wires the `supervisor` and `radiusPresence`
/// singletons, but with the injected production seams this port uses.
fn build_production_supervisor() -> OrchestratorSupervisor {
    let radius = RadiusPresence::new(
        Box::new(production_radius_transport()),
        Box::new(SystemRadiusClock),
    );
    OrchestratorSupervisor::new(
        radius,
        Arc::new(RealRpcProcessSpawner),
        Arc::new(SystemRadiusClock),
    )
}

/// The production radius `HttpTransport`.
///
/// The standalone binary has no host `fetch`, so it reaches providers directly
/// through the native [`ReqwestTransport`]. That transport honors the ambient
/// proxy environment (`HTTPS_PROXY`/`NO_PROXY`) and applies no total timeout,
/// mirroring pi's undici client. It is only consulted when radius is enabled.
fn production_radius_transport() -> ReqwestTransport {
    ReqwestTransport::new()
}

/// pi's `serve.ts` body between "server bound" and the signal wait: recover
/// supervised instances, then start radius presence (or report it disabled).
///
/// Returns the startup lines pi logs, so the flow is unit-testable without a
/// socket or signal handlers.
async fn prepare_runtime(
    supervisor: &OrchestratorSupervisor,
    socket_path: &Path,
) -> Result<Vec<String>, SupervisorError> {
    supervisor.recover_after_restart().await?;

    let mut lines = Vec::new();
    if supervisor.is_radius_enabled() {
        let outcome = supervisor.start_radius_presence(None)?;
        lines.push(format!(
            "radius integration enabled: {} -> {}",
            socket_path.display(),
            get_radius_orchestrator_base_url()
        ));
        if let Some(outcome) = outcome {
            lines.push(format!("radius machine id: {}", outcome.machine.id));
        }
    } else {
        lines.push(
            "radius integration disabled: login radius in ~/.pi/agent/auth.json or set \
             RADIUS_API_KEY"
                .to_string(),
        );
    }
    Ok(lines)
}

/// The graceful shutdown sequence (pi's `shutdown`): stop accepting connections,
/// shut the supervisor down, stop radius presence, and unlink the socket.
///
/// pi's shutdown is fired via `void shutdown(...)`, so a failure would be an
/// unhandled rejection; here each fallible step logs and continues so the socket
/// is always unlinked.
async fn graceful_shutdown(
    server: &IpcServer,
    supervisor: &OrchestratorSupervisor,
    socket_path: &Path,
) {
    server.abort();
    if let Err(error) = supervisor.shutdown().await {
        eprintln!("Failed to shut down supervisor: {error}");
    }
    if let Err(error) = supervisor.stop_radius_presence() {
        eprintln!("Failed to stop radius presence: {error}");
    }
    remove_socket_if_present(socket_path);
}

/// The directory that holds the socket (pi's `dirname(socketPath)`).
fn socket_parent_dir(socket_path: &Path) -> Option<&Path> {
    socket_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Unlink the socket file if it exists (pi's `if (existsSync(socketPath))
/// unlinkSync(socketPath)`).
fn remove_socket_if_present(socket_path: &Path) {
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
}

/// Block until a `SIGINT` or `SIGTERM` arrives (pi's signal handlers + keep-alive
/// promise). On non-Unix targets this falls back to `Ctrl-C`.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("Failed to install SIGINT handler: {error}");
                return;
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("Failed to install SIGTERM handler: {error}");
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::seams::http::ScriptedTransport;
    use std::path::PathBuf;
    use std::sync::MutexGuard;

    /// A tempdir-backed environment that steers the storage/radius helpers and
    /// serializes on the crate-wide env lock (these vars are process-global).
    struct TestEnv {
        _lock: MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        saved_dir: Option<String>,
        saved_api_key: Option<String>,
        saved_agent_dir: Option<String>,
    }

    impl TestEnv {
        fn new() -> Self {
            let lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved_dir = std::env::var("PI_ORCHESTRATOR_DIR").ok();
            let saved_api_key = std::env::var("RADIUS_API_KEY").ok();
            let saved_agent_dir = std::env::var("PI_CODING_AGENT_DIR").ok();
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("PI_ORCHESTRATOR_DIR", dir.path());
            std::env::remove_var("RADIUS_API_KEY");
            // Point the coding-agent dir at the empty tempdir so radius credential
            // reads (pi's `readStoredCredential`) find no `auth.json` and radius
            // stays deterministically disabled, independent of the real `~/.pi`.
            std::env::set_var("PI_CODING_AGENT_DIR", dir.path());
            TestEnv {
                _lock: lock,
                _dir: dir,
                saved_dir,
                saved_api_key,
                saved_agent_dir,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            match &self.saved_dir {
                Some(value) => std::env::set_var("PI_ORCHESTRATOR_DIR", value),
                None => std::env::remove_var("PI_ORCHESTRATOR_DIR"),
            }
            match &self.saved_api_key {
                Some(value) => std::env::set_var("RADIUS_API_KEY", value),
                None => std::env::remove_var("RADIUS_API_KEY"),
            }
            match &self.saved_agent_dir {
                Some(value) => std::env::set_var("PI_CODING_AGENT_DIR", value),
                None => std::env::remove_var("PI_CODING_AGENT_DIR"),
            }
        }
    }

    /// A supervisor over the given radius transport, the real RPC spawner (never
    /// invoked in these tests — no instance is spawned), and the system clock.
    fn supervisor_with(transport: ScriptedTransport) -> OrchestratorSupervisor {
        let radius = RadiusPresence::new(Box::new(transport), Box::new(SystemRadiusClock));
        OrchestratorSupervisor::new(
            radius,
            Arc::new(RealRpcProcessSpawner),
            Arc::new(SystemRadiusClock),
        )
    }

    #[test]
    fn socket_parent_dir_is_the_socket_directory() {
        assert_eq!(
            socket_parent_dir(Path::new("/data/orch/orchestrator.sock")),
            Some(Path::new("/data/orch"))
        );
        // A bare filename has no meaningful parent directory.
        assert_eq!(socket_parent_dir(Path::new("orchestrator.sock")), None);
    }

    #[test]
    fn remove_socket_if_present_unlinks_only_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orchestrator.sock");
        // Missing file: a no-op that does not error.
        remove_socket_if_present(&path);
        std::fs::write(&path, b"").unwrap();
        remove_socket_if_present(&path);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn prepare_runtime_recovers_and_reports_radius_disabled() {
        let _env = TestEnv::new();
        // A stale `online` instance from a previous run should be recovered to
        // `stopped` by `recoverAfterRestart`, and radius should read as disabled.
        let stale = crate::types::InstanceRecord {
            id: "i-old".to_string(),
            status: crate::types::InstanceStatus::Online,
            cwd: "/work".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: Some("2026-01-01T00:00:00.000Z".to_string()),
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        };
        crate::storage::save_instances(std::slice::from_ref(&stale)).unwrap();

        let supervisor = supervisor_with(ScriptedTransport::new());
        let socket_path = PathBuf::from("/data/orch/orchestrator.sock");
        let lines = prepare_runtime(&supervisor, &socket_path).await.unwrap();

        assert_eq!(
            lines,
            vec![
                "radius integration disabled: login radius in ~/.pi/agent/auth.json or set \
                 RADIUS_API_KEY"
                    .to_string()
            ]
        );
        // The stale instance was rewritten to `stopped` (recover wiring ran).
        let recovered = crate::storage::load_instances().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, crate::types::InstanceStatus::Stopped);
    }

    #[tokio::test]
    async fn prepare_runtime_starts_radius_presence_when_enabled() {
        let _env = TestEnv::new();
        std::env::set_var("RADIUS_API_KEY", "radius-key");

        // The scripted transport answers the machine-register POST radius makes.
        let transport = ScriptedTransport::new();
        transport.push_ok(r#"{"id":"machine-1","heartbeatIntervalMs":25000,"expiresInMs":60000}"#);
        let supervisor = supervisor_with(transport);

        let socket_path = PathBuf::from("/data/orch/orchestrator.sock");
        let lines = prepare_runtime(&supervisor, &socket_path).await.unwrap();

        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("radius integration enabled: /data/orch/orchestrator.sock -> "),
            "unexpected first line: {}",
            lines[0]
        );
        assert_eq!(lines[1], "radius machine id: machine-1");
    }

    #[tokio::test]
    async fn prepare_runtime_propagates_a_radius_start_failure() {
        let _env = TestEnv::new();
        std::env::set_var("RADIUS_API_KEY", "radius-key");

        // Enabled, but the register POST fails: the error propagates (pi's `try`
        // rethrow, which `serve()` turns into a startup failure).
        let supervisor = supervisor_with(ScriptedTransport::new());
        let socket_path = PathBuf::from("/data/orch/orchestrator.sock");
        assert!(prepare_runtime(&supervisor, &socket_path).await.is_err());
    }
}
