//! Port of pi-coding-agent's `core/package-manager.ts`, command boundary.
//!
//! pi's `DefaultPackageManager` mixes two concerns: pure filesystem resolution
//! (settings parsing, resource discovery, pattern filtering) and external
//! command execution (npm install/uninstall/view, git fetch/reset/clean/clone,
//! `npm root -g`, `pnpm list -g`). This module ports the *command* concern — the
//! 43-site command-mock cohort of `package-manager.test.ts` — as
//! [`CommandFlowMachine`]s whose planned [`CommandRequest`]s are byte-exact with
//! pi's `runCommand` / `runCommandCapture` / `runCommandSync` argv (and, where
//! pi asserts them, `cwd` / `timeoutMs` / `env`).
//!
//! # Config injection
//!
//! pi reads the package-manager command from `settingsManager.getNpmCommand()`
//! (an argv array such as `["mise", "exec", "node@20", "--", "npm"]`) and pins
//! install roots to `cwd` / `agentDir`. Here [`PackageManagerConfig`] captures
//! the same three inputs (`cwd`, `agent_dir`, `npm_command`) so the planned argv
//! — including any `mise` wrapper prefix and the `--prefix <root>` path — matches
//! pi exactly. Everything the argv depends on is derived from that config the
//! same way pi derives it (`getNpmCommand`, `getPackageManagerName`,
//! `getNpmInstallRoot`, `getNpmInstallArgs`, `getGitDependencyInstallArgs`).
//!
//! # Scope of this port
//!
//! The machines take already-parsed inputs (npm specs/names, install roots, git
//! refs and target dirs, and filesystem facts such as "package.json exists")
//! rather than re-porting pi's `parseSource` / `parseGitUrl` / path-resolution
//! machinery, which is a separate (non-command) concern. Filesystem facts that
//! pi checks inline (`existsSync(packageJson)`) are passed in by the host shim,
//! which owns the filesystem seam; the machines remain pure command planners.

use atilla_ai::seams::subprocess::{CommandOutput, CommandRequest};
use std::path::PathBuf;

use crate::core::command_flow::{CommandFlowMachine, CommandStep, OneShotCommand};

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

fn join_path(base: &str, parts: &[&str]) -> String {
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

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn git_capture(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args)
        .with_cwd(cwd)
        .with_timeout(NETWORK_TIMEOUT_MS)
}

fn git_run(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args).with_cwd(cwd)
}

fn git_remote(args: Vec<String>, cwd: &str) -> CommandRequest {
    CommandRequest::new("git", args)
        .with_cwd(cwd)
        .with_env("GIT_TERMINAL_PROMPT", "0")
        .with_timeout(NETWORK_TIMEOUT_MS)
}

// ---------------------------------------------------------------------------
// One-shot builders
// ---------------------------------------------------------------------------

/// pi's `installNpm` / `installNpmBatch`: `<npm> install <specs> --prefix <root>
/// ...` planned as a single command, `cwd` unset (pi passes `options`
/// undefined).
pub fn npm_install(
    cfg: &PackageManagerConfig,
    specs: &[String],
    scope: InstallScope,
) -> OneShotCommand {
    let root = cfg.npm_install_root(scope);
    let sub_args = npm_install_args(&cfg.package_manager_name(), specs, &root);
    OneShotCommand::new(cfg.npm_command_request(&sub_args, None))
}

/// pi's `uninstallNpm`: `<npm> uninstall <name> --prefix <root> ...`, `cwd`
/// unset.
pub fn npm_uninstall(
    cfg: &PackageManagerConfig,
    name: &str,
    scope: InstallScope,
) -> OneShotCommand {
    let root = cfg.npm_install_root(scope);
    let sub_args = npm_uninstall_args(&cfg.package_manager_name(), name, &root);
    OneShotCommand::new(cfg.npm_command_request(&sub_args, None))
}

/// pi's git-dependency install inside a checkout: `<npm> install [--omit=dev]`
/// pinned to the checkout dir.
pub fn git_dependency_install(cfg: &PackageManagerConfig, target_dir: &str) -> OneShotCommand {
    let sub_args = git_dependency_install_args(cfg.npm_configured());
    OneShotCommand::new(cfg.npm_command_request(&sub_args, Some(target_dir)))
}

// ---------------------------------------------------------------------------
// npm global-root helpers (pi's runCommandSync operations)
// ---------------------------------------------------------------------------

/// pi's `getGlobalNpmRoot()`: one sync command whose output is the global root.
///
/// For bun, pi runs `pm bin -g` then derives
/// `dirname(binDir)/install/global/node_modules`; every other manager runs
/// `root -g` and trims. The [`CommandFlowMachine::Output`] is the computed root.
#[derive(Debug, Clone)]
pub struct GlobalNpmRootMachine {
    request: Option<CommandRequest>,
    is_bun: bool,
}

impl GlobalNpmRootMachine {
    /// Plan the global-root lookup for `cfg`.
    pub fn new(cfg: &PackageManagerConfig) -> Self {
        let is_bun = cfg.package_manager_name() == "bun";
        let sub_args = if is_bun {
            strings(&["pm", "bin", "-g"])
        } else {
            strings(&["root", "-g"])
        };
        Self {
            request: Some(cfg.npm_command_request(&sub_args, None)),
            is_bun,
        }
    }
}

impl CommandFlowMachine for GlobalNpmRootMachine {
    type Output = String;

    fn start(&mut self) -> CommandStep<String> {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done {
                result: String::new(),
            },
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<String> {
        let trimmed = output.stdout.trim();
        let result = if self.is_bun {
            let bin_dir = PathBuf::from(trimmed);
            let parent = bin_dir
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            join_path(&parent, &["install", "global", "node_modules"])
        } else {
            trimmed.to_string()
        };
        CommandStep::Done { result }
    }
}

/// pi's `getPnpmGlobalPackagePath(name)`: `<pnpm> list -g --depth 0 --json`, then
/// pull `dependencies[name].path` out of the JSON. The output is the package
/// path, if present.
#[derive(Debug, Clone)]
pub struct PnpmGlobalListMachine {
    request: Option<CommandRequest>,
    package_name: String,
}

impl PnpmGlobalListMachine {
    /// Plan the `pnpm list -g` lookup for `package_name`.
    pub fn new(cfg: &PackageManagerConfig, package_name: impl Into<String>) -> Self {
        let sub_args = strings(&["list", "-g", "--depth", "0", "--json"]);
        Self {
            request: Some(cfg.npm_command_request(&sub_args, None)),
            package_name: package_name.into(),
        }
    }
}

impl CommandFlowMachine for PnpmGlobalListMachine {
    type Output = Option<String>;

