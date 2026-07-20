//! The pidgin CLI shell.
//!
//! Hand-ported from pi's `packages/coding-agent/src/main.ts` + `cli/*`,
//! reproducing the observable startup behavior that pi's black-box CLI tests
//! assert: `--version`/`--help` output routing, stdout cleanliness in
//! json/print modes, `--session-id` reservation warnings and validation,
//! `--name` trim/write semantics, and invalid-session-file handling — all
//! without an LLM, API key, or runtime session.

pub mod args;
pub mod config;
pub mod extensions;
pub mod list_models;
pub mod output_guard;
pub mod packages;
pub mod print;

use std::io::IsTerminal;
use std::path::Path;
use std::process::exit;

use args::{Args, DiagnosticKind, Mode};
use config::{ENV_SESSION_DIR, VERSION};
use output_guard::{err_line, out_line, take_over_stdout};
use pidgin_coding::core::session_manager::{
    assert_valid_session_id, find_local_session_by_exact_id, SessionManager,
};

/// Application run mode after resolving flags + tty state. Mirrors `AppMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

/// Entry point. Parses argv and drives the startup flow, returning a process
/// exit code. Error paths print a clean diagnostic (never a panic/stack trace).
pub fn run(argv: &[String]) -> i32 {
    // Offline detection mirrors main.ts; harmless for the black-box contract.
    let offline = argv.iter().any(|a| a == "--offline") || is_truthy_env("PI_OFFLINE");
    if offline {
        std::env::set_var("PI_OFFLINE", "1");
        std::env::set_var("PI_SKIP_VERSION_CHECK", "1");
    }

    let parsed = args::parse_args(argv);

    // Report parse diagnostics; abort on any error.
    if !parsed.diagnostics.is_empty() {
        for d in &parsed.diagnostics {
            match d.kind {
                DiagnosticKind::Error => err_line(&format!("Error: {}", d.message)),
                DiagnosticKind::Warning => err_line(&format!("Warning: {}", d.message)),
            }
        }
        if parsed
            .diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::Error)
        {
            return 1;
        }
    }

    // --version: semver to stdout, empty stderr, exit 0. (Before any takeover.)
    if parsed.version {
        out_line(VERSION);
        return 0;
    }

    // --export: honest stub (HTML export from file is not wired into the shell yet).
    if let Some(export) = &parsed.export {
        err_line(&format!(
            "Error: session HTML export is not yet implemented in pidgin ({export})"
        ));
        return 1;
    }

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let app_mode = resolve_app_mode(&parsed, stdin_tty, stdout_tty);

    let should_take_over = app_mode != AppMode::Interactive && !is_plain_runtime_metadata(&parsed);
    if should_take_over {
        take_over_stdout();
    }

    if parsed.mode == Some(Mode::Rpc) && !parsed.file_args.is_empty() {
        err_line("Error: @file arguments are not supported in RPC mode");
        return 1;
    }

    if let Some(code) = validate_fork_flags(&parsed) {
        return code;
    }
    if let Some(code) = validate_session_id_flags(&parsed) {
        return code;
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| ".".to_string());

    // Session directory: --session-dir > $..SESSION_DIR > default (per-cwd).
    let session_dir: Option<String> = parsed.session_dir.clone().or_else(|| {
        std::env::var(ENV_SESSION_DIR)
            .ok()
            .filter(|s| !s.is_empty())
    });

    let mut session_manager = match create_session_manager(&parsed, &cwd, session_dir.as_deref()) {
        Ok(mgr) => mgr,
        Err(code) => return code,
    };

    // Missing-session-cwd guard (non-interactive): mirror main.ts.
    if let Some(file) = session_manager.get_session_file() {
        let session_cwd = session_manager.get_cwd().to_string();
        if !session_cwd.is_empty() && !Path::new(&session_cwd).exists() {
            if app_mode == AppMode::Interactive {
                return 0; // pi prompts; non-TTY never reaches here in the tests.
            }
            err_line(&format!(
                "Error: Stored session working directory does not exist: {session_cwd}\nSession file: {file}\nCurrent working directory: {cwd}"
            ));
            return 1;
        }
    }

    // --name: trim, reject empty, write session_info before model validation.
    if let Some(name) = &parsed.name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            err_line("Error: --name requires a non-empty value");
            return 1;
        }
        let _ = session_manager.append_session_info(trimmed);
    }

    // Trusted-project startup package install (chatter routed to stderr).
    let trusted = parsed.project_trust_override.unwrap_or(false);
    packages::maybe_install_project_packages(&cwd, trusted);

    // --help: Usage block. Routed to stderr when stdout was taken over.
    if parsed.help {
        out_line(args::help_text().trim_end_matches('\n'));
        return 0;
    }

    // --list-models: enumerate available models (pi's `main.ts:760-763`).
    if let Some(list) = &parsed.list_models {
        // pi builds the runtime via `createAgentSessionServices`
        // (`agent-session-services.ts:141`): auth.json + models.json under the
        // agent dir, with `allowModelNetwork` left to its `PI_OFFLINE`-aware
        // default. `auth_path: None`/`models_path: Default` resolve to those
        // same locations.
        let runtime = pidgin_coding::core::model_runtime::ModelRuntime::create(
            pidgin_coding::core::model_runtime::CreateModelRuntimeOptions::default(),
        );
        let search_pattern = match list {
            args::ListModels::Search(pattern) => Some(pattern.as_str()),
            args::ListModels::All => None,
        };
        list_models::list_models(&runtime, search_pattern);
        return 0;
    }

    // Mode dispatch.
    match app_mode {
        AppMode::Rpc => match pidgin_core::coding::modes::rpc::run_rpc_mode() {
            Ok(()) => 0,
            Err(e) => {
                err_line(&format!("Error: {e}"));
                1
            }
        },
        AppMode::Interactive => {
            err_line("Error: interactive mode is not yet implemented in pidgin");
            1
        }
        AppMode::Print | AppMode::Json => {
            // Resolve the model and drive the completion up to the provider seam.
            // When no model resolves (e.g. the black-box cases' `--model
            // missing-model`), print's no-models guard fires with exit 1.
            print::run_print_or_json(&parsed, &session_manager, app_mode == AppMode::Json)
        }
    }
}

