//! Instance lifecycle supervision, mirroring
//! `packages/orchestrator/src/supervisor.ts`.
//!
//! The [`OrchestratorSupervisor`] owns the live-instance map, spawns and stops
//! RPC child processes, persists machine/instance records through
//! [`crate::storage`], and keeps radius presence in step through
//! [`crate::radius`]. It is the runtime core that ties the RPC child process, the
//! storage layer, and radius together — pi's `OrchestratorSupervisor` class plus
//! its module-level `supervisor` singleton and `radiusPresence.setCoordinator`
//! wiring.
//!
//! # Seams and injection
//!
//! pi reaches for module-global singletons: a `supervisor` instance, a
//! `radiusPresence` instance, and `createRpcProcessInstance` imported directly.
//! This port keeps the same behaviour but injects the collaborators so the
//! lifecycle can be unit-tested without spawning a real child, binding a socket,
//! or touching the network:
//!
//! * **The RPC child** is created through the [`RpcProcessSpawner`] seam and used
//!   through the [`RpcProcess`] trait. Production wires [`RealRpcProcessSpawner`]
//!   (which builds a real [`RpcProcessInstance`]); tests inject a fake.
//! * **Radius** is an owned [`RadiusPresence`] (constructed with its own injected
//!   HTTP / credential / clock seams), not a global. The supervisor wires itself
//!   as radius's coordinator at construction, mirroring pi's module-load
//!   `radiusPresence.setCoordinator(...)`.
//! * **Time** (pi's `new Date().toISOString()`) comes from an injected
//!   [`RadiusClock`], reused as the supervisor's ISO-timestamp source.
//! * **Instance ids** use [`uuidv7`] rather than pi's `crypto.randomUUID` (v4),
//!   matching how the rest of atilla mints ids (see [`crate::rpc_process`]). Only
//!   the id format differs.
//!
//! # Streaming caveat
//!
//! Full `rpc_stream` / [`AgentSessionEvent`] streaming parity is deferred until
//! atilla-coding gains a live agent runtime that emits session events (see the
//! seam-decisions record). Until then the supervisor faithfully relays whatever
//! opaque [`serde_json::Value`] frames the RPC child produces, but no live agent
//! generates them.

// straitjacket-allow-file:duplication — the live-instance state machine, the
// per-field record updates, and the coordinator callbacks parallel pi's
// supervisor.ts closely; the repetition is a faithful mirror of pi's control
// flow, not extractable shared logic.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use atilla_agent::harness::session::uuidv7;

use crate::ipc::protocol::{
    AgentSessionEvent, RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse,
};
use crate::radius::{
    RadiusClock, RadiusError, RadiusPresence, RadiusPresenceCoordinator, StartOutcome,
};
use crate::rpc_process::{
    create_rpc_process_instance, RpcProcessError, RpcProcessInstance, RpcProcessOptions,
    Unsubscribe,
};
use crate::storage::{
    get_instance, load_instances, remove_instance, save_instances, upsert_instance,
};
use crate::types::{InstanceRecord, InstanceStatus};

/// A boxed, `Send` future — the return shape for the object-safe [`RpcProcess`]
/// trait's async methods.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A streaming session-event listener (pi's `AgentSessionEventListener`).
pub type AgentSessionEventListener = Box<dyn Fn(&AgentSessionEvent) + Send>;

/// The handler for an `extension_ui_request` frame (pi's UI-request callback).
pub type UiRequestHandler = Box<dyn Fn(&RpcExtensionUIRequest) + Send>;

/// A listener invoked once when the RPC child exits (pi's exit callback).
pub type ExitListener = Box<dyn Fn(Option<&RpcProcessError>) + Send>;

// ===========================================================================
// Errors
// ===========================================================================