    fn start(&mut self) -> CommandStep<Option<String>> {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done { result: None },
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<Option<String>> {
        let result = parse_pnpm_global_path(&output.stdout, &self.package_name);
        CommandStep::Done { result }
    }
}

fn parse_pnpm_global_path(stdout: &str, package_name: &str) -> Option<String> {
    let entries: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let entries = entries.as_array()?;
    for entry in entries {
        if let Some(path) = entry
            .get("dependencies")
            .and_then(|deps| deps.get(package_name))
            .and_then(|pkg| pkg.get("path"))
            .and_then(|path| path.as_str())
        {
            return Some(path.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// npm version probe + maybe-install (pi's update / shouldUpdateNpmSource)
// ---------------------------------------------------------------------------

/// pi's per-source npm update: probe `<npm> view <spec> version --json`
/// (capture, timed), compare to the installed version, and only then plan
/// `<npm> install ...`. When the versions match, no install command is planned.
///
/// Mirrors `shouldUpdateNpmSource` + `updateNpmBatch`: when no version is
/// installed, pi skips the probe and installs directly; when the probe fails or
/// returns an unexpected shape, pi preserves update behavior and installs.
#[derive(Debug, Clone)]
pub struct NpmUpdateMachine {
    cfg: PackageManagerConfig,
    scope: InstallScope,
    /// The spec passed to `npm view` (pi: `version ? spec : name`).
    view_spec: String,
    /// The semver range constraining version selection, if the source is ranged.
    range: Option<String>,
    /// The currently installed version, if any.
    installed_version: Option<String>,
    /// The spec to install (pi: `version ? spec : name@latest`).
    install_spec: String,
    phase: UpdatePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdatePhase {
    Start,
    AwaitView,
    AwaitInstall,
    Done,
}

impl NpmUpdateMachine {
    /// Plan an npm update for a single source.
    pub fn new(
        cfg: &PackageManagerConfig,
        scope: InstallScope,
        view_spec: impl Into<String>,
        range: Option<String>,
        installed_version: Option<String>,
        install_spec: impl Into<String>,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            scope,
            view_spec: view_spec.into(),
            range,
            installed_version,
            install_spec: install_spec.into(),
            phase: UpdatePhase::Start,
        }
    }

    fn view_request(&self) -> CommandRequest {
        let sub_args = vec![
            "view".to_string(),
            self.view_spec.clone(),
            "version".to_string(),
            "--json".to_string(),
        ];
        self.cfg
            .npm_command_request(&sub_args, Some(&self.cfg.cwd))
            .with_timeout(NETWORK_TIMEOUT_MS)
    }

    fn install_request(&self) -> CommandRequest {
        let root = self.cfg.npm_install_root(self.scope);
        let specs = [self.install_spec.clone()];
        let sub_args = npm_install_args(&self.cfg.package_manager_name(), &specs, &root);
        self.cfg.npm_command_request(&sub_args, None)
    }
}

impl CommandFlowMachine for NpmUpdateMachine {
    /// `true` when an install command was planned, `false` for the up-to-date
    /// no-op.
    type Output = bool;

    fn start(&mut self) -> CommandStep<bool> {
        // No installed version: pi's shouldUpdateNpmSource returns true without a
        // probe, and the batch installs directly.
        if self.installed_version.is_none() {
            self.phase = UpdatePhase::AwaitInstall;
            return CommandStep::Run {
                request: self.install_request(),
            };
        }
        self.phase = UpdatePhase::AwaitView;
        CommandStep::Run {
            request: self.view_request(),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<bool> {
        match self.phase {
            UpdatePhase::AwaitView => {
                let installed = self.installed_version.clone().unwrap_or_default();
                let should_update = if !output.success() {
                    // Probe failed: pi preserves update behavior.
                    true
                } else {
                    match parse_npm_view_version(&output.stdout, self.range.as_deref()) {
                        Some(target) => target != installed,
                        // Unexpected/empty response: pi's catch returns true.
                        None => true,
                    }
                };
                if should_update {
                    self.phase = UpdatePhase::AwaitInstall;
                    CommandStep::Run {
                        request: self.install_request(),
                    }
                } else {
                    self.phase = UpdatePhase::Done;
                    CommandStep::Done { result: false }
                }
            }
            UpdatePhase::AwaitInstall => {
                self.phase = UpdatePhase::Done;
                CommandStep::Done { result: true }
            }
            UpdatePhase::Start | UpdatePhase::Done => CommandStep::Done { result: false },
        }
    }
}

/// pi's `getLatestNpmVersion` parse: a bare string version, or the best match
/// from a JSON array (max satisfying `range`, else highest by semver).
pub fn parse_npm_view_version(stdout: &str, range: Option<&str>) -> Option<String> {
    let raw = stdout.trim();
    if raw.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let Some(single) = value.as_str() {
        return Some(single.to_string());
    }
    if let Some(array) = value.as_array() {
        let versions: Vec<String> = array
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .collect();
        return select_version(&versions, range);
    }
    None
}

fn select_version(versions: &[String], range: Option<&str>) -> Option<String> {
    match range {
        Some(range) => {
            let req = semver::VersionReq::parse(range).ok()?;
            versions
                .iter()
                .filter_map(|v| {
                    semver::Version::parse(v)
                        .ok()
                        .map(|parsed| (v.clone(), parsed))
                })
                .filter(|(_, parsed)| req.matches(parsed))
                .max_by(|(_, a), (_, b)| a.cmp(b))
                .map(|(raw, _)| raw)
        }
        None => versions
            .iter()
            .filter_map(|v| {
                semver::Version::parse(v)
                    .ok()
                    .map(|parsed| (v.clone(), parsed))
            })
            .max_by(|(_, a), (_, b)| a.cmp(b))
            .map(|(raw, _)| raw),
    }
}

// ---------------------------------------------------------------------------
// git reconcile: ensureGitRef (fetch / rev-parse / reset / clean / install)
// ---------------------------------------------------------------------------

/// pi's `ensureGitRef(targetDir, fetchArgs, ref)`: fetch the ref, compare local
/// HEAD to `<ref>^{commit}`, and — only when they differ — `reset --hard`,
/// `clean -fdx`, and reinstall git deps (when a package.json is present).
#[derive(Debug, Clone)]
pub struct GitEnsureRefMachine {
    cfg: PackageManagerConfig,
    target_dir: String,
    fetch_args: Vec<String>,
    commit_ref: String,
    has_package_json: bool,
    phase: EnsurePhase,
    local_head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsurePhase {
    Start,
    AwaitFetch,
    AwaitLocalHead,
    AwaitTargetHead,
    AwaitReset,
    AwaitClean,
    AwaitInstall,
    Done,
}

impl GitEnsureRefMachine {
    /// Plan a reconcile of `target_dir` to `ref_`, fetching via `fetch_args`.
    ///
    /// `has_package_json` is the host's `existsSync(join(targetDir,
    /// "package.json"))` after the fetch.
    pub fn new(
        cfg: &PackageManagerConfig,
        target_dir: impl Into<String>,
        fetch_args: Vec<String>,
        ref_: &str,
        has_package_json: bool,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            target_dir: target_dir.into(),
            fetch_args,
            commit_ref: format!("{ref_}^{{commit}}"),
            has_package_json,
            phase: EnsurePhase::Start,
            local_head: String::new(),
        }
    }

    fn install_request(&self) -> CommandRequest {
        let sub_args = git_dependency_install_args(self.cfg.npm_configured());
        self.cfg
            .npm_command_request(&sub_args, Some(&self.target_dir))
    }
}

impl CommandFlowMachine for GitEnsureRefMachine {
    type Output = ();

    fn start(&mut self) -> CommandStep<()> {
        self.phase = EnsurePhase::AwaitFetch;
        CommandStep::Run {
            request: git_run(self.fetch_args.clone(), &self.target_dir),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<()> {
        match self.phase {
            EnsurePhase::AwaitFetch => {
                self.phase = EnsurePhase::AwaitLocalHead;
                CommandStep::Run {
                    request: git_capture(strings(&["rev-parse", "HEAD"]), &self.target_dir),
                }
            }
            EnsurePhase::AwaitLocalHead => {
                self.local_head = output.stdout.trim().to_string();
                self.phase = EnsurePhase::AwaitTargetHead;
                CommandStep::Run {
                    request: git_capture(
                        vec!["rev-parse".to_string(), self.commit_ref.clone()],
                        &self.target_dir,
                    ),
                }
            }
            EnsurePhase::AwaitTargetHead => {
                let target_head = output.stdout.trim();
                if self.local_head == target_head {
                    self.phase = EnsurePhase::Done;
                    return CommandStep::Done { result: () };
                }
                self.phase = EnsurePhase::AwaitReset;
                CommandStep::Run {
                    request: git_run(
                        vec![
                            "reset".to_string(),
                            "--hard".to_string(),
                            self.commit_ref.clone(),
                        ],
                        &self.target_dir,
                    ),
                }
            }
            EnsurePhase::AwaitReset => {
                self.phase = EnsurePhase::AwaitClean;
                CommandStep::Run {
                    request: git_run(strings(&["clean", "-fdx"]), &self.target_dir),
                }
            }
            EnsurePhase::AwaitClean => {
                if self.has_package_json {
                    self.phase = EnsurePhase::AwaitInstall;
                    CommandStep::Run {
                        request: self.install_request(),
                    }
                } else {
                    self.phase = EnsurePhase::Done;
                    CommandStep::Done { result: () }
                }
            }
            EnsurePhase::AwaitInstall | EnsurePhase::Start | EnsurePhase::Done => {
                self.phase = EnsurePhase::Done;
                CommandStep::Done { result: () }
            }
        }
    }
}

/// pi's fresh-clone `installGit`: `git clone <repo> <targetDir>`, an optional
/// `git checkout <ref>`, then a git-dependency install (when a package.json is
/// present after clone).
#[derive(Debug, Clone)]
pub struct GitCloneMachine {
    cfg: PackageManagerConfig,
    repo: String,
    target_dir: String,
    ref_: Option<String>,
    has_package_json: bool,
    phase: ClonePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClonePhase {
    Start,
    AwaitClone,
    AwaitCheckout,
    AwaitInstall,
    Done,
}

impl GitCloneMachine {
    /// Plan a fresh clone of `repo` into `target_dir`.
    ///
    /// `has_package_json` is the host's `existsSync(join(targetDir,
    /// "package.json"))` after the clone/checkout.
    pub fn new(
        cfg: &PackageManagerConfig,
        repo: impl Into<String>,
        target_dir: impl Into<String>,
        ref_: Option<String>,
        has_package_json: bool,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            repo: repo.into(),
            target_dir: target_dir.into(),
            ref_,
            has_package_json,
            phase: ClonePhase::Start,
        }
    }

    fn install_or_done(&mut self) -> CommandStep<()> {
        if self.has_package_json {
            self.phase = ClonePhase::AwaitInstall;
            let sub_args = git_dependency_install_args(self.cfg.npm_configured());
            CommandStep::Run {
                request: self
                    .cfg
                    .npm_command_request(&sub_args, Some(&self.target_dir)),
            }
        } else {
            self.phase = ClonePhase::Done;
            CommandStep::Done { result: () }
        }
    }
}

impl CommandFlowMachine for GitCloneMachine {
    type Output = ();

    fn start(&mut self) -> CommandStep<()> {
        self.phase = ClonePhase::AwaitClone;
        CommandStep::Run {
            request: CommandRequest::new(
                "git",
                vec![
                    "clone".to_string(),
                    self.repo.clone(),
                    self.target_dir.clone(),
                ],
            ),
        }
    }

    fn advance(&mut self, _output: CommandOutput) -> CommandStep<()> {
        match self.phase {
            ClonePhase::AwaitClone => match self.ref_.clone() {
                Some(ref_) => {
                    self.phase = ClonePhase::AwaitCheckout;
                    CommandStep::Run {
                        request: git_run(vec!["checkout".to_string(), ref_], &self.target_dir),
                    }
                }
                None => self.install_or_done(),
            },
            ClonePhase::AwaitCheckout => self.install_or_done(),
            ClonePhase::AwaitInstall | ClonePhase::Start | ClonePhase::Done => {
                self.phase = ClonePhase::Done;
                CommandStep::Done { result: () }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// git upstream / update-target resolution
// ---------------------------------------------------------------------------

/// The resolved local git update target, mirroring pi's
/// `getLocalGitUpdateTarget` return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitUpdateTarget {
    /// The ref to reconcile to (`@{upstream}` or `origin/HEAD`).
    pub ref_: String,
    /// The resolved head commit of that ref.
    pub head: String,
    /// The `git fetch` argv to fetch it.
    pub fetch_args: Vec<String>,
}

fn upstream_fetch_args(branch: &str) -> Vec<String> {
    vec![
        "fetch".to_string(),
        "--prune".to_string(),
        "--no-tags".to_string(),
        "origin".to_string(),
        format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
    ]
}

/// pi's `getLocalGitUpdateTarget(installedPath)`: resolve the fetch/reset target
/// from the tracking branch, with the `remote set-head` / `symbolic-ref`
/// fallback chain when there is no usable `@{upstream}`.
#[derive(Debug, Clone)]
pub struct GitLocalUpdateTargetMachine {
    installed_path: String,
    phase: TargetPhase,
    head: String,
    pending_fetch_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetPhase {
    Start,
    AwaitUpstream,
    AwaitUpstreamHead,
    AwaitSetHead,
    AwaitOriginHead,
    AwaitSymbolicRef,
    Done,
}

impl GitLocalUpdateTargetMachine {
    /// Plan the update-target resolution for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        Self {
            installed_path: installed_path.into(),
            phase: TargetPhase::Start,
            head: String::new(),
            pending_fetch_args: Vec::new(),
        }
    }

    fn enter_fallback(&mut self) -> CommandStep<GitUpdateTarget> {
        self.phase = TargetPhase::AwaitSetHead;
        CommandStep::Run {
            request: git_run(
                strings(&["remote", "set-head", "origin", "-a"]),
                &self.installed_path,
            ),
        }
    }
}

impl CommandFlowMachine for GitLocalUpdateTargetMachine {
    type Output = GitUpdateTarget;

    fn start(&mut self) -> CommandStep<GitUpdateTarget> {
        self.phase = TargetPhase::AwaitUpstream;
        CommandStep::Run {
            request: git_capture(
                strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                &self.installed_path,
            ),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<GitUpdateTarget> {
        match self.phase {
            TargetPhase::AwaitUpstream => {
                let trimmed = output.stdout.trim();
                if !output.success() {
                    return self.enter_fallback();
                }
                let Some(branch) = trimmed.strip_prefix("origin/") else {
                    return self.enter_fallback();
                };
                if branch.is_empty() {
                    return self.enter_fallback();
                }
                // Stash the fetch args (built from the validated branch) for the
                // Done step after we resolve the upstream head.
                self.pending_fetch_args = upstream_fetch_args(branch);
                self.phase = TargetPhase::AwaitUpstreamHead;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["rev-parse", "@{upstream}"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitUpstreamHead => {
                self.phase = TargetPhase::Done;
                CommandStep::Done {
                    result: GitUpdateTarget {
                        ref_: "@{upstream}".to_string(),
                        head: output.stdout.trim().to_string(),
                        fetch_args: self.pending_fetch_args.clone(),
                    },
                }
            }
            TargetPhase::AwaitSetHead => {
                // set-head failure is ignored (pi's `.catch(() => {})`).
                self.phase = TargetPhase::AwaitOriginHead;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["rev-parse", "origin/HEAD"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitOriginHead => {
                self.head = output.stdout.trim().to_string();
                self.phase = TargetPhase::AwaitSymbolicRef;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["symbolic-ref", "refs/remotes/origin/HEAD"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitSymbolicRef => {
                let origin_head_ref = if output.success() {
                    output.stdout.trim()
                } else {
                    ""
                };
                let branch = origin_head_ref
                    .strip_prefix("refs/remotes/origin/")
                    .unwrap_or("");
                let fetch_args = if !branch.is_empty() {
                    upstream_fetch_args(branch)
                } else {
                    strings(&[
                        "fetch",
                        "--prune",
                        "--no-tags",
                        "origin",
                        "+HEAD:refs/remotes/origin/HEAD",
                    ])
                };
                self.phase = TargetPhase::Done;
                CommandStep::Done {
                    result: GitUpdateTarget {
                        ref_: "origin/HEAD".to_string(),
                        head: self.head.clone(),
                        fetch_args,
                    },
                }
            }
            TargetPhase::Start | TargetPhase::Done => CommandStep::Done {
                result: GitUpdateTarget {
                    ref_: "origin/HEAD".to_string(),
                    head: self.head.clone(),
                    fetch_args: Vec::new(),
                },
            },
        }
    }
}

// ---------------------------------------------------------------------------
// git remote head resolution + available-update check
// ---------------------------------------------------------------------------

/// The outcome of resolving a remote git head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteHead {
    /// The resolved 40-char commit SHA.
    Head(String),
    /// pi threw "Failed to determine remote HEAD" (or a probe rejected).
    Failed,
}

fn first_sha(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?m)^([0-9a-f]{40})\s+").expect("valid regex");
    re.captures(text).map(|caps| caps[1].to_string())
}

fn head_sha(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?m)^([0-9a-f]{40})\s+HEAD$").expect("valid regex");
    re.captures(text).map(|caps| caps[1].to_string())
}

/// pi's `getRemoteGitHead(installedPath)`: probe the tracking branch
/// (`rev-parse --abbrev-ref @{upstream}`), `ls-remote origin <upstreamRef>` when
/// there is one, and fall back to `ls-remote origin HEAD`. Remote reads carry
/// `GIT_TERMINAL_PROMPT=0` and the network timeout.
#[derive(Debug, Clone)]
pub struct GitRemoteHeadMachine {
    installed_path: String,
    phase: RemoteHeadPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteHeadPhase {
    Start,
    AwaitUpstream,
    AwaitUpstreamLsRemote,
    AwaitHeadLsRemote,
    Done,
}

impl GitRemoteHeadMachine {
    /// Plan the remote-head resolution for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        Self {
            installed_path: installed_path.into(),
            phase: RemoteHeadPhase::Start,
        }
    }

    fn ls_remote_head(&mut self) -> CommandStep<RemoteHead> {
        self.phase = RemoteHeadPhase::AwaitHeadLsRemote;
        CommandStep::Run {
            request: git_remote(
                strings(&["ls-remote", "origin", "HEAD"]),
                &self.installed_path,
            ),
        }
    }
}

impl CommandFlowMachine for GitRemoteHeadMachine {
    type Output = RemoteHead;

    fn start(&mut self) -> CommandStep<RemoteHead> {
        self.phase = RemoteHeadPhase::AwaitUpstream;
        CommandStep::Run {
            request: git_capture(
                strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                &self.installed_path,
            ),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<RemoteHead> {
        match self.phase {
            RemoteHeadPhase::AwaitUpstream => {
                // getGitUpstreamRef: only origin/<branch> yields a usable ref.
                let upstream_ref = if output.success() {
                    output
                        .stdout
                        .trim()
                        .strip_prefix("origin/")
                        .filter(|branch| !branch.is_empty())
                        .map(|branch| format!("refs/heads/{branch}"))
                } else {
                    None
                };
                match upstream_ref {
                    Some(ref_) => {
                        self.phase = RemoteHeadPhase::AwaitUpstreamLsRemote;
                        CommandStep::Run {
                            request: git_remote(
                                vec!["ls-remote".to_string(), "origin".to_string(), ref_],
                                &self.installed_path,
                            ),
                        }
                    }
                    None => self.ls_remote_head(),
                }
            }
            RemoteHeadPhase::AwaitUpstreamLsRemote => {
                if !output.success() {
                    self.phase = RemoteHeadPhase::Done;
                    return CommandStep::Done {
                        result: RemoteHead::Failed,
                    };
                }
                match first_sha(&output.stdout) {
                    Some(sha) => {
                        self.phase = RemoteHeadPhase::Done;
                        CommandStep::Done {
                            result: RemoteHead::Head(sha),
                        }
                    }
                    None => self.ls_remote_head(),
                }
            }
            RemoteHeadPhase::AwaitHeadLsRemote => {
                self.phase = RemoteHeadPhase::Done;
                let result = if !output.success() {
                    RemoteHead::Failed
                } else {
                    match head_sha(&output.stdout) {
                        Some(sha) => RemoteHead::Head(sha),
                        None => RemoteHead::Failed,
                    }
                };
                CommandStep::Done { result }
            }
            RemoteHeadPhase::Start | RemoteHeadPhase::Done => CommandStep::Done {
                result: RemoteHead::Failed,
            },
        }
    }
}

/// pi's `gitHasAvailableUpdate(installedPath)`: compare local `rev-parse HEAD`
/// with the resolved remote head; any probe failure yields `false`.
#[derive(Debug, Clone)]
pub struct GitHasUpdateMachine {
    installed_path: String,
    phase: HasUpdatePhase,
    local_head: String,
    remote: GitRemoteHeadMachine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HasUpdatePhase {
    Start,
    AwaitLocalHead,
    InRemote,
    Done,
}

impl GitHasUpdateMachine {
    /// Plan the available-update check for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        let installed_path = installed_path.into();
        Self {
            remote: GitRemoteHeadMachine::new(installed_path.clone()),
            installed_path,
            phase: HasUpdatePhase::Start,
            local_head: String::new(),
        }
    }
}

impl CommandFlowMachine for GitHasUpdateMachine {
    type Output = bool;

    fn start(&mut self) -> CommandStep<bool> {
        self.phase = HasUpdatePhase::AwaitLocalHead;
        CommandStep::Run {
            request: git_capture(strings(&["rev-parse", "HEAD"]), &self.installed_path),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep<bool> {
        match self.phase {
            HasUpdatePhase::AwaitLocalHead => {
                if !output.success() {
                    self.phase = HasUpdatePhase::Done;
                    return CommandStep::Done { result: false };
                }
                self.local_head = output.stdout.trim().to_string();
                self.phase = HasUpdatePhase::InRemote;
                match self.remote.start() {
                    CommandStep::Run { request } => CommandStep::Run { request },
                    CommandStep::Done { result } => {
                        self.phase = HasUpdatePhase::Done;
                        CommandStep::Done {
                            result: self.compare(result),
                        }
                    }
                }
            }
            HasUpdatePhase::InRemote => match self.remote.advance(output) {
                CommandStep::Run { request } => CommandStep::Run { request },
                CommandStep::Done { result } => {
                    self.phase = HasUpdatePhase::Done;
                    CommandStep::Done {
                        result: self.compare(result),
                    }
                }
            },
            HasUpdatePhase::Start | HasUpdatePhase::Done => CommandStep::Done { result: false },
        }
    }
}

impl GitHasUpdateMachine {
    fn compare(&self, remote: RemoteHead) -> bool {
        match remote {
            RemoteHead::Head(head) => self.local_head != head.trim(),
            RemoteHead::Failed => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }

    /// Drive a machine to completion, returning every planned request in order
    /// plus the final result. Each scripted output is fed in sequence.
    fn drive<M: CommandFlowMachine>(
        machine: &mut M,
        outputs: Vec<CommandOutput>,
    ) -> (Vec<CommandRequest>, M::Output) {
        let mut requests = Vec::new();
        let mut outputs = outputs.into_iter();
        let mut step = machine.start();
        loop {
            match step {
                CommandStep::Run { request } => {
                    requests.push(request);
                    let output = outputs
                        .next()
                        .expect("machine planned more commands than scripted outputs");
                    step = machine.advance(output);
                }
                CommandStep::Done { result } => return (requests, result),
            }
        }
    }

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput::ok(stdout)
    }

    fn fail() -> CommandOutput {
        CommandOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    // --- npm install argv parity (mirrors "should use npmCommand argv for npm
    // installs") ---
    #[test]
    fn npm_install_uses_npm_command_argv_and_prefix() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "node@20", "--", "npm"])),
        );
        let mut machine = npm_install(&cfg, &s(&["@scope/pkg"]), InstallScope::User);
        let (requests, ()) = drive(&mut machine, vec![ok("")]);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "mise",
                s(&[
                    "exec",
                    "node@20",
                    "--",
                    "npm",
                    "install",
                    "@scope/pkg",
                    "--prefix",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--legacy-peer-deps",
                ]),
            )
        );
        assert_eq!(requests[0].cwd, None);
    }

    // --- bun install argv (mirrors "should use bun --cwd for npm package
    // installs") ---
    #[test]
    fn npm_install_uses_bun_cwd() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "bun@1", "--", "bun"])),
        );
        let mut machine = npm_install(&cfg, &s(&["@scope/pkg"]), InstallScope::User);
        let (requests, ()) = drive(&mut machine, vec![ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "mise",
                s(&[
                    "exec",
                    "bun@1",
                    "--",
                    "bun",
                    "install",
                    "@scope/pkg",
                    "--cwd",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--omit=peer",
                ]),
            )
        );
    }

    // --- pnpm managed install argv (mirrors "should install user npm packages
    // into the pi-managed npm root") ---
    #[test]
    fn npm_install_uses_pnpm_config_flags() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["pnpm"])));
        let mut machine = npm_install(&cfg, &s(&["pnpm-pkg"]), InstallScope::User);
        let (requests, ()) = drive(&mut machine, vec![ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "pnpm",
                s(&[
                    "install",
                    "pnpm-pkg",
                    "--prefix",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--config.auto-install-peers=false",
                    "--config.strict-peer-dependencies=false",
                    "--config.strict-dep-builds=false",
                ]),
            )
        );
    }

    // --- uninstall argv (mirrors "should pass legacy peer deps when
    // uninstalling npm packages") ---
    #[test]
    fn npm_uninstall_adds_legacy_peer_deps() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let mut machine = npm_uninstall(&cfg, "@scope/pkg", InstallScope::User);
        let (requests, ()) = drive(&mut machine, vec![ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "npm",
                s(&[
                    "uninstall",
                    "@scope/pkg",
                    "--prefix",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--legacy-peer-deps",
                ]),
            )
        );
    }

    // --- git fresh-clone deps install argv (mirrors "should install git package
    // dependencies with --omit=dev") ---
    #[test]
    fn git_clone_then_omit_dev_install() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let mut machine = GitCloneMachine::new(&cfg, "github.com/user/repo", &target, None, true);
        let (requests, ()) = drive(&mut machine, vec![ok(""), ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["clone", "github.com/user/repo", &target])),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("npm", s(&["install", "--omit=dev"])).with_cwd(&target),
        );
    }

    // --- plain install when npm command configured (mirrors "should use plain
    // install for git package dependencies when npmCommand is configured") ---
    #[test]
    fn git_clone_plain_install_when_configured() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["pnpm"])));
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let mut machine = GitCloneMachine::new(&cfg, "github.com/user/repo", &target, None, true);
        let (requests, ()) = drive(&mut machine, vec![ok(""), ok("")]);
        assert_eq!(
            requests[1],
            CommandRequest::new("pnpm", s(&["install"])).with_cwd(&target),
        );
    }

    // --- ensureGitRef reconcile to pinned ref (mirrors "should reconcile an
    // existing git checkout to a pinned ref during install") ---
    #[test]
    fn git_ensure_ref_reconciles_pinned_ref() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            &target,
            s(&["fetch", "origin", "v2"]),
            "FETCH_HEAD",
            true,
        );
        // fetch, rev-parse HEAD -> old, rev-parse FETCH_HEAD^{commit} -> new,
        // reset, clean, npm install.
        let (requests, ()) = drive(
            &mut machine,
            vec![
                ok(""),
                ok("old-head"),
                ok("new-head"),
                ok(""),
                ok(""),
                ok(""),
            ],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["fetch", "origin", "v2"])).with_cwd(&target),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["rev-parse", "HEAD"]))
                .with_cwd(&target)
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[2],
            CommandRequest::new("git", s(&["rev-parse", "FETCH_HEAD^{commit}"]))
                .with_cwd(&target)
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["reset", "--hard", "FETCH_HEAD^{commit}"]))
                .with_cwd(&target),
        );
        assert_eq!(
            requests[4],
            CommandRequest::new("git", s(&["clean", "-fdx"])).with_cwd(&target),
        );
        assert_eq!(
            requests[5],
            CommandRequest::new("npm", s(&["install", "--omit=dev"])).with_cwd(&target),
        );
    }

    // --- ensureGitRef early-exit when heads match ---
    #[test]
    fn git_ensure_ref_skips_when_heads_match() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = "/tmp/checkout";
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            target,
            s(&["fetch", "origin", "v2"]),
            "FETCH_HEAD",
            true,
        );
        let (requests, ()) = drive(&mut machine, vec![ok(""), ok("same"), ok("same")]);
        // fetch + two rev-parse captures only; no reset/clean/install.
        assert_eq!(requests.len(), 3);
    }

    // --- ensureGitRef via update target, no package.json (mirrors "should
    // reconcile an existing git checkout to its update target when installing
    // without a ref") ---
    #[test]
    fn git_ensure_ref_update_target_without_package_json() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let fetch_args = s(&[
            "fetch",
            "--prune",
            "--no-tags",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ]);
        let mut machine =
            GitEnsureRefMachine::new(&cfg, &target, fetch_args.clone(), "origin/HEAD", false);
        let (requests, ()) = drive(
            &mut machine,
            vec![ok(""), ok("old-head"), ok("new-head"), ok(""), ok("")],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", fetch_args).with_cwd(&target)
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["reset", "--hard", "origin/HEAD^{commit}"]))
                .with_cwd(&target),
        );
        assert_eq!(
            requests[4],
            CommandRequest::new("git", s(&["clean", "-fdx"])).with_cwd(&target),
        );
        // No package.json -> no npm install.
        assert_eq!(requests.len(), 5);
    }

    // --- update git deps through wrapped pnpm (mirrors "should use plain
    // install through npmCommand argv when updating git package dependencies") ---
    #[test]
    fn git_ensure_ref_wrapped_pnpm_install() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "node@20", "--", "pnpm"])),
        );
        let target = join_path("/tmp/proj", &[".pi", "git", "github.com", "user", "repo"]);
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            &target,
            s(&["fetch", "origin", "main"]),
            "@{upstream}",
            true,
        );
        let (requests, ()) = drive(
            &mut machine,
            vec![
                ok(""),
                ok("local-head"),
                ok("remote-head"),
                ok(""),
                ok(""),
                ok(""),
            ],
        );
        assert_eq!(
            requests[5],
            CommandRequest::new("mise", s(&["exec", "node@20", "--", "pnpm", "install"]))
                .with_cwd(&target),
        );
    }

    // --- npm root -g lookup argv (mirrors "should use npmCommand argv for npm
    // root lookup") ---
    #[test]
    fn global_npm_root_lookup_argv() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "node@20", "--", "npm"])),
        );
        let mut machine = GlobalNpmRootMachine::new(&cfg);
        let root = "/tmp/node20/lib/node_modules";
        let (requests, result) = drive(&mut machine, vec![ok(root)]);
        assert_eq!(
            requests[0],
            CommandRequest::new("mise", s(&["exec", "node@20", "--", "npm", "root", "-g"])),
        );
        assert_eq!(result, root);
    }

    // --- bun global root derives install/global/node_modules ---
    #[test]
    fn global_npm_root_bun_derivation() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["bun"])));
        let mut machine = GlobalNpmRootMachine::new(&cfg);
        let (requests, result) = drive(&mut machine, vec![ok("/home/u/.bun/bin")]);
        assert_eq!(
            requests[0],
            CommandRequest::new("bun", s(&["pm", "bin", "-g"]))
        );
        assert_eq!(
            result,
            join_path("/home/u/.bun", &["install", "global", "node_modules"])
        );
    }

    // --- pnpm global list argv + path extraction (mirrors "should resolve
    // wrapped pnpm global package paths from pnpm list output") ---
    #[test]
    fn pnpm_global_list_argv_and_path() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "node@20", "--", "pnpm"])),
        );
        let mut machine = PnpmGlobalListMachine::new(&cfg, "pnpm-pkg");
        let json = r#"[{"path":"/root","dependencies":{"pnpm-pkg":{"path":"/root/nm/pnpm-pkg"}}}]"#;
        let (requests, result) = drive(&mut machine, vec![ok(json)]);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "mise",
                s(&["exec", "node@20", "--", "pnpm", "list", "-g", "--depth", "0", "--json"]),
            ),
        );
        assert_eq!(result, Some("/root/nm/pnpm-pkg".to_string()));
    }

    #[test]
    fn pnpm_global_list_ignores_malformed() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["pnpm"])));
        let mut machine = PnpmGlobalListMachine::new(&cfg, "pnpm-pkg");
        let (_requests, result) = drive(&mut machine, vec![ok("not json")]);
        assert_eq!(result, None);
    }

    // --- npm update: view then install (mirrors "should update npm range
    // packages using the configured spec") ---
    #[test]
    fn npm_update_probes_then_installs_when_outdated() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let mut machine = NpmUpdateMachine::new(
            &cfg,
            InstallScope::Project,
            "example@^1.0.0",
            Some("^1.0.0".to_string()),
            Some("1.0.0".to_string()),
            "example@^1.0.0",
        );
        let (requests, updated) = drive(&mut machine, vec![ok(r#"["1.0.0","1.2.0"]"#), ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new("npm", s(&["view", "example@^1.0.0", "version", "--json"]))
                .with_cwd("/tmp/proj")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new(
                "npm",
                s(&[
                    "install",
                    "example@^1.0.0",
                    "--prefix",
                    &join_path("/tmp/proj", &[".pi", "npm"]),
                    "--legacy-peer-deps",
                ]),
            )
        );
        assert!(updated);
    }

    // --- npm update up-to-date no-op (mirrors "should skip project npm update
    // when installed version matches latest") ---
    #[test]
    fn npm_update_skips_install_when_current() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let mut machine = NpmUpdateMachine::new(
            &cfg,
            InstallScope::Project,
            "example@^1.0.0",
            Some("^1.0.0".to_string()),
            Some("1.3.1".to_string()),
            "example@^1.0.0",
        );
        let (requests, updated) = drive(&mut machine, vec![ok(r#"["1.0.0","1.3.1","1.0.2"]"#)]);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program, "npm");
        assert_eq!(requests[0].args[0], "view");
        assert!(!updated, "no install should be planned when up to date");
    }

    // --- npm update: no installed version installs directly, no probe (mirrors
    // "should migrate legacy user npm installs into the managed npm root during
    // update") ---
    #[test]
    fn npm_update_installs_directly_without_installed_version() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let mut machine = NpmUpdateMachine::new(
            &cfg,
            InstallScope::User,
            "legacy-pkg",
            None,
            None,
            "legacy-pkg@latest",
        );
        let (requests, updated) = drive(&mut machine, vec![ok("")]);
        assert_eq!(requests.len(), 1, "no npm view probe should be planned");
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "npm",
                s(&[
                    "install",
                    "legacy-pkg@latest",
                    "--prefix",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--legacy-peer-deps",
                ]),
            )
        );
        assert!(updated);
    }

    // --- batch update argv (mirrors "should batch npm updates per scope ...") ---
    #[test]
    fn npm_install_batch_argv() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let mut machine = npm_install(
            &cfg,
            &s(&["user-old@latest", "user-unknown@latest"]),
            InstallScope::User,
        );
        let (requests, ()) = drive(&mut machine, vec![ok("")]);
        assert_eq!(
            requests[0],
            CommandRequest::new(
                "npm",
                s(&[
                    "install",
                    "user-old@latest",
                    "user-unknown@latest",
                    "--prefix",
                    &join_path("/tmp/proj/agent", &["npm"]),
                    "--legacy-peer-deps",
                ]),
            )
        );
    }

    // --- getLatestNpmVersion view argv + parse (mirrors "should use npm view to
    // fetch latest version" and its npmCommand variant) ---
    #[test]
    fn npm_view_version_parses_string() {
        assert_eq!(
            parse_npm_view_version(r#""1.2.3""#, None),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn npm_view_version_selects_max_satisfying() {
        assert_eq!(
            parse_npm_view_version(r#"["1.0.0","1.2.0"]"#, Some("^1.0.0")),
            Some("1.2.0".to_string())
        );
        assert_eq!(
            parse_npm_view_version(r#"["1.0.0","2.0.0"]"#, Some("^1.0.0")),
            Some("1.0.0".to_string())
        );
        assert_eq!(
            parse_npm_view_version(r#"["1.0.0","2.0.0"]"#, None),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn npm_view_version_empty_is_none() {
        assert_eq!(parse_npm_view_version("   ", None), None);
    }

    // --- getLocalGitUpdateTarget upstream path (argv-depends-on-output) ---
    #[test]
    fn local_update_target_upstream_branch() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        let (requests, target) = drive(&mut machine, vec![ok("origin/main"), ok("remote-head")]);
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["rev-parse", "--abbrev-ref", "@{upstream}"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["rev-parse", "@{upstream}"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            target,
            GitUpdateTarget {
                ref_: "@{upstream}".to_string(),
                head: "remote-head".to_string(),
                fetch_args: s(&[
                    "fetch",
                    "--prune",
                    "--no-tags",
                    "origin",
                    "+refs/heads/main:refs/remotes/origin/main",
                ]),
            }
        );
    }

    // --- getLocalGitUpdateTarget fallback chain (set-head -> rev-parse ->
    // symbolic-ref) ---
    #[test]
    fn local_update_target_fallback_chain() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        // upstream probe fails -> set-head, rev-parse origin/HEAD, symbolic-ref.
        let (requests, target) = drive(
            &mut machine,
            vec![
                fail(),
                ok(""),
                ok("head-sha"),
                ok("refs/remotes/origin/trunk"),
            ],
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["remote", "set-head", "origin", "-a"]))
                .with_cwd("/tmp/checkout"),
        );
        assert_eq!(
            requests[2],
            CommandRequest::new("git", s(&["rev-parse", "origin/HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["symbolic-ref", "refs/remotes/origin/HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            target,
            GitUpdateTarget {
                ref_: "origin/HEAD".to_string(),
                head: "head-sha".to_string(),
                fetch_args: s(&[
                    "fetch",
                    "--prune",
                    "--no-tags",
                    "origin",
                    "+refs/heads/trunk:refs/remotes/origin/trunk",
                ]),
            }
        );
    }

    #[test]
    fn local_update_target_fallback_no_symbolic_ref() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        // upstream is non-origin -> set-head, rev-parse origin/HEAD, symbolic-ref
        // (which fails), yielding the +HEAD fallback fetch args.
        let (_requests, target) = drive(
            &mut machine,
            vec![ok("weird/remote"), ok(""), ok("head-sha"), fail()],
        );
        assert_eq!(
            target.fetch_args,
            s(&[
                "fetch",
                "--prune",
                "--no-tags",
                "origin",
                "+HEAD:refs/remotes/origin/HEAD",
            ])
        );
        assert_eq!(target.ref_, "origin/HEAD");
    }

    // --- getRemoteGitHead upstream ls-remote chain ---
    #[test]
    fn remote_head_upstream_ls_remote() {
        let mut machine = GitRemoteHeadMachine::new("/tmp/checkout");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let (requests, result) = drive(
            &mut machine,
            vec![ok("origin/main"), ok(&format!("{sha}\trefs/heads/main"))],
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["ls-remote", "origin", "refs/heads/main"]))
                .with_cwd("/tmp/checkout")
                .with_env("GIT_TERMINAL_PROMPT", "0")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(result, RemoteHead::Head(sha.to_string()));
    }

    #[test]
    fn remote_head_falls_back_to_head_ls_remote() {
        let mut machine = GitRemoteHeadMachine::new("/tmp/checkout");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // No upstream -> ls-remote origin HEAD.
        let (requests, result) = drive(&mut machine, vec![fail(), ok(&format!("{sha}\tHEAD"))]);
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["ls-remote", "origin", "HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_env("GIT_TERMINAL_PROMPT", "0")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(result, RemoteHead::Head(sha.to_string()));
    }

    // --- gitHasAvailableUpdate boolean ---
    #[test]
    fn git_has_update_true_when_heads_differ() {
        let mut machine = GitHasUpdateMachine::new("/tmp/checkout");
        let remote = "0123456789abcdef0123456789abcdef01234567";
        let (requests, has_update) = drive(
            &mut machine,
            vec![
                ok("localsha"),
                ok("origin/main"),
                ok(&format!("{remote}\trefs/heads/main")),
            ],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["rev-parse", "HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert!(has_update);
    }

    #[test]
    fn git_has_update_false_when_local_probe_fails() {
        let mut machine = GitHasUpdateMachine::new("/tmp/checkout");
        let (requests, has_update) = drive(&mut machine, vec![fail()]);
        assert_eq!(requests.len(), 1);
        assert!(!has_update);
    }

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
