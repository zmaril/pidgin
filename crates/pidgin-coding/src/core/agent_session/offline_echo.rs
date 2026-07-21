//! Offline / echo [`AgentSession`] builders — **pidgin host scaffolding**.
//!
//! This module is **not** a port of pi. It is coordinator-approved host
//! scaffolding that lifts the private `#[cfg(test)]` harness construction
//! (`super::test_support::create_harness`) into a plain `pub fn` built entirely
//! from **public** APIs, so consumers can stand up a ready-to-drive
//! [`AgentSession`] offline without re-deriving the faux-provider / faux-auth
//! wiring by hand.
//!
//! # This session is FAUX / OFFLINE — never wire it into a live model path
//!
//! The sessions built here run against a **faux** provider whose responses come
//! from an in-process [`StreamFn`] (an echo of the last user message, or a
//! caller-supplied canned script). They do **no** network I/O, hold **no** real
//! credential, and their model runtime is created with model-network access
//! disabled. The `faux` provider id, the `faux-1` model id, and the seeded
//! `faux-key` runtime key exist only to make the turn-runner's model/auth
//! preflight pass offline. Nothing here may be substituted for a real,
//! credentialed model path: the names are deliberately loud (`offline`, `echo`,
//! `faux`) so this is never mistaken for a production turn wire.
//!
//! # Consumers
//!
//! * The **TUI interactive shell**'s offline wire — it drives a live
//!   [`AgentSession`] end to end without contacting a provider.
//! * The **Python** and **sdk** cross-crate **e2e test** lanes — they need a
//!   deterministic, offline session that runs a real turn ([`build_faux_session`]
//!   supplies scripted responses for those assertions).
//!
//! # What it builds
//!
//! Exactly what `create_harness` builds, via public constructors: a real
//! [`Agent`] driven by a faux [`StreamFn`], an in-memory
//! [`SessionManager`](SessionManager::in_memory), a minimal
//! [`SettingsManager`](SettingsManager::create), an offline [`ModelRuntime`] with
//! the faux provider registered and its auth seeded, a
//! [`DefaultResourceLoader`], and the default
//! [`StubExtensionRunner`](crate::core::extensions::runner::StubExtensionRunner)
//! (via `extension_runner: None`).
//!
//! ## The faux-auth seed (the gotcha)
//!
//! `prompt()`'s preflight rejects a turn unless the selected model's provider
//! reports configured auth ([`ModelRuntime::has_configured_auth`]). Two public
//! calls arrange that here: [`ModelRuntime::register_provider`] makes `faux` a
//! known provider (with an api key in its config, the auth method the composer
//! requires), and [`ModelRuntime::set_runtime_api_key`] seeds the runtime
//! credential so the provider reads as configured. Miss either and the preflight
//! fails with "no api key found".

// This builder lifts the private `#[cfg(test)]` harness construction
// (`test_support::create_harness`) into public code, so its session/model wiring
// and the small faux helpers (`mock_stream`, `exhausted_response`, the message
// text extraction) necessarily clone the harness. The duplication cannot be
// shared away because `test_support` is test-only and this module is not.
// straitjacket-allow-file:duplication

use std::sync::{Arc, Mutex};

use pidgin_agent::agent::{Agent, AgentOptions, InitialAgentState};
use pidgin_agent::types::StreamFn;
use pidgin_ai::providers::faux::{faux_assistant_message, FauxAssistantOptions};
use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Message, Modality, Model,
    ModelCost, StopReason, StreamOptions, UserContent,
};

use crate::core::extensions::events::session::SessionStartEvent;
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPath};
use crate::core::provider_composer::{ExtensionModelConfig, ProviderConfigInput};
use crate::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions,
};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;

use super::runtime::{
    AgentSessionRuntimeFactoryOptions, AgentSessionRuntimeResult, CreateAgentSessionRuntimeFactory,
};
use super::session::{AgentSession, AgentSessionConfig};

