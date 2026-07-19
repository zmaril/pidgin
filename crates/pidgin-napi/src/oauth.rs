//! Node-API surface for the OAuth flow state machines (`OAuthFlowCore` and
//! `DeviceCodePollCore`).
//!
//! These expose the two one-way OAuth state machines
//! ([`pidgin_ai::auth::oauth::flow::OAuthFlowMachine`] and
//! [`pidgin_ai::auth::DeviceCodePollMachine`]) to pi's OAuth conformance tests.
//!
//! # The one-way napi boundary
//!
//! pi's OAuth flows are async and reach for ambient `fetch` / `Date.now()` /
//! `setTimeout`, so those effects must stay in JS for `vi.stubGlobal("fetch")`
//! and fake timers to keep intercepting. The boundary is therefore one-way: Rust
//! never performs an effect here — it yields the next `Step` / `DevicePollStep`
//! (serialized as JSON), the JS shim performs the effect (fetch, sleep, prompt,
//! notify), then re-enters Rust with the result as a `StepInput` /
//! `DevicePollInput`. See `pidgin_ai::auth::oauth::flow` for the contract.
//!
//! Because the trait methods take `&mut self` while napi hands out shared `&self`
//! references to a class instance, each core wraps its machine in a [`RefCell`]
//! and `borrow_mut`s inside the method. `now_ms` crosses as an `f64` (JS
//! `Date.now()` is a safe-integer double) and is cast to `i64`, sidestepping napi
//! BigInt friction.

// straitjacket-allow-file:duplication — `OAuthFlowCore` and `DeviceCodePollCore`
// expose the same faithful `start`/`advance` napi wrapper shape over two
// different machines; the parallel structure is the point, not accidental copy.

use std::cell::RefCell;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Deserialize;

use pidgin_ai::auth::oauth::device_code::DeviceCodePollOptions;
use pidgin_ai::auth::oauth::flow::{OAuthFlowMachine, StepInput};
use pidgin_ai::auth::{oauth_flow_for, DeviceCodePollMachine, DevicePollInput, OAuthFlowMode};

/// The Rust-backed multi-step OAuth login/refresh flow, exposed to JavaScript as
/// `OAuthFlowCore`.
///
/// The JS provider shim constructs one per `login`/`refresh` call and drives it:
/// `start` yields the first step, and each `advance` consumes the JSON result of
/// the previous step's effect and yields the next step, until `done`/`error`.
#[napi(js_name = "OAuthFlowCore")]
pub struct OAuthFlowCore {
    /// The wrapped flow machine. `RefCell` because the trait's `start`/`advance`
    /// take `&mut self` while the napi methods receive a shared `&self`.
    machine: RefCell<Box<dyn OAuthFlowMachine>>,
}

#[napi]
impl OAuthFlowCore {
    /// Resolve a provider id + mode into a driveable flow machine.
    ///
    /// `mode` is `"login"` or `"refresh"` (case-insensitive); `credential_json`
    /// is the serialized `OAuthCredential` required for `"refresh"` (ignored for
    /// `"login"`). Errors map an `AuthFlowError` (unknown provider, missing /
    /// malformed refresh credential) to a thrown JS `Error`.
    #[napi(constructor)]
    pub fn new(provider: String, mode: String, credential_json: Option<String>) -> Result<Self> {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "login" => OAuthFlowMode::Login,
            "refresh" => OAuthFlowMode::Refresh,
            other => {
                return Err(Error::from_reason(format!(
                    "unknown OAuth flow mode: {other}"
                )))
            }
        };
        let machine = oauth_flow_for(&provider, mode, credential_json.as_deref())
            .map_err(|error| Error::from_reason(error.message))?;
        Ok(Self {
            machine: RefCell::new(machine),
        })
    }

    /// Begin the flow, returning the first `Step` as JSON.
    #[napi(js_name = "start")]
    pub fn start(&self, now_ms: f64) -> Result<String> {
        let step = self.machine.borrow_mut().start(now_ms as i64);
        serde_json::to_string(&step).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Consume the JSON `StepInput` result of the previous step and return the
    /// next `Step` as JSON.
    #[napi(js_name = "advance")]
    pub fn advance(&self, input_json: String, now_ms: f64) -> Result<String> {
        let input: StepInput = serde_json::from_str(&input_json)
            .map_err(|error| Error::from_reason(format!("invalid step input: {error}")))?;
        let step = self.machine.borrow_mut().advance(input, now_ms as i64);
        serde_json::to_string(&step).map_err(|error| Error::from_reason(error.to_string()))
    }
}

/// JSON shape of pi's `OAuthDeviceCodePollOptions` subset the machine needs,
/// parsed at the boundary and mapped onto [`DeviceCodePollOptions`] (which is not
/// itself `Deserialize`). Field names are pi's camelCase.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DevicePollOptionsJson {
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
    wait_before_first_poll: bool,
}

/// The Rust-backed RFC 8628 device-code poll loop, exposed to JavaScript as
/// `DeviceCodePollCore`.
///
/// The JS `pollOAuthDeviceCodeFlow` shim constructs one from the poll options and
/// drives it: `start` yields the first step (poll now or wait one interval), and
/// each `advance` consumes the JSON result of the caller's `poll()` and yields
/// the next step, until `done`/`error`.
#[napi(js_name = "DeviceCodePollCore")]
pub struct DeviceCodePollCore {
    /// The wrapped poll machine, behind a `RefCell` for the same `&mut self`
    /// reason as [`OAuthFlowCore`].
    machine: RefCell<DeviceCodePollMachine>,
}

#[napi]
impl DeviceCodePollCore {
    /// Build a poll machine from pi's poll options, JSON-encoded.
    #[napi(constructor)]
    pub fn new(options_json: String) -> Result<Self> {
        let parsed: DevicePollOptionsJson = if options_json.trim().is_empty() {
            DevicePollOptionsJson::default()
        } else {
            serde_json::from_str(&options_json).map_err(|error| {
                Error::from_reason(format!("invalid device poll options: {error}"))
            })?
        };
        let options = DeviceCodePollOptions {
            interval_seconds: parsed.interval_seconds,
            expires_in_seconds: parsed.expires_in_seconds,
            wait_before_first_poll: parsed.wait_before_first_poll,
        };
        Ok(Self {
            machine: RefCell::new(DeviceCodePollMachine::new(options)),
        })
    }

    /// Begin the poll loop, returning the first `DevicePollStep` as JSON.
    #[napi(js_name = "start")]
    pub fn start(&self, now_ms: f64) -> Result<String> {
        let step = self.machine.borrow_mut().start(now_ms as i64);
        serde_json::to_string(&step).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Consume the JSON `DevicePollInput` result of the previous poll and return
    /// the next `DevicePollStep` as JSON.
    #[napi(js_name = "advance")]
    pub fn advance(&self, input_json: String, now_ms: f64) -> Result<String> {
        let input: DevicePollInput = serde_json::from_str(&input_json)
            .map_err(|error| Error::from_reason(format!("invalid device poll input: {error}")))?;
        let step = self.machine.borrow_mut().advance(input, now_ms as i64);
        serde_json::to_string(&step).map_err(|error| Error::from_reason(error.to_string()))
    }
}