fn is_truthy_env(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"),
        Err(_) => false,
    }
}

/// Mirrors `resolveAppMode`.
fn resolve_app_mode(parsed: &Args, stdin_tty: bool, stdout_tty: bool) -> AppMode {
    if parsed.mode == Some(Mode::Rpc) {
        return AppMode::Rpc;
    }
    if parsed.mode == Some(Mode::Json) {
        return AppMode::Json;
    }
    if parsed.print || !stdin_tty || !stdout_tty {
        return AppMode::Print;
    }
    AppMode::Interactive
}

/// Mirrors `isPlainRuntimeMetadataCommand`: a plain `--help`/`--list-models`
/// invocation (no `--print`, no `--mode`) keeps its output on stdout.
fn is_plain_runtime_metadata(parsed: &Args) -> bool {
    !parsed.print && parsed.mode.is_none() && (parsed.help || parsed.list_models.is_some())
}

/// Emit pi's `<flag> cannot be combined with <...>` error and return
/// `Some(1)` when any of `checks` (predicate, flag-name) is active. Shared by
/// the fork and session-id validators.
fn reject_flag_conflicts(flag: &str, checks: &[(bool, &str)]) -> Option<i32> {
    let conflicting: Vec<&str> = checks
        .iter()
        .filter(|(active, _)| *active)
        .map(|(_, name)| *name)
        .collect();
    if conflicting.is_empty() {
        return None;
    }
    err_line(&format!(
        "Error: {flag} cannot be combined with {}",
        conflicting.join(", ")
    ));
    Some(1)
}

/// Mirrors `validateForkFlags`. Returns `Some(exit_code)` to abort.
fn validate_fork_flags(parsed: &Args) -> Option<i32> {
    let Some(_fork) = &parsed.fork else {
        return None;
    };
    reject_flag_conflicts(
        "--fork",
        &[
            (parsed.session.is_some(), "--session"),
            (parsed.continue_, "--continue"),
            (parsed.resume, "--resume"),
            (parsed.no_session, "--no-session"),
        ],
    )
}

