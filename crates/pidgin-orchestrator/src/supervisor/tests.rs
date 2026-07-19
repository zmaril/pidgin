use super::*;
use crate::radius::SystemRadiusClock;
use pidgin_ai::seams::http::ScriptedTransport;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard};

// -- test environment ----------------------------------------------------

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
        // reads (pi's `readStoredCredential`) find no `auth.json` and radius stays
        // deterministically disabled, independent of the real `~/.pi`.
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

/// A deterministic clock (pi's stubbed `Date`).
#[derive(Clone, Copy)]
struct FixedClock;
impl RadiusClock for FixedClock {
    fn now_iso(&self) -> String {
        "2026-01-01T00:00:00.000Z".to_string()
    }
}

// -- fake RPC child ------------------------------------------------------

#[derive(Default)]
struct FakeState {
    sent: Vec<RpcCommand>,
    ui_responses: Vec<RpcExtensionUIResponse>,
    event_listeners: HashMap<u64, AgentSessionEventListener>,
    exit_listeners: HashMap<u64, ExitListener>,
    ui_handler: Option<UiRequestHandler>,
    next_listener_id: u64,
    disposed: bool,
}

/// A fake [`RpcProcess`] with a scripted `get_state` response; every other
/// command echoes back a canonical success frame. Listeners live behind a
/// shared [`Arc`] so a test can emit events/exit deterministically and so the
/// unsubscribe handles can reach the same state.
struct FakeRpcProcess {
    state: Arc<Mutex<FakeState>>,
    get_state: Value,
    default_response: Value,
}

impl FakeRpcProcess {
    fn new_arc() -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(FakeState::default())),
            get_state: json!({
                "type": "response",
                "success": true,
                "command": "get_state",
                "data": { "sessionId": "sess-1", "sessionFile": "/s/sess-1.jsonl" },
            }),
            default_response: json!({ "type": "response", "success": true }),
        })
    }

    fn sent_commands(&self) -> Vec<RpcCommand> {
        self.state.lock().unwrap().sent.clone()
    }

    fn ui_responses(&self) -> Vec<RpcExtensionUIResponse> {
        self.state.lock().unwrap().ui_responses.clone()
    }

    fn was_disposed(&self) -> bool {
        self.state.lock().unwrap().disposed
    }

    /// Deliver an event to every registered listener (the child's stdout
    /// event path).
    fn emit_event(&self, event: &AgentSessionEvent) {
        let guard = self.state.lock().unwrap();
        for listener in guard.event_listeners.values() {
            listener(event);
        }
    }

    /// Deliver a UI request through the installed handler (the child's
    /// `extension_ui_request` path).
    fn emit_ui_request(&self, request: &RpcExtensionUIRequest) {
        let guard = self.state.lock().unwrap();
        if let Some(handler) = &guard.ui_handler {
            handler(request);
        }
    }
}

impl RpcProcess for FakeRpcProcess {
    fn send(&self, command: RpcCommand) -> BoxFuture<'_, Result<RpcResponse, RpcProcessError>> {
        let mut guard = self.state.lock().unwrap();
        guard.sent.push(command.clone());
        let response = if command.get("type").and_then(Value::as_str) == Some("get_state") {
            self.get_state.clone()
        } else {
            self.default_response.clone()
        };
        Box::pin(async move { Ok(response) })
    }

    fn handle_ui_response(&self, response: RpcExtensionUIResponse) -> BoxFuture<'_, ()> {
        self.state.lock().unwrap().ui_responses.push(response);
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

    fn on_exit(&self, listener: ExitListener) -> Unsubscribe {
        let id = {
            let mut guard = self.state.lock().unwrap();
            guard.next_listener_id += 1;
            let id = guard.next_listener_id;
            guard.exit_listeners.insert(id, listener);
            id
        };
        let state = self.state.clone();
        Unsubscribe::from_fn(move || {
            state.lock().unwrap().exit_listeners.remove(&id);
        })
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        self.state.lock().unwrap().disposed = true;
        Box::pin(async {})
    }
}

