//! IPC request dispatch, mirroring `packages/orchestrator/src/handler.ts`.
//!
//! pi's `handler.ts` exports two things bound to the module-level `supervisor`
//! singleton: `handleIpcRequest`, which maps each [`OrchestratorRequest`] variant
//! to a supervisor call and back to an [`OrchestratorResponse`], and
//! `openRpcStream`, the adapter that bridges an incoming `rpc_stream` to a
//! supervised RPC child.
//!
//! This port binds both to an injected [`OrchestratorSupervisor`] through the
//! [`OrchestratorHandler`], which implements the [`IpcRequestHandler`] seam the
//! [`crate::ipc::server`] drives (so the server relays requests to the supervisor
//! without either side importing the other). The `rpc_stream` adapter is a
//! [`RpcStreamSession`] built over the supervisor's [`SupervisorRpcStream`],
//! relaying frames through the server's [`RpcStreamSink`].
//!
//! # Error boundary
//!
//! pi's `handleIpcRequest` does not catch — the IPC server's connection
//! `try/catch` turns a thrown supervisor error into an error frame. The Rust
//! [`IpcRequestHandler::handle`] seam is infallible (it returns an
//! [`OrchestratorResponse`]), so that boundary moves here: supervisor failures
//! are mapped to an [`ErrorResponse`] with the error's message, exactly the frame
//! pi's server would have produced.
//!
//! # Streaming caveat
//!
//! Full `rpc_stream` / [`AgentSessionEvent`] streaming parity is deferred until
//! pidgin-coding emits live agent events (see the seam-decisions record); until
//! then the adapter relays whatever opaque [`serde_json::Value`] frames the child
//! produces.

// straitjacket-allow-file:duplication — the per-request dispatch arms and the
// summary/error-response construction parallel pi's handler.ts closely; the
// repetition is a faithful mirror of pi's control flow, not extractable logic.

use std::future::Future;
use std::pin::Pin;

use crate::ipc::protocol::{
    AgentSessionEvent, ErrorResponse, InstanceSummary, ListResponse, OrchestratorRequest,
    OrchestratorResponse, ResponseBase, RpcBridgeResponse, RpcClientMessage, RpcExtensionUIRequest,
    RpcReadyResponse, RpcResponse, SpawnResponse, StatusResponse, StopResponse,
};
use crate::ipc::server::{IpcRequestHandler, RpcStreamSession, RpcStreamSink};
use crate::supervisor::{OrchestratorSupervisor, SpawnOptions, SupervisorRpcStream};
use crate::types::InstanceRecord;

/// A boxed, `Send` future — the return shape for the object-safe seam traits.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Summarize a record for a response (pi's `toInstanceSummary`).
fn to_instance_summary(instance: &InstanceRecord) -> InstanceSummary {
    InstanceSummary {
        id: instance.id.clone(),
        status: instance.status,
        cwd: instance.cwd.clone(),
        label: instance.label.clone(),
        session_id: instance.session_id.clone(),
        session_file: instance.session_file.clone(),
        radius_pi_id: instance.radius_pi_id.clone(),
    }
}

/// The `{ ok: true }` response base shared by successful responses.
fn ok_base() -> ResponseBase {
    ResponseBase {
        ok: true,
        error: None,
    }
}

/// The `{ type: "error", ok: false, error: "Unknown instance: <id>" }` response
/// (pi's `unknownInstanceError`).
fn unknown_instance_error(instance_id: &str) -> OrchestratorResponse {
    error_response(format!("Unknown instance: {instance_id}"))
}

/// An `error` response carrying `message` (pi's server `try/catch` frame).
fn error_response(message: String) -> OrchestratorResponse {
    OrchestratorResponse::Error(ErrorResponse {
        ok: false,
        error: message,
    })
}

