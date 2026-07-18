//! Package-manager config, install roots, and argv/command-request helpers.
//!
//! Ports pi's `getNpmCommand` / `getPackageManagerName` / `getNpmInstallRoot` /
//! `getNpmInstallArgs` / `getGitDependencyInstallArgs`, plus the small
//! command-request builders shared by the npm and git machines.

use atilla_ai::seams::subprocess::CommandRequest;
use std::path::PathBuf;

/// Network timeout applied to capturing git/npm probes, mirroring pi's
/// `NETWORK_TIMEOUT_MS`.
pub const NETWORK_TIMEOUT_MS: u64 = 10_000;

/// pi's `CONFIG_DIR_NAME` default (`packages/coding-agent/src/config.ts`).
pub const CONFIG_DIR_NAME: &str = ".pi";

/// Which managed install root an operation targets, mirroring pi's non-temporary
/// `SourceScope` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    /// The user-global root under the agent dir (`<agentDir>/npm`).
    User,
    /// The project-local root under the project config dir (`<cwd>/.pi/npm`).
    Project,
}

/// The resolved package-manager command: a program plus fixed leading args.
///
/// Mirrors pi's `getNpmCommand()` return `{ command, args }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmCommand {
    /// The program to invoke (`npm`, `mise`, ...).
    pub command: String,
    /// Fixed leading arguments (e.g. `["exec", "node@20", "--", "npm"]`).
    pub args: Vec<String>,
}

/// Config the command argv depends on, mirroring pi's package-manager options
/// plus `settingsManager.getNpmCommand()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManagerConfig {
    /// The working directory (pi's `options.cwd`, already resolved).
    pub cwd: String,
    /// The agent dir (pi's `options.agentDir`, already resolved).
    pub agent_dir: String,
    /// The configured npm command argv (pi's `settings.npmCommand`); `None`
    /// falls back to bare `npm`.
    pub npm_command: Option<Vec<String>>,
}

pub(crate) fn join_path(base: &str, parts: &[&str]) -> String {
    let mut path = PathBuf::from(base);
    for part in parts {
        path.push(part);
    }
    path.to_string_lossy().into_owned()
}

/// Strip a trailing `.cmd` / `.exe` (case-insensitive), mirroring pi's
/// `getPackageManagerName` suffix trim.
fn strip_executable_suffix(name: &str) -> &str {
    for suffix in [".cmd", ".exe"] {
        if name.len() >= suffix.len()
            && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            return &name[..name.len() - suffix.len()];
        }
    }
    name
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl PackageManagerConfig {
    /// Build a config from the three inputs pi injects.
    pub fn new(
        cwd: impl Into<String>,
        agent_dir: impl Into<String>,
        npm_command: Option<Vec<String>>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            npm_command,
        }
    }

    /// pi's `getNpmCommand()`: split the configured argv into program + args, or
    /// fall back to bare `npm`.
    pub fn npm_command(&self) -> NpmCommand {
        match &self.npm_command {
            Some(argv) if !argv.is_empty() => NpmCommand {
                command: argv[0].clone(),
                args: argv[1..].to_vec(),
            },
            _ => NpmCommand {
                command: "npm".to_string(),
                args: Vec::new(),
            },
        }
    }

    /// Whether an npm command is explicitly configured (pi's non-empty
    /// `settings.npmCommand`), which selects plain `install` for git deps.
    pub fn npm_configured(&self) -> bool {
        self.npm_command
            .as_ref()
            .is_some_and(|argv| !argv.is_empty())
    }

    /// pi's `getPackageManagerName()`: the token after the last `--`, basenamed
    /// and stripped of a `.cmd` / `.exe` suffix.
    pub fn package_manager_name(&self) -> String {
        let npm = self.npm_command();
        let mut parts = vec![npm.command.clone()];
        parts.extend(npm.args.clone());
        let pm = match parts.iter().rposition(|p| p == "--") {
            Some(index) if index + 1 < parts.len() => parts[index + 1].clone(),
            _ => npm.command.clone(),
        };
        if pm.is_empty() {
            return String::new();
        }
        strip_executable_suffix(basename(&pm)).to_string()
    }

    /// pi's `getNpmInstallRoot(scope, false)` for managed (non-temporary) roots.
    pub fn npm_install_root(&self, scope: InstallScope) -> String {
        match scope {
            InstallScope::User => join_path(&self.agent_dir, &["npm"]),
            InstallScope::Project => join_path(&self.cwd, &[CONFIG_DIR_NAME, "npm"]),
        }
    }

    /// pi's `runNpmCommand`/`runNpmCommandSync`: prepend the configured args to
    /// the sub-command, optionally pinned to `cwd`.
    pub fn npm_command_request(&self, sub_args: &[String], cwd: Option<&str>) -> CommandRequest {
        let npm = self.npm_command();
        let mut args = npm.args;
        args.extend(sub_args.iter().cloned());
        let request = CommandRequest::new(npm.command, args);
        match cwd {
            Some(cwd) => request.with_cwd(cwd),
            None => request,
        }
    }
}

