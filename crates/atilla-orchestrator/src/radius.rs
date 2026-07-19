//! Radius presence registration and heartbeat, mirroring
//! `packages/orchestrator/src/radius.ts`.
//!
//! pi's radius module keeps the machine and each running Pi registered with the
//! radius coordinator (`radius.pi.dev`) via periodic HTTP heartbeats. On a
//! transient failure it backs off exponentially with jitter; after three
//! consecutive `404`s (a stale registration the coordinator has forgotten) it
//! re-registers the machine and every live Pi.
//!
//! # Runtime seams
//!
//! pi calls the ambient `fetch`, `os.hostname()`, `Date.now()`, and
//! `setTimeout`. This port keeps I/O out of the core, following the same pattern
//! as atilla-ai's OAuth flows:
//!
//! - **HTTP** goes through atilla-ai's injected [`HttpTransport`] seam (the same
//!   transport pi's `fetch` stubs map onto), not a bundled HTTP client.
//! - **Credentials** are read through atilla-ai's [`CredentialStore`] seam; the
//!   production wiring is [`crate::credential_store::FileCredentialStore`] over
//!   `~/.pi/agent/auth.json` (pi's `readStoredCredential`).
//! - **Time** (the `createdAt`/`lastSeenAt` ISO stamps) comes from an injected
//!   [`RadiusClock`].
//! - **The backoff / 404 state machine is pure** ([`HeartbeatBackoff`] +
//!   [`compute_backoff_delay_ms`]): given the current counters and a heartbeat
//!   outcome it returns the next delay or a "re-register" decision, touching
//!   neither the network nor the clock, so tests drive it deterministically.
//! - **Timer scheduling** (pi's `setTimeout` loop) is expressed as a returned
//!   [`HeartbeatStep`] delay: each single-step method performs one heartbeat and
//!   reports when the next should fire. Driving those steps on a real timer is
//!   left to the serve/supervisor runtime wiring (a later port stage), exactly as
//!   the OAuth port leaves the loop to its driver.
//!
//! pi's module-level `radiusPresence` singleton is intentionally *not* mirrored
//! as a global: [`RadiusPresence`] is constructed with its seams and owned by the
//! supervisor. pi's `index.ts` barrel does not re-export radius, so neither does
//! this crate's `lib.rs`.

// straitjacket-allow-file[:duplication] — the `format_iso_millis` epoch-to-ISO
// helper and the `RadiusEnvGuard` test scaffold faithfully mirror the parallel
// implementations in `atilla-coding`'s `session_manager.rs` and `config.rs`.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use atilla_ai::auth::{Credential, CredentialStore};
use atilla_ai::seams::http::{HttpRequest, HttpResponse, HttpTransport};

use crate::config::{get_orchestrator_dir, get_socket_path, VERSION};
use crate::storage::{load_machine, save_machine};
use crate::types::{InstanceRecord, MachineRecord};

/// Default radius base URL (`DEFAULT_RADIUS_URL`).
const DEFAULT_RADIUS_URL: &str = "https://radius.pi.dev/";
/// Default orchestrator API base path (`DEFAULT_ORCHESTRATOR_BASE_PATH`).
const DEFAULT_ORCHESTRATOR_BASE_PATH: &str = "/v1/";
/// Credential-store provider id for radius (`RADIUS_PROVIDER`).
const RADIUS_PROVIDER: &str = "radius";

mod backoff;

pub use backoff::{
    compute_backoff_delay_ms, HeartbeatBackoff, HeartbeatDecision, HeartbeatOutcome,
};

// ===========================================================================
// Errors
// ===========================================================================

/// A radius operation failure. `Http { status: 404, .. }` is the stale-
/// registration signal the heartbeat loop keys off (pi's `RadiusHttpError` with
/// `status === 404`).
#[derive(Debug)]
pub enum RadiusError {
    /// The request completed with a non-2xx status (pi's `RadiusHttpError`).
    Http {
        /// The HTTP status code.
        status: u16,
        /// The response body text.
        body: String,
    },
    /// The transport itself failed (pi's rejected `fetch`).
    Transport(io::Error),
    /// A response body could not be decoded as the expected JSON.
    Decode(serde_json::Error),
    /// No radius credential is configured (pi's thrown "Radius credentials are
    /// required ...").
    MissingCredentials,
    /// A Pi registration was requested with no machine available (pi's "No
    /// registered machine available for Pi registration").
    NoMachine,
    /// Local machine-record persistence failed.
    Storage(io::Error),
}

impl RadiusError {
    /// Whether this is a `404`, i.e. a stale registration (pi's
    /// `isNotFoundError`).
    pub fn is_not_found(&self) -> bool {
        matches!(self, RadiusError::Http { status: 404, .. })
    }
}