/// An error raised by a supervisor lifecycle operation.
///
/// pi lets the various failures (RPC-child spawn/send, radius HTTP, JSON storage)
/// propagate as thrown `Error`s that the IPC server's `try/catch` turns into an
/// error frame. This enum is the typed union of those failure sources; its
/// [`Display`](fmt::Display) is what the handler relays as the error message
/// (pi's `error instanceof Error ? error.message : String(error)`).
#[derive(Debug)]
pub enum SupervisorError {
    /// The RPC child process failed (spawn, send, or exit).
    RpcProcess(RpcProcessError),
    /// A radius registration/disconnect failed.
    Radius(RadiusError),
    /// Reading or writing the persisted instance list failed.
    Storage(io::Error),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SupervisorError::RpcProcess(error) => write!(f, "{error}"),
            SupervisorError::Radius(error) => write!(f, "{error}"),
            SupervisorError::Storage(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<RpcProcessError> for SupervisorError {
    fn from(error: RpcProcessError) -> Self {
        SupervisorError::RpcProcess(error)
    }
}

impl From<RadiusError> for SupervisorError {
    fn from(error: RadiusError) -> Self {
        SupervisorError::Radius(error)
    }
}

impl From<io::Error> for SupervisorError {
    fn from(error: io::Error) -> Self {
        SupervisorError::Storage(error)
    }
}

// ===========================================================================
// RPC process seam
// ===========================================================================

/// The RPC child-process operations the supervisor drives, factored into a trait
/// so a fake can be injected in tests.
///
/// Mirrors the subset of [`RpcProcessInstance`]'s surface pi's supervisor uses:
/// `send`, `handleUiResponse`, `setUiRequestHandler`, `onEvent`, `onExit`, and
/// `dispose`.
pub trait RpcProcess: Send + Sync {
    /// Send a command and await the correlated response (pi's `send`).
    fn send(&self, command: RpcCommand) -> BoxFuture<'_, Result<RpcResponse, RpcProcessError>>;

    /// Write a UI response back to the child (pi's `handleUiResponse`).
    fn handle_ui_response(&self, response: RpcExtensionUIResponse) -> BoxFuture<'_, ()>;

    /// Set (or clear) the extension-UI request handler (pi's `setUiRequestHandler`).
    fn set_ui_request_handler(&self, handler: Option<UiRequestHandler>);

    /// Register an event listener, returning its unsubscribe handle (pi's `onEvent`).
    fn on_event(&self, listener: AgentSessionEventListener) -> Unsubscribe;

    /// Register an exit listener, returning its unsubscribe handle (pi's `onExit`).
    fn on_exit(&self, listener: ExitListener) -> Unsubscribe;

    /// Terminate the child and reject pending requests (pi's `dispose`).
    fn dispose(&self) -> BoxFuture<'_, ()>;
}

impl RpcProcess for RpcProcessInstance {
    fn send(&self, command: RpcCommand) -> BoxFuture<'_, Result<RpcResponse, RpcProcessError>> {
        Box::pin(RpcProcessInstance::send(self, command))
    }

    fn handle_ui_response(&self, response: RpcExtensionUIResponse) -> BoxFuture<'_, ()> {
        Box::pin(RpcProcessInstance::handle_ui_response(self, response))
    }

    fn set_ui_request_handler(&self, handler: Option<UiRequestHandler>) {
        RpcProcessInstance::set_ui_request_handler(self, handler);
    }

    fn on_event(&self, listener: AgentSessionEventListener) -> Unsubscribe {
        RpcProcessInstance::on_event(self, listener)
    }

    fn on_exit(&self, listener: ExitListener) -> Unsubscribe {
        RpcProcessInstance::on_exit(self, listener)
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(RpcProcessInstance::dispose(self))
    }
}

/// Creates [`RpcProcess`] children, factored into a trait so tests can inject a
/// fake in place of a real spawn.
///
/// Mirrors pi's direct call to `createRpcProcessInstance({ cwd })`.
pub trait RpcProcessSpawner: Send + Sync {
    /// Spawn a new RPC child for `options`, or fail as pi's constructor would.
    fn spawn(&self, options: RpcProcessOptions) -> Result<Arc<dyn RpcProcess>, RpcProcessError>;
}

/// The production [`RpcProcessSpawner`]: builds a real [`RpcProcessInstance`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRpcProcessSpawner;

impl RpcProcessSpawner for RealRpcProcessSpawner {
    fn spawn(&self, options: RpcProcessOptions) -> Result<Arc<dyn RpcProcess>, RpcProcessError> {
        Ok(Arc::new(create_rpc_process_instance(options)?))
    }
}

// ===========================================================================
// Live instance
// ===========================================================================

/// Options for [`OrchestratorSupervisor::spawn_instance`] (pi's
/// `{ cwd: string; label?: string }`).
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// Working directory for the spawned child.
    pub cwd: String,
    /// Optional human-readable label.
    pub label: Option<String>,
}

/// A live, supervised instance: its persisted record, the acquired runtime
/// resources, and the streaming subscribers/handlers wired to it.
///
/// Mirrors pi's `LiveInstance` (`record`, `resources`, `subscribers`,
/// `onUiRequest`, `unsubscribeEvents`, `unsubscribeExit`).
struct LiveInstance {
    record: InstanceRecord,
    rpc_process: Option<Arc<dyn RpcProcess>>,
    /// The acquired radius Pi id (pi's `resources.radiusPiId`), read to decide
    /// whether a disconnect is needed.
    radius_pi_id: Option<String>,
    /// The acquired session id (pi's `resources.sessionId`), tracked alongside
    /// the record.
    session_id: Option<String>,
    /// Streaming session-event subscribers, keyed by a handle so an individual
    /// `openRpcStream` caller can remove exactly its own (pi's `Set` membership
    /// by callback identity).
    subscribers: HashMap<u64, AgentSessionEventListener>,
    next_subscriber_id: u64,
    /// The current extension-UI request handler (pi's `onUiRequest`).
    on_ui_request: Option<UiRequestHandler>,
    /// Bumped on each `on_ui_request` set so a stream's `close` clears the handler
    /// only when it is still the one that stream installed (pi's
    /// `if (live.onUiRequest === onUiRequest)` identity check).
    ui_request_generation: u64,
    unsubscribe_events: Option<Unsubscribe>,
    unsubscribe_exit: Option<Unsubscribe>,
}

