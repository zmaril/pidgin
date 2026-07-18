//! The OAuth flow state-machine contract and its Rust-native driver.
//!
//! # The one-way napi boundary
//!
//! pi's OAuth flows are async and reach for ambient `fetch` / `Date.now()` /
//! `setTimeout`. Across the napi boundary the effects must stay in JS so pi's
//! `vi.stubGlobal("fetch")` and fake timers keep intercepting unchanged. So the
//! boundary is **one-way**: Rust never performs an effect on the conformance
//! path — it yields the next [`Step`], the JS shim performs the effect (fetch,
//! sleep, prompt, notify), then re-enters Rust with the result as a
//! [`StepInput`]. Multi-step flows are therefore modelled as state machines
//! ([`OAuthFlowMachine`]): [`OAuthFlowMachine::start`] yields the first step and
//! [`OAuthFlowMachine::advance`] consumes an input and yields the next.
//!
//! The machine is the single source of truth. The JS shim and the pure-Rust
//! [`run_flow`] driver both consume the same machine, so unit tests exercise the
//! exact logic the shim drives.

use serde::{Deserialize, Serialize};

use crate::seams::clock::{Clock, Timers};
use crate::seams::http::{HttpRequest, HttpResponse, HttpTransport};
use crate::seams::provider::AbortSignal;

use crate::auth::error::AuthFlowError;
use crate::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, OAuthAuth, OAuthCredential};

use super::device_code::abortable_sleep;

/// One action yielded by an OAuth flow, serialized to JSON across the one-way
/// napi boundary.
///
/// The JS shim matches on `kind` and performs the effect:
/// - `Request` — `fetch(request)`, then re-enter with [`StepInput::Response`].
/// - `Wait` — `setTimeout(delay_ms)`, then `fetch(request)`, then
///   [`StepInput::Response`].
/// - `Prompt` — call the caller's `prompt()`, then [`StepInput::Input`].
/// - `Notify` — call the caller's `notify()`, then [`StepInput::Ack`].
/// - `Done` / `Error` — terminal.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// Perform an HTTP request, then re-enter with the response.
    Request {
        /// The request the shim should `fetch`.
        request: HttpRequest,
    },
    /// Sleep `delay_ms`, then perform an HTTP request (device-code polling).
    Wait {
        /// The delay before the request, in ms.
        delay_ms: u64,
        /// The request the shim should `fetch` after the delay.
        request: HttpRequest,
    },
    /// Prompt the caller, then re-enter with the entered/selected value.
    Prompt {
        /// The prompt to surface.
        prompt: AuthPrompt,
    },
    /// Surface a login-progress event, then re-enter with an ack.
    Notify {
        /// The event to surface.
        event: AuthEvent,
    },
    /// Terminal success — the flow produced this credential.
    Done {
        /// The resolved OAuth credential.
        credential: OAuthCredential,
    },
    /// Terminal failure — the flow failed with this message.
    Error {
        /// The failure message.
        message: String,
    },
}

/// The result of performing a [`Step`]'s effect, fed back into
/// [`OAuthFlowMachine::advance`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepInput {
    /// The response to a [`Step::Request`] / [`Step::Wait`]. Serializes as a map
    /// carrying the tag: `{"kind":"response","status":..,"headers":..,"body":..}`.
    Response(HttpResponse),
    /// The value returned by a [`Step::Prompt`] (pasted code / selected id).
    Input {
        /// The entered/selected value.
        value: String,
    },
    /// The acknowledgement of a [`Step::Notify`].
    Ack,
    /// The shim's `AbortSignal` fired mid-flow. `advance` returns
    /// `Error { "Login cancelled" }`.
    Aborted,
}

/// A resumable OAuth flow: `start` yields the first [`Step`]; `advance` consumes
/// a [`StepInput`] and yields the next.
///
/// Phase lives in the machine (via `&mut self`); `now_ms` is passed on both
/// entry points (device-code deadline base + token-expiry math) so the machine
/// never reads a wall clock itself.
pub trait OAuthFlowMachine {
    /// Begin the flow, yielding the first step.
    fn start(&mut self, now_ms: i64) -> Step;
    /// Consume the result of the previous step and yield the next.
    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step;
}

/// Drive a machine to completion against the Rust seams.
///
/// Used by unit tests ([`crate::seams::http::ScriptedTransport`] +
/// [`crate::seams::clock::FakeClock`]) and any pure-Rust runtime. The machine is
/// the single source of truth; this driver and the JS shim both consume it.
///
/// The loop mirrors the shim: `Request` → `http.send` → advance; `Wait` →
/// [`abortable_sleep`] then `http.send` → advance; `Prompt` →
/// [`AuthInteraction::prompt`] → advance; `Notify` →
/// [`AuthInteraction::notify`] → advance; `Done` returns the credential; `Error`
/// returns [`AuthFlowError`]. The abort signal is checked each iteration; when
/// tripped the machine is fed [`StepInput::Aborted`].
pub fn run_flow(
    machine: &mut dyn OAuthFlowMachine,
    http: &dyn HttpTransport,
    timers: &dyn Timers,
    clock: &dyn Clock,
    interaction: &dyn AuthInteraction,
    signal: Option<&AbortSignal>,
) -> Result<OAuthCredential, AuthFlowError> {
    // Perform a request and feed its response back into the machine — the shared
    // tail of the `Request` and `Wait` arms.
    let send_and_advance = |machine: &mut dyn OAuthFlowMachine,
                            request: &HttpRequest|
     -> Result<Step, AuthFlowError> {
        let response = http
            .send(request)
            .map_err(|error| AuthFlowError::new(error.to_string()))?;
        Ok(machine.advance(StepInput::Response(response), clock.now_ms()))
    };

    let mut step = machine.start(clock.now_ms());
    loop {
        // Abort each iteration, but never override a terminal step.
        if !matches!(step, Step::Done { .. } | Step::Error { .. })
            && signal.is_some_and(AbortSignal::is_aborted)
        {
            step = machine.advance(StepInput::Aborted, clock.now_ms());
        }

        step = match step {
            Step::Request { request } => send_and_advance(machine, &request)?,
            Step::Wait { delay_ms, request } => {
                abortable_sleep(timers, delay_ms, signal)?;
                send_and_advance(machine, &request)?
            }
            Step::Prompt { prompt } => {
                let value = interaction.prompt(prompt)?;
                machine.advance(StepInput::Input { value }, clock.now_ms())
            }
            Step::Notify { event } => {
                interaction.notify(event);
                machine.advance(StepInput::Ack, clock.now_ms())
            }
            Step::Done { credential } => return Ok(credential),
            Step::Error { message } => return Err(AuthFlowError::new(message)),
        };
    }
}