impl fmt::Display for RadiusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // pi's RadiusHttpError message shape.
            RadiusError::Http { status, body } => {
                write!(f, "Radius request failed: {status} {body}")
            }
            RadiusError::Transport(error) => write!(f, "{error}"),
            RadiusError::Decode(error) => write!(f, "{error}"),
            RadiusError::MissingCredentials => write!(
                f,
                "Radius credentials are required in ~/.pi/agent/auth.json or RADIUS_API_KEY"
            ),
            RadiusError::NoMachine => {
                write!(f, "No registered machine available for Pi registration")
            }
            RadiusError::Storage(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RadiusError {}

/// Format an error for a retry log line, mirroring pi's `formatRadiusError`
/// (`HTTP <status>: <message>` for HTTP errors).
fn format_radius_error(error: &RadiusError) -> String {
    match error {
        RadiusError::Http { status, .. } => format!("HTTP {status}: {error}"),
        other => other.to_string(),
    }
}

/// Log a retry, mirroring pi's `logRadiusRetry` `console.error` line.
fn log_radius_retry(
    scope: &str,
    action: &str,
    delay_ms: i64,
    failure_count: u32,
    error: &RadiusError,
) {
    eprintln!(
        "{scope} {action} failed (attempt {failure_count}); retrying in {delay_ms}ms: {}",
        format_radius_error(error)
    );
}

// ===========================================================================
// URL + env helpers (exported functions)
// ===========================================================================

/// Read an environment variable, treating unset and empty as absent (pi's
/// `process.env[X]` truthiness).
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// The radius base URL (`getRadiusUrl`): `PI_RADIUS_URL` or the default.
pub fn get_radius_url() -> String {
    non_empty_env("PI_RADIUS_URL").unwrap_or_else(|| DEFAULT_RADIUS_URL.to_string())
}

/// The orchestrator API base URL (`getRadiusOrchestratorBaseUrl`):
/// `PI_RADIUS_ORCHESTRATOR_URL` if set, else `/v1/` resolved against the radius
/// URL.
pub fn get_radius_orchestrator_base_url() -> String {
    if let Some(explicit) = non_empty_env("PI_RADIUS_ORCHESTRATOR_URL") {
        return explicit;
    }
    resolve_url(&get_radius_url(), DEFAULT_ORCHESTRATOR_BASE_PATH)
}

/// Resolve `reference` against `base`, covering the WHATWG `new URL(reference,
/// base)` cases radius uses: an absolute-path reference replaces the base path
/// (keeping scheme + authority); a relative reference resolves against the base
/// directory (radius base URLs always end in `/`); an absolute-URL reference is
/// returned as-is.
fn resolve_url(base: &str, reference: &str) -> String {
    if reference.contains("://") {
        return reference.to_string();
    }
    if let Some(rest) = reference.strip_prefix('/') {
        // Absolute path: keep scheme://authority, replace the path.
        let origin = origin_of(base);
        return format!("{origin}/{rest}");
    }
    // Relative reference: resolve against the base's directory.
    let base_no_query = base.split(['?', '#']).next().unwrap_or(base);
    let dir_end = base_no_query.rfind('/').map(|i| i + 1).unwrap_or(0);
    format!("{}{reference}", &base_no_query[..dir_end])
}

/// The `scheme://authority` prefix of a URL (everything up to the path's leading
/// `/`), used by [`resolve_url`] for absolute-path references.
fn origin_of(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority_end = rest.find('/').unwrap_or(rest.len());
            format!("{scheme}://{}", &rest[..authority_end])
        }
        None => url.trim_end_matches('/').to_string(),
    }
}

// ===========================================================================
// Credential resolution (exported functions)
// ===========================================================================

/// The stored radius OAuth credential, if the store holds an OAuth entry for the
/// radius provider (pi's `getStoredRadiusCredential`). Any read failure is
/// treated as "not configured", matching pi's swallow-and-`undefined`.
fn stored_radius_credential(
    store: &dyn CredentialStore,
) -> Option<atilla_ai::auth::OAuthCredential> {
    match store.read(RADIUS_PROVIDER) {
        Ok(Some(Credential::OAuth(oauth))) => Some(oauth),
        _ => None,
    }
}

/// The radius access token (`getRadiusAccessToken`): the stored OAuth access
/// token if present and non-empty, else `RADIUS_API_KEY`, else an error.
pub fn get_radius_access_token(store: &dyn CredentialStore) -> Result<String, RadiusError> {
    if let Some(oauth) = stored_radius_credential(store) {
        if !oauth.access.is_empty() {
            return Ok(oauth.access);
        }
    }
    if let Some(api_key) = non_empty_env("RADIUS_API_KEY") {
        return Ok(api_key);
    }
    Err(RadiusError::MissingCredentials)
}

/// Whether radius presence is enabled (`isRadiusEnabled`): a stored access token
/// or `RADIUS_API_KEY` is present.
pub fn is_radius_enabled(store: &dyn CredentialStore) -> bool {
    stored_radius_credential(store)
        .map(|oauth| !oauth.access.is_empty())
        .unwrap_or(false)
        || non_empty_env("RADIUS_API_KEY").is_some()
}

// ===========================================================================
// Host facts (hostname / platform / arch)
// ===========================================================================

/// The system hostname (`os.hostname()`), or `"localhost"` if unavailable.
#[cfg(unix)]
fn hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(not(unix))]
fn hostname() -> String {
    "localhost".to_string()
}

/// Map a Rust `std::env::consts::OS` value to Node's `os.platform()` string.
fn map_node_platform(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Map a Rust `std::env::consts::ARCH` value to Node's `process.arch` string.
fn map_node_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        other => other,
    }
}

/// Node's `os.platform()` for the current build target.
fn node_platform() -> &'static str {
    map_node_platform(std::env::consts::OS)
}

/// Node's `process.arch` for the current build target.
fn node_arch() -> &'static str {
    map_node_arch(std::env::consts::ARCH)
}

// ===========================================================================
// Clock seam
// ===========================================================================

/// A source of the ISO-8601 wall-clock stamps radius writes into the machine
/// record (pi's `new Date().toISOString()`). Injected so tests are deterministic.
pub trait RadiusClock: Send + Sync {
    /// The current time as an ISO-8601 string (`YYYY-MM-DDTHH:MM:SS.sssZ`).
    fn now_iso(&self) -> String;
}