impl LiveInstance {
    fn new(record: InstanceRecord) -> Self {
        Self {
            record,
            rpc_process: None,
            radius_pi_id: None,
            session_id: None,
            subscribers: HashMap::new(),
            next_subscriber_id: 0,
            on_ui_request: None,
            ui_request_generation: 0,
            unsubscribe_events: None,
            unsubscribe_exit: None,
        }
    }

    /// A defensive copy of the record (pi's `cloneInstance`).
    fn clone_record(&self) -> InstanceRecord {
        self.record.clone()
    }

    /// Set the status and refresh `lastSeenAt`, then persist (pi's `setStatus`).
    fn set_status(&mut self, status: InstanceStatus, clock: &dyn RadiusClock) -> io::Result<()> {
        self.record.status = status;
        self.record.last_seen_at = Some(clock.now_iso());
        upsert_instance(self.record.clone())
    }

    /// Refresh `lastSeenAt` and persist without changing other fields (pi's
    /// `updateRecord(live, {})`).
    fn touch(&mut self, clock: &dyn RadiusClock) -> io::Result<()> {
        self.record.last_seen_at = Some(clock.now_iso());
        upsert_instance(self.record.clone())
    }

    /// Update the session id/file, sync the acquired session id, and persist
    /// (pi's `updateRecord(live, { sessionId, sessionFile })`).
    fn set_session(
        &mut self,
        session_id: Option<String>,
        session_file: Option<String>,
        clock: &dyn RadiusClock,
    ) -> io::Result<()> {
        self.record.session_id = session_id.clone();
        self.record.session_file = session_file;
        self.session_id = session_id;
        self.record.last_seen_at = Some(clock.now_iso());
        upsert_instance(self.record.clone())
    }

    /// Update the radius Pi id, sync the acquired id, and persist (pi's
    /// `updateRecord(live, { radiusPiId })`).
    fn set_radius_pi_id(
        &mut self,
        radius_pi_id: Option<String>,
        clock: &dyn RadiusClock,
    ) -> io::Result<()> {
        self.record.radius_pi_id = radius_pi_id.clone();
        self.radius_pi_id = radius_pi_id;
        self.record.last_seen_at = Some(clock.now_iso());
        upsert_instance(self.record.clone())
    }

    /// Tear down the streaming bindings (pi's `clearBindings`).
    ///
    /// Drops the event/exit unsubscribe handles, clears the UI-request handler,
    /// and clears the child's UI-request forwarder. The unsubscribe handles and
    /// the `set_ui_request_handler` call reach into the child's own lock, never
    /// the instance lock, so there is no lock-order inversion with the instance
    /// map held here.
    fn clear_bindings(&mut self) {
        if let Some(unsubscribe) = self.unsubscribe_events.take() {
            unsubscribe.unsubscribe();
        }
        if let Some(unsubscribe) = self.unsubscribe_exit.take() {
            unsubscribe.unsubscribe();
        }
        self.on_ui_request = None;
        if let Some(rpc_process) = &self.rpc_process {
            rpc_process.set_ui_request_handler(None);
        }
    }

    /// Register a streaming subscriber, returning its removal handle (pi's
    /// `subscribers.add(onEvent)`).
    fn add_subscriber(&mut self, listener: AgentSessionEventListener) -> u64 {
        self.next_subscriber_id += 1;
        let id = self.next_subscriber_id;
        self.subscribers.insert(id, listener);
        id
    }

    /// Install a UI-request handler, returning the generation stamp (pi's
    /// `live.onUiRequest = onUiRequest`).
    fn set_on_ui_request(&mut self, handler: UiRequestHandler) -> u64 {
        self.ui_request_generation += 1;
        self.on_ui_request = Some(handler);
        self.ui_request_generation
    }

    /// Clear the UI-request handler only if it is still the one stamped
    /// `generation` (pi's identity-guarded clear on stream close).
    fn clear_on_ui_request_if(&mut self, generation: u64) {
        if self.ui_request_generation == generation {
            self.on_ui_request = None;
        }
    }
}

// ===========================================================================
// Instances state + coordinator
// ===========================================================================

/// The supervisor's shared live-instance map plus the ISO clock its record
/// stamps read.
///
/// Held behind a [`Mutex`] and shared (via [`Arc`]) with the radius coordinator,
/// which reads and writes the same records (pi's `getLiveInstance` /
/// `listLiveInstances` / `updateInstance`).
struct InstancesState {
    live: HashMap<String, LiveInstance>,
    clock: Arc<dyn RadiusClock>,
}

impl InstancesState {
    fn new(clock: Arc<dyn RadiusClock>) -> Self {
        Self {
            live: HashMap::new(),
            clock,
        }
    }

    /// The live record for `instance_id`, cloned (pi's `getLiveInstance`).
    fn get_live_record(&self, instance_id: &str) -> Option<InstanceRecord> {
        self.live.get(instance_id).map(LiveInstance::clone_record)
    }