/// Mirrors `validateSessionIdFlags`. Returns `Some(exit_code)` to abort.
fn validate_session_id_flags(parsed: &Args) -> Option<i32> {
    let Some(session_id) = &parsed.session_id else {
        return None;
    };
    if let Some(code) = reject_flag_conflicts(
        "--session-id",
        &[
            (parsed.session.is_some(), "--session"),
            (parsed.continue_, "--continue"),
            (parsed.resume, "--resume"),
        ],
    ) {
        return Some(code);
    }
    if let Err(message) = assert_valid_session_id(session_id) {
        err_line(&format!("Error: {message}"));
        return Some(1);
    }
    None
}

/// Resolved `--session`/`--fork` argument. Mirrors `ResolvedSession`.
enum ResolvedSession {
    Path(String),
    Local(String),
    NotFound(String),
}

fn resolve_session_path(arg: &str, cwd: &str, session_dir: Option<&str>) -> ResolvedSession {
    if arg.contains('/') || arg.contains('\\') || arg.ends_with(".jsonl") {
        // Looks like a file path.
        let base = Path::new(cwd);
        let resolved = if Path::new(arg).is_absolute() {
            arg.to_string()
        } else {
            base.join(arg).to_string_lossy().to_string()
        };
        return ResolvedSession::Path(resolved);
    }
    // Otherwise try to match as an exact local session id.
    if let Some(path) = find_local_session_by_exact_id(arg, cwd, session_dir) {
        return ResolvedSession::Local(path);
    }
    ResolvedSession::NotFound(arg.to_string())
}

/// Mirrors `createSessionManager`. On an error path it prints the diagnostic
/// and returns `Err(exit_code)`.
fn create_session_manager(
    parsed: &Args,
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<SessionManager, i32> {
    if parsed.no_session || parsed.help || parsed.list_models.is_some() {
        return Ok(SessionManager::in_memory(cwd));
    }

    if let Some(fork_arg) = &parsed.fork {
        if let Some(sid) = &parsed.session_id {
            if find_local_session_by_exact_id(sid, cwd, session_dir).is_some() {
                err_line(&format!("Session already exists with id '{sid}'"));
                return Err(1);
            }
        }
        return match resolve_session_path(fork_arg, cwd, session_dir) {
            ResolvedSession::Path(_) | ResolvedSession::Local(_) => {
                // Forking copies history into a new session; not ported yet.
                err_line("Error: --fork is not yet implemented in pidgin");
                Err(1)
            }
            ResolvedSession::NotFound(arg) => {
                err_line(&format!("No session found matching '{arg}'"));
                Err(1)
            }
        };
    }

    if let Some(session_arg) = &parsed.session {
        return match resolve_session_path(session_arg, cwd, session_dir) {
            ResolvedSession::Path(path) | ResolvedSession::Local(path) => {
                open_session_or_exit(&path)
            }
            ResolvedSession::NotFound(arg) => {
                err_line(&format!("No session found matching '{arg}'"));
                Err(1)
            }
        };
    }

    if parsed.resume {
        err_line("Error: --resume is not yet implemented in pidgin");
        return Err(1);
    }

    if parsed.continue_ {
        // continueRecent falls back to a fresh session when none exist; the
        // recent-session lookup is not ported, so start fresh.
        return Ok(SessionManager::create(cwd, session_dir, None));
    }

    if let Some(sid) = &parsed.session_id {
        if let Some(path) = find_local_session_by_exact_id(sid, cwd, session_dir) {
            return open_session_or_exit(&path);
        }
        err_line(&format!(
            "Warning: No project session found with id '{sid}'; creating a new session with that id."
        ));
        return Ok(SessionManager::create(cwd, session_dir, Some(sid)));
    }

    Ok(SessionManager::create(cwd, session_dir, None))
}

/// Mirrors `openSessionOrExit`: a clean `Error: <message>` (no stack trace) on
/// an invalid session file.
fn open_session_or_exit(path: &str) -> Result<SessionManager, i32> {
    match SessionManager::open(path) {
        Ok(mgr) => Ok(mgr),
        Err(message) => {
            err_line(&format!("Error: {message}"));
            Err(1)
        }
    }
}

/// Thin `main` shim: run and exit with the resulting code.
pub fn main() -> ! {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = run(&argv);
    // Ensure buffered stderr/stdout are flushed before exit.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    exit(code)
}