/// Dispatches orchestrator IPC requests against an [`OrchestratorSupervisor`].
///
/// Mirrors pi's `handler.ts`, whose free functions are bound to the module
/// `supervisor`; here the supervisor is injected so the handler (and the server
/// it plugs into) can be driven in tests.
#[derive(Clone)]
pub struct OrchestratorHandler {
    supervisor: OrchestratorSupervisor,
}

impl OrchestratorHandler {
    /// Build a handler over `supervisor`.
    pub fn new(supervisor: OrchestratorSupervisor) -> Self {
        Self { supervisor }
    }

    /// Dispatch one request to the supervisor and build its response.
    ///
    /// Mirrors pi's `handleIpcRequest`, with supervisor failures mapped to an
    /// error frame (the boundary pi's server owns).
    pub async fn handle_ipc_request(&self, request: OrchestratorRequest) -> OrchestratorResponse {
        match request {
            OrchestratorRequest::Spawn(request) => {
                match self
                    .supervisor
                    .spawn_instance(SpawnOptions {
                        cwd: request.cwd,
                        label: request.label,
                    })
                    .await
                {
                    Ok(instance) => OrchestratorResponse::Spawn(SpawnResponse {
                        base: ok_base(),
                        instance: Some(to_instance_summary(&instance)),
                    }),
                    Err(error) => error_response(error.to_string()),
                }
            }

            OrchestratorRequest::List(_) => match self.supervisor.list_instances() {
                Ok(instances) => OrchestratorResponse::List(ListResponse {
                    base: ok_base(),
                    instances: Some(instances.iter().map(to_instance_summary).collect()),
                }),
                Err(error) => error_response(error.to_string()),
            },

            OrchestratorRequest::Status(request) => {
                match self.supervisor.get_instance(&request.instance_id) {
                    Ok(Some(instance)) => OrchestratorResponse::Status(StatusResponse {
                        base: ok_base(),
                        instance: Some(to_instance_summary(&instance)),
                    }),
                    Ok(None) => unknown_instance_error(&request.instance_id),
                    Err(error) => error_response(error.to_string()),
                }
            }

            OrchestratorRequest::Stop(request) => {
                match self.supervisor.stop_instance(&request.instance_id).await {
                    Ok(Some(_)) => OrchestratorResponse::Stop(StopResponse {
                        base: ok_base(),
                        instance_id: Some(request.instance_id),
                    }),
                    Ok(None) => unknown_instance_error(&request.instance_id),
                    Err(error) => error_response(error.to_string()),
                }
            }

            OrchestratorRequest::Rpc(request) => {
                match self
                    .supervisor
                    .handle_rpc(&request.instance_id, request.command)
                    .await
                {
                    Ok(Some(response)) => OrchestratorResponse::Rpc(RpcBridgeResponse {
                        base: ok_base(),
                        response,
                    }),
                    Ok(None) => unknown_instance_error(&request.instance_id),
                    Err(error) => error_response(error.to_string()),
                }
            }

            OrchestratorRequest::RpcStream(request) => {
                match self.supervisor.get_instance(&request.instance_id) {
                    Ok(Some(instance)) => OrchestratorResponse::RpcReady(RpcReadyResponse {
                        base: ok_base(),
                        instance: Some(to_instance_summary(&instance)),
                    }),
                    Ok(None) => unknown_instance_error(&request.instance_id),
                    Err(error) => error_response(error.to_string()),
                }
            }
        }
    }

    /// Open an RPC stream adapter for `instance_id` (pi's free `openRpcStream`).
    ///
    /// Relays child-produced session events and UI requests to the server sink,
    /// and returns a session that relays client frames to the child. `None` if the
    /// instance has no live child (pi returns `undefined`).
    pub fn open_rpc_stream(
        &self,
        instance_id: &str,
        sink: RpcStreamSink,
    ) -> Option<SupervisorStreamSession> {
        let events_sink = sink.clone();
        let ui_sink = sink.clone();
        let on_event = Box::new(move |event: &AgentSessionEvent| {
            events_sink.send_session_event(event.clone());
        });
        let on_ui_request = Box::new(move |request: &RpcExtensionUIRequest| {
            ui_sink.send_ui_request(request.clone());
        });
        let stream = self
            .supervisor
            .open_rpc_stream(instance_id, on_event, on_ui_request)?;
        Some(SupervisorStreamSession { stream, sink })
    }
}