/// The offline faux provider id. Present only to satisfy the offline preflight;
/// it must never name a real provider.
const FAUX_PROVIDER: &str = "faux";
/// The offline faux model id.
const FAUX_MODEL_ID: &str = "faux-1";
/// The offline faux base URL. Unreachable by construction — the faux
/// [`StreamFn`] answers in process, so no request is ever sent here.
const FAUX_BASE_URL: &str = "https://faux.test/v1";
/// The api dialect advertised by the faux provider/model.
const FAUX_API: &str = "openai-completions";
/// The placeholder runtime credential seeded so the preflight's configured-auth
/// check passes offline. Carries no secret.
const FAUX_API_KEY: &str = "faux-key";
/// The system prompt the offline session starts with.
const OFFLINE_SYSTEM_PROMPT: &str = "You are an offline echo assistant.";

/// A caller-supplied faux provider response for [`build_faux_session`], the
/// analog of the harness's scripted response step: either a fixed message or one
/// computed from the streaming [`Context`].
pub enum FauxResponse {
    /// A fixed message returned verbatim.
    Message(Box<AssistantMessage>),
    /// A message computed from the request context.
    #[allow(clippy::type_complexity)]
    Fn(Box<dyn Fn(&Context) -> AssistantMessage + Send + Sync>),
}

/// A failure building an offline session.
#[derive(Debug)]
pub enum OfflineEchoError {
    /// The offline faux provider could not be registered on the model runtime
    /// (carries the composer's message).
    ProviderRegistration(String),
}

impl std::fmt::Display for OfflineEchoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OfflineEchoError::ProviderRegistration(message) => {
                write!(f, "failed to register the offline faux provider: {message}")
            }
        }
    }
}

impl std::error::Error for OfflineEchoError {}

/// Build a ready-to-drive **offline echo** [`AgentSession`].
///
/// The default faux [`StreamFn`] echoes the text of the last user-role message
/// in each request context back as the assistant reply, so a `prompt("hello")`
/// turn produces an assistant message of `"hello"`. This is the simple default;
/// it delegates the (faux-auth-seeded) construction to the same builder
/// [`build_faux_session`] uses.
///
/// **Offline only.** See the module docs: the returned session is faux and must
/// not be wired into a live model path.
pub fn build_offline_echo_session(cwd: String) -> Result<AgentSession, OfflineEchoError> {
    let session_manager = SessionManager::in_memory(&cwd);
    build_offline_session(cwd, echo_stream_fn(), session_manager, None)
}

/// The default faux [`StreamFn`] shared by [`build_offline_echo_session`] and
/// [`build_offline_echo_runtime_factory`]: it echoes the text of the last
/// user-role message in each request context back as the assistant reply.
fn echo_stream_fn() -> StreamFn {
    Arc::new(
        |_model: &Model,
         context: &Context,
         _options: Option<&StreamOptions>,
         _signal: Option<&AbortSignal>| {
            let echoed = last_user_text(context);
            mock_stream(assistant_text(&echoed))
        },
    )
}

/// Build a [`CreateAgentSessionRuntimeFactory`] that stands up **offline echo**
/// [`AgentSession`]s for an [`AgentSessionRuntime`](super::runtime::AgentSessionRuntime).
///
/// Unlike [`build_offline_echo_session`] (which hardcodes an in-memory manager and
/// no `session_start` event), the returned factory threads the runtime-supplied
/// `session_manager` and `session_start_event` through to the built session, so the
/// runtime can drive `/new`, `/resume`, and `/fork` — each of which opens or
/// branches its own manager and carries a `session_start` metadata event — against
/// the offline echo assistant. The resolved `cwd` is echoed back and there is never
/// a model-fallback warning offline (`model_fallback_message` is always `None`).
///
/// The factory type is infallible, but the offline session build can only fail when
/// registering the fixed faux provider config — a build invariant, not a runtime
/// condition — so a failure there panics rather than being surfaced through the
/// runtime.
///
/// **Offline only.** See the module docs: every session the factory builds is faux
/// and must not be wired into a live model path.
pub fn build_offline_echo_runtime_factory() -> CreateAgentSessionRuntimeFactory {
    Box::new(|options: AgentSessionRuntimeFactoryOptions| {
        let cwd = options.cwd;
        let session = build_offline_session(
            cwd.clone(),
            echo_stream_fn(),
            options.session_manager,
            options.session_start_event,
        )
        .expect("offline echo faux provider registers");
        AgentSessionRuntimeResult {
            session,
            cwd,
            model_fallback_message: None,
        }
    })
}