/// A no-op [`AuthInteraction`] for flows that never prompt or notify (refresh).
struct NoInteraction;

impl AuthInteraction for NoInteraction {
    fn prompt(&self, _prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        Err(AuthFlowError::new("refresh flow does not prompt"))
    }
    fn notify(&self, _event: AuthEvent) {}
}

/// Build the provider's login machine and drive it to completion.
pub fn run_login(
    auth: &dyn OAuthAuth,
    http: &dyn HttpTransport,
    timers: &dyn Timers,
    clock: &dyn Clock,
    interaction: &dyn AuthInteraction,
    signal: Option<&AbortSignal>,
) -> Result<OAuthCredential, AuthFlowError> {
    let mut machine = auth.login_machine();
    run_flow(machine.as_mut(), http, timers, clock, interaction, signal)
}

/// Build the provider's refresh machine and drive it to completion.
///
/// Refresh flows never prompt or notify, so a no-op interaction is supplied.
pub fn run_refresh(
    auth: &dyn OAuthAuth,
    credential: &OAuthCredential,
    http: &dyn HttpTransport,
    timers: &dyn Timers,
    clock: &dyn Clock,
    signal: Option<&AbortSignal>,
) -> Result<OAuthCredential, AuthFlowError> {
    let mut machine = auth.refresh_machine(credential);
    run_flow(
        machine.as_mut(),
        http,
        timers,
        clock,
        &NoInteraction,
        signal,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_step_serializes_seam_request_verbatim() {
        let request = HttpRequest::post("https://example/token", "{\"a\":1}")
            .with_header("accept", "application/json");
        let step = Step::Request { request };
        // Struct-variant: the seam request nests under `request`, tagged by `kind`.
        // Pins the exact wire JSON the host `fetch` shim consumes — the seam types'
        // derived serde must match the previous serde-remote shim byte-for-byte
        // (single-word field names, `headers` a string→string object, `body` a
        // JSON string when present).
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            json!({
                "kind": "request",
                "request": {
                    "method": "POST",
                    "url": "https://example/token",
                    "headers": { "accept": "application/json" },
                    "body": "{\"a\":1}",
                },
            })
        );
    }

    #[test]
    fn request_step_serializes_absent_body_as_null() {
        // A GET has no body; the seam's `body: Option<String>` must serialize as
        // JSON `null`, exactly as the retired shim did.
        let step = Step::Request {
            request: HttpRequest::get("https://example/authorize"),
        };
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            json!({
                "kind": "request",
                "request": {
                    "method": "GET",
                    "url": "https://example/authorize",
                    "headers": {},
                    "body": null,
                },
            })
        );
    }

    #[test]
    fn response_input_round_trips_through_tagged_map() {
        // The shim consumes `{"kind":"response","status":..,"headers":..,"body":..}`
        // — the tag plus the seam response's fields flattened alongside it. Pin
        // that exact wire shape and that the inner seam type re-serializes to the
        // same field JSON the shim emitted.
        let value = json!({
            "kind": "response",
            "status": 200,
            "headers": { "content-type": "application/json" },
            "body": "{\"ok\":true}",
        });
        let input: StepInput = serde_json::from_value(value).unwrap();
        match input {
            StepInput::Response(response) => {
                assert_eq!(response.status, 200);
                assert_eq!(response.body, "{\"ok\":true}");
                assert_eq!(
                    response.headers.get("content-type").unwrap(),
                    "application/json"
                );
                // The seam response's own wire JSON is byte-identical to the map
                // body the shim round-tripped (minus the `kind` tag).
                assert_eq!(
                    serde_json::to_value(&response).unwrap(),
                    json!({
                        "status": 200,
                        "headers": { "content-type": "application/json" },
                        "body": "{\"ok\":true}",
                    })
                );
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn input_and_ack_and_aborted_deserialize() {
        match serde_json::from_value::<StepInput>(json!({"kind":"input","value":"x"})).unwrap() {
            StepInput::Input { value } => assert_eq!(value, "x"),
            _ => panic!("expected input"),
        }
        assert!(matches!(
            serde_json::from_value::<StepInput>(json!({"kind":"ack"})).unwrap(),
            StepInput::Ack
        ));
        assert!(matches!(
            serde_json::from_value::<StepInput>(json!({"kind":"aborted"})).unwrap(),
            StepInput::Aborted
        ));
    }
}