impl IpcRequestHandler for OrchestratorHandler {
    fn handle(&self, request: OrchestratorRequest) -> BoxFuture<'_, OrchestratorResponse> {
        Box::pin(self.handle_ipc_request(request))
    }

    fn open_rpc_stream(
        &self,
        instance_id: String,
        sink: RpcStreamSink,
    ) -> Option<Box<dyn RpcStreamSession>> {
        OrchestratorHandler::open_rpc_stream(self, &instance_id, sink)
            .map(|session| Box::new(session) as Box<dyn RpcStreamSession>)
    }
}

/// The RPC-stream session the handler hands the server, mirroring the object pi's
/// `openRpcStream` returns (`{ handleRequest, close }`).
pub struct SupervisorStreamSession {
    stream: SupervisorRpcStream,
    sink: RpcStreamSink,
}

impl RpcStreamSession for SupervisorStreamSession {
    fn handle_request(&mut self, message: RpcClientMessage) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            // pi routes an `extension_ui_response` straight to the child; every
            // other frame is an RPC command whose response is relayed back.
            if message.get("type").and_then(|value| value.as_str()) == Some("extension_ui_response")
            {
                self.stream.handle_ui_response(message).await;
                return Ok(());
            }
            match self.stream.handle_rpc(message).await {
                Ok(response) => {
                    let response: RpcResponse = response;
                    self.sink.send_response(response);
                    Ok(())
                }
                // A relayed error becomes an error frame on the bridge (pi's
                // per-request `try/catch`).
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn close(&mut self) {
        self.stream.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::{
        RpcRequest, RpcStreamRequest, SpawnRequest, StatusRequest, StopRequest,
    };
    use crate::radius::{RadiusClock, RadiusPresence, SystemRadiusClock};
    use crate::rpc_process::Unsubscribe;
    use crate::rpc_process::{RpcProcessError, RpcProcessOptions};
    use crate::supervisor::{
        AgentSessionEventListener, ExitListener, RpcProcess, RpcProcessSpawner, UiRequestHandler,
    };
    use crate::types::InstanceStatus;
    use pidgin_ai::seams::http::ScriptedTransport;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, MutexGuard};
    use tokio::sync::mpsc;

    // -- test environment (storage + radius-enable env are process-global) ----

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

    #[derive(Clone, Copy)]
    struct FixedClock;
    impl RadiusClock for FixedClock {
        fn now_iso(&self) -> String {
            "2026-01-01T00:00:00.000Z".to_string()
        }
    }

    // -- minimal fake child (echo get_state + success) -----------------------

    #[derive(Default)]
    struct FakeState {
        event_listeners: HashMap<u64, AgentSessionEventListener>,
        ui_handler: Option<UiRequestHandler>,
        next_listener_id: u64,
    }

    struct FakeRpcProcess {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeRpcProcess {
        fn new_arc() -> Arc<Self> {
            Arc::new(Self {
                state: Arc::new(Mutex::new(FakeState::default())),
            })
        }

        fn emit_event(&self, event: &Value) {
            let guard = self.state.lock().unwrap();
            for listener in guard.event_listeners.values() {
                listener(event);
            }
        }
    }

    impl RpcProcess for FakeRpcProcess {
        fn send(
            &self,
            command: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, RpcProcessError>> + Send + '_>> {
            let response = if command.get("type").and_then(Value::as_str) == Some("get_state") {
                json!({
                    "type": "response",
                    "success": true,
                    "command": "get_state",
                    "data": { "sessionId": "sess-1" },
                })
            } else {
                json!({ "type": "response", "success": true, "echo": command })
            };
            Box::pin(async move { Ok(response) })
        }

        fn handle_ui_response(
            &self,
            _response: Value,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {})
        }

        fn set_ui_request_handler(&self, handler: Option<UiRequestHandler>) {
            self.state.lock().unwrap().ui_handler = handler;
        }

        fn on_event(&self, listener: AgentSessionEventListener) -> Unsubscribe {
            let id = {
                let mut guard = self.state.lock().unwrap();
                guard.next_listener_id += 1;
                let id = guard.next_listener_id;
                guard.event_listeners.insert(id, listener);
                id
            };
            let state = self.state.clone();
            Unsubscribe::from_fn(move || {
                state.lock().unwrap().event_listeners.remove(&id);
            })
        }

        fn on_exit(&self, _listener: ExitListener) -> Unsubscribe {
            Unsubscribe::from_fn(|| {})
        }

        fn dispose(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {})
        }
    }

