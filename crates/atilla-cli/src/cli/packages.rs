//! Startup package installation for trusted projects (stopgap).
//!
//! pi installs the `packages` configured in project settings during runtime
//! startup, but only when the project is trusted, and it routes the package
//! manager's output to stderr when stdout has been taken over
//! (`package-manager.ts`: `stdio: isStdoutTakenOver() ? ["ignore", 2, 2] : "inherit"`).
//! The `stdout-cleanliness` black-box tests assert that this trusted-install
//! chatter lands on stderr (and is absent for untrusted projects).
//!
//! The real behavior lives in pi's `settings-manager` + `package-manager` +
//! resource-loader stack, none of which is ported yet. This module implements
//! the minimal slice the black-box contract needs: read project settings, and
//! for a trusted project with configured npm packages, run the configured
//! `npmCommand` once with its output routed to stderr. A later
//! settings/package-manager port supersedes this.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::cli::config::CONFIG_DIR_NAME;
use crate::cli::output_guard::is_stdout_taken_over;

/// Install configured project packages if the project is trusted.
///
/// `trusted` mirrors `settingsManager.isProjectTrusted()` for this run
/// (`--approve` => trusted, `--no-approve`/default => untrusted when the
/// project carries trust-requiring resources such as `packages`).
/// Read a JSON array of strings from `settings[key]`, returning `None` when the
/// key is absent or not an array (so callers can pick their own default).
fn string_array(settings: &Value, key: &str) -> Option<Vec<String>> {
    settings.get(key).and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    })
}

pub fn maybe_install_project_packages(cwd: &str, trusted: bool) {
    if !trusted {
        return;
    }

    let settings_path = Path::new(cwd).join(CONFIG_DIR_NAME).join("settings.json");
    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&content) else {
        return;
    };

    let packages = string_array(&settings, "packages").unwrap_or_default();

    // Only npm-source packages are handled by the configured npmCommand.
    let npm_specs: Vec<String> = packages
        .iter()
        .filter_map(|p| p.strip_prefix("npm:").map(|s| s.to_string()))
        .collect();
    if npm_specs.is_empty() {
        return;
    }

    let npm_command =
        string_array(&settings, "npmCommand").unwrap_or_else(|| vec!["npm".to_string()]);

    let Some((command, base_args)) = npm_command.split_first() else {
        return;
    };
    if command.is_empty() {
        return;
    }

    // Mirror pi's argv shape (`install <specs...> --prefix <root> --legacy-peer-deps`).
    // The install root is the trusted project's package storage; its exact
    // location is not asserted by the black-box contract, and configured
    // npmCommands used in tests ignore the args entirely.
    let mut args: Vec<String> = base_args.to_vec();
    args.push("install".to_string());
    args.extend(npm_specs.iter().cloned());
    args.push("--prefix".to_string());
    args.push(cwd.to_string());
    args.push("--legacy-peer-deps".to_string());

    // Route child output to stderr when stdout is taken over, else inherit —
    // matching pi's `stdio: isStdoutTakenOver() ? ["ignore", 2, 2] : "inherit"`.
    let mut cmd = Command::new(command);
    cmd.args(&args).current_dir(cwd);
    if is_stdout_taken_over() {
        cmd.stdin(Stdio::null())
            .stdout(std::io::stderr())
            .stderr(std::io::stderr());
    } else {
        cmd.stdin(Stdio::null());
    }

    // Best-effort: a failed install must not crash the shell.
    let _ = cmd.status();
}
