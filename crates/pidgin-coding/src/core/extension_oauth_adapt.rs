//! Adapt an extension's callback-driven OAuth login into a canonical
//! [`OAuthAuth`] flow machine — the flow-machine re-inversion of pi's
//! `adaptOAuth` (`packages/coding-agent/src/core/provider-composer.ts:230`).
//!
//! In pi this bridge lives in the coding-agent package's `provider-composer.ts`
//! alongside `composeModelProvider`, and so it does here: this module sits in
//! pidgin-coding next to
//! [`provider_composer_auth`](super::provider_composer_auth), whose
//! [`adapt_oauth`](super::provider_composer_auth::adapt_oauth) is the thin call
//! into [`adapt_extension_oauth`]. The extension-OAuth *type surface* and the
//! [`ExtensionOAuthLogin`] seam it drives stay in pidgin-ai
//! ([`pidgin_ai::auth::oauth::extension`]); this module reaches them cross-crate.
//!
//! # The re-inversion
//!
//! pi's `adaptOAuth` bridges a **push**-based extension login — one that is
//! handed an [`OAuthLoginCallbacks`] and calls `onAuth`/`onDeviceCode`/
//! `onProgress` (fire-and-forget) and `onPrompt`/`onManualCodeInput`/`onSelect`
//! (awaiting a reply) — onto the ai `OAuthAuth.login(interaction)`'s
//! `interaction.notify` / `interaction.prompt`. pidgin-ai's [`OAuthAuth`] is not
//! callback-shaped: it is the **pull** flow machine ([`OAuthFlowMachine`]) whose
//! `start`/`advance` yield [`Step`]s (`Notify`/`Prompt`/`Done`/`Error`) and
//! suspend on a prompt until resumed with input. So this module *re-inverts*: it
//! presents a push login as a pull machine.
//!
//! # The bridge mechanism (thread + channel pair)
//!
//! The crate has no async runtime, so the natural coroutine bridge is a
//! `std::thread` running the extension login, wired to the machine by a pair of
//! [`std::sync::mpsc`] channels:
//!
//! - a **step channel** the login thread sends [`Step`]s over, and
//! - a **resume channel** the machine sends the prompt reply back over.
//!
//! A fire-and-forget callback (`onAuth`/`onDeviceCode`/`onProgress`) sends a
//! [`Step::Notify`] and returns immediately. A reply-shaped callback
//! (`onPrompt`/`onManualCodeInput`/`onSelect`) sends a [`Step::Prompt`] and then
//! **blocks** receiving the resume value — so the login truly suspends until the
//! machine is advanced with input, reproducing pi's await-on-`callbacks.prompt`
//! observable behavior. The machine's [`start`](OAuthFlowMachine::start) reads
//! the first step; [`advance`](OAuthFlowMachine::advance) forwards a
//! [`StepInput::Input`] back over the resume channel (or just reads the next step
//! on [`StepInput::Ack`]) and reads the next step. Login completion becomes
//! [`Step::Done`]; a login error becomes [`Step::Error`]; a
//! [`StepInput::Aborted`] unwinds the thread and yields the conventional
//! `"Login cancelled"` error. The channels are unbounded, so a send never blocks
//! and the thread is always joinable once its pending prompt is resumed or the
//! resume channel is dropped.
//!
//! # The extension-login seam
//!
//! The concrete extension login is JavaScript living in the extension plane
//! (pidgin-extensions), not here. pidgin-ai defines the Rust callback surface
//! ([`OAuthLoginCallbacks`]) and the login callable ([`ExtensionOAuthLogin`])
//! that the extension plane implements over its JS closures; wiring JS to the
//! trait is that plane's job. [`adapt_extension_oauth`] drives whatever
//! [`ExtensionOAuthLogin`] it is given.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use pidgin_ai::auth::error::AuthFlowError;
use pidgin_ai::auth::oauth::device_code::CANCEL_MESSAGE;
use pidgin_ai::auth::oauth::extension::{
    ExtensionOAuthLogin, OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
    OAuthSelectPrompt,
};
use pidgin_ai::auth::oauth::flow::{OAuthFlowMachine, Step, StepInput};
use pidgin_ai::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, AuthSelectOption, ModelAuth, OAuthAuth, OAuthCredential,
};

/// Build a canonical [`OAuthAuth`] from an extension login callable — the
/// flow-machine analog of pi's `adaptOAuth` (`provider-composer.ts:230-248`).
///
/// `login` is `None` when the extension plane has not wired the JS login to the
/// trait; the returned handler's flow machines then yield a wiring error rather
/// than panicking.
pub fn adapt_extension_oauth(
    name: String,
    login: Option<Arc<dyn ExtensionOAuthLogin>>,
) -> Box<dyn OAuthAuth> {
    Box::new(ExtensionOAuthAuth { name, login })
}

/// The [`OAuthAuth`] returned by [`adapt_extension_oauth`], carrying the
/// extension login callable.
struct ExtensionOAuthAuth {
    name: String,
    login: Option<Arc<dyn ExtensionOAuthLogin>>,
}

