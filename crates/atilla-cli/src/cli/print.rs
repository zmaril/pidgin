//! Non-interactive print / json mode wiring.
//!
//! Mirrors the print-mode branch of pi's `main.ts`: build the model runtime,
//! resolve the model from CLI flags (or the settings/available-model fallback),
//! report resolution diagnostics, and — when a model resolves — assemble the
//! agent-session runtime and drive `runPrintMode`.
//!
//! The completion routes through the harness's `Provider` seam
//! ([`atilla_coding::modes::print::provider_stream`]). No native HTTP transport
//! exists in this workspace, so a real builtin model surfaces a provider-
//! unavailable error faithfully; the faux provider (which the conformance suite
//! drives) completes offline when selected. When no model resolves, pi's
//! byte-faithful `formatNoModelsAvailableMessage()` is emitted to stderr with
//! exit code 1 — the guard the `-p`/`--mode json` black-box cases (which pass
//! `--model missing-model`) exercise.

use atilla_coding::core::auth::auth_guidance::format_no_models_available_message;
use atilla_coding::core::model_resolver::{
    find_initial_model, resolve_cli_model, FindInitialModelOptions, ModelRuntimeView,
    ResolveCliModelOptions,
};
use atilla_coding::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use atilla_coding::core::session_manager::SessionManager;
use atilla_coding::core::settings_manager::SettingsManager;
use atilla_coding::core::skills::get_agent_dir;
use atilla_coding::modes::print::{
    build_harness, builtin_models_registry, run_print_mode, PrintModeOptions, PrintOutputMode,
};
use atilla_core::ai::types::Model;

use crate::cli::args::Args;
use crate::cli::output_guard::err_line;

/// A [`ModelRuntimeView`] adapter over a live [`ModelRuntime`], bridging the
/// resolver's read-only seam to the runtime's snapshot accessors.
struct RuntimeView<'a>(&'a ModelRuntime);

impl ModelRuntimeView for RuntimeView<'_> {
    fn get_models(&self) -> Vec<Model> {
        self.0.get_models(None)
    }

    fn get_available(&self) -> Vec<Model> {
        self.0.get_available_snapshot().to_vec()
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.0.get_model(provider, model_id)
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        self.0.has_configured_auth(provider)
    }
}

/// Resolve the CLI model, mirroring `buildSessionOptions` + `createAgentSession`'s
/// `findInitialModel`. Emits resolution diagnostics to stderr exactly as pi's
/// `reportDiagnostics` (`Error: …` / `Warning: …`). Returns `Err(exit_code)` on a
/// resolution error, `Ok(None)` when no model resolves, `Ok(Some(model))`
/// otherwise.
fn resolve_model(
    parsed: &Args,
    runtime: &ModelRuntime,
    settings: &SettingsManager,
) -> Result<Option<Model>, i32> {
    let view = RuntimeView(runtime);

    // 1. CLI `--model` (with optional `--provider`).
    if parsed.model.is_some() {
        let resolved = resolve_cli_model(
            ResolveCliModelOptions {
                cli_provider: parsed.provider.as_deref(),
                cli_model: parsed.model.as_deref(),
                cli_thinking: None,
            },
            &view,
        );
        if let Some(warning) = &resolved.warning {
            err_line(&format!("Warning: {warning}"));
        }
        if let Some(error) = &resolved.error {
            err_line(&format!("Error: {error}"));
            return Err(1);
        }
        if let Some(model) = resolved.model {
            return Ok(Some(model));
        }
    }

    // 2. Settings default / first available model with valid auth.
    let result = find_initial_model(
        FindInitialModelOptions {
            cli_provider: None,
            cli_model: None,
            scoped_models: Vec::new(),
            is_continuing: false,
            default_provider: settings.get_default_provider().as_deref(),
            default_model_id: settings.get_default_model().as_deref(),
            default_thinking_level: None,
        },
        &view,
    );
    match result {
        Ok(result) => Ok(result.model),
        Err(error) => {
            err_line(&format!("Error: {error}"));
            Err(1)
        }
    }
}

/// Run non-interactive print / json mode. Returns the process exit code.
pub fn run_print_or_json(parsed: &Args, session_manager: &SessionManager, json: bool) -> i32 {
    let cwd = session_manager.get_cwd().to_string();
    let agent_dir = get_agent_dir();

    // Build the model runtime offline (no network). The builtin catalog is the
    // resolution source; auth (if any) is read from the default auth store.
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        allow_model_network: Some(false),
        ..CreateModelRuntimeOptions::default()
    });
    let settings = SettingsManager::create(&cwd, &agent_dir);

    let model = match resolve_model(parsed, &runtime, &settings) {
        Ok(Some(model)) => model,
        Ok(None) => {
            // pi's non-interactive no-model guard (`main.ts:800`).
            err_line(&format_no_models_available_message());
            return 1;
        }
        Err(code) => return code,
    };

    // Assemble the agent-session runtime and drive the completion.
    let harness = match build_harness(model, &cwd, builtin_models_registry()) {
        Ok(harness) => harness,
        Err(error) => {
            err_line(&format!("Error: {error}"));
            return 1;
        }
    };

    // Split the CLI messages into the initial message and the rest, mirroring
    // pi's `buildInitialMessage` (which shifts `messages[0]` into the initial
    // prompt). @file / stdin combination is not ported.
    let mut messages = parsed.messages.clone();
    let initial_message = if messages.is_empty() {
        None
    } else {
        Some(messages.remove(0))
    };

    let header = if json {
        session_manager
            .get_header()
            .and_then(|header| serde_json::to_value(header).ok())
    } else {
        None
    };

    let options = PrintModeOptions {
        mode: if json {
            PrintOutputMode::Json
        } else {
            PrintOutputMode::Text
        },
        messages,
        initial_message,
    };

    run_print_mode(&harness, header.as_ref(), &options)
}