/// A spawner that always yields the same shared fake, so a test can inspect it.
struct FakeSpawner {
    rpc: Arc<FakeRpcProcess>,
}

impl RpcProcessSpawner for FakeSpawner {
    fn spawn(&self, _options: RpcProcessOptions) -> Result<Arc<dyn RpcProcess>, RpcProcessError> {
        Ok(self.rpc.clone())
    }
}

/// A spawner that fails, exercising the spawn-failure cleanup path.
struct FailingSpawner;

impl RpcProcessSpawner for FailingSpawner {
    fn spawn(&self, _options: RpcProcessOptions) -> Result<Arc<dyn RpcProcess>, RpcProcessError> {
        Err(RpcProcessError::new("spawn boom"))
    }
}

// -- supervisor construction --------------------------------------------

fn disabled_radius() -> RadiusPresence {
    RadiusPresence::new(
        Box::new(ScriptedTransport::new()),
        Box::new(SystemRadiusClock),
    )
}

fn supervisor_with(rpc: Arc<FakeRpcProcess>) -> OrchestratorSupervisor {
    OrchestratorSupervisor::new(
        disabled_radius(),
        Arc::new(FakeSpawner { rpc }),
        Arc::new(FixedClock),
    )
}

/// Spawn an unlabeled instance in `cwd`, unwrapping the result.
async fn spawn(supervisor: &OrchestratorSupervisor, cwd: &str) -> InstanceRecord {
    supervisor
        .spawn_instance(SpawnOptions {
            cwd: cwd.to_string(),
            label: None,
        })
        .await
        .unwrap()
}

// -- tests ---------------------------------------------------------------

#[tokio::test]
async fn spawn_instance_registers_and_brings_online() {
    let _env = TestEnv::new();
    let fake = FakeRpcProcess::new_arc();
    let supervisor = supervisor_with(fake.clone());

    let record = supervisor
        .spawn_instance(SpawnOptions {
            cwd: "/work".to_string(),
            label: Some("primary".to_string()),
        })
        .await
        .unwrap();

    assert_eq!(record.status, InstanceStatus::Online);
    assert_eq!(record.cwd, "/work");
    assert_eq!(record.label.as_deref(), Some("primary"));
    // syncInstanceRecord pulled session metadata from get_state.
    assert_eq!(record.session_id.as_deref(), Some("sess-1"));
    assert_eq!(record.session_file.as_deref(), Some("/s/sess-1.jsonl"));

    // Persisted and live.
    let stored = supervisor.get_instance(&record.id).unwrap().unwrap();
    assert_eq!(stored.status, InstanceStatus::Online);
    assert_eq!(supervisor.list_live_instances().len(), 1);

    // The child received a get_state during sync.
    assert!(fake
        .sent_commands()
        .iter()
        .any(|command| command.get("type") == Some(&json!("get_state"))));
}

#[tokio::test]
async fn spawn_failure_cleans_up_and_drops_instance() {
    let _env = TestEnv::new();
    let supervisor = OrchestratorSupervisor::new(
        disabled_radius(),
        Arc::new(FailingSpawner),
        Arc::new(FixedClock),
    );

    let error = supervisor
        .spawn_instance(SpawnOptions {
            cwd: "/work".to_string(),
            label: None,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("spawn boom"));

    // No live instance remains, and the persisted record was marked stopped.
    assert!(supervisor.list_live_instances().is_empty());
    let stored = supervisor.list_instances().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, InstanceStatus::Stopped);
}

#[tokio::test]
async fn list_instances_reads_persisted_records() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    spawn(&supervisor, "/a").await;
    let listed = supervisor.list_instances().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].cwd, "/a");
}