impl OAuthAuth for ExtensionOAuthAuth {
    fn name(&self) -> &str {
        // pi's `adaptOAuth` sets `name: config.name` (`provider-composer.ts:232`).
        &self.name
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(ExtensionLoginMachine::new(self.login.clone()))
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        // pi's `refresh: (credential) => ({ ...refreshToken(credential), type: "oauth" })`
        // (`provider-composer.ts:245`) — a single blocking exchange, no prompts.
        Box::new(ExtensionRefreshMachine::new(
            self.login.clone(),
            credential.clone(),
        ))
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // pi's `toAuth: (credential) => ({ apiKey: getApiKey(credential) })`
        // (`provider-composer.ts:246`).
        let login = self
            .login
            .as_ref()
            .ok_or_else(|| AuthFlowError::new(NOT_WIRED))?;
        Ok(ModelAuth {
            api_key: Some(login.get_api_key(credential)?),
            ..ModelAuth::default()
        })
    }
}

/// The error yielded when the extension plane has not wired a JS login onto the
/// [`ExtensionOAuthLogin`] trait.
const NOT_WIRED: &str = "extension OAuth login is not wired";

/// A prompt reply (or abort) the machine forwards to a suspended login thread.
enum Resume {
    /// The value entered/selected for a pending prompt.
    Value(String),
    /// The flow was aborted; the pending prompt should error out.
    Aborted,
}

/// The live channels + join handle for a running login thread.
struct Running {
    /// Steps yielded by the login thread.
    step_rx: Receiver<Step>,
    /// Prompt replies forwarded to the login thread.
    resume_tx: Sender<Resume>,
    /// The login thread, joined once terminal.
    handle: Option<JoinHandle<()>>,
}

/// The login-side bridge: presents a push [`ExtensionOAuthLogin::login`] as a
/// pull [`OAuthFlowMachine`] via a thread + channel pair.
struct ExtensionLoginMachine {
    /// The login callable, taken on `start`. `None` means the plane never wired
    /// a login onto the trait.
    login: Option<Arc<dyn ExtensionOAuthLogin>>,
    /// The running thread's channels, present between `start` and the terminal
    /// step.
    running: Option<Running>,
    /// Whether a terminal step has been yielded (guards double-drive).
    finished: bool,
}

impl ExtensionLoginMachine {
    fn new(login: Option<Arc<dyn ExtensionOAuthLogin>>) -> Self {
        Self {
            login,
            running: None,
            finished: false,
        }
    }

    /// Join the login thread and drop the channels; idempotent.
    fn finish(&mut self) {
        self.finished = true;
        if let Some(mut running) = self.running.take() {
            if let Some(handle) = running.handle.take() {
                // The resume channel is dropped below (with `running`), so a
                // prompt-suspended thread unblocks; sends are unbounded so the
                // thread never blocks. The join therefore always returns.
                let _ = handle.join();
            }
        }
    }

    /// Read the next step from the login thread, joining on a terminal step or a
    /// closed channel.
    fn next_step(&mut self) -> Step {
        let received = self.running.as_ref().map(|running| running.step_rx.recv());
        match received {
            Some(Ok(step)) => {
                if matches!(step, Step::Done { .. } | Step::Error { .. }) {
                    self.finish();
                }
                step
            }
            // The thread ended without a terminal step (panicked / dropped).
            Some(Err(_)) | None => {
                self.finish();
                Step::Error {
                    message: "extension OAuth login ended without a result".to_string(),
                }
            }
        }
    }
}

impl OAuthFlowMachine for ExtensionLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        let Some(login) = self.login.take() else {
            self.finished = true;
            return Step::Error {
                message: NOT_WIRED.to_string(),
            };
        };
        let (step_tx, step_rx) = std::sync::mpsc::channel::<Step>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<Resume>();
        let handle = std::thread::spawn(move || {
            let callbacks = ChannelCallbacks { step_tx, resume_rx };
            // pi's `login` returns `{ ...credential, type: "oauth" }`; the `type`
            // tag lives on the `Credential` enum here, so `OAuthCredential` is
            // yielded directly (`provider-composer.ts:234-243`).
            let final_step = match login.login(&callbacks) {
                Ok(credential) => Step::Done { credential },
                Err(error) => Step::Error {
                    message: error.message,
                },
            };
            let _ = callbacks.step_tx.send(final_step);
        });
        self.running = Some(Running {
            step_rx,
            resume_tx,
            handle: Some(handle),
        });
        self.next_step()
    }

    fn advance(&mut self, input: StepInput, _now_ms: i64) -> Step {
        if self.finished {
            return Step::Error {
                message: "extension OAuth login already completed".to_string(),
            };
        }
        match input {
            StepInput::Input { value } => {
                if let Some(running) = self.running.as_ref() {
                    let _ = running.resume_tx.send(Resume::Value(value));
                }
                self.next_step()
            }
            // A notify callback did not block, so there is nothing to resume; the
            // login thread has already progressed to the next step.
            StepInput::Ack => self.next_step(),
            StepInput::Aborted => {
                // Unblock a prompt-suspended thread so it unwinds, then join. Its
                // eventual error step is discarded in favor of the conventional
                // cancellation message.
                if let Some(running) = self.running.as_ref() {
                    let _ = running.resume_tx.send(Resume::Aborted);
                }
                self.finish();
                Step::Error {
                    message: CANCEL_MESSAGE.to_string(),
                }
            }
            StepInput::Response(_) => {
                self.finish();
                Step::Error {
                    message: "extension OAuth login does not perform host requests".to_string(),
                }
            }
        }
    }
}