    struct FakeSpawner {
        rpc: Arc<FakeRpcProcess>,
    }

    impl RpcProcessSpawner for FakeSpawner {
        fn spawn(
            &self,
            _options: RpcProcessOptions,
        ) -> Result<Arc<dyn RpcProcess>, RpcProcessError> {
            Ok(self.rpc.clone())
        }
    }

    fn handler_with(rpc: Arc<FakeRpcProcess>) -> OrchestratorHandler {
        let radius = RadiusPresence::new(
            Box::new(ScriptedTransport::new()),
            Box::new(SystemRadiusClock),
        );
        let supervisor = OrchestratorSupervisor::new(
            radius,
            Arc::new(FakeSpawner { rpc }),
            Arc::new(FixedClock),
        );
        OrchestratorHandler::new(supervisor)
    }

    async fn spawn_one(handler: &OrchestratorHandler) -> String {
        let response = handler
            .handle_ipc_request(OrchestratorRequest::Spawn(SpawnRequest {
                cwd: "/work".to_string(),
                label: None,
                provider: None,
                model: None,
            }))
            .await;
        match response {
            OrchestratorResponse::Spawn(spawn) => spawn.instance.unwrap().id,
            other => panic!("expected spawn_result, got {other:?}"),
        }
    }

    // -- dispatch tests ------------------------------------------------------

    #[tokio::test]
    async fn spawn_then_list_and_status() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let id = spawn_one(&handler).await;

        let list = handler
            .handle_ipc_request(OrchestratorRequest::List(
                crate::ipc::protocol::ListRequest {},
            ))
            .await;
        match list {
            OrchestratorResponse::List(list) => {
                let instances = list.instances.unwrap();
                assert_eq!(instances.len(), 1);
                assert_eq!(instances[0].id, id);
                assert_eq!(instances[0].status, InstanceStatus::Online);
            }
            other => panic!("expected list_result, got {other:?}"),
        }