/// Build a ready-to-drive **offline faux** [`AgentSession`] whose assistant
/// replies come from the caller-supplied `responses`, consumed in order (one per
/// turn). Once the list is exhausted the faux provider streams an error message,
/// matching the harness. Intended for the Python / sdk e2e lanes' deterministic
/// turns.
///
/// **Offline only.** See the module docs: the returned session is faux and must
/// not be wired into a live model path.
pub fn build_faux_session(
    cwd: String,
    responses: Vec<FauxResponse>,
) -> Result<AgentSession, OfflineEchoError> {
    let scripted: Arc<Mutex<(Vec<FauxResponse>, usize)>> = Arc::new(Mutex::new((responses, 0)));
    let stream_fn: StreamFn = Arc::new(
        move |_model: &Model,
              context: &Context,
              _options: Option<&StreamOptions>,
              _signal: Option<&AbortSignal>| {
            let message = {
                let mut guard = scripted.lock().unwrap();
                let (list, index) = &mut *guard;
                match list.get(*index) {
                    Some(FauxResponse::Message(message)) => {
                        *index += 1;
                        (**message).clone()
                    }
                    Some(FauxResponse::Fn(builder)) => {
                        let message = builder(context);
                        *index += 1;
                        message
                    }
                    None => exhausted_response(),
                }
            };
            mock_stream(message)
        },
    );
    let session_manager = SessionManager::in_memory(&cwd);
    build_offline_session(cwd, stream_fn, session_manager, None)
}

/// The shared offline construction every builder calls: wire `stream_fn` into a
/// real [`Agent`], the caller-supplied `session_manager`/`session_start_event`, an
/// in-memory settings manager, and an offline model runtime with the faux provider
/// registered and its auth seeded. The **only** fallible step is registering the
/// faux provider.
///
/// The `session_manager` and `session_start_event` are parameterized (rather than
/// hardcoded to [`SessionManager::in_memory`] / `None`) so the runtime factory
/// ([`build_offline_echo_runtime_factory`]) can hand the runtime-opened manager and
/// `session_start` metadata straight through for `/new`, `/resume`, and `/fork`.
/// The `agent_dir` is derived from `cwd` (`{cwd}/.agent`), matching the harness this
/// builder lifts.
fn build_offline_session(
    cwd: String,
    stream_fn: StreamFn,
    session_manager: SessionManager,
    session_start_event: Option<SessionStartEvent>,
) -> Result<AgentSession, OfflineEchoError> {
    let agent_dir = format!("{cwd}/.agent");
    let model_runtime = build_offline_model_runtime()?;

    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        ..Default::default()
    });

    let initial_state = InitialAgentState {
        system_prompt: Some(OFFLINE_SYSTEM_PROMPT.to_string()),
        model: Some(offline_faux_model()),
        thinking_level: None,
        tools: Some(Vec::new()),
        messages: None,
    };
    let agent = Agent::new(AgentOptions {
        initial_state: Some(initial_state),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let settings_manager = SettingsManager::create(&cwd, &agent_dir);

    Ok(AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd,
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime,
        initial_active_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        // `None` selects the default `StubExtensionRunner` (no extensions).
        extension_runner: None,
        session_start_event,
        summarization_models: None,
    }))
}

/// Build the offline [`ModelRuntime`]: model-network access disabled, no
/// `models.json` on disk, the faux provider registered from a public config, and
/// its runtime auth seeded so the turn preflight passes (see the module docs'
/// "faux-auth seed" note).
fn build_offline_model_runtime() -> Result<ModelRuntime, OfflineEchoError> {
    let mut runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: ModelsPath::Disabled,
        allow_model_network: Some(false),
        ..Default::default()
    });
    // Make `faux` a known provider. The api key in the config is the auth method
    // the composer requires; without it `register_provider` rejects the provider.
    runtime
        .register_provider(
            FAUX_PROVIDER,
            ProviderConfigInput {
                name: Some("Faux (offline)".to_string()),
                base_url: Some(FAUX_BASE_URL.to_string()),
                api_key: Some(FAUX_API_KEY.to_string()),
                api: Some(FAUX_API.to_string()),
                models: Some(vec![offline_faux_model_config()]),
                ..Default::default()
            },
        )
        .map_err(|error| OfflineEchoError::ProviderRegistration(error.0))?;
    // Seed the runtime credential so `has_configured_auth("faux")` is true and the
    // prompt preflight accepts the turn offline. This is the gotcha the harness
    // solves with `set_runtime_api_key("faux", "faux-key")`.
    runtime.set_runtime_api_key(FAUX_PROVIDER, FAUX_API_KEY);
    Ok(runtime)
}