    /// Every live record, cloned (pi's `listLiveInstances`).
    fn list_live_records(&self) -> Vec<InstanceRecord> {
        self.live.values().map(LiveInstance::clone_record).collect()
    }

    /// Replace a live instance's record and persist it (pi's `updateInstance`).
    ///
    /// Also syncs the acquired radius Pi id and session id from the record, as pi
    /// does. Storage failures are logged rather than propagated: pi's coordinator
    /// `updateInstance` is fire-and-forget from radius's heartbeat loop.
    fn update_instance(&mut self, instance: InstanceRecord) {
        if let Some(live) = self.live.get_mut(&instance.id) {
            live.radius_pi_id = instance.radius_pi_id.clone();
            live.session_id = instance.session_id.clone();
            live.record = instance.clone();
        }
        if let Err(error) = upsert_instance(instance) {
            eprintln!("Failed to persist instance update: {error}");
        }
    }
}

/// The radius coordinator backed by the supervisor's live-instance map.
///
/// Mirrors the object pi passes to `radiusPresence.setCoordinator(...)`. It holds
/// only the shared instance map (never the whole supervisor), so wiring it into
/// the owned [`RadiusPresence`] introduces no reference cycle.
struct SupervisorCoordinator {
    instances: Arc<Mutex<InstancesState>>,
}

impl RadiusPresenceCoordinator for SupervisorCoordinator {
    fn get_live_instance(&self, instance_id: &str) -> Option<InstanceRecord> {
        self.instances
            .lock()
            .expect("instances poisoned")
            .get_live_record(instance_id)
    }

    fn list_live_instances(&self) -> Vec<InstanceRecord> {
        self.instances
            .lock()
            .expect("instances poisoned")
            .list_live_records()
    }

    fn update_instance(&self, instance: InstanceRecord) {
        self.instances
            .lock()
            .expect("instances poisoned")
            .update_instance(instance);
    }
}

// ===========================================================================
// Supervisor
// ===========================================================================

/// The shared inner state of an [`OrchestratorSupervisor`].
///
/// Held behind an [`Arc`] so the streaming callbacks bound to each RPC child can
/// reach back into the supervisor (via a [`Weak`] handle, avoiding a cycle).
struct SupervisorInner {
    instances: Arc<Mutex<InstancesState>>,
    radius: Mutex<RadiusPresence>,
    spawner: Arc<dyn RpcProcessSpawner>,
    clock: Arc<dyn RadiusClock>,
}

/// Supervises the lifecycle of orchestrated RPC-child instances.
///
/// Mirrors pi's `OrchestratorSupervisor` class together with its module-level
/// `supervisor` singleton and `radiusPresence.setCoordinator(...)` wiring: at
/// construction the supervisor installs itself as radius's coordinator.
#[derive(Clone)]
pub struct OrchestratorSupervisor {
    inner: Arc<SupervisorInner>,
}

impl OrchestratorSupervisor {
    /// Build a supervisor over the given radius presence, RPC spawner, and clock,
    /// wiring the radius coordinator (pi's module-load `setCoordinator`).
    pub fn new(
        mut radius: RadiusPresence,
        spawner: Arc<dyn RpcProcessSpawner>,
        clock: Arc<dyn RadiusClock>,
    ) -> Self {
        let instances = Arc::new(Mutex::new(InstancesState::new(clock.clone())));
        radius.set_coordinator(Box::new(SupervisorCoordinator {
            instances: instances.clone(),
        }));
        Self {
            inner: Arc::new(SupervisorInner {
                instances,
                radius: Mutex::new(radius),
                spawner,
                clock,
            }),
        }
    }