        let status = handler
            .handle_ipc_request(OrchestratorRequest::Status(StatusRequest {
                instance_id: id.clone(),
            }))
            .await;
        match status {
            OrchestratorResponse::Status(status) => {
                assert_eq!(status.instance.unwrap().id, id);
            }
            other => panic!("expected status_result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_unknown_instance_is_error_frame() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let response = handler
            .handle_ipc_request(OrchestratorRequest::Status(StatusRequest {
                instance_id: "ghost".to_string(),
            }))
            .await;
        match response {
            OrchestratorResponse::Error(error) => {
                assert!(!error.ok);
                assert_eq!(error.error, "Unknown instance: ghost");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rpc_relays_command_response() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let id = spawn_one(&handler).await;

        let response = handler
            .handle_ipc_request(OrchestratorRequest::Rpc(RpcRequest {
                instance_id: id,
                command: json!({ "type": "ping", "n": 1 }),
            }))
            .await;
        match response {
            OrchestratorResponse::Rpc(rpc) => {
                assert_eq!(rpc.response.get("success"), Some(&json!(true)));
                assert_eq!(
                    rpc.response.get("echo"),
                    Some(&json!({ "type": "ping", "n": 1 }))
                );
            }
            other => panic!("expected rpc_result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rpc_unknown_instance_is_error_frame() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let response = handler
            .handle_ipc_request(OrchestratorRequest::Rpc(RpcRequest {
                instance_id: "ghost".to_string(),
                command: json!({ "type": "ping" }),
            }))
            .await;
        assert!(matches!(response, OrchestratorResponse::Error(_)));
    }

    #[tokio::test]
    async fn stop_returns_result_then_unknown() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let id = spawn_one(&handler).await;

        let stop = handler
            .handle_ipc_request(OrchestratorRequest::Stop(StopRequest {
                instance_id: id.clone(),
            }))
            .await;
        match stop {
            OrchestratorResponse::Stop(stop) => {
                assert_eq!(stop.instance_id.as_deref(), Some(id.as_str()))
            }
            other => panic!("expected stop_result, got {other:?}"),
        }

        // Second stop: already removed -> unknown-instance error.
        let again = handler
            .handle_ipc_request(OrchestratorRequest::Stop(StopRequest { instance_id: id }))
            .await;
        assert!(matches!(again, OrchestratorResponse::Error(_)));
    }

    #[tokio::test]
    async fn rpc_stream_handshake_reports_ready() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let id = spawn_one(&handler).await;

        let ready = handler
            .handle_ipc_request(OrchestratorRequest::RpcStream(RpcStreamRequest {
                instance_id: id.clone(),
            }))
            .await;
        match ready {
            OrchestratorResponse::RpcReady(ready) => {
                assert!(ready.base.ok);
                assert_eq!(ready.instance.unwrap().id, id);
            }
            other => panic!("expected rpc_ready, got {other:?}"),
        }
    }

    // -- openRpcStream adapter -----------------------------------------------

    #[tokio::test]
    async fn open_rpc_stream_relays_response_and_events_through_sink() {
        let _env = TestEnv::new();
        let fake = FakeRpcProcess::new_arc();
        let handler = handler_with(fake.clone());
        let id = spawn_one(&handler).await;

        // Build the server-facing sink over a channel we can drain.
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let sink = RpcStreamSink::from_sender(tx);

        let mut session = handler
            .open_rpc_stream(&id, sink)
            .expect("stream opens for a live instance");

        // A client RPC command relays the child's response frame to the sink.
        session
            .handle_request(json!({ "type": "ping", "n": 7 }))
            .await
            .unwrap();
        let response = rx.recv().await.unwrap();
        assert_eq!(response.get("success"), Some(&json!(true)));
        assert_eq!(
            response.get("echo"),
            Some(&json!({ "type": "ping", "n": 7 }))
        );

        // A child session event flows to the sink too.
        fake.emit_event(&json!({ "type": "agent_event", "delta": "hi" }));
        let event = rx.recv().await.unwrap();
        assert_eq!(event, json!({ "type": "agent_event", "delta": "hi" }));

        // An extension_ui_response is routed to the child (no sink frame), so the
        // next drained frame is a fresh event, not a response.
        session
            .handle_request(json!({ "type": "extension_ui_response", "id": "u1" }))
            .await
            .unwrap();
        fake.emit_event(&json!({ "type": "agent_event", "delta": "again" }));
        let event = rx.recv().await.unwrap();
        assert_eq!(event, json!({ "type": "agent_event", "delta": "again" }));

        // After close, child events no longer reach the sink.
        session.close();
        fake.emit_event(&json!({ "type": "agent_event", "delta": "bye" }));
        assert!(rx.try_recv().is_err(), "no frame delivered after close");
    }

    #[tokio::test]
    async fn open_rpc_stream_unknown_instance_is_none() {
        let _env = TestEnv::new();
        let handler = handler_with(FakeRpcProcess::new_arc());
        let (tx, _rx) = mpsc::unbounded_channel::<Value>();
        let sink = RpcStreamSink::from_sender(tx);
        assert!(handler.open_rpc_stream("ghost", sink).is_none());
    }
}