/// The [`OAuthLoginCallbacks`] the login thread drives: each callback sends a
/// [`Step`] and, for prompts, blocks receiving the [`Resume`] reply.
struct ChannelCallbacks {
    step_tx: Sender<Step>,
    resume_rx: Receiver<Resume>,
}

impl ChannelCallbacks {
    /// Send a prompt step and block for the reply (shared by the three
    /// reply-shaped callbacks).
    fn prompt(&self, kind: AuthPromptKind) -> Result<String, AuthFlowError> {
        // If the machine dropped, the step send fails; surface it as a cancel.
        if self
            .step_tx
            .send(Step::Prompt {
                prompt: AuthPrompt { signal: None, kind },
            })
            .is_err()
        {
            return Err(AuthFlowError::new(CANCEL_MESSAGE));
        }
        match self.resume_rx.recv() {
            Ok(Resume::Value(value)) => Ok(value),
            Ok(Resume::Aborted) | Err(_) => Err(AuthFlowError::new(CANCEL_MESSAGE)),
        }
    }

    /// Send a notify step; fire-and-forget (the caller does not wait).
    fn notify(&self, event: AuthEvent) {
        let _ = self.step_tx.send(Step::Notify { event });
    }
}

impl OAuthLoginCallbacks for ChannelCallbacks {
    fn on_auth(&self, info: OAuthAuthInfo) {
        // `onAuth: (info) => callbacks.notify({ type: "auth_url", ...info })`.
        self.notify(AuthEvent::AuthUrl {
            url: info.url,
            instructions: info.instructions,
        });
    }

    fn on_device_code(&self, info: OAuthDeviceCodeInfo) {
        // `onDeviceCode: (info) => callbacks.notify({ type: "device_code", ...info })`.
        self.notify(AuthEvent::DeviceCode {
            user_code: info.user_code,
            verification_uri: info.verification_uri,
            interval_seconds: info.interval_seconds,
            expires_in_seconds: info.expires_in_seconds,
        });
    }

    fn on_prompt(&self, prompt: OAuthPrompt) -> Result<String, AuthFlowError> {
        // `onPrompt: (prompt) => callbacks.prompt({ type: "text", ...prompt })`.
        self.prompt(AuthPromptKind::Text {
            message: prompt.message,
            placeholder: prompt.placeholder,
        })
    }

    fn on_progress(&self, message: String) {
        // `onProgress: (message) => callbacks.notify({ type: "progress", message })`.
        self.notify(AuthEvent::Progress { message });
    }

    fn on_manual_code_input(&self) -> Result<String, AuthFlowError> {
        // `onManualCodeInput: () => callbacks.prompt({ type: "manual_code",
        //   message: "Paste the authorization code" })`.
        self.prompt(AuthPromptKind::ManualCode {
            message: "Paste the authorization code".to_string(),
            placeholder: None,
        })
    }

    fn on_select(&self, prompt: OAuthSelectPrompt) -> Result<Option<String>, AuthFlowError> {
        // `onSelect: (prompt) => callbacks.prompt({ type: "select", ...prompt })`.
        let options = prompt
            .options
            .into_iter()
            .map(|option| AuthSelectOption {
                id: option.id,
                label: option.label,
                description: None,
            })
            .collect();
        self.prompt(AuthPromptKind::Select {
            message: prompt.message,
            options,
        })
        .map(Some)
    }
}

/// The refresh-side bridge: runs [`ExtensionOAuthLogin::refresh_token`] as a
/// single terminal step (refresh never prompts or notifies).
struct ExtensionRefreshMachine {
    login: Option<Arc<dyn ExtensionOAuthLogin>>,
    credential: OAuthCredential,
}

impl ExtensionRefreshMachine {
    fn new(login: Option<Arc<dyn ExtensionOAuthLogin>>, credential: OAuthCredential) -> Self {
        Self { login, credential }
    }

    fn terminal(&self) -> Step {
        let Some(login) = self.login.as_ref() else {
            return Step::Error {
                message: NOT_WIRED.to_string(),
            };
        };
        match login.refresh_token(&self.credential) {
            Ok(credential) => Step::Done { credential },
            Err(error) => Step::Error {
                message: error.message,
            },
        }
    }
}

impl OAuthFlowMachine for ExtensionRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.terminal()
    }

    fn advance(&mut self, _input: StepInput, _now_ms: i64) -> Step {
        // `start` already yields a terminal step, so the driver never advances.
        Step::Error {
            message: "extension OAuth refresh already completed".to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