/// The production [`RadiusClock`], reading `SystemTime::now()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRadiusClock;

impl RadiusClock for SystemRadiusClock {
    fn now_iso(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        format_iso_millis(millis)
    }
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ` (pi's
/// `Date.toISOString()`), by settling into 400-year blocks and walking the
/// remaining years and months.
fn format_iso_millis(millis: i64) -> String {
    let total_seconds = millis.div_euclid(1000);
    let sub_ms = millis.rem_euclid(1000);
    let mut day_count = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (hours, minutes, seconds) = (
        seconds_of_day / 3600,
        seconds_of_day % 3600 / 60,
        seconds_of_day % 60,
    );

    let mut year = 1970 + 400 * day_count.div_euclid(146_097);
    day_count = day_count.rem_euclid(146_097);
    while day_count >= year_length(year) {
        day_count -= year_length(year);
        year += 1;
    }

    let months = month_lengths(year);
    let mut month_index = 0;
    while day_count >= months[month_index] {
        day_count -= months[month_index];
        month_index += 1;
    }

    format!(
        "{year:04}-{:02}-{:02}T{hours:02}:{minutes:02}:{seconds:02}.{sub_ms:03}Z",
        month_index + 1,
        day_count + 1,
    )
}

/// Whether `year` is a Gregorian leap year.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `year`.
fn year_length(year: i64) -> i64 {
    if is_leap_year(year) {
        366
    } else {
        365
    }
}

/// The per-month day counts for `year`.
fn month_lengths(year: i64) -> [i64; 12] {
    [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}

// ===========================================================================
// Coordinator hook + heartbeat step
// ===========================================================================

/// The supervisor callbacks radius needs to re-register live Pis after a stale
/// machine registration (pi's `RadiusPresenceCoordinator`).
pub trait RadiusPresenceCoordinator: Send + Sync {
    /// The live instance for `instance_id`, if the supervisor still has it.
    fn get_live_instance(&self, instance_id: &str) -> Option<InstanceRecord>;
    /// Every live instance (for machine re-registration).
    fn list_live_instances(&self) -> Vec<InstanceRecord>;
    /// Persist an instance updated with a fresh `radiusPiId`.
    fn update_instance(&self, instance: InstanceRecord);
}

/// The result of a single heartbeat step: when (or whether) to fire the next one.
///
/// This is the port's stand-in for pi's `setTimeout`: the state machine reports
/// the delay and the runtime driver owns the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatStep {
    /// Schedule the next heartbeat after this delay.
    Next {
        /// The delay before the next heartbeat, in milliseconds.
        delay_ms: i64,
    },
    /// Stop heartbeating this target (disabled, or the target is gone).
    Stop,
}

/// The successful outcome of [`RadiusPresence::start`]: the registered machine
/// and the heartbeat interval the caller should schedule against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOutcome {
    /// The registered machine record.
    pub machine: MachineRecord,
    /// The heartbeat interval radius assigned, in milliseconds.
    pub heartbeat_interval_ms: i64,
}

/// The outcome of [`RadiusPresence::register_pi`]: the instance (updated with its
/// `radiusPiId` when registration happened) and the heartbeat interval to
/// schedule against, if enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiRegistration {
    /// The instance, carrying its `radiusPiId` when registration occurred.
    pub instance: InstanceRecord,
    /// The heartbeat interval radius assigned, or `None` when radius is disabled.
    pub heartbeat_interval_ms: Option<i64>,
}

/// Per-Pi heartbeat state: the radius id plus its backoff counters.
struct PiHeartbeatState {
    radius_pi_id: String,
    backoff: HeartbeatBackoff,
}

// ===========================================================================
// Registration response
// ===========================================================================

/// The register/re-register response body (`RegisterMachineResponse` /
/// `RegisterPiResponse`): a `RadiusRegistration` plus an `id`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterResponse {
    id: String,
    heartbeat_interval_ms: i64,
    #[allow(dead_code)]
    #[serde(default)]
    expires_in_ms: i64,
}

// ===========================================================================
// RadiusPresence
// ===========================================================================

/// A jitter source returning a `[0, 1)` sample (pi's `Math.random()`).
type JitterFn = Box<dyn Fn() -> f64 + Send + Sync>;

/// Machine + per-Pi radius presence: registration, heartbeats with exponential
/// backoff, and `404` re-registration. Mirrors pi's `RadiusPresence` class, with
/// I/O behind the injected seams (see the module docs).
pub struct RadiusPresence {
    http: Box<dyn HttpTransport>,
    credentials: Box<dyn CredentialStore>,
    clock: Box<dyn RadiusClock>,
    jitter: JitterFn,
    coordinator: Option<Box<dyn RadiusPresenceCoordinator>>,
    machine: Option<MachineRecord>,
    machine_backoff: HeartbeatBackoff,
    pi_states: BTreeMap<String, PiHeartbeatState>,
}

impl RadiusPresence {
    /// Build a presence over the injected transport, credential store, and clock.
    pub fn new(
        http: Box<dyn HttpTransport>,
        credentials: Box<dyn CredentialStore>,
        clock: Box<dyn RadiusClock>,
    ) -> Self {
        Self {
            http,
            credentials,
            clock,
            jitter: Box::new(default_jitter),
            coordinator: None,
            machine: None,
            machine_backoff: HeartbeatBackoff::new(0),
            pi_states: BTreeMap::new(),
        }
    }

    /// Override the jitter source (pi's `Math.random()`), for deterministic tests.
    pub fn with_jitter(mut self, jitter: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.jitter = Box::new(jitter);
        self
    }

    /// Wire the supervisor coordinator (pi's `setCoordinator`).
    pub fn set_coordinator(&mut self, coordinator: Box<dyn RadiusPresenceCoordinator>) {
        self.coordinator = Some(coordinator);
    }

    /// The currently registered machine, if any.
    pub fn machine(&self) -> Option<&MachineRecord> {
        self.machine.as_ref()
    }

    fn enabled(&self) -> bool {
        is_radius_enabled(self.credentials.as_ref())
    }

    fn jitter(&self) -> f64 {
        (self.jitter)()
    }

    /// Register the machine and prime its heartbeat (pi's `start`). Returns
    /// `None` when radius is disabled.
    pub fn start(&mut self, label: Option<String>) -> Result<Option<StartOutcome>, RadiusError> {
        if !self.enabled() {
            return Ok(None);
        }
        let registered = self.register_machine(label)?;
        self.machine_backoff.interval_ms = registered.heartbeat_interval_ms;
        Ok(Some(StartOutcome {
            machine: self
                .machine
                .clone()
                .expect("machine set by register_machine"),
            heartbeat_interval_ms: registered.heartbeat_interval_ms,
        }))
    }

    /// Disconnect the machine and clear all heartbeat state (pi's `stop`).
    pub fn stop(&mut self) -> Result<(), RadiusError> {
        self.pi_states.clear();
        let machine_id = match &self.machine {
            Some(machine) => machine.id.clone(),
            None => return Ok(()),
        };
        if !self.enabled() {
            return Ok(());
        }
        self.disconnect(&format!("machines/{machine_id}/disconnect"))
    }

    /// Register a Pi and prime its heartbeat (pi's `registerPi`). When radius is
    /// disabled the instance is returned unchanged.
    pub fn register_pi(&mut self, instance: InstanceRecord) -> Result<PiRegistration, RadiusError> {
        if !self.enabled() {
            return Ok(PiRegistration {
                instance,
                heartbeat_interval_ms: None,
            });
        }
        let machine = match &self.machine {
            Some(machine) => machine.clone(),
            None => load_machine()
                .map_err(RadiusError::Storage)?
                .ok_or(RadiusError::NoMachine)?,
        };

        let mut body = Map::new();
        body.insert("machineId".to_string(), json!(machine.id));
        if let Some(label) = &instance.label {
            body.insert("label".to_string(), json!(label));
        }
        body.insert("cwd".to_string(), json!(instance.cwd));
        body.insert("hostname".to_string(), json!(hostname()));
        body.insert("pid".to_string(), json!(std::process::id()));
        body.insert("transport".to_string(), json!("local-rpc"));
        body.insert(
            "capabilities".to_string(),
            json!({ "rpc": true, "relay": false, "iroh": false }),
        );
        if let Some(session_id) = &instance.session_id {
            body.insert("sessionId".to_string(), json!(session_id));
        }

        let registered: RegisterResponse = self.post_json("pis/register", &Value::Object(body))?;
        let mut registered_instance = instance.clone();
        registered_instance.radius_pi_id = Some(registered.id.clone());
        self.start_pi_heartbeat(
            &instance.id,
            registered.heartbeat_interval_ms,
            registered.id,
        );
        Ok(PiRegistration {
            instance: registered_instance,
            heartbeat_interval_ms: Some(registered.heartbeat_interval_ms),
        })
    }

    /// Disconnect a Pi and drop its heartbeat state (pi's `disconnectPi`).
    pub fn disconnect_pi(&mut self, instance: &InstanceRecord) -> Result<(), RadiusError> {
        self.pi_states.remove(&instance.id);
        if !self.enabled() {
            return Ok(());
        }
        match &instance.radius_pi_id {
            Some(radius_pi_id) => self.disconnect(&format!("pis/{radius_pi_id}/disconnect")),
            None => Ok(()),
        }
    }

    /// Perform one machine heartbeat and report the next step (pi's
    /// `heartbeatMachine`).
    pub fn heartbeat_machine(&mut self) -> HeartbeatStep {
        if self.machine.is_none() || !self.enabled() {
            return HeartbeatStep::Stop;
        }
        let machine_id = self.machine.as_ref().expect("machine present").id.clone();
        let body = json!({
            "cwd": get_orchestrator_dir().to_string_lossy(),
            "socketPath": get_socket_path().to_string_lossy(),
        });

        match self.maybe_post(&format!("machines/{machine_id}/heartbeat"), &body) {
            Ok(()) => next_from(self.machine_backoff.apply(HeartbeatOutcome::Success, 0.0)),
            Err(error) if !error.is_not_found() => {
                let decision = self
                    .machine_backoff
                    .apply(HeartbeatOutcome::TransientError, self.jitter());
                if let HeartbeatDecision::Reschedule { delay_ms } = decision {
                    log_radius_retry(
                        "Radius machine",
                        "heartbeat",
                        delay_ms,
                        self.machine_backoff.transient_failure,
                        &error,
                    );
                }
                next_from(decision)
            }
            Err(_not_found) => match self.machine_backoff.apply(HeartbeatOutcome::NotFound, 0.0) {
                HeartbeatDecision::Reschedule { delay_ms } => HeartbeatStep::Next { delay_ms },
                HeartbeatDecision::ReRegister => self.recover_machine(),
            },
        }
    }

    /// Re-register the machine and all live Pis, returning the next machine
    /// heartbeat step. On failure, back off (pi's re-registration branch).
    fn recover_machine(&mut self) -> HeartbeatStep {
        match self.re_register_machine_and_pis() {
            Ok(()) => HeartbeatStep::Next {
                delay_ms: self.machine_backoff.interval_ms,
            },
            Err(error) => {
                self.machine_backoff.transient_failure += 1;
                let delay_ms =
                    compute_backoff_delay_ms(self.machine_backoff.transient_failure, self.jitter());
                log_radius_retry(
                    "Radius machine",
                    "re-registration",
                    delay_ms,
                    self.machine_backoff.transient_failure,
                    &error,
                );
                HeartbeatStep::Next { delay_ms }
            }
        }
    }

    /// Perform one Pi heartbeat and report the next step (pi's `heartbeatPi`).
    pub fn heartbeat_pi(&mut self, instance_id: &str) -> HeartbeatStep {
        if !self.enabled() {
            return HeartbeatStep::Stop;
        }
        let radius_pi_id = match self.pi_states.get(instance_id) {
            Some(state) => state.radius_pi_id.clone(),
            None => return HeartbeatStep::Stop,
        };

        match self.maybe_post(&format!("pis/{radius_pi_id}/heartbeat"), &json!({})) {
            Ok(()) => {
                let state = self.pi_states.get_mut(instance_id).expect("state present");
                next_from(state.backoff.apply(HeartbeatOutcome::Success, 0.0))
            }
            Err(error) if !error.is_not_found() => {
                let jitter = self.jitter();
                let state = self.pi_states.get_mut(instance_id).expect("state present");
                let decision = state
                    .backoff
                    .apply(HeartbeatOutcome::TransientError, jitter);
                if let HeartbeatDecision::Reschedule { delay_ms } = decision {
                    let failure_count = state.backoff.transient_failure;
                    log_radius_retry(
                        &format!("Radius Pi {instance_id}"),
                        "heartbeat",
                        delay_ms,
                        failure_count,
                        &error,
                    );
                }
                next_from(decision)
            }
            Err(_not_found) => {
                let decision = {
                    let state = self.pi_states.get_mut(instance_id).expect("state present");
                    state.backoff.apply(HeartbeatOutcome::NotFound, 0.0)
                };
                match decision {
                    HeartbeatDecision::Reschedule { delay_ms } => HeartbeatStep::Next { delay_ms },
                    HeartbeatDecision::ReRegister => self.recover_pi(instance_id),
                }
            }
        }
    }

    /// Re-register a single Pi after its registration went stale, returning the
    /// next Pi heartbeat step (pi's `heartbeatPi` re-registration branch).
    fn recover_pi(&mut self, instance_id: &str) -> HeartbeatStep {
        match self.re_register_pi(instance_id) {
            Ok(true) => match self.pi_states.get(instance_id) {
                // re_register_pi re-primed the interval via start_pi_heartbeat.
                Some(state) => HeartbeatStep::Next {
                    delay_ms: state.backoff.interval_ms,
                },
                None => HeartbeatStep::Stop,
            },
            Ok(false) => {
                // The instance is gone and its state was dropped: stop.
                eprintln!("Radius Pi {instance_id} re-registration skipped");
                HeartbeatStep::Stop
            }
            Err(error) => {
                let jitter = self.jitter();
                let delay_ms = match self.pi_states.get_mut(instance_id) {
                    Some(state) => {
                        state.backoff.transient_failure += 1;
                        compute_backoff_delay_ms(state.backoff.transient_failure, jitter)
                    }
                    None => return HeartbeatStep::Stop,
                };
                log_radius_retry(
                    &format!("Radius Pi {instance_id}"),
                    "re-registration",
                    delay_ms,
                    self.pi_states
                        .get(instance_id)
                        .map(|s| s.backoff.transient_failure)
                        .unwrap_or(0),
                    &error,
                );
                HeartbeatStep::Next { delay_ms }
            }
        }
    }

    /// Register (or refresh) the machine record (pi's private `registerMachine`).
    fn register_machine(&mut self, label: Option<String>) -> Result<RegisterResponse, RadiusError> {
        let existing = match &self.machine {
            Some(machine) => Some(machine.clone()),
            None => load_machine().map_err(RadiusError::Storage)?,
        };

        let mut body = Map::new();
        if let Some(machine) = &existing {
            body.insert("machineId".to_string(), json!(machine.id));
        }
        if let Some(label) = &label {
            body.insert("label".to_string(), json!(label));
        }
        body.insert("hostname".to_string(), json!(hostname()));
        body.insert("platform".to_string(), json!(node_platform()));
        body.insert("arch".to_string(), json!(node_arch()));
        body.insert("version".to_string(), json!(VERSION));
        body.insert(
            "capabilities".to_string(),
            json!({ "spawn": true, "relay": false, "iroh": false }),
        );

        let registered: RegisterResponse =
            self.post_json("machines/register", &Value::Object(body))?;

        let timestamp = self.clock.now_iso();
        let machine = MachineRecord {
            id: registered.id.clone(),
            created_at: existing
                .as_ref()
                .map(|machine| machine.created_at.clone())
                .unwrap_or_else(|| timestamp.clone()),
            last_seen_at: Some(timestamp),
            label,
        };
        save_machine(&machine).map_err(RadiusError::Storage)?;
        self.machine = Some(machine);
        self.machine_backoff.consecutive_not_found = 0;
        self.machine_backoff.transient_failure = 0;
        Ok(registered)
    }

    /// Re-register the machine and every live Pi (pi's `reRegisterMachineAndPis`).
    fn re_register_machine_and_pis(&mut self) -> Result<(), RadiusError> {
        let label = self
            .machine
            .as_ref()
            .and_then(|machine| machine.label.clone());
        let registered = self.register_machine(label)?;
        self.machine_backoff.interval_ms = registered.heartbeat_interval_ms;

        let instances = self
            .coordinator
            .as_ref()
            .map(|coordinator| coordinator.list_live_instances())
            .unwrap_or_default();
        for instance in instances {
            if let Err(error) = self.re_register_pi(&instance.id) {
                eprintln!(
                    "Radius Pi {} re-registration failed: {}",
                    instance.id,
                    format_radius_error(&error)
                );
            }
        }
        Ok(())
    }

    /// Re-register one Pi against the coordinator (pi's `reRegisterPi`). Returns
    /// `false` when the instance is no longer live (its state is dropped).
    fn re_register_pi(&mut self, instance_id: &str) -> Result<bool, RadiusError> {
        let instance = self
            .coordinator
            .as_ref()
            .and_then(|coordinator| coordinator.get_live_instance(instance_id));
        let instance = match instance {
            Some(instance) => instance,
            None => {
                self.pi_states.remove(instance_id);
                return Ok(false);
            }
        };

        if self.machine.is_none() {
            self.re_register_machine_and_pis()?;
            return Ok(true);
        }

        let registered = self.register_pi(instance)?;
        if let Some(coordinator) = &self.coordinator {
            coordinator.update_instance(registered.instance);
        }
        Ok(true)
    }

    /// Prime a Pi's heartbeat state, resetting counters (pi's `startPiHeartbeat`).
    fn start_pi_heartbeat(&mut self, instance_id: &str, interval_ms: i64, radius_pi_id: String) {
        let state = self
            .pi_states
            .entry(instance_id.to_string())
            .or_insert_with(|| PiHeartbeatState {
                radius_pi_id: radius_pi_id.clone(),
                backoff: HeartbeatBackoff::new(interval_ms),
            });
        state.radius_pi_id = radius_pi_id;
        state.backoff.interval_ms = interval_ms;
        state.backoff.consecutive_not_found = 0;
        state.backoff.transient_failure = 0;
    }

    /// POST expecting a typed JSON body back (pi's `post<T>`).
    fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, RadiusError> {
        let response = self.request(path, body)?;
        serde_json::from_str(&response.body).map_err(RadiusError::Decode)
    }

    /// POST discarding the body (pi's `maybePost`).
    fn maybe_post(&self, path: &str, body: &Value) -> Result<(), RadiusError> {
        self.request(path, body).map(|_| ())
    }

    /// POST to a disconnect endpoint, swallowing a `404` (pi's `try/catch`
    /// around the disconnect `maybePost`).
    fn disconnect(&self, path: &str) -> Result<(), RadiusError> {
        match self.maybe_post(path, &json!({})) {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Perform an authenticated POST against the orchestrator base URL (pi's
    /// shared `fetch` body in `post` / `maybePost`).
    fn request(&self, path: &str, body: &Value) -> Result<HttpResponse, RadiusError> {
        let token = get_radius_access_token(self.credentials.as_ref())?;
        let url = resolve_url(&get_radius_orchestrator_base_url(), path);
        let serialized = serde_json::to_string(body).map_err(RadiusError::Decode)?;
        let request = HttpRequest::post(url, serialized)
            .with_header("authorization", format!("Bearer {token}"))
            .with_header("content-type", "application/json");
        let response = self.http.send(&request).map_err(RadiusError::Transport)?;
        if !response.is_ok() {
            return Err(RadiusError::Http {
                status: response.status,
                body: response.body,
            });
        }
        Ok(response)
    }
}

/// Turn a reschedule decision into a heartbeat step. A `ReRegister` decision is
/// handled by the caller before reaching here, so it maps to an immediate retry.
fn next_from(decision: HeartbeatDecision) -> HeartbeatStep {
    match decision {
        HeartbeatDecision::Reschedule { delay_ms } => HeartbeatStep::Next { delay_ms },
        HeartbeatDecision::ReRegister => HeartbeatStep::Next { delay_ms: 0 },
    }
}

/// The default jitter source: a `[0, 1)` sample derived from the sub-second wall
/// clock. A weak, dependency-free stand-in for `Math.random()`; jitter only
/// spreads retries, so it need not be cryptographic.
fn default_jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1_000_000) as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use atilla_ai::auth::{InMemoryCredentialStore, OAuthCredential};
    use atilla_ai::seams::http::{HttpResponse, ScriptedTransport};
    use std::sync::MutexGuard;

    // --- pure helpers --------------------------------------------------------

    #[test]
    fn resolve_url_covers_radius_cases() {
        // Absolute path replaces the base path.
        assert_eq!(
            resolve_url("https://radius.pi.dev/", "/v1/"),
            "https://radius.pi.dev/v1/"
        );
        // Relative reference resolves against the base directory.
        assert_eq!(
            resolve_url("https://radius.pi.dev/v1/", "machines/register"),
            "https://radius.pi.dev/v1/machines/register"
        );
        assert_eq!(
            resolve_url("https://radius.pi.dev/v1/", "pis/abc/heartbeat"),
            "https://radius.pi.dev/v1/pis/abc/heartbeat"
        );
        // Absolute URL passes through.
        assert_eq!(
            resolve_url("https://radius.pi.dev/v1/", "https://other/x"),
            "https://other/x"
        );
    }

    #[test]
    fn node_platform_and_arch_map_to_node_strings() {
        assert_eq!(map_node_platform("macos"), "darwin");
        assert_eq!(map_node_platform("windows"), "win32");
        assert_eq!(map_node_platform("linux"), "linux");
        assert_eq!(map_node_arch("x86_64"), "x64");
        assert_eq!(map_node_arch("aarch64"), "arm64");
        assert_eq!(map_node_arch("x86"), "ia32");
    }

    #[test]
    fn format_iso_millis_matches_to_iso_string() {
        assert_eq!(format_iso_millis(0), "1970-01-01T00:00:00.000Z");
        // 2021-01-01T00:00:00.000Z.
        assert_eq!(
            format_iso_millis(1_609_459_200_000),
            "2021-01-01T00:00:00.000Z"
        );
        // Sub-second millis are zero-padded.
        assert_eq!(
            format_iso_millis(1_609_459_200_007),
            "2021-01-01T00:00:00.007Z"
        );
    }

    // --- credential resolution ----------------------------------------------

    struct RadiusEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl RadiusEnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            RadiusEnvGuard { _lock: lock, saved }
        }
    }

    impl Drop for RadiusEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn oauth_store(access: &str) -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let mut set = |_current: Option<Credential>| {
            Ok(Some(Credential::OAuth(OAuthCredential {
                refresh: "refresh".into(),
                access: access.to_string(),
                expires: 0,
                extra: serde_json::Map::new(),
            })))
        };
        store.modify(RADIUS_PROVIDER, &mut set).unwrap();
        store
    }

    #[test]
    fn enabled_and_token_from_stored_oauth() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY"]);
        let store = oauth_store("access-token");
        assert!(is_radius_enabled(&store));
        assert_eq!(get_radius_access_token(&store).unwrap(), "access-token");
    }

    #[test]
    fn enabled_and_token_fall_back_to_env_api_key() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY"]);
        std::env::set_var("RADIUS_API_KEY", "env-key");
        let store = InMemoryCredentialStore::new();
        assert!(is_radius_enabled(&store));
        assert_eq!(get_radius_access_token(&store).unwrap(), "env-key");
    }

    #[test]
    fn disabled_without_credential_or_env() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY"]);
        let store = InMemoryCredentialStore::new();
        assert!(!is_radius_enabled(&store));
        assert!(matches!(
            get_radius_access_token(&store),
            Err(RadiusError::MissingCredentials)
        ));
    }

    #[test]
    fn orchestrator_base_url_defaults_and_env_override() {
        let _guard = RadiusEnvGuard::new(&["PI_RADIUS_URL", "PI_RADIUS_ORCHESTRATOR_URL"]);
        assert_eq!(
            get_radius_orchestrator_base_url(),
            "https://radius.pi.dev/v1/"
        );
        std::env::set_var("PI_RADIUS_ORCHESTRATOR_URL", "https://staging.example/api/");
        assert_eq!(
            get_radius_orchestrator_base_url(),
            "https://staging.example/api/"
        );
    }

    // --- integration through the fake transport -----------------------------

    struct FixedClock;
    impl RadiusClock for FixedClock {
        fn now_iso(&self) -> String {
            "2026-01-01T00:00:00.000Z".to_string()
        }
    }

    fn machine_record() -> MachineRecord {
        MachineRecord {
            id: "machine-1".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: None,
            label: None,
        }
    }

    fn instance_record(id: &str) -> InstanceRecord {
        InstanceRecord {
            id: id.to_string(),
            status: crate::types::InstanceStatus::Online,
            cwd: "/home/user/project".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: None,
            label: Some("primary".to_string()),
            session_id: Some("session-1".to_string()),
            session_file: None,
            radius_pi_id: None,
        }
    }

    fn presence(transport: ScriptedTransport, access: &str) -> RadiusPresence {
        RadiusPresence::new(
            Box::new(transport),
            Box::new(oauth_store(access)),
            Box::new(FixedClock),
        )
        .with_jitter(|| 0.0)
    }

    #[test]
    fn register_pi_builds_expected_request() {
        let _guard = RadiusEnvGuard::new(&[
            "RADIUS_API_KEY",
            "PI_RADIUS_URL",
            "PI_RADIUS_ORCHESTRATOR_URL",
        ]);
        let transport = ScriptedTransport::new();
        transport.push_ok(r#"{"id":"pi-77","heartbeatIntervalMs":20000,"expiresInMs":60000}"#);

        let mut presence = presence(transport.clone(), "tok");
        presence.machine = Some(machine_record());

        let registration = presence.register_pi(instance_record("i1")).unwrap();
        assert_eq!(registration.instance.radius_pi_id.as_deref(), Some("pi-77"));
        assert_eq!(registration.heartbeat_interval_ms, Some(20_000));

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://radius.pi.dev/v1/pis/register");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer tok")
        );
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["machineId"], "machine-1");
        assert_eq!(body["label"], "primary");
        assert_eq!(body["cwd"], "/home/user/project");
        assert_eq!(body["transport"], "local-rpc");
        assert_eq!(body["sessionId"], "session-1");
        assert_eq!(body["capabilities"]["rpc"], true);
        assert_eq!(body["capabilities"]["relay"], false);
        assert!(body.get("hostname").is_some());
        assert!(body.get("pid").is_some());

        // The heartbeat state was primed at the registered interval.
        assert_eq!(
            presence.pi_states.get("i1").map(|s| s.backoff.interval_ms),
            Some(20_000)
        );
    }

    #[test]
    fn register_pi_without_machine_errors() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY", "PI_ORCHESTRATOR_DIR"]);
        // Point machine storage at an empty temp dir so load_machine returns None.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PI_ORCHESTRATOR_DIR", dir.path());

        let transport = ScriptedTransport::new();
        let mut presence = presence(transport, "tok");
        assert!(matches!(
            presence.register_pi(instance_record("i1")),
            Err(RadiusError::NoMachine)
        ));
    }

    #[test]
    fn register_pi_disabled_returns_instance_unchanged() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY"]);
        let transport = ScriptedTransport::new();
        let mut presence = RadiusPresence::new(
            Box::new(transport.clone()),
            Box::new(InMemoryCredentialStore::new()),
            Box::new(FixedClock),
        );
        let registration = presence.register_pi(instance_record("i1")).unwrap();
        assert_eq!(registration.instance.radius_pi_id, None);
        assert_eq!(registration.heartbeat_interval_ms, None);
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn machine_heartbeat_success_reschedules_at_interval() {
        let _guard = RadiusEnvGuard::new(&[
            "RADIUS_API_KEY",
            "PI_RADIUS_URL",
            "PI_RADIUS_ORCHESTRATOR_URL",
        ]);
        let transport = ScriptedTransport::new();
        transport.push_ok("{}");
        let mut presence = presence(transport.clone(), "tok");
        presence.machine = Some(machine_record());
        presence.machine_backoff.interval_ms = 15_000;

        let step = presence.heartbeat_machine();
        assert_eq!(step, HeartbeatStep::Next { delay_ms: 15_000 });

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://radius.pi.dev/v1/machines/machine-1/heartbeat"
        );
        let body: Value = serde_json::from_str(requests[0].body.as_deref().unwrap()).unwrap();
        assert!(body.get("cwd").is_some());
        assert!(body.get("socketPath").is_some());
    }

    #[test]
    fn machine_heartbeat_transient_error_backs_off() {
        let _guard = RadiusEnvGuard::new(&[
            "RADIUS_API_KEY",
            "PI_RADIUS_URL",
            "PI_RADIUS_ORCHESTRATOR_URL",
        ]);
        let transport = ScriptedTransport::new();
        transport.push_response(Ok(HttpResponse {
            status: 500,
            headers: Default::default(),
            body: "boom".to_string(),
        }));
        let mut presence = presence(transport, "tok");
        presence.machine = Some(machine_record());
        presence.machine_backoff.interval_ms = 15_000;

        // Zero jitter -> exactly the first backoff step.
        let step = presence.heartbeat_machine();
        assert_eq!(step, HeartbeatStep::Next { delay_ms: 1_000 });
        assert_eq!(presence.machine_backoff.transient_failure, 1);
    }

    #[test]
    fn machine_heartbeat_404s_reregister_at_threshold() {
        let _guard = RadiusEnvGuard::new(&[
            "RADIUS_API_KEY",
            "PI_RADIUS_URL",
            "PI_RADIUS_ORCHESTRATOR_URL",
            "PI_ORCHESTRATOR_DIR",
        ]);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PI_ORCHESTRATOR_DIR", dir.path());

        let transport = ScriptedTransport::new();
        let not_found = || {
            Ok(HttpResponse {
                status: 404,
                headers: Default::default(),
                body: "gone".to_string(),
            })
        };
        // Three consecutive 404 heartbeats, then a successful re-register.
        transport.push_response(not_found());
        transport.push_response(not_found());
        transport.push_response(not_found());
        transport.push_ok(r#"{"id":"machine-2","heartbeatIntervalMs":25000,"expiresInMs":60000}"#);

        let mut presence = presence(transport.clone(), "tok");
        presence.machine = Some(machine_record());
        presence.machine_backoff.interval_ms = 15_000;

        // First two 404s stay below the threshold.
        assert_eq!(
            presence.heartbeat_machine(),
            HeartbeatStep::Next { delay_ms: 15_000 }
        );
        assert_eq!(
            presence.heartbeat_machine(),
            HeartbeatStep::Next { delay_ms: 15_000 }
        );
        // The third crosses it and re-registers, adopting the new interval.
        let step = presence.heartbeat_machine();
        assert_eq!(step, HeartbeatStep::Next { delay_ms: 25_000 });
        assert_eq!(presence.machine().map(|m| m.id.as_str()), Some("machine-2"));
        assert_eq!(presence.machine_backoff.consecutive_not_found, 0);

        // The last request was the re-registration POST.
        let requests = transport.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[3].url,
            "https://radius.pi.dev/v1/machines/register"
        );
    }

    #[test]
    fn pi_heartbeat_success_reschedules_at_interval() {
        let _guard = RadiusEnvGuard::new(&[
            "RADIUS_API_KEY",
            "PI_RADIUS_URL",
            "PI_RADIUS_ORCHESTRATOR_URL",
        ]);
        let transport = ScriptedTransport::new();
        transport.push_ok(r#"{"id":"pi-9","heartbeatIntervalMs":18000,"expiresInMs":60000}"#);
        transport.push_ok("{}");

        let mut presence = presence(transport.clone(), "tok");
        presence.machine = Some(machine_record());
        presence.register_pi(instance_record("i1")).unwrap();

        let step = presence.heartbeat_pi("i1");
        assert_eq!(step, HeartbeatStep::Next { delay_ms: 18_000 });
        let requests = transport.requests();
        assert_eq!(
            requests[1].url,
            "https://radius.pi.dev/v1/pis/pi-9/heartbeat"
        );
    }

    #[test]
    fn pi_heartbeat_unknown_instance_stops() {
        let _guard = RadiusEnvGuard::new(&["RADIUS_API_KEY"]);
        let transport = ScriptedTransport::new();
        let mut presence = presence(transport, "tok");
        assert_eq!(presence.heartbeat_pi("missing"), HeartbeatStep::Stop);
    }
}
