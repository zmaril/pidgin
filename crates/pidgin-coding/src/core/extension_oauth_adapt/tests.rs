//! Tests for the extension-OAuth bridge, exercising pi's `adaptOAuth` callback
//! mapping (`provider-composer.ts:230-248`) re-inverted onto the flow machine.
//!
//! Each test drives the machine inside [`bounded`], a watchdog that fails fast
//! rather than letting a thread/channel deadlock hang CI.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Map;

use pidgin_ai::auth::error::AuthFlowError;
use pidgin_ai::auth::oauth::extension::{
    ExtensionOAuthLogin, OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
    OAuthSelectOption, OAuthSelectPrompt,
};
use pidgin_ai::auth::oauth::flow::{OAuthFlowMachine, Step, StepInput};
use pidgin_ai::auth::types::{AuthEvent, AuthPromptKind, OAuthCredential};

use super::adapt_extension_oauth;

/// Run `f` on a worker thread and fail fast if it does not finish within the
/// bound — so a bridge deadlock surfaces as a test failure, never a CI hang.
fn bounded<T: Send + 'static>(label: &'static str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(value) => value,
        Err(_) => panic!("{label}: extension OAuth bridge deadlocked"),
    }
}

fn credential() -> OAuthCredential {
    OAuthCredential {
        refresh: "refresh-token".to_string(),
        access: "access-token".to_string(),
        expires: 1_700_000_000_000,
        extra: Map::new(),
    }
}

/// Which callbacks a [`FakeLogin`] drives before returning its outcome.
enum Mode {
    /// Drive every callback in order (auth / device-code / progress / prompt /
    /// manual-code / select), recording each prompt reply.
    AllCallbacks,
    /// Drive a single text prompt and record its reply after resuming.
    PromptOnce,
    /// Touch no callback and return the outcome directly.
    NoCallbacks,
}

/// A single configurable extension login: one trait impl covering every test's
/// callback pattern and outcome, so the tests carry no duplicated boilerplate.
struct FakeLogin {
    mode: Mode,
    log: Arc<Mutex<Vec<String>>>,
    outcome: Result<OAuthCredential, String>,
}