/// pi's `getNpmInstallArgs(specs, installRoot)`: peer-dependency handling differs
/// per package manager.
pub fn npm_install_args(
    package_manager_name: &str,
    specs: &[String],
    install_root: &str,
) -> Vec<String> {
    let mut args = vec!["install".to_string()];
    args.extend(specs.iter().cloned());
    match package_manager_name {
        "bun" => {
            args.push("--cwd".to_string());
            args.push(install_root.to_string());
            args.push("--omit=peer".to_string());
        }
        "pnpm" => {
            args.push("--prefix".to_string());
            args.push(install_root.to_string());
            args.push("--config.auto-install-peers=false".to_string());
            args.push("--config.strict-peer-dependencies=false".to_string());
            args.push("--config.strict-dep-builds=false".to_string());
        }
        _ => {
            args.push("--prefix".to_string());
            args.push(install_root.to_string());
            args.push("--legacy-peer-deps".to_string());
        }
    }
    args
}

/// pi's `uninstallNpm` argv (bun uses `--cwd`; npm adds `--legacy-peer-deps`,
/// pnpm does not).
pub fn npm_uninstall_args(
    package_manager_name: &str,
    name: &str,
    install_root: &str,
) -> Vec<String> {
    if package_manager_name == "bun" {
        return vec![
            "uninstall".to_string(),
            name.to_string(),
            "--cwd".to_string(),
            install_root.to_string(),
        ];
    }
    let mut args = vec![
        "uninstall".to_string(),
        name.to_string(),
        "--prefix".to_string(),
        install_root.to_string(),
    ];
    if package_manager_name != "pnpm" {
        args.push("--legacy-peer-deps".to_string());
    }
    args
}

/// pi's `getGitDependencyInstallArgs()`: plain `install` when an npm command is
/// configured, else `install --omit=dev`.
pub fn git_dependency_install_args(npm_configured: bool) -> Vec<String> {
    if npm_configured {
        vec!["install".to_string()]
    } else {
        vec!["install".to_string(), "--omit=dev".to_string()]
    }
}

pub(crate) fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

pub(crate) fn git_capture(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args)
        .with_cwd(cwd)
        .with_timeout(NETWORK_TIMEOUT_MS)
}

pub(crate) fn git_run(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args).with_cwd(cwd)
}

pub(crate) fn git_remote(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args)
        .with_cwd(cwd)
        .with_env("GIT_TERMINAL_PROMPT", "0")
        .with_timeout(NETWORK_TIMEOUT_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package_manager::test_support::s;

    // --- config helpers ---
    #[test]
    fn package_manager_name_after_last_separator() {
        let cfg = PackageManagerConfig::new(
            "/c",
            "/a",
            Some(s(&["mise", "exec", "node@20", "--", "pnpm"])),
        );
        assert_eq!(cfg.package_manager_name(), "pnpm");
    }

    #[test]
    fn package_manager_name_strips_exe_suffix() {
        let cfg = PackageManagerConfig::new("/c", "/a", Some(s(&["npm.cmd"])));
        assert_eq!(cfg.package_manager_name(), "npm");
    }

    #[test]
    fn npm_command_falls_back_to_bare_npm() {
        let cfg = PackageManagerConfig::new("/c", "/a", None);
        assert_eq!(cfg.package_manager_name(), "npm");
        assert!(!cfg.npm_configured());
    }
}