#[tokio::test]
async fn status_falls_back_to_stored_when_not_live() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    let record = spawn(&supervisor, "/a").await;
    supervisor.stop_instance(&record.id).await.unwrap();

    // Not live anymore; get_instance must still return the (removed) record
    // as None since stop removes it from storage.
    assert!(supervisor.get_live_instance(&record.id).is_none());
    assert!(supervisor.get_instance(&record.id).unwrap().is_none());
}

#[tokio::test]
async fn stop_instance_removes_and_disposes() {
    let _env = TestEnv::new();
    let fake = FakeRpcProcess::new_arc();
    let supervisor = supervisor_with(fake.clone());
    let record = spawn(&supervisor, "/a").await;

    let stopped = supervisor.stop_instance(&record.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, InstanceStatus::Stopped);
    assert!(supervisor.list_live_instances().is_empty());
    assert!(fake.was_disposed(), "child should be disposed on stop");
    assert!(supervisor.list_instances().unwrap().is_empty());
}

#[tokio::test]
async fn stop_unknown_instance_is_none() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    assert!(supervisor.stop_instance("ghost").await.unwrap().is_none());
}

#[tokio::test]
async fn handle_rpc_relays_and_refreshes_on_session_command() {
    let _env = TestEnv::new();
    let fake = FakeRpcProcess::new_arc();
    let supervisor = supervisor_with(fake.clone());
    let record = spawn(&supervisor, "/a").await;
    let before = fake.sent_commands().len();

    // A non-session command relays without a follow-up get_state.
    let response = supervisor
        .handle_rpc(&record.id, json!({ "type": "ping" }))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.get("success"), Some(&json!(true)));
    let after_ping = fake.sent_commands();
    assert_eq!(after_ping.len(), before + 1, "only the ping was sent");

    // A session-mutating command triggers a get_state refresh.
    supervisor
        .handle_rpc(&record.id, json!({ "type": "prompt", "text": "hi" }))
        .await
        .unwrap()
        .unwrap();
    let after_prompt = fake.sent_commands();
    assert_eq!(after_prompt.len(), before + 3, "prompt + get_state sent");
    assert_eq!(
        after_prompt.last().unwrap().get("type"),
        Some(&json!("get_state"))
    );
}