impl FakeLogin {
    fn record(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

impl ExtensionOAuthLogin for FakeLogin {
    fn login(&self, callbacks: &dyn OAuthLoginCallbacks) -> Result<OAuthCredential, AuthFlowError> {
        match self.mode {
            Mode::AllCallbacks => {
                callbacks.on_auth(OAuthAuthInfo {
                    url: "https://auth.example/go".to_string(),
                    instructions: Some("open the link".to_string()),
                });
                callbacks.on_device_code(OAuthDeviceCodeInfo {
                    user_code: "WDJB-MJHT".to_string(),
                    verification_uri: "https://auth.example/device".to_string(),
                    interval_seconds: Some(5.0),
                    expires_in_seconds: Some(900.0),
                });
                callbacks.on_progress("exchanging".to_string());
                let typed = callbacks.on_prompt(OAuthPrompt {
                    message: "Enter code".to_string(),
                    placeholder: Some("code".to_string()),
                    allow_empty: None,
                })?;
                self.record(format!("prompt:{typed}"));
                let pasted = callbacks.on_manual_code_input()?;
                self.record(format!("manual:{pasted}"));
                let selected = callbacks.on_select(OAuthSelectPrompt {
                    message: "Pick an account".to_string(),
                    options: vec![
                        OAuthSelectOption {
                            id: "a".to_string(),
                            label: "Account A".to_string(),
                        },
                        OAuthSelectOption {
                            id: "b".to_string(),
                            label: "Account B".to_string(),
                        },
                    ],
                })?;
                self.record(format!("select:{}", selected.unwrap_or_default()));
            }
            Mode::PromptOnce => {
                let value = callbacks.on_prompt(OAuthPrompt {
                    message: "Enter code".to_string(),
                    placeholder: None,
                    allow_empty: None,
                })?;
                self.record(format!("resumed:{value}"));
            }
            Mode::NoCallbacks => {}
        }
        self.outcome.clone().map_err(AuthFlowError::new)
    }

    fn refresh_token(
        &self,
        _credential: &OAuthCredential,
    ) -> Result<OAuthCredential, AuthFlowError> {
        self.outcome.clone().map_err(AuthFlowError::new)
    }

    fn get_api_key(&self, credential: &OAuthCredential) -> Result<String, AuthFlowError> {
        Ok(credential.access.clone())
    }
}

/// Build a `FakeLogin` with an empty reply log.
fn make_login(
    mode: Mode,
    outcome: Result<OAuthCredential, String>,
) -> (Arc<FakeLogin>, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let login = Arc::new(FakeLogin {
        mode,
        log: log.clone(),
        outcome,
    });
    (login, log)
}

/// Drive a login machine to a terminal step, feeding `Ack` for each notify and
/// the next `replies` value for each prompt; collect every yielded step.
fn drive(mut machine: Box<dyn OAuthFlowMachine>, replies: Vec<&str>) -> Vec<Step> {
    let mut replies = replies.into_iter();
    let mut steps = Vec::new();
    let mut step = machine.start(0);
    loop {
        steps.push(step.clone());
        step = match &step {
            Step::Notify { .. } => machine.advance(StepInput::Ack, 0),
            Step::Prompt { .. } => {
                let value = replies.next().unwrap_or("").to_string();
                machine.advance(StepInput::Input { value }, 0)
            }
            _ => break,
        };
    }
    steps
}

// provider-composer.ts:235-240 — each extension callback maps to the canonical
// notify/prompt shape: onAuth/onDeviceCode/onProgress -> Notify; onPrompt/
// onManualCodeInput/onSelect -> Prompt; login completion -> Done.
#[test]
fn each_callback_maps_to_expected_step_and_login_completes() {
    let (login, received) = make_login(Mode::AllCallbacks, Ok(credential()));
    let auth = adapt_extension_oauth("Extension subscription".to_string(), Some(login));
    assert_eq!(auth.name(), "Extension subscription");

    let steps = bounded("map", move || {
        drive(
            auth.login_machine(),
            vec!["typed-value", "pasted-code", "b"],
        )
    });

    // onAuth -> Notify(auth_url).
    match &steps[0] {
        Step::Notify {
            event: AuthEvent::AuthUrl { url, instructions },
        } => {
            assert_eq!(url, "https://auth.example/go");
            assert_eq!(instructions.as_deref(), Some("open the link"));
        }
        other => panic!("step 0 expected auth_url notify, got {other:?}"),
    }
    // onDeviceCode -> Notify(device_code).
    match &steps[1] {
        Step::Notify {
            event:
                AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                },
        } => {
            assert_eq!(user_code, "WDJB-MJHT");
            assert_eq!(verification_uri, "https://auth.example/device");
            assert_eq!(*interval_seconds, Some(5.0));
            assert_eq!(*expires_in_seconds, Some(900.0));
        }
        other => panic!("step 1 expected device_code notify, got {other:?}"),
    }
    // onProgress -> Notify(progress).
    match &steps[2] {
        Step::Notify {
            event: AuthEvent::Progress { message },
        } => assert_eq!(message, "exchanging"),
        other => panic!("step 2 expected progress notify, got {other:?}"),
    }
    // onPrompt -> Prompt(text).
    match &steps[3] {
        Step::Prompt { prompt } => match &prompt.kind {
            AuthPromptKind::Text {
                message,
                placeholder,
            } => {
                assert_eq!(message, "Enter code");
                assert_eq!(placeholder.as_deref(), Some("code"));
            }
            other => panic!("step 3 expected text prompt, got {other:?}"),
        },
        other => panic!("step 3 expected prompt, got {other:?}"),
    }
    // onManualCodeInput -> Prompt(manual_code) with pi's verbatim message.
    match &steps[4] {
        Step::Prompt { prompt } => match &prompt.kind {
            AuthPromptKind::ManualCode { message, .. } => {
                assert_eq!(message, "Paste the authorization code");
            }
            other => panic!("step 4 expected manual_code prompt, got {other:?}"),
        },
        other => panic!("step 4 expected prompt, got {other:?}"),
    }
    // onSelect -> Prompt(select) carrying the options.
    match &steps[5] {
        Step::Prompt { prompt } => match &prompt.kind {
            AuthPromptKind::Select { message, options } => {
                assert_eq!(message, "Pick an account");
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].id, "a");
                assert_eq!(options[1].id, "b");
            }
            other => panic!("step 5 expected select prompt, got {other:?}"),
        },
        other => panic!("step 5 expected prompt, got {other:?}"),
    }
    // Login completion -> Done with the credential.
    match &steps[6] {
        Step::Done { credential: cred } => {
            assert_eq!(cred.access, "access-token");
            assert_eq!(cred.refresh, "refresh-token");
        }
        other => panic!("step 6 expected done, got {other:?}"),
    }
    assert_eq!(steps.len(), 7);

    // Each prompt reply reached the login as its return value, in order.
    let received = received.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![
            "prompt:typed-value".to_string(),
            "manual:pasted-code".to_string(),
            "select:b".to_string(),
        ]
    );
}

