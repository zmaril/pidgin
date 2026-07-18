//! Node-API surface for the package-manager command flow (`CommandCore`).
//!
//! This exposes the Rust command-flow state machines
//! ([`atilla_coding::core::package_manager`], driven through
//! [`atilla_coding::core::command_flow::CommandFlowMachine`]) to pi's
//! `package-manager.test.ts`. pi's `DefaultPackageManager` reaches the outside
//! world through three private runners — `runCommand`, `runCommandCapture`, and
//! `runCommandSync` — whose exact argv (and, where present, `cwd` / `timeoutMs` /
//! `env`) the suite spies and asserts.
//!
//! Rather than spawn processes, each package-manager operation is a pure
//! [`CommandFlowMachine`] that *plans* the next [`CommandRequest`]. `CommandCore`
//! wraps one boxed machine behind a JSON in/out driver loop; the JS shim
//! (`package-manager.ts`) constructs a `CommandCore` per operation, then:
//!
//! ```text
//! let step = JSON.parse(core.start());
//! while (step.type === "run") {
//!   const out = <run step.request via pi's own runCommand*/runCommandSync>;
//!   step = JSON.parse(core.advance(JSON.stringify(out)));
//! }
//! return step.result; // step.type === "done"
//! ```
//!
//! # Driver-loop contract
//!
//! - [`CommandCore::start`] / [`CommandCore::advance`] return a JSON-serialized
//!   `CommandStep`:
//!   - a run step is `{ "type": "run", "request": { program, args, cwd, env,
//!     timeoutMs } }` — the wire shape of [`CommandRequest`] (`env` is an array
//!     of `[name, value]` pairs; `cwd`/`timeoutMs` are `null` when unset).
//!   - a done step is `{ "type": "done", "result": <value> }` — the operation's
//!     serialized result (`null` for the one-shots that plan a command and
//!     discard the output).
//! - [`CommandCore::advance`] consumes a JSON `CommandOutput` (`{ code, stdout,
//!   stderr }`), the result of running the last planned request.
//!
//! Both methods are pure and synchronous (the machines carry no tokio runtime),
//! so the JS driver owns all async/subprocess concerns.

// straitjacket-allow-file:duplication — the op dispatcher's per-op arms share a
// faithful "parse params / build machine" shape at the Node boundary; the arms
// mirror pi's package-manager operations and are kept distinct.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Deserialize;
use serde_json::{json, Value};

use atilla_ai::seams::subprocess::{CommandOutput, CommandRequest};
use atilla_coding::core::command_flow::{CommandFlowMachine, CommandStep};
use atilla_coding::core::package_manager::{
    git_dependency_install, npm_install, npm_uninstall, parse_npm_view_version, GitCloneMachine,
    GitEnsureRefMachine, GitHasUpdateMachine, GitLocalUpdateTargetMachine, GitRemoteHeadMachine,
    GlobalNpmRootMachine, InstallScope, PackageManagerConfig, PnpmGlobalListMachine,
    NETWORK_TIMEOUT_MS,
};

/// JSON shape of the package-manager config the command argv depends on
/// (pi's `options.cwd` / `options.agentDir` / `settings.npmCommand`), parsed at
/// the boundary and mapped onto [`PackageManagerConfig`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigJson {
    cwd: String,
    agent_dir: String,
    #[serde(default)]
    npm_command: Option<Vec<String>>,
}

/// JSON shape of the per-op params blob. Every field is optional; each op reads
/// the fields it needs (mirroring pi's already-parsed inputs). `config` is
/// required for the ops that build argv from a package-manager command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParamsJson {
    config: Option<ConfigJson>,
    #[serde(default)]
    specs: Vec<String>,
    name: Option<String>,
    scope: Option<String>,
    package_name: Option<String>,
    view_spec: Option<String>,
    range: Option<String>,
    target_dir: Option<String>,
    repo: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    #[serde(default)]
    fetch_args: Vec<String>,
    #[serde(default)]
    has_package_json: bool,
    installed_path: Option<String>,
}

impl ParamsJson {
    fn config(&self) -> Result<PackageManagerConfig> {
        self.config
            .as_ref()
            .map(|c| PackageManagerConfig::new(c.cwd.clone(), c.agent_dir.clone(), c.npm_command.clone()))
            .ok_or_else(|| Error::from_reason("missing `config` for op"))
    }

    fn scope(&self) -> Result<InstallScope> {
        match self.scope.as_deref() {
            Some("user") => Ok(InstallScope::User),
            Some("project") => Ok(InstallScope::Project),
            other => Err(Error::from_reason(format!(
                "invalid or missing scope: {other:?} (expected \"user\" | \"project\")"
            ))),
        }
    }

    fn require(&self, field: Option<&String>, name: &str) -> Result<String> {
        field
            .cloned()
            .ok_or_else(|| Error::from_reason(format!("missing `{name}` for op")))
    }
}

/// pi's `getLatestNpmVersion` command flow: plan a single
/// `<npm> view <spec> version --json` capture (timed, pinned to `cwd`), then
/// parse the reported version out of the JSON. `Done` carries the version string
/// or `null` (pi throws in that case; the shim maps `null` back to a throw).
struct NpmViewMachine {
    request: Option<CommandRequest>,
    range: Option<String>,
}

impl NpmViewMachine {
    fn new(cfg: &PackageManagerConfig, view_spec: &str, range: Option<String>) -> Self {
        let sub_args = vec![
            "view".to_string(),
            view_spec.to_string(),
            "version".to_string(),
            "--json".to_string(),
        ];
        let request = cfg
            .npm_command_request(&sub_args, Some(&cfg.cwd))
            .with_timeout(NETWORK_TIMEOUT_MS);
        Self {
            request: Some(request),
            range,
        }
    }
}

