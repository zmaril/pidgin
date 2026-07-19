//! `modes/print` — single-shot print mode.
//!
//! Faithful port of pi's `packages/coding-agent/src/modes/print-mode.ts`
//! (`runPrintMode`). Print mode sends one or more prompts to the agent session
//! and writes the result, then exits:
//!
//! - `pi -p "prompt"` → text output (the final assistant response).
//! - `pi --mode json "prompt"` → a JSON event stream.
//!
//! # Seam boundary
//!
//! [`run_print_mode`] drives an already-assembled [`AgentHarness`]. Calling
//! [`AgentHarness::prompt`] is **the provider/model completion call**: it runs
//! the agent turn, which routes through the harness's
//! [`ProviderStream`](pidgin_agent::harness::options::ProviderStream) seam to a
//! [`Provider`](pidgin_ai::seams::Provider). [`provider_stream`] builds that
//! seam so a registered faux provider (pi's `registerFauxProvider`, the
//! provider the conformance suite drives) completes **offline**, while a real
//! builtin model — which has no native HTTP transport in this workspace — falls
//! through to the builtin registry and surfaces a provider-unavailable error
//! faithfully (a terminal `error` assistant message).
//!
//! # Deviations from pi
//!
//! - **Session runtime**: pi drives an `AgentSession` wrapping a `SessionManager`
//!   plus a resource loader; the harness-backed session here carries the
//!   conversation and the provider seam, which is what the completion path
//!   needs. Extension binding / signal-handler lifecycle (`bindExtensions`,
//!   `registerSignalHandlers`, `disposeRuntime`) have no pidgin analogue yet and
//!   are omitted.
//! - **JSON event stream**: pi writes every `session.subscribe` event as a JSON
//!   line. The harness re-broadcasts loop [`AgentEvent`]s (which serialize to
//!   pi's exact `{ "type": … }` shapes) and its own harness events. Only the
//!   loop events are serialized here; faithful serialization of the harness
//!   own-event union (`AgentHarnessOwnEvent`, which is not a plain serde enum) is
//!   a follow-up. Text mode is fully faithful.

use std::io::Write;
use std::rc::Rc;

use serde_json::Value;

use pidgin_agent::harness::agent_harness::{AgentHarness, AgentHarnessEvent};
use pidgin_agent::harness::env::MemoryExecutionEnv;
use pidgin_agent::harness::options::{AgentHarnessError, AgentHarnessOptions, ProviderStream};
use pidgin_agent::harness::session::{InMemorySessionStorage, Session};
use pidgin_agent::{CompletionOptions, Models as CompactionModels};
use pidgin_ai::providers::registry::{create_models, Models as AiModels, MutableModels};
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::types::{AssistantMessage, Context, Model};

/// Output mode for print mode. Mirrors pi's `PrintModeOptions.mode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrintOutputMode {
    /// `-p` / `--print`: emit the final assistant text.
    Text,
    /// `--mode json`: emit the header and a JSON event stream.
    Json,
}

/// Options for [`run_print_mode`], mirroring pi's `PrintModeOptions`.
pub struct PrintModeOptions {
    /// Output mode.
    pub mode: PrintOutputMode,
    /// Additional prompts sent after `initial_message`.
    pub messages: Vec<String>,
    /// First message to send (may already contain `@file` content).
    pub initial_message: Option<String>,
}

/// Write a line (text + `\n`) to real stdout (fd 1), bypassing the CLI's soft
/// output guard exactly as pi's `writeRawStdout` bypasses the `console.log`
/// takeover. Only the structured print payload reaches stdout.
fn write_raw_stdout(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.write_all(b"\n");
    let _ = lock.flush();
}

/// Write a line to stderr. The equivalent of pi's `console.error`.
fn write_stderr(text: &str) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.write_all(b"\n");
    let _ = lock.flush();
}

/// Run print (single-shot) mode against an assembled harness. Returns the
/// process exit code (0 on success; 1 when the final assistant message is an
/// error/abort, or a prompt fails). Mirrors pi's `runPrintMode`.
pub fn run_print_mode(
    harness: &AgentHarness,
    header: Option<&Value>,
    options: &PrintModeOptions,
) -> i32 {
    // JSON mode writes the session header line first (pi: before rebindSession).
    if options.mode == PrintOutputMode::Json {
        if let Some(header) = header {
            if let Ok(line) = serde_json::to_string(header) {
                write_raw_stdout(&line);
            }
        }
    }

    // JSON mode subscribes to the event stream, writing each loop event as a
    // JSON line (pi: `session.subscribe((event) => writeRawStdout(...))`).
    let _subscription = if options.mode == PrintOutputMode::Json {
        Some(harness.subscribe(Rc::new(
            move |event: &AgentHarnessEvent, _signal: Option<&AbortSignal>| {
                if let AgentHarnessEvent::Loop(loop_event) = event {
                    if let Ok(line) = serde_json::to_string(loop_event) {
                        write_raw_stdout(&line);
                    }
                }
            },
        )))
    } else {
        None
    };

    // Send prompts. THIS is the provider/model completion call.
    let mut last: Option<Result<Value, String>> = None;
    if let Some(initial) = &options.initial_message {
        last = Some(harness.prompt(initial, None).map_err(|e| e.to_string()));
    }
    for message in &options.messages {
        last = Some(harness.prompt(message, None).map_err(|e| e.to_string()));
    }

    if options.mode == PrintOutputMode::Text {
        let message = match last {
            // A prompt threw (pi's `catch`): print the message, exit 1.
            Some(Err(message)) => {
                write_stderr(&message);
                return 1;
            }
            Some(Ok(message)) => message,
            None => return 0,
        };

        // Only an assistant message produces text output (pi's `role` check).
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return 0;
        }

        let stop_reason = message.get("stopReason").and_then(Value::as_str);
        if stop_reason == Some("error") || stop_reason == Some("aborted") {
            let error_message = message
                .get("errorMessage")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Request {}", stop_reason.unwrap_or_default()));
            write_stderr(&error_message);
            return 1;
        }

        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        write_raw_stdout(text);
                    }
                }
            }
        }
    }

    0
}

