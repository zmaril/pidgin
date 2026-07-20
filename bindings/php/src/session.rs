//! Hand-authored `Pidgin\Session` surface: a stateful, multi-turn agent session
//! runnable from PHP.
//!
//! Unlike the `Pidgin` class in [`crate::generated`], this surface is **not**
//! round-tripped through fluessig: fluessig does not yet model stateful,
//! streaming session objects (an owned agent harness that retains conversation
//! context across calls), so the class is authored by hand here as a peer to
//! `core_impl.rs`. It reaches the engine only through the `pidgin-core` façade.
//!
//! # Storage: an `!Send` harness stored behind a `RefCell`
//!
//! The engine's [`AgentHarness`] is `Rc`-backed and therefore `!Send`. PHP NTS
//! is single-threaded and ext-php-rs 0.13.1 accepts an `!Send` `#[php_class]`,
//! so the clean design is to **store the harness itself** inside the class. The
//! harness owns an in-memory [`AgentSession`], so consecutive `send()` /
//! `sendStream()` calls naturally retain multi-turn conversation context.
//!
//! # Two paths
//!
//! - **Faux (offline, primary):** a custom [`ProviderStream`] seam closes over a
//!   [`FauxProvider`]; each turn echoes the latest user message with a
//!   deterministic canned reply. No network, no API key, never errors.
//! - **Live:** the builtin model registry + its provider stream (pi's print-mode
//!   wiring). Real HTTP is compiled in only under the `native-http` feature;
//!   without it (or without `ANTHROPIC_API_KEY`) a turn resolves to an assistant
//!   message with `stopReason == "error"`, whose `errorMessage` we surface as a
//!   thrown [`PhpException`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ext_php_rs::prelude::*;
use serde_json::Value;

use pidgin_core::agent::harness::agent_harness::{AgentHarness, AgentHarnessEvent};
use pidgin_core::agent::harness::env::MemoryExecutionEnv;
use pidgin_core::agent::harness::options::{
    AgentHarnessOptions, ProviderStream, SystemPromptSource,
};
use pidgin_core::agent::harness::session::{InMemorySessionStorage, Session as AgentSession};
use pidgin_core::ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxProvider, FauxResponseStep,
    RegisterFauxProviderOptions,
};
use pidgin_core::ai::providers::registry::{create_models, Models};
use pidgin_core::ai::seams::{AbortSignal, Provider};
use pidgin_core::ai::types::{ContentBlock, Context, Message, Model, UserContent};
use pidgin_core::coding::modes::print::{
    builtin_models_registry, provider_stream, RegistryCompaction,
};

/// Execution-environment root for the in-memory session (no real filesystem is
/// touched by a completion-only turn).
const DEFAULT_CWD: &str = ".";

/// Default provider for the live path.
const DEFAULT_PROVIDER: &str = "anthropic";

/// Default live model id. Present in the builtin anthropic catalog (verified via
/// `pidgin_ai::providers::builtins` tests, e.g. the `("anthropic",
/// "claude-opus-4-8")` compat/enumeration assertions in `builtins.rs`).
const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// A core-layer failure becomes a thrown PHP exception (PHP is synchronous, so a
/// fallible op returns `PhpResult` and ext-php-rs raises on `Err`).
fn err(e: impl std::fmt::Display) -> PhpException {
    PhpException::default(e.to_string())
}

/// Live interior of a [`Session`]: the owned harness plus, on the faux path, the
/// `FauxProvider` kept alive for the seam closure that borrows it.
struct SessionState {
    harness: AgentHarness,
    _faux: Option<Arc<FauxProvider>>,
}

/// A stateful, multi-turn pidgin agent session.
///
/// PHP: `Pidgin\Session`.
#[php_class(name = "Pidgin\\Session")]
pub struct Session {
    inner: RefCell<SessionState>,
}