#[tokio::test]
async fn handle_rpc_unknown_instance_is_none() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    assert!(supervisor
        .handle_rpc("ghost", json!({ "type": "ping" }))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn open_rpc_stream_relays_events_and_ui_and_closes() {
    let _env = TestEnv::new();
    let fake = FakeRpcProcess::new_arc();
    let supervisor = supervisor_with(fake.clone());
    let record = spawn(&supervisor, "/a").await;

    let events = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let ui = Arc::new(AtomicUsize::new(0));
    let events_sink = events.clone();
    let ui_sink = ui.clone();
    let stream = supervisor
        .open_rpc_stream(
            &record.id,
            Box::new(move |event| events_sink.lock().unwrap().push(event.clone())),
            Box::new(move |_request| {
                ui_sink.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("stream opens for a live instance");

    // A child event reaches the subscriber; a UI request reaches the handler.
    fake.emit_event(&json!({ "type": "agent_event", "delta": "hi" }));
    fake.emit_ui_request(&json!({ "type": "extension_ui_request", "id": "u1" }));
    assert_eq!(events.lock().unwrap().len(), 1);
    assert_eq!(
        events.lock().unwrap()[0],
        json!({ "type": "agent_event", "delta": "hi" })
    );
    assert_eq!(ui.load(Ordering::SeqCst), 1);

    // handle_rpc relays a command to the child.
    let response = stream.handle_rpc(json!({ "type": "ping" })).await.unwrap();
    assert_eq!(response.get("success"), Some(&json!(true)));

    // handle_ui_response forwards to the child.
    stream
        .handle_ui_response(json!({ "type": "extension_ui_response", "id": "u1" }))
        .await;
    assert_eq!(fake.ui_responses().len(), 1);

    // After close, events no longer reach the (removed) subscriber.
    stream.close();
    fake.emit_event(&json!({ "type": "agent_event", "delta": "bye" }));
    assert_eq!(events.lock().unwrap().len(), 1, "no delivery after close");
}

#[tokio::test]
async fn open_rpc_stream_unknown_instance_is_none() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    assert!(supervisor
        .open_rpc_stream("ghost", Box::new(|_| {}), Box::new(|_| {}))
        .is_none());
}

#[tokio::test]
async fn unexpected_exit_marks_error_then_removes() {
    let _env = TestEnv::new();
    let fake = FakeRpcProcess::new_arc();
    let supervisor = supervisor_with(fake.clone());
    let record = spawn(&supervisor, "/a").await;

    supervisor.handle_unexpected_rpc_exit(&record.id).await;
    assert!(supervisor.list_live_instances().is_empty());

    // A stale exit for an already-removed instance is a no-op.
    supervisor.handle_unexpected_rpc_exit(&record.id).await;
}

#[tokio::test]
async fn recover_after_restart_stops_online_instances() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    // Persist an "online" and an "error" record directly.
    save_instances(&[
        InstanceRecord {
            id: "a".to_string(),
            status: InstanceStatus::Online,
            cwd: "/a".to_string(),
            created_at: "t".to_string(),
            last_seen_at: None,
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        },
        InstanceRecord {
            id: "b".to_string(),
            status: InstanceStatus::Error,
            cwd: "/b".to_string(),
            created_at: "t".to_string(),
            last_seen_at: None,
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        },
    ])
    .unwrap();

    supervisor.recover_after_restart().await.unwrap();
    let recovered = supervisor.list_instances().unwrap();
    let a = recovered.iter().find(|r| r.id == "a").unwrap();
    let b = recovered.iter().find(|r| r.id == "b").unwrap();
    assert_eq!(a.status, InstanceStatus::Stopped, "online -> stopped");
    assert_eq!(b.status, InstanceStatus::Error, "error left as-is");
    assert!(a.last_seen_at.is_some());
}

#[tokio::test]
async fn shutdown_stops_every_live_instance() {
    let _env = TestEnv::new();
    let supervisor = supervisor_with(FakeRpcProcess::new_arc());
    spawn(&supervisor, "/a").await;
    supervisor.shutdown().await.unwrap();
    assert!(supervisor.list_live_instances().is_empty());
}

// -- pure-helper tests ---------------------------------------------------

#[test]
fn session_metadata_policy_matches_pi() {
    for command_type in SESSION_METADATA_COMMANDS {
        assert!(should_refresh_session_metadata(
            &json!({ "type": command_type })
        ));
    }
    assert!(!should_refresh_session_metadata(&json!({ "type": "ping" })));
    assert!(!should_refresh_session_metadata(&json!({ "no": "type" })));
}

#[test]
fn get_state_success_parsing() {
    assert_eq!(
        parse_get_state_success(&json!({
            "success": true,
            "command": "get_state",
            "data": { "sessionId": "s1", "sessionFile": "/f" },
        })),
        Some(("s1".to_string(), Some("/f".to_string())))
    );
    // Missing sessionFile is allowed.
    assert_eq!(
        parse_get_state_success(&json!({
            "success": true,
            "command": "get_state",
            "data": { "sessionId": "s1" },
        })),
        Some(("s1".to_string(), None))
    );
    // Wrong command, not-success, or missing data => None.
    assert_eq!(
        parse_get_state_success(
            &json!({ "success": false, "command": "get_state", "data": { "sessionId": "s1" } })
        ),
        None
    );
    assert_eq!(
        parse_get_state_success(&json!({ "success": true, "command": "other", "data": {} })),
        None
    );
    assert_eq!(
        parse_get_state_success(&json!({ "success": true, "command": "get_state" })),
        None
    );
}