/// Build a [`Models`](AiModels) registry populated with the builtin providers.
///
/// Under the `native-http` feature (the shipped CLI binary's default) the
/// builtins are bound to a live reqwest transport via
/// [`builtin_providers_with_transport`](pidgin_ai::builtin_providers_with_transport),
/// so a model whose dialect has an adapter (e.g. anthropic) routes through
/// `ApiRouting::Single` over real HTTP; a turn with configured auth reaches the
/// provider, and an unconfigured one surfaces a clean not-configured error.
///
/// Without the feature the builtins have no transport, so every builtin model
/// routes via `ApiRouting::Unimplemented`; a stream attempt yields the faithful
/// provider-unavailable error rather than a network call.
pub fn builtin_models_registry() -> Rc<AiModels> {
    let mut models = create_models();
    for provider in builtin_registry_providers() {
        models.set_provider(provider);
    }
    Rc::new(models)
}

/// The builtin providers wired into the registry, with a live transport bound
/// when `native-http` is enabled.
#[cfg(feature = "native-http")]
fn builtin_registry_providers() -> Vec<pidgin_ai::RegistryProvider> {
    use std::sync::Arc;

    use pidgin_ai::seams::clock::{Clock, SystemClock};
    use pidgin_ai::seams::http::HttpTransport;
    use pidgin_ai::seams::ReqwestTransport;

    // Default builder: honor the ambient proxy (no `.no_proxy()`, which is a
    // loopback-test-only bypass). The production clock reads real wall time.
    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::builder().build());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    pidgin_ai::builtin_providers_with_transport(transport, clock)
}

/// The builtin providers with no transport bound: every model routes
/// `Unimplemented`, matching the reqwest-free default build.
#[cfg(not(feature = "native-http"))]
fn builtin_registry_providers() -> Vec<pidgin_ai::RegistryProvider> {
    pidgin_ai::builtin_providers()
}

/// Build the harness's [`ProviderStream`] seam.
///
/// Routing mirrors pi's `stream` dispatch: a request first tries the process
/// api registry (pi's `compat.stream`, populated by `registerFauxProvider`), so
/// a faux-api model completes offline. When no api provider is registered for
/// the model's api (the real-builtin case, since no native transport is
/// ported), it falls through to the builtin `registry`, whose `Unimplemented`
/// routing / unconfigured auth surfaces a terminal `error` message.
pub fn provider_stream(registry: Rc<AiModels>) -> ProviderStream {
    Rc::new(
        move |req| match pidgin_ai::compat::stream(req.model, req.context, None, req.signal) {
            Ok(result) => result,
            Err(_) => registry.stream(req.model, req.context, None, req.signal),
        },
    )
}

/// The compaction/branch-summary [`Models`](CompactionModels) bridge the harness
/// needs. Compaction is not reached by a single-shot text turn, but the harness
/// requires the seam; it routes through the same provider dispatch as
/// [`provider_stream`].
pub struct RegistryCompaction {
    registry: Rc<AiModels>,
}

impl RegistryCompaction {
    /// Wrap a builtin registry for compaction completions.
    pub fn new(registry: Rc<AiModels>) -> Self {
        Self { registry }
    }
}

impl CompactionModels for RegistryCompaction {
    fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        _options: &CompletionOptions,
    ) -> AssistantMessage {
        match pidgin_ai::compat::stream(model, context, None, None) {
            Ok(result) => result.message,
            Err(_) => self.registry.complete_simple(model, context, None, None),
        }
    }
}

/// Assemble the agent-session harness that print mode drives. `model` is the
/// resolved model; `cwd` is the execution environment root; `registry` is the
/// builtin `Models` collection wired into the compaction and provider seams.
///
/// The harness carries the conversation in an in-memory session and the
/// `Provider` seam ([`provider_stream`]); tools, resources, and a system prompt
/// are not attached (a single-shot completion reaches the provider before any
/// tool use).
pub fn build_harness(
    model: Model,
    cwd: &str,
    registry: Rc<AiModels>,
) -> Result<AgentHarness, AgentHarnessError> {
    AgentHarness::new(AgentHarnessOptions {
        env: Box::new(MemoryExecutionEnv::new(cwd)),
        session: Session::new(Rc::new(InMemorySessionStorage::new())),
        models: Box::new(RegistryCompaction::new(registry.clone())),
        stream: provider_stream(registry),
        tools: None,
        resources: None,
        system_prompt: None,
        stream_options: None,
        model,
        thinking_level: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    })
}

#[cfg(test)]
mod tests;