#[php_impl]
impl Session {
    /// `__construct(?string $model = null, ?string $provider = null, ?string $systemPrompt = null, ?bool $faux = null)`
    ///
    /// `faux = true` forces the offline canned provider (no key required); a null
    /// or absent `faux` means `false` (live path). `model` / `provider` /
    /// `systemPrompt` default to null (sensible internal defaults are chosen).
    ///
    /// Note: `faux` is `?bool` (semantic default `false`) rather than a bare
    /// `bool $faux = false` because ext-php-rs 0.13.1's `#[defaults]` codegen
    /// mishandles a `bool` literal default (a type-inference failure); `Option`
    /// is the equivalent, working spelling.
    #[optional(model)]
    pub fn __construct(
        model: Option<String>,
        provider: Option<String>,
        system_prompt: Option<String>,
        faux: Option<bool>,
    ) -> PhpResult<Self> {
        let state = if faux.unwrap_or(false) {
            let (harness, faux_provider) = build_faux_harness(system_prompt).map_err(err)?;
            SessionState {
                harness,
                _faux: Some(faux_provider),
            }
        } else {
            let provider = provider.unwrap_or_else(|| DEFAULT_PROVIDER.to_string());
            let model_id = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
            let harness = build_live_harness(&provider, &model_id, system_prompt).map_err(err)?;
            SessionState {
                harness,
                _faux: None,
            }
        };
        Ok(Session {
            inner: RefCell::new(state),
        })
    }

    /// `send(string $message): string` — run one turn synchronously (blocking)
    /// and return the assistant's full text reply. Conversation context is
    /// retained for subsequent calls.
    pub fn send(&self, message: String) -> PhpResult<String> {
        let value = self.run_turn(&message).map_err(err)?;
        Ok(extract_text(&value))
    }

    /// `sendStream(string $message): iterable` — run the turn and return the
    /// assistant reply decomposed into text deltas as a PHP array (a PHP array is
    /// iterable / `foreach`-able). The underlying turn genuinely streams: we
    /// subscribe to the harness event stream and collect the incremental text
    /// deltas. Context is retained just like [`send`](Self::send).
    pub fn send_stream(&self, message: String) -> PhpResult<Vec<String>> {
        let deltas: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        // Subscribe before the turn; the closure records each streamed text
        // delta (the loop's `text_delta` events). Dropped after the turn.
        let subscription = {
            let deltas = deltas.clone();
            let state = self.inner.borrow();
            state.harness.subscribe(Rc::new(
                move |event: &AgentHarnessEvent, _signal: Option<&AbortSignal>| {
                    if let AgentHarnessEvent::Loop(loop_event) = event {
                        if let Some(delta) = text_delta_of(loop_event) {
                            if !delta.is_empty() {
                                deltas.borrow_mut().push(delta);
                            }
                        }
                    }
                },
            ))
        };

        let turn = self.run_turn(&message);
        drop(subscription);
        let value = turn.map_err(err)?;

        let mut collected = deltas.borrow().clone();
        if collected.is_empty() {
            // No streamed updates were observed (e.g. an empty reply): fall back
            // to the final text as a single element so the iterable is faithful.
            let full = extract_text(&value);
            if !full.is_empty() {
                collected.push(full);
            }
        }
        Ok(collected)
    }
}

impl Session {
    /// Run one blocking turn against the owned harness, mapping both a harness
    /// failure and a terminal `error`/`aborted` assistant message to `Err`.
    fn run_turn(&self, message: &str) -> Result<Value, String> {
        let state = self.inner.borrow();
        let value = state
            .harness
            .prompt(message, None)
            .map_err(|e| e.to_string())?;
        check_turn_error(&value)?;
        Ok(value)
    }
}

/// Assemble the faux (offline) harness plus the `FauxProvider` the seam borrows.
fn build_faux_harness(
    system_prompt: Option<String>,
) -> Result<(AgentHarness, Arc<FauxProvider>), String> {
    let faux = Arc::new(FauxProvider::new(RegisterFauxProviderOptions::default()));
    let model = faux
        .get_model(None)
        .ok_or_else(|| "faux provider has no default model".to_string())?;

    let stream: ProviderStream = {
        let faux = faux.clone();
        Rc::new(move |req| {
            // Echo the latest user message (mirrors pi's interactive faux turn),
            // offline and deterministic.
            let reply = faux_reply(req.context);
            faux.set_responses(vec![FauxResponseStep::from(faux_assistant_message(
                vec![faux_text(reply)],
                FauxAssistantOptions::default(),
                0,
            ))]);
            faux.stream(req.model, req.context, None, req.signal)
        })
    };

    // Compaction is not reached by a single completion turn, but the harness
    // requires the seam; an empty registry suffices for the faux path.
    let registry = Rc::new(create_models());
    let harness = build_harness(model, stream, registry, system_prompt)?;

    Ok((harness, faux))
}