// The prompt truly suspends the login: the code after `on_prompt` does not run
// until the machine is advanced with the reply (pi awaits `callbacks.prompt`).
#[test]
fn prompt_suspends_until_resumed() {
    let (login, log) = make_login(Mode::PromptOnce, Ok(credential()));
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));

    let (suspended_len, steps) = bounded("suspend", move || {
        let mut machine = auth.login_machine();
        let first = machine.start(0);
        assert!(matches!(first, Step::Prompt { .. }));
        // The login is blocked in `on_prompt`; nothing after it has run yet.
        let suspended_len = log.lock().unwrap().len();
        let done = machine.advance(
            StepInput::Input {
                value: "resume-value".to_string(),
            },
            0,
        );
        (suspended_len, vec![first, done])
    });

    assert_eq!(suspended_len, 0, "login ran past the prompt before resume");
    assert!(matches!(steps[1], Step::Done { .. }));
}

// A login that returns before touching a callback yields Done immediately.
#[test]
fn successful_login_without_callbacks_yields_done() {
    let (login, _log) = make_login(Mode::NoCallbacks, Ok(credential()));
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));
    let steps = bounded("noop-done", move || drive(auth.login_machine(), vec![]));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        Step::Done { credential: cred } => assert_eq!(cred.access, "access-token"),
        other => panic!("expected done, got {other:?}"),
    }
}

// A login error becomes a terminal Error step carrying the message.
#[test]
fn login_error_yields_error_step() {
    let (login, _log) = make_login(
        Mode::NoCallbacks,
        Err("device authorization failed".to_string()),
    );
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));
    let steps = bounded("error", move || drive(auth.login_machine(), vec![]));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        Step::Error { message } => assert_eq!(message, "device authorization failed"),
        other => panic!("expected error, got {other:?}"),
    }
}

// An abort at a pending prompt unwinds the login thread and yields the
// conventional "Login cancelled" error (matching the other OAuth machines).
#[test]
fn abort_at_prompt_yields_login_cancelled() {
    let (login, _log) = make_login(Mode::AllCallbacks, Ok(credential()));
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));

    let final_step = bounded("abort", move || {
        let mut machine = auth.login_machine();
        let mut step = machine.start(0);
        // Advance past the notifies to the first prompt.
        while matches!(step, Step::Notify { .. }) {
            step = machine.advance(StepInput::Ack, 0);
        }
        assert!(matches!(step, Step::Prompt { .. }));
        machine.advance(StepInput::Aborted, 0)
    });

    match final_step {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancelled error, got {other:?}"),
    }
}

// The refresh machine runs `refresh_token` as a single terminal step.
#[test]
fn refresh_machine_yields_done() {
    let refreshed = OAuthCredential {
        refresh: "next-refresh".to_string(),
        access: "next-access".to_string(),
        expires: 1_800_000_000_000,
        extra: Map::new(),
    };
    let (login, _log) = make_login(Mode::NoCallbacks, Ok(refreshed));
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));
    let mut machine = auth.refresh_machine(&credential());
    match machine.start(0) {
        Step::Done { credential: cred } => {
            assert_eq!(cred.access, "next-access");
            assert_eq!(cred.refresh, "next-refresh");
        }
        other => panic!("expected refresh done, got {other:?}"),
    }
}

// `to_auth` derives request auth from `get_api_key` (pi's
// `toAuth: (credential) => ({ apiKey: getApiKey(credential) })`).
#[test]
fn to_auth_uses_get_api_key() {
    let (login, _log) = make_login(Mode::NoCallbacks, Ok(credential()));
    let auth = adapt_extension_oauth("ext".to_string(), Some(login));
    let model_auth = auth.to_auth(&credential()).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("access-token"));
}

// With no login wired (the extension plane has not connected its JS closure),
// the flow machines and `to_auth` report a wiring error instead of panicking.
#[test]
fn unwired_login_reports_wiring_error() {
    let auth = adapt_extension_oauth("ext".to_string(), None);
    let mut machine = auth.login_machine();
    match machine.start(0) {
        Step::Error { message } => assert_eq!(message, "extension OAuth login is not wired"),
        other => panic!("expected wiring error, got {other:?}"),
    }
    let err = auth.to_auth(&credential()).unwrap_err();
    assert_eq!(err.message, "extension OAuth login is not wired");
}