    fn lock_instances(&self) -> std::sync::MutexGuard<'_, InstancesState> {
        self.inner.instances.lock().expect("instances poisoned")
    }

    fn now_iso(&self) -> String {
        self.inner.clock.now_iso()
    }

    // -- coordinator-facing views (pi's getLiveInstance / listLiveInstances /
    //    updateInstance) ----------------------------------------------------

    /// The live record for `instance_id`, if present (pi's `getLiveInstance`).
    pub fn get_live_instance(&self, instance_id: &str) -> Option<InstanceRecord> {
        self.lock_instances().get_live_record(instance_id)
    }

    /// Every live record (pi's `listLiveInstances`).
    pub fn list_live_instances(&self) -> Vec<InstanceRecord> {
        self.lock_instances().list_live_records()
    }

    /// Replace a live instance's record and persist it (pi's `updateInstance`).
    pub fn update_instance(&self, instance: InstanceRecord) {
        self.lock_instances().update_instance(instance);
    }

    // -- radius presence (pi's module `radiusPresence`, owned here) ----------

    /// Whether radius presence is enabled (pi's `serve.ts` `isRadiusEnabled()`).
    ///
    /// pi's `serve.ts` reaches the module-global `radiusPresence`; this port owns
    /// it inside the supervisor, so the serve entrypoint queries it through here.
    pub fn is_radius_enabled(&self) -> bool {
        self.inner
            .radius
            .lock()
            .expect("radius poisoned")
            .is_enabled()
    }

    /// Register the machine and prime its heartbeat (pi's `radiusPresence.start()`).
    pub fn start_radius_presence(
        &self,
        label: Option<String>,
    ) -> Result<Option<StartOutcome>, SupervisorError> {
        Ok(self
            .inner
            .radius
            .lock()
            .expect("radius poisoned")
            .start(label)?)
    }

    /// Disconnect the machine and clear heartbeat state (pi's `radiusPresence.stop()`).
    pub fn stop_radius_presence(&self) -> Result<(), SupervisorError> {
        self.inner.radius.lock().expect("radius poisoned").stop()?;
        Ok(())
    }

    // -- read paths ---------------------------------------------------------

    /// The stored instance list, cloned (pi's `listInstances`).
    pub fn list_instances(&self) -> Result<Vec<InstanceRecord>, SupervisorError> {
        Ok(load_instances()?)
    }

    /// The instance for `instance_id`: the live record if supervised, else the
    /// stored one (pi's `getInstance`).
    pub fn get_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceRecord>, SupervisorError> {
        if let Some(record) = self.get_live_instance(instance_id) {
            return Ok(Some(record));
        }
        Ok(get_instance(instance_id)?)
    }

    // -- binding ------------------------------------------------------------

    /// Bind an RPC child to a live instance: wire the event/exit/UI forwarders
    /// (pi's `bindRpcProcess`).
    fn bind_rpc_process(&self, instance_id: &str, rpc_process: &Arc<dyn RpcProcess>) {
        let weak_instances = Arc::downgrade(&self.inner.instances);
        let weak_inner = Arc::downgrade(&self.inner);

        let mut guard = self.lock_instances();
        let live = match guard.live.get_mut(instance_id) {
            Some(live) => live,
            None => return,
        };
        live.clear_bindings();
        live.rpc_process = Some(rpc_process.clone());

        // onEvent: broadcast each frame to the live instance's subscribers.
        let events_instances = weak_instances.clone();
        let events_id = instance_id.to_string();
        let unsubscribe_events = rpc_process.on_event(Box::new(move |event| {
            if let Some(instances) = events_instances.upgrade() {
                let guard = instances.lock().expect("instances poisoned");
                if let Some(live) = guard.live.get(&events_id) {
                    for subscriber in live.subscribers.values() {
                        subscriber(event);
                    }
                }
            }
        }));

        // onExit: schedule the unexpected-exit cleanup off the child's callback
        // stack (pi's fire-and-forget `void handleUnexpectedRpcExit(...)`), so it
        // never re-enters the child's lock synchronously.
        let exit_id = instance_id.to_string();
        let unsubscribe_exit = rpc_process.on_exit(Box::new(move |_error| {
            if let Some(inner) = weak_inner.upgrade() {
                let supervisor = OrchestratorSupervisor { inner };
                let exit_id = exit_id.clone();
                tokio::spawn(async move {
                    supervisor.handle_unexpected_rpc_exit(&exit_id).await;
                });
            }
        }));

        // setUiRequestHandler: forward each request to the live instance's handler.
        let ui_id = instance_id.to_string();
        rpc_process.set_ui_request_handler(Some(Box::new(move |request| {
            if let Some(instances) = weak_instances.upgrade() {
                let guard = instances.lock().expect("instances poisoned");
                if let Some(live) = guard.live.get(&ui_id) {
                    if let Some(handler) = &live.on_ui_request {
                        handler(request);
                    }
                }
            }
        })));

        live.unsubscribe_events = Some(unsubscribe_events);
        live.unsubscribe_exit = Some(unsubscribe_exit);
    }

    // -- session metadata sync ---------------------------------------------

    /// Refresh the persisted session metadata from the child's `get_state` (pi's
    /// `syncInstanceRecord`).
    ///
    /// With no live child, or on a non-`get_state`-success response, only
    /// `lastSeenAt` is refreshed (pi's `updateRecord(live, {})`).
    async fn sync_instance_record(&self, instance_id: &str) -> Result<(), SupervisorError> {
        let rpc_process = {
            let guard = self.lock_instances();
            match guard.live.get(instance_id) {
                Some(live) => live.rpc_process.clone(),
                None => return Ok(()),
            }
        };
        let rpc_process = match rpc_process {
            Some(rpc_process) => rpc_process,
            None => {
                self.touch_record(instance_id)?;
                return Ok(());
            }
        };

        let response = rpc_process
            .send(serde_json::json!({ "type": "get_state" }))
            .await?;

        match parse_get_state_success(&response) {
            Some((session_id, session_file)) => {
                let mut guard = self.lock_instances();
                let clock = guard.clock.clone();
                if let Some(live) = guard.live.get_mut(instance_id) {
                    live.set_session(Some(session_id), session_file, clock.as_ref())?;
                }
                Ok(())
            }
            None => {
                self.touch_record(instance_id)?;
                Ok(())
            }
        }
    }

    /// Refresh `lastSeenAt` for `instance_id` and persist (pi's
    /// `updateRecord(live, {})`).
    fn touch_record(&self, instance_id: &str) -> Result<(), SupervisorError> {
        let mut guard = self.lock_instances();
        let clock = guard.clock.clone();
        if let Some(live) = guard.live.get_mut(instance_id) {
            live.touch(clock.as_ref())?;
        }
        Ok(())
    }

    // -- resource cleanup ---------------------------------------------------

    /// Release a live instance's acquired resources (pi's
    /// `cleanupAcquiredResources`): clear bindings, disconnect radius, and dispose
    /// the child. Radius and dispose run without the instance lock held.
    async fn cleanup_acquired_resources(&self, instance_id: &str) -> Result<(), SupervisorError> {
        // Phase 1 (instance lock): read the child handle, clear bindings, and
        // snapshot the record for a possible radius disconnect.
        let (rpc_process, disconnect_record) = {
            let mut guard = self.lock_instances();
            let live = match guard.live.get_mut(instance_id) {
                Some(live) => live,
                None => return Ok(()),
            };
            let rpc_process = live.rpc_process.clone();
            live.clear_bindings();
            let disconnect_record = if live.radius_pi_id.is_some() {
                Some(live.record.clone())
            } else {
                None
            };
            (rpc_process, disconnect_record)
        };

        // Phase 2 (radius lock): disconnect the Pi and clear the acquired id.
        if let Some(record) = disconnect_record {
            self.inner
                .radius
                .lock()
                .expect("radius poisoned")
                .disconnect_pi(&record)?;
            let mut guard = self.lock_instances();
            if let Some(live) = guard.live.get_mut(instance_id) {
                live.radius_pi_id = None;
                live.record.radius_pi_id = None;
                live.record.last_seen_at = Some(self.inner.clock.now_iso());
            }
        }

        // Clear the acquired session id and drop the child handle.
        {
            let mut guard = self.lock_instances();
            if let Some(live) = guard.live.get_mut(instance_id) {
                live.session_id = None;
                live.rpc_process = None;
            }
        }

        // Phase 3 (no lock): dispose the child.
        if let Some(rpc_process) = rpc_process {
            rpc_process.dispose().await;
        }
        Ok(())
    }

    // -- unexpected exit ----------------------------------------------------

    /// Handle an RPC child exiting without an explicit stop (pi's
    /// `handleUnexpectedRpcExit`).
    ///
    /// A stale exit for an already-stopped or already-removed instance is a
    /// no-op. Otherwise the instance transitions to `error`, its bindings are
    /// cleared, radius is disconnected, and it is dropped from the live map. This
    /// is exposed to the crate so the exit callback (and tests) can drive it.
    pub(crate) async fn handle_unexpected_rpc_exit(&self, instance_id: &str) {
        // Phase 1 (instance lock): guard, mark error, clear bindings, snapshot.
        let disconnect_record = {
            let mut guard = self.lock_instances();
            let clock = guard.clock.clone();
            let live = match guard.live.get_mut(instance_id) {
                Some(live) => live,
                // Already removed (e.g. via an explicit stop): nothing to do.
                None => return,
            };
            if matches!(
                live.record.status,
                InstanceStatus::Stopping | InstanceStatus::Stopped
            ) {
                return;
            }
            if let Err(error) = live.set_status(InstanceStatus::Error, clock.as_ref()) {
                eprintln!("Failed to persist error status for {instance_id}: {error}");
            }
            live.clear_bindings();
            let had_radius = live.radius_pi_id.is_some();
            live.rpc_process = None;
            if had_radius {
                Some(live.record.clone())
            } else {
                None
            }
        };

        // Phase 2 (radius lock): disconnect and clear the record's radius id.
        if let Some(record) = disconnect_record {
            let result = self
                .inner
                .radius
                .lock()
                .expect("radius poisoned")
                .disconnect_pi(&record);
            match result {
                Ok(()) => {
                    let mut guard = self.lock_instances();
                    let clock = guard.clock.clone();
                    if let Some(live) = guard.live.get_mut(instance_id) {
                        if let Err(error) = live.set_radius_pi_id(None, clock.as_ref()) {
                            eprintln!("Failed to persist radius clear for {instance_id}: {error}");
                        }
                    }
                }
                Err(error) => {
                    eprintln!("Failed to disconnect Radius Pi {instance_id}: {error}");
                }
            }
        }

        self.lock_instances().live.remove(instance_id);
    }

    /// Fail a spawn: mark error, clean up, then mark stopped and drop the
    /// instance (pi's `failSpawn`), returning the failure to propagate.
    async fn fail_spawn(&self, instance_id: &str, error: SupervisorError) -> SupervisorError {
        {
            let mut guard = self.lock_instances();
            let clock = guard.clock.clone();
            if let Some(live) = guard.live.get_mut(instance_id) {
                let _ = live.set_status(InstanceStatus::Error, clock.as_ref());
            }
        }
        // pi awaits cleanup inside a `try`, but always runs the `finally`
        // (stopped + delete). A cleanup failure supersedes the original error, as
        // in pi (the rejected cleanup propagates past the unreached `throw`).
        let cleanup = self.cleanup_acquired_resources(instance_id).await;
        {
            let mut guard = self.lock_instances();
            let clock = guard.clock.clone();
            if let Some(live) = guard.live.get_mut(instance_id) {
                let _ = live.set_status(InstanceStatus::Stopped, clock.as_ref());
            }
            guard.live.remove(instance_id);
        }
        match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => cleanup_error,
        }
    }

    // -- lifecycle ----------------------------------------------------------

    /// Spawn, bind, register, and bring a new instance online (pi's
    /// `spawnInstance`). On any failure the partial instance is cleaned up and
    /// dropped, and the error is returned (pi rethrows).
    pub async fn spawn_instance(
        &self,
        options: SpawnOptions,
    ) -> Result<InstanceRecord, SupervisorError> {
        let now = self.now_iso();
        let id = uuidv7();
        let record = InstanceRecord {
            id: id.clone(),
            status: InstanceStatus::Starting,
            cwd: options.cwd.clone(),
            created_at: now.clone(),
            last_seen_at: Some(now),
            label: options.label,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        };
        {
            let mut guard = self.lock_instances();
            guard
                .live
                .insert(id.clone(), LiveInstance::new(record.clone()));
        }
        upsert_instance(record)?;

        match self.spawn_instance_inner(&id, &options.cwd).await {
            Ok(record) => Ok(record),
            Err(error) => Err(self.fail_spawn(&id, error).await),
        }
    }

    /// The fallible body of [`Self::spawn_instance`], mirroring pi's `try` block.
    async fn spawn_instance_inner(
        &self,
        instance_id: &str,
        cwd: &str,
    ) -> Result<InstanceRecord, SupervisorError> {
        let rpc_process = self.inner.spawner.spawn(RpcProcessOptions {
            cwd: PathBuf::from(cwd),
        })?;
        self.bind_rpc_process(instance_id, &rpc_process);
        self.sync_instance_record(instance_id).await?;

        let record = self.get_live_instance(instance_id).ok_or_else(|| {
            SupervisorError::RpcProcess(RpcProcessError::new("instance vanished"))
        })?;
        let registration = self
            .inner
            .radius
            .lock()
            .expect("radius poisoned")
            .register_pi(record)?;

        {
            let mut guard = self.lock_instances();
            let clock = guard.clock.clone();
            if let Some(live) = guard.live.get_mut(instance_id) {
                live.set_radius_pi_id(registration.instance.radius_pi_id, clock.as_ref())?;
                live.set_status(InstanceStatus::Online, clock.as_ref())?;
            }
        }

        self.get_live_instance(instance_id)
            .ok_or_else(|| SupervisorError::RpcProcess(RpcProcessError::new("instance vanished")))
    }

    /// Stop and remove an instance (pi's `stopInstance`), or `None` if it is not
    /// live.
    pub async fn stop_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceRecord>, SupervisorError> {
        {
            let guard = self.lock_instances();
            if !guard.live.contains_key(instance_id) {
                return Ok(None);
            }
        }

        {
            let mut guard = self.lock_instances();
            let clock = guard.clock.clone();
            if let Some(live) = guard.live.get_mut(instance_id) {
                live.set_status(InstanceStatus::Stopping, clock.as_ref())?;
            }
        }

        // pi awaits cleanup inside a `try`, but always runs the `finally`
        // (stopped record + delete from map + removeInstance). A cleanup failure
        // then propagates out.
        let cleanup = self.cleanup_acquired_resources(instance_id).await;

        let record = {
            let mut guard = self.lock_instances();
            let now = guard.clock.now_iso();
            let record = guard.live.get_mut(instance_id).map(|live| {
                live.record.status = InstanceStatus::Stopped;
                live.record.last_seen_at = Some(now);
                live.record.clone()
            });
            guard.live.remove(instance_id);
            record
        };
        remove_instance(instance_id)?;
        cleanup?;
        Ok(record)
    }

    /// Relay one RPC command to a live instance, refreshing session metadata for
    /// the commands that can change it (pi's `handleRpc`). `None` if the instance
    /// is not live.
    pub async fn handle_rpc(
        &self,
        instance_id: &str,
        command: RpcCommand,
    ) -> Result<Option<RpcResponse>, SupervisorError> {
        let rpc_process = {
            let guard = self.lock_instances();
            match guard.live.get(instance_id) {
                Some(live) => live.rpc_process.clone(),
                None => return Ok(None),
            }
        };
        let rpc_process = match rpc_process {
            Some(rpc_process) => rpc_process,
            None => return Ok(None),
        };

        let response = rpc_process.send(command.clone()).await?;
        if should_refresh_session_metadata(&command) {
            self.sync_instance_record(instance_id).await?;
        }
        Ok(Some(response))
    }

    /// Open a bidirectional RPC stream to a live instance (pi's supervisor
    /// `openRpcStream`). `None` if the instance has no live child.
    ///
    /// The `on_event` subscriber and `on_ui_request` handler are registered on the
    /// live instance; the returned [`SupervisorRpcStream`] relays commands to the
    /// child and, on `close`, removes exactly those registrations.
    pub fn open_rpc_stream(
        &self,
        instance_id: &str,
        on_event: AgentSessionEventListener,
        on_ui_request: UiRequestHandler,
    ) -> Option<SupervisorRpcStream> {
        let mut guard = self.lock_instances();
        let live = guard.live.get_mut(instance_id)?;
        let rpc_process = live.rpc_process.clone()?;
        let subscriber_id = live.add_subscriber(on_event);
        let ui_generation = live.set_on_ui_request(on_ui_request);
        drop(guard);
        Some(SupervisorRpcStream {
            supervisor: self.clone(),
            instance_id: instance_id.to_string(),
            rpc_process,
            subscriber_id,
            ui_generation,
        })
    }

    /// After a restart, mark forgotten `online`/`starting` instances `stopped`,
    /// disconnect their radius presence, and re-persist (pi's
    /// `recoverAfterRestart`).
    pub async fn recover_after_restart(&self) -> Result<(), SupervisorError> {
        let recovered_at = self.now_iso();
        let instances: Vec<InstanceRecord> = load_instances()?
            .into_iter()
            .map(|mut instance| {
                if matches!(
                    instance.status,
                    InstanceStatus::Online | InstanceStatus::Starting
                ) {
                    instance.status = InstanceStatus::Stopped;
                }
                instance.last_seen_at = Some(recovered_at.clone());
                instance
            })
            .collect();
        for instance in &instances {
            self.inner
                .radius
                .lock()
                .expect("radius poisoned")
                .disconnect_pi(instance)?;
        }
        save_instances(&instances)?;
        Ok(())
    }

    /// Stop every live instance (pi's `shutdown`).
    pub async fn shutdown(&self) -> Result<(), SupervisorError> {
        let ids: Vec<String> = {
            let guard = self.lock_instances();
            guard.live.keys().cloned().collect()
        };
        for id in ids {
            self.stop_instance(&id).await?;
        }
        Ok(())
    }
}

