//! npm-flavoured operations: install/uninstall one-shots, the global-root and
//! pnpm-global-path probes, the version-probe-then-install update machine, and
//! `npm view` version parsing. Mirrors pi's `runCommand*` npm argv.

use atilla_ai::seams::subprocess::{CommandOutput, CommandRequest};
use serde_json::{json, Value};
use std::path::PathBuf;

use super::config::{
    git_dependency_install_args, join_path, npm_install_args, npm_uninstall_args, strings,
    InstallScope, PackageManagerConfig, NETWORK_TIMEOUT_MS,
};
use crate::core::command_flow::{CommandFlowMachine, CommandStep, OneShotCommand};

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
/// `root -g` and trims. `Done` carries the computed root as a JSON string.
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
    fn start(&mut self) -> CommandStep {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done { result: json!("") },
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
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
        CommandStep::Done {
            result: json!(result),
        }
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
    fn start(&mut self) -> CommandStep {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done {
                result: Value::Null,
            },
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        let result = parse_pnpm_global_path(&output.stdout, &self.package_name);
        CommandStep::Done {
            result: json!(result),
        }
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

// `Done` carries `true` when an install command was planned, `false` for the
// up-to-date no-op.
impl CommandFlowMachine for NpmUpdateMachine {
    fn start(&mut self) -> CommandStep {
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

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
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
                    CommandStep::Done {
                        result: json!(false),
                    }
                }
            }
            UpdatePhase::AwaitInstall => {
                self.phase = UpdatePhase::Done;
                CommandStep::Done {
                    result: json!(true),
                }
            }
            UpdatePhase::Start | UpdatePhase::Done => CommandStep::Done {
                result: json!(false),
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package_manager::test_support::{drive, ok, s};

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
        let (requests, _) = drive(&mut machine, vec![ok("")]);
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
        let (requests, _) = drive(&mut machine, vec![ok("")]);
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
        let (requests, _) = drive(&mut machine, vec![ok("")]);
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
        let (requests, _) = drive(&mut machine, vec![ok("")]);
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
        assert_eq!(result, json!(root));
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
            json!(join_path(
                "/home/u/.bun",
                &["install", "global", "node_modules"]
            ))
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
        assert_eq!(result, json!("/root/nm/pnpm-pkg"));
    }

    #[test]
    fn pnpm_global_list_ignores_malformed() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["pnpm"])));
        let mut machine = PnpmGlobalListMachine::new(&cfg, "pnpm-pkg");
        let (_requests, result) = drive(&mut machine, vec![ok("not json")]);
        assert_eq!(result, Value::Null);
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
        assert_eq!(updated, json!(true));
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
        assert_eq!(
            updated,
            json!(false),
            "no install should be planned when up to date"
        );
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
        assert_eq!(updated, json!(true));
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
        let (requests, _) = drive(&mut machine, vec![ok("")]);
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
}