/// Assemble the live harness against the builtin registry and its provider
/// stream (pi's print-mode wiring). Real HTTP is present only under
/// `native-http`; otherwise a turn surfaces a faithful provider-unavailable
/// error at `prompt()` time.
fn build_live_harness(
    provider: &str,
    model_id: &str,
    system_prompt: Option<String>,
) -> Result<AgentHarness, String> {
    let registry = builtin_models_registry();
    let model = registry
        .get_model(provider, model_id)
        .ok_or_else(|| format!("unknown model: {provider}/{model_id}"))?;
    let stream = provider_stream(registry.clone());

    build_harness(model, stream, registry, system_prompt)
}

/// Assemble a completion-only `AgentHarness`: the shared in-memory env, session
/// storage, and compaction wiring, parameterized only by the resolved `model`,
/// the provider `stream` seam, and the `registry` the compaction bridge wraps.
fn build_harness(
    model: Model,
    stream: ProviderStream,
    registry: Rc<Models>,
    system_prompt: Option<String>,
) -> Result<AgentHarness, String> {
    AgentHarness::new(AgentHarnessOptions {
        env: Box::new(MemoryExecutionEnv::new(DEFAULT_CWD)),
        session: AgentSession::new(Rc::new(InMemorySessionStorage::new())),
        models: Box::new(RegistryCompaction::new(registry)),
        stream,
        tools: None,
        resources: None,
        system_prompt: system_prompt.map(SystemPromptSource::Static),
        stream_options: None,
        model,
        thinking_level: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    })
    .map_err(|e| e.to_string())
}

/// The deterministic faux reply, mirroring pi's interactive faux turn text.
fn faux_reply(context: &Context) -> String {
    let prompt = last_user_text(context);
    format!(
        "Hello from the offline faux assistant.\n\n\
         You said: {prompt}\n\n\
         This turn streamed in with no network and no API key."
    )
}

/// The text of the most recent user message in the LLM-ready context.
fn last_user_text(context: &Context) -> String {
    for message in context.messages.iter().rev() {
        if let Message::User(user) = message {
            return match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
        }
    }
    String::new()
}

/// If a loop event is an assistant `text_delta` update, return its delta string.
///
/// The harness re-broadcasts each `message_update` carrying an
/// `assistantMessageEvent`; the `text_delta` variant holds the incremental
/// `delta` (pi's streaming text chunk).
fn text_delta_of(event: &pidgin_core::agent::types::AgentEvent) -> Option<String> {
    let value = serde_json::to_value(event).ok()?;
    if value.get("type").and_then(Value::as_str)? != "message_update" {
        return None;
    }
    let inner = value.get("assistantMessageEvent")?;
    if inner.get("type").and_then(Value::as_str)? != "text_delta" {
        return None;
    }
    inner
        .get("delta")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Concatenate the `text` blocks of an assistant message value (exactly as pi's
/// print mode extracts the final text).
fn extract_text(message: &Value) -> String {
    let mut out = String::new();
    if let Some(content) = message.get("content").and_then(Value::as_array) {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
    }
    out
}

/// Map a terminal `error`/`aborted` assistant message to `Err(errorMessage)`.
fn check_turn_error(message: &Value) -> Result<(), String> {
    let stop_reason = message.get("stopReason").and_then(Value::as_str);
    if stop_reason == Some("error") || stop_reason == Some("aborted") {
        let error_message = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Request {}", stop_reason.unwrap_or("failed")));
        return Err(error_message);
    }
    Ok(())
}