/// A live RPC stream handle (pi's supervisor `openRpcStream` return object:
/// `{ handleRpc, handleUiResponse, close }`).
pub struct SupervisorRpcStream {
    supervisor: OrchestratorSupervisor,
    instance_id: String,
    rpc_process: Arc<dyn RpcProcess>,
    subscriber_id: u64,
    ui_generation: u64,
}

impl SupervisorRpcStream {
    /// Relay a command to the child and refresh session metadata for the commands
    /// that can change it (pi's stream `handleRpc`).
    pub async fn handle_rpc(&self, command: RpcCommand) -> Result<RpcResponse, SupervisorError> {
        let response = self.rpc_process.send(command.clone()).await?;
        if should_refresh_session_metadata(&command) {
            self.supervisor
                .sync_instance_record(&self.instance_id)
                .await?;
        }
        Ok(response)
    }

    /// Write a UI response back to the child (pi's stream `handleUiResponse`).
    pub async fn handle_ui_response(&self, response: RpcExtensionUIResponse) {
        self.rpc_process.handle_ui_response(response).await;
    }

    /// Remove this stream's subscriber and UI handler (pi's stream `close`).
    pub fn close(&self) {
        let mut guard = self.supervisor.lock_instances();
        if let Some(live) = guard.live.get_mut(&self.instance_id) {
            live.clear_on_ui_request_if(self.ui_generation);
            live.subscribers.remove(&self.subscriber_id);
        }
    }
}