/// The offline faux [`Model`] the agent starts with (its `provider` is the id the
/// preflight checks auth for).
fn offline_faux_model() -> Model {
    Model {
        id: FAUX_MODEL_ID.to_string(),
        name: FAUX_MODEL_ID.to_string(),
        api: FAUX_API.to_string(),
        provider: FAUX_PROVIDER.to_string(),
        base_url: FAUX_BASE_URL.to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: zero_cost(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

/// The faux model as a provider-registration config entry.
fn offline_faux_model_config() -> ExtensionModelConfig {
    ExtensionModelConfig {
        id: FAUX_MODEL_ID.to_string(),
        name: FAUX_MODEL_ID.to_string(),
        api: None,
        base_url: None,
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: zero_cost(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

/// A zeroed cost table for the free offline model.
fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

/// A plain-text assistant response.
fn assistant_text(text: &str) -> AssistantMessage {
    faux_assistant_message(
        vec![ContentBlock::Text {
            text: text.to_string(),
            text_signature: None,
        }],
        FauxAssistantOptions::default(),
        0,
    )
}

/// The error the faux provider streams once its scripted list is exhausted
/// (matching the harness / pi's faux provider).
fn exhausted_response() -> AssistantMessage {
    faux_assistant_message(
        Vec::new(),
        FauxAssistantOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("No more faux responses queued".to_string()),
            ..Default::default()
        },
        0,
    )
}

/// The text of the last user-role message in `context`, or `""` when there is
/// none. User content is either a bare string or a block list; text blocks are
/// joined with newlines.
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
                    .join("\n"),
            };
        }
    }
    String::new()
}

/// A [`StreamResult`] whose only event is the terminal `done`/`error` carrying
/// the final message.
fn mock_stream(message: AssistantMessage) -> StreamResult {
    let reason = message.stop_reason;
    let event = if matches!(reason, StopReason::Error | StopReason::Aborted) {
        AssistantMessageEvent::Error {
            reason,
            error: message.clone(),
        }
    } else {
        AssistantMessageEvent::Done {
            reason,
            message: message.clone(),
        }
    };
    StreamResult {
        events: vec![event],
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;

    use crate::core::agent_session::events::AgentSessionEvent;
    use crate::core::agent_session::runtime::{
        create_agent_session_runtime, ForkOptions, NewSessionOptions,
    };
    use crate::core::extensions::events::session::ForkPosition;

    /// A throwaway cwd for an offline session.
    fn temp_cwd() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().to_string();
        (dir, cwd)
    }

    /// The joined text of every `assistant`-role message persisted in the session.
    fn assistant_texts(session: &AgentSession) -> Vec<String> {
        session
            .messages()
            .iter()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .map(|message| match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            })
            .collect()
    }

    /// Whether a [`AgentSessionEvent::MessageEnd`] carrying an assistant message
    /// whose text equals `expected` was emitted.
    fn saw_assistant_message_end(events: &[AgentSessionEvent], expected: &str) -> bool {
        events.iter().any(|event| match event {
            AgentSessionEvent::MessageEnd { message } => {
                message.get("role").and_then(Value::as_str) == Some("assistant")
                    && message
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|blocks| {
                            blocks.iter().any(|block| {
                                block.get("text").and_then(Value::as_str) == Some(expected)
                            })
                        })
                        .unwrap_or(false)
            }
            _ => false,
        })
    }

    // Drive the whole thing through only the public surface, as an external
    // consumer would.

    #[test]
    fn echo_session_echoes_the_last_user_message() {
        let (_dir, cwd) = temp_cwd();
        let session = build_offline_echo_session(cwd).expect("build offline echo session");

        let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _unsubscribe = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            sink.lock().unwrap().push(event.clone());
        }));

        session
            .prompt("hello", None, None)
            .expect("offline echo turn runs");

        let recorded = events.lock().unwrap();
        assert!(
            saw_assistant_message_end(&recorded, "hello"),
            "expected a MessageEnd echoing \"hello\", saw: {recorded:?}"
        );
        assert!(
            assistant_texts(&session).iter().any(|text| text == "hello"),
            "expected the persisted assistant message to echo \"hello\", saw: {:?}",
            assistant_texts(&session)
        );
    }

    #[test]
    fn echo_session_preflight_accepts_the_turn() {
        // The faux-auth seed lets `prompt()`'s model/auth preflight pass offline:
        // a missing seed would surface as a `PromptError::Preflight`.
        let (_dir, cwd) = temp_cwd();
        let session = build_offline_echo_session(cwd).expect("build offline echo session");

        let result = session.prompt("does the preflight pass?", None, None);

        assert!(
            result.is_ok(),
            "preflight rejected the offline turn: {result:?}"
        );
    }

    #[test]
    fn faux_session_returns_the_canned_response() {
        let (_dir, cwd) = temp_cwd();
        let canned = "canned-offline-answer";
        let responses = vec![FauxResponse::Message(Box::new(assistant_text(canned)))];
        let session = build_faux_session(cwd, responses).expect("build faux session");

        let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _unsubscribe = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            sink.lock().unwrap().push(event.clone());
        }));

        session
            .prompt("anything at all", None, None)
            .expect("faux turn runs");

        let recorded = events.lock().unwrap();
        assert!(
            saw_assistant_message_end(&recorded, canned),
            "expected a MessageEnd carrying the canned response, saw: {recorded:?}"
        );
        assert!(
            assistant_texts(&session).iter().any(|text| text == canned),
            "expected the persisted assistant message to be the canned response, saw: {:?}",
            assistant_texts(&session)
        );
    }

    /// Drive an `AgentSessionRuntime` built from the offline-echo factory end to
    /// end: the initial session echoes a turn, `/new` rebuilds a working session
    /// that echoes again, and an in-memory `/fork` leaves a session that still
    /// echoes — confirming the factory is reused faithfully for every swap.
    #[test]
    fn runtime_factory_echoes_across_new_and_fork() {
        let (_dir, cwd) = temp_cwd();
        let session_manager = SessionManager::in_memory(&cwd);
        let mut runtime = create_agent_session_runtime(
            build_offline_echo_runtime_factory(),
            AgentSessionRuntimeFactoryOptions {
                cwd: cwd.clone(),
                agent_dir: format!("{cwd}/.agent"),
                session_manager,
                session_start_event: None,
            },
        )
        .expect("build offline echo runtime");

        // The initial session echoes the last user message.
        runtime
            .session()
            .prompt("hello", None, None)
            .expect("initial echo turn runs");
        assert!(
            assistant_texts(runtime.session())
                .iter()
                .any(|text| text == "hello"),
            "expected the initial session to echo \"hello\", saw: {:?}",
            assistant_texts(runtime.session())
        );

        // `/new` tears down the current session and the factory rebuilds a fresh,
        // working one that still echoes.
        let switch = runtime.new_session(NewSessionOptions::default());
        assert!(!switch.cancelled, "new_session was unexpectedly cancelled");
        assert!(
            runtime.session().messages().is_empty(),
            "expected the new session to start empty"
        );
        runtime
            .session()
            .prompt("world", None, None)
            .expect("post-/new echo turn runs");
        assert!(
            assistant_texts(runtime.session())
                .iter()
                .any(|text| text == "world"),
            "expected the rebuilt session to echo \"world\", saw: {:?}",
            assistant_texts(runtime.session())
        );

        // An in-memory `/fork` at the current leaf rebuilds another working
        // session that echoes.
        let leaf_id = runtime
            .session()
            .session_manager()
            .get_leaf_id()
            .map(String::from)
            .expect("forked-from session has a leaf entry");
        let fork = runtime
            .fork(
                &leaf_id,
                ForkOptions {
                    position: Some(ForkPosition::At),
                },
            )
            .expect("in-memory fork succeeds");
        assert!(!fork.cancelled, "fork was unexpectedly cancelled");
        runtime
            .session()
            .prompt("again", None, None)
            .expect("post-fork echo turn runs");
        assert!(
            assistant_texts(runtime.session())
                .iter()
                .any(|text| text == "again"),
            "expected the forked session to echo \"again\", saw: {:?}",
            assistant_texts(runtime.session())
        );
    }
}