impl CommandFlowMachine for NpmViewMachine {
    fn start(&mut self) -> CommandStep {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done {
                result: Value::Null,
            },
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        let result = parse_npm_view_version(&output.stdout, self.range.as_deref());
        CommandStep::Done {
            result: match result {
                Some(version) => json!(version),
                None => Value::Null,
            },
        }
    }
}

/// Build the boxed machine for `op` from its JSON params.
fn build_machine(op: &str, params: &ParamsJson) -> Result<Box<dyn CommandFlowMachine>> {
    match op {
        "npmInstall" => {
            let cfg = params.config()?;
            let scope = params.scope()?;
            Ok(Box::new(npm_install(&cfg, &params.specs, scope)))
        }
        "npmUninstall" => {
            let cfg = params.config()?;
            let scope = params.scope()?;
            let name = params.require(params.name.as_ref(), "name")?;
            Ok(Box::new(npm_uninstall(&cfg, &name, scope)))
        }
        "gitDependencyInstall" => {
            let cfg = params.config()?;
            let target_dir = params.require(params.target_dir.as_ref(), "targetDir")?;
            Ok(Box::new(git_dependency_install(&cfg, &target_dir)))
        }
        "npmGlobalRoot" => {
            let cfg = params.config()?;
            Ok(Box::new(GlobalNpmRootMachine::new(&cfg)))
        }
        "pnpmGlobalPath" => {
            let cfg = params.config()?;
            let package_name = params.require(params.package_name.as_ref(), "packageName")?;
            Ok(Box::new(PnpmGlobalListMachine::new(&cfg, package_name)))
        }
        "npmView" => {
            let cfg = params.config()?;
            let view_spec = params.require(params.view_spec.as_ref(), "viewSpec")?;
            Ok(Box::new(NpmViewMachine::new(&cfg, &view_spec, params.range.clone())))
        }
        "gitEnsureRef" => {
            let cfg = params.config()?;
            let target_dir = params.require(params.target_dir.as_ref(), "targetDir")?;
            let ref_ = params.require(params.ref_.as_ref(), "ref")?;
            Ok(Box::new(GitEnsureRefMachine::new(
                &cfg,
                target_dir,
                params.fetch_args.clone(),
                &ref_,
                params.has_package_json,
            )))
        }
        "gitClone" => {
            let cfg = params.config()?;
            let repo = params.require(params.repo.as_ref(), "repo")?;
            let target_dir = params.require(params.target_dir.as_ref(), "targetDir")?;
            Ok(Box::new(GitCloneMachine::new(
                &cfg,
                repo,
                target_dir,
                params.ref_.clone(),
                params.has_package_json,
            )))
        }
        "gitLocalUpdateTarget" => {
            let installed_path = params.require(params.installed_path.as_ref(), "installedPath")?;
            Ok(Box::new(GitLocalUpdateTargetMachine::new(installed_path)))
        }
        "gitRemoteHead" => {
            let installed_path = params.require(params.installed_path.as_ref(), "installedPath")?;
            Ok(Box::new(GitRemoteHeadMachine::new(installed_path)))
        }
        "gitHasUpdate" => {
            let installed_path = params.require(params.installed_path.as_ref(), "installedPath")?;
            Ok(Box::new(GitHasUpdateMachine::new(installed_path)))
        }
        other => Err(Error::from_reason(format!("unknown CommandCore op: {other}"))),
    }
}

/// Serialize a [`CommandStep`] to the driver-loop JSON contract.
fn step_to_json(step: CommandStep) -> Result<String> {
    let value = match step {
        CommandStep::Run { request } => {
            let request = serde_json::to_value(&request)
                .map_err(|e| Error::from_reason(e.to_string()))?;
            json!({ "type": "run", "request": request })
        }
        CommandStep::Done { result } => json!({ "type": "done", "result": result }),
    };
    serde_json::to_string(&value).map_err(|e| Error::from_reason(e.to_string()))
}

/// The Rust-backed package-manager command flow, exposed to JavaScript as
/// `CommandCore`. Holds one boxed [`CommandFlowMachine`]; the JS shim drives it
/// through [`CommandCore::start`] / [`CommandCore::advance`].
#[napi(js_name = "CommandCore")]
pub struct CommandCore {
    machine: Box<dyn CommandFlowMachine>,
}

#[napi]
impl CommandCore {
    /// Build a command core for `op` from its JSON params (see the module docs
    /// for the op catalog and param shapes).
    #[napi(constructor)]
    pub fn new(op: String, params_json: String) -> Result<Self> {
        let params: ParamsJson = if params_json.trim().is_empty() {
            serde_json::from_str("{}")
        } else {
            serde_json::from_str(&params_json)
        }
        .map_err(|e| Error::from_reason(format!("invalid CommandCore params: {e}")))?;
        Ok(Self {
            machine: build_machine(&op, &params)?,
        })
    }

    /// Plan the first command (or finish immediately). Returns the JSON-encoded
    /// `CommandStep` (`{ type: "run", request }` or `{ type: "done", result }`).
    #[napi]
    pub fn start(&mut self) -> Result<String> {
        step_to_json(self.machine.start())
    }

    /// Consume the JSON-encoded `CommandOutput` (`{ code, stdout, stderr }`) of
    /// the last planned command and plan the next one (or finish). Returns the
    /// JSON-encoded `CommandStep`.
    #[napi]
    pub fn advance(&mut self, output_json: String) -> Result<String> {
        let output: CommandOutput = serde_json::from_str(&output_json)
            .map_err(|e| Error::from_reason(format!("invalid CommandOutput: {e}")))?;
        step_to_json(self.machine.advance(output))
    }
}