// ===========================================================================
// Session-metadata policy + get_state parsing
// ===========================================================================

/// The RPC commands after which the persisted session metadata may need a
/// refresh (pi's `SESSION_METADATA_COMMANDS`).
const SESSION_METADATA_COMMANDS: [&str; 6] = [
    "new_session",
    "switch_session",
    "fork",
    "clone",
    "set_session_name",
    "prompt",
];

/// Whether `command` is one after which session metadata should be re-synced
/// (pi's `shouldRefreshSessionMetadata`).
fn should_refresh_session_metadata(command: &RpcCommand) -> bool {
    command
        .get("type")
        .and_then(|value| value.as_str())
        .map(|command_type| SESSION_METADATA_COMMANDS.contains(&command_type))
        .unwrap_or(false)
}

/// Extract `(sessionId, sessionFile)` from a successful `get_state` response, or
/// `None` otherwise (pi's `isGetStateSuccess`).
///
/// pi requires `success === true`, `command === "get_state"`, and a `data` object
/// carrying a string `sessionId` (with an optional string `sessionFile`).
fn parse_get_state_success(response: &RpcResponse) -> Option<(String, Option<String>)> {
    if response.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    if response.get("command").and_then(serde_json::Value::as_str) != Some("get_state") {
        return None;
    }
    let data = response.get("data")?;
    let session_id = data.get("sessionId").and_then(serde_json::Value::as_str)?;
    let session_file = data
        .get("sessionFile")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Some((session_id.to_string(), session_file))
}

#[cfg(test)]
mod tests;
