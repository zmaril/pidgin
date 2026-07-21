//! Hand-written core implementation behind the generated `PidginCore` trait.
//!
//! The generated binding (`src/generated.rs`) routes every JS-visible op through
//! this trait impl, so the engine wiring lives here — hand-written and stable —
//! while the napi surface is regenerated from the fluessig api schema
//! (`schema/api.json`). See `regen.sh` for the regeneration pipeline.

/// The engine-backed implementation of the generated `Pidgin` contract.
///
/// Stateless: every op delegates straight into the leaf engine crates, reaching
/// the SAME underlying logic the hand-written `#[napi]` exports called before the
/// fluessig swap, so the JS-visible behavior is byte-for-byte unchanged.
///
/// - `version` reports this addon crate's own `CARGO_PKG_VERSION`.
/// - the `path-utils` ops (`expandPath`, `resolveToCwd`, and the three private
///   macOS filename transforms) route into `pidgin_coding::core::tools::path_utils`.
///   The two fallible ops map `PathError` through `anyhow::Error`; because
///   `PathError`'s `Display` is its message and the generated wrapper throws
///   `napi::Error::from_reason(e.to_string())`, the thrown message is identical to
///   the pre-swap hand-written `map_err(|e| Error::from_reason(e.to_string()))`.
/// - the `keys` ops (`parseKey`, `matchesKey`, the two decoders, and
///   `setKittyProtocolActive`) route into `pidgin_tui::keys`. The kitty-protocol
///   flag lives in a Rust static, so the setter and readers share one addon
///   instance and stay consistent — identical to the pre-swap hand-written pair.
/// - the tui width ops (`visibleWidth`, `normalizeTerminalOutput`,
///   `truncateToWidth`, `wrapTextWithAnsi`, `sliceWithWidth`, `extractSegments`)
///   route into the `pidgin_tui` width layer, backing the native `utils.ts`
///   shim. Numeric params/returns cross as `int32` (JS `number`) and are widened
///   to the engine's `i64`/`usize` at the seam, matching the pre-swap `as i64`
///   casts — the JS-visible width values are identical.
/// - the tui word-navigation ops (`findWordBackward`, `findWordForward`) route
///   into `pidgin_tui::word_navigation`'s default-segmenter path, backing the
///   native `word-navigation.ts` shim. Cursors are UTF-16 string indices crossing
///   as `int32` (JS `number`) and widened to the engine's `usize` at the seam —
///   the JS-visible cursor values are identical to the pre-swap wrappers.
pub struct PidginImpl;

impl crate::generated::PidginCore for PidginImpl {
    fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn expand_path(file_path: String) -> anyhow::Result<String> {
        pidgin_coding::core::tools::path_utils::expand_path(&file_path).map_err(anyhow::Error::from)
    }

    fn resolve_to_cwd(file_path: String, cwd: String) -> anyhow::Result<String> {
        pidgin_coding::core::tools::path_utils::resolve_to_cwd(&file_path, &cwd)
            .map_err(anyhow::Error::from)
    }

    fn path_try_macos_screenshot_path(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_macos_screenshot_path(&file_path)
    }

    fn path_try_nfd_variant(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_nfd_variant(&file_path)
    }

    fn path_try_curly_quote_variant(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_curly_quote_variant(&file_path)
    }

    fn parse_key(data: String) -> Option<String> {
        pidgin_tui::parse_key(&data)
    }

    fn matches_key(data: String, key_id: String) -> bool {
        pidgin_tui::matches_key(&data, &key_id)
    }

    fn decode_kitty_printable(data: String) -> Option<String> {
        pidgin_tui::decode_kitty_printable(&data)
    }

    fn decode_printable_key(data: String) -> Option<String> {
        pidgin_tui::decode_printable_key(&data)
    }

    fn set_kitty_protocol_active(active: bool) {
        pidgin_tui::set_kitty_protocol_active(active);
    }

    fn visible_width(s: String) -> i32 {
        pidgin_tui::visible_width(&s) as i32
    }

    fn normalize_terminal_output(s: String) -> String {
        pidgin_tui::normalize_terminal_output(&s)
    }

    fn truncate_to_width(text: String, max_width: i32, ellipsis: String, pad: bool) -> String {
        pidgin_tui::truncate_to_width(&text, max_width as i64, &ellipsis, pad)
    }

    fn wrap_text_with_ansi(text: String, width: i32) -> Vec<String> {
        pidgin_tui::wrap_text_with_ansi(&text, width.max(0) as usize)
    }

    fn slice_with_width(
        line: String,
        start_col: i32,
        length: i32,
        strict: bool,
    ) -> crate::generated::SliceWithWidth {
        let (text, width) =
            pidgin_tui::slice_with_width(&line, start_col as i64, length as i64, strict);
        crate::generated::SliceWithWidth {
            text,
            width: width as i32,
        }
    }

    fn extract_segments(
        line: String,
        before_end: i32,
        after_start: i32,
        after_len: i32,
        strict_after: bool,
    ) -> crate::generated::ExtractSegmentsResult {
        let r = pidgin_tui::extract_segments(
            &line,
            before_end as i64,
            after_start as i64,
            after_len as i64,
            strict_after,
        );
        crate::generated::ExtractSegmentsResult {
            before: r.before,
            before_width: r.before_width as i32,
            after: r.after,
            after_width: r.after_width as i32,
        }
    }

    fn find_word_backward(text: String, cursor: i32) -> i32 {
        pidgin_tui::find_word_backward(
            &text,
            cursor as usize,
            &pidgin_tui::WordNavOptions::default(),
        ) as i32
    }

    fn find_word_forward(text: String, cursor: i32) -> i32 {
        pidgin_tui::find_word_forward(
            &text,
            cursor as usize,
            &pidgin_tui::WordNavOptions::default(),
        ) as i32
    }

    fn parse_git_url(source: String) -> Option<String> {
        let parsed = pidgin_coding::utils::git_url::parse_git_url(&source)?;
        let mut obj = serde_json::Map::new();
        obj.insert("type".to_string(), serde_json::json!(parsed.kind));
        obj.insert("repo".to_string(), serde_json::json!(parsed.repo));
        obj.insert("host".to_string(), serde_json::json!(parsed.host));
        obj.insert("path".to_string(), serde_json::json!(parsed.path));
        if let Some(git_ref) = parsed.git_ref {
            obj.insert("ref".to_string(), serde_json::json!(git_ref));
        }
        obj.insert("pinned".to_string(), serde_json::json!(parsed.pinned));
        Some(serde_json::Value::Object(obj).to_string())
    }

    fn strip_ansi(value: String) -> String {
        pidgin_coding::utils::ansi::strip_ansi(&value)
    }

    fn get_missing_session_cwd_issue(
        session_cwd: String,
        session_file: Option<String>,
        fallback_cwd: String,
    ) -> Option<crate::generated::SessionCwdIssueJs> {
        let source = SessionCwdSourceArgs {
            cwd: session_cwd,
            session_file,
        };
        pidgin_coding::core::session_cwd::get_missing_session_cwd_issue(&source, &fallback_cwd)
            .map(crate::generated::SessionCwdIssueJs::from)
    }

    fn format_missing_session_cwd_error(issue: crate::generated::SessionCwdIssueJs) -> String {
        pidgin_coding::core::session_cwd::format_missing_session_cwd_error(&issue.into())
    }

    fn format_missing_session_cwd_prompt(issue: crate::generated::SessionCwdIssueJs) -> String {
        pidgin_coding::core::session_cwd::format_missing_session_cwd_prompt(&issue.into())
    }

    fn parse_command_args(args_string: String) -> Vec<String> {
        pidgin_agent::harness::prompt_templates::parse_command_args(&args_string)
    }

    fn substitute_args(content: String, args: Vec<String>) -> String {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        pidgin_agent::harness::prompt_templates::substitute_args(&content, &refs)
    }
}

// --- coding-agent session-cwd seam (core/session-cwd.ts) --------------------
//
// The decisions live in `pidgin_coding::core::session_cwd` (the empty-cwd guard
// and the `existsSync` -> `Path::exists` probe); the shim reads the two strings
// off pi's `SessionCwdSource` JS-side and passes them here. `MissingSessionCwdError`
// class identity stays in TS. These From impls and the source adapter bridge the
// generated `SessionCwdIssueJs` DTO to pi's `SessionCwdIssue`, reaching the SAME
// underlying logic the pre-swap hand-written exports called.
use pidgin_coding::core::session_cwd::{SessionCwdIssue, SessionCwdSource as CoreSessionCwdSource};

impl From<SessionCwdIssue> for crate::generated::SessionCwdIssueJs {
    fn from(issue: SessionCwdIssue) -> Self {
        Self {
            session_file: issue.session_file,
            session_cwd: issue.session_cwd,
            fallback_cwd: issue.fallback_cwd,
        }
    }
}

impl From<crate::generated::SessionCwdIssueJs> for SessionCwdIssue {
    fn from(issue: crate::generated::SessionCwdIssueJs) -> Self {
        Self {
            session_file: issue.session_file,
            session_cwd: issue.session_cwd,
            fallback_cwd: issue.fallback_cwd,
        }
    }
}

/// The two strings the shim reads from pi's `SessionCwdSource` (`getCwd()` /
/// `getSessionFile()`), adapting them to the Rust trait so the real port owns
/// the empty-cwd guard and filesystem probe — no logic is reimplemented here.
struct SessionCwdSourceArgs {
    cwd: String,
    session_file: Option<String>,
}

impl CoreSessionCwdSource for SessionCwdSourceArgs {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }
    fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }
}

// --- tui keybindings layer (packages/tui/src/keybindings.ts) ----------------
//
// The engine-backed implementation behind the generated `KeybindingsManagerCore`
// handle class (its ctor + `matches`/`getKeys`/`getConflictsJson`/
// `getResolvedBindingsJson` methods). Wraps `pidgin_tui::KeybindingsManager`,
// reaching the SAME resolution logic the hand-written `#[napi]` class called
// before the fluessig swap. The core is immutable per construction (all `&self`);
// the shim's `setUserBindings` builds a fresh core. Definitions and user bindings
// cross as ordered JSON arrays (not objects) so JS insertion order is preserved
// without relying on serde_json's `preserve_order` feature.

/// JSON shape of a keybinding definition crossing into the ctor
/// (`[{ id, defaultKeys, description? }]`).
#[derive(serde::Deserialize)]
struct KeybindingDefinitionIn {
    id: String,
    #[serde(rename = "defaultKeys")]
    default_keys: Vec<String>,
    description: Option<String>,
}

/// JSON shape of a user binding crossing into the ctor (`[{ id, keys }]`).
#[derive(serde::Deserialize)]
struct UserBindingIn {
    id: String,
    // `null` = pi's explicit `undefined` (falls back to the default keys).
    keys: Option<Vec<String>>,
}

/// The engine-backed implementation of the generated `KeybindingsManagerCore`
/// contract. Holds one immutable `pidgin_tui::KeybindingsManager`; the generated
/// handle class owns it as `Arc<KeybindingsManagerCoreImpl>` and delegates each
/// method straight through, so the JS-visible behavior is byte-for-byte unchanged
/// from the pre-swap hand-written class. The ctor reproduces the hand-written
/// parse-error messages (`invalid definitions: …` / `invalid userBindings: …`)
/// via `anyhow`, which the generated wrapper throws through
/// `napi::Error::from_reason(e.to_string())`.
pub struct KeybindingsManagerCoreImpl {
    inner: pidgin_tui::KeybindingsManager,
}

impl crate::generated::KeybindingsManagerCoreCore for KeybindingsManagerCoreImpl {
    fn new(definitions_json: String, user_bindings_json: String) -> anyhow::Result<Self> {
        let defs_in: Vec<KeybindingDefinitionIn> = serde_json::from_str(&definitions_json)
            .map_err(|e| anyhow::anyhow!("invalid definitions: {e}"))?;
        let user_in: Vec<UserBindingIn> = serde_json::from_str(&user_bindings_json)
            .map_err(|e| anyhow::anyhow!("invalid userBindings: {e}"))?;

        let defs_owned: Vec<(String, pidgin_tui::KeybindingDefinition)> = defs_in
            .into_iter()
            .map(|d| {
                (
                    d.id,
                    pidgin_tui::KeybindingDefinition {
                        default_keys: d.default_keys,
                        description: d.description,
                    },
                )
            })
            .collect();
        let definitions: Vec<(&str, pidgin_tui::KeybindingDefinition)> = defs_owned
            .iter()
            .map(|(id, def)| (id.as_str(), def.clone()))
            .collect();
        let user_bindings: Vec<(&str, Option<Vec<String>>)> = user_in
            .iter()
            .map(|u| (u.id.as_str(), u.keys.clone()))
            .collect();

        Ok(Self {
            inner: pidgin_tui::KeybindingsManager::new(definitions, user_bindings),
        })
    }

    fn matches(&self, data: String, keybinding: String) -> bool {
        self.inner.matches(&data, &keybinding)
    }

    fn get_keys(&self, keybinding: String) -> Vec<String> {
        self.inner.get_keys(&keybinding)
    }

    fn get_conflicts_json(&self) -> anyhow::Result<String> {
        let conflicts: Vec<serde_json::Value> = self
            .inner
            .get_conflicts()
            .into_iter()
            .map(|c| serde_json::json!({ "key": c.key, "keybindings": c.keybindings }))
            .collect();
        serde_json::to_string(&conflicts).map_err(anyhow::Error::from)
    }

    fn get_resolved_bindings_json(&self) -> anyhow::Result<String> {
        let resolved: Vec<(String, Vec<String>)> = self.inner.get_resolved_bindings();
        serde_json::to_string(&resolved).map_err(anyhow::Error::from)
    }
}

// --- package-manager command flow (coding-agent/src/core/package-manager.ts) --
//
// The engine-backed implementation behind the generated `CommandCore` handle
// class (its ctor + `start`/`advance` methods). Wraps one boxed
// `pidgin_coding::core::command_flow::CommandFlowMachine`, reaching the SAME
// command-flow planning logic the hand-written `#[napi]` class called before the
// fluessig swap. The machine carries mutable step state (`&mut self` on
// `start`/`advance`), while fluessig's generated handle methods take `&self`, so
// the core holds the machine behind a `Mutex` — interior mutability is the bridge
// that lets a `&self` trait method drive a `&mut self` machine. JSON crosses
// in/out exactly as the pre-swap class defined (the `CommandStep` / `CommandOutput`
// wire shapes), and the error messages are reproduced verbatim through `anyhow`
// (the generated wrapper throws `napi::Error::from_reason(e.to_string())`), so the
// JS-visible behavior is byte-for-byte unchanged.
//
// straitjacket-allow-file:duplication is not needed here: the per-op match arms
// live in `build_machine` below and mirror pi's package-manager operations.

use pidgin_ai::seams::subprocess::CommandOutput;
use pidgin_coding::core::command_flow::{CommandFlowMachine, CommandStep};
use pidgin_coding::core::package_manager::{
    git_dependency_install, npm_install, npm_uninstall, GitCloneMachine, GitEnsureRefMachine,
    GitHasUpdateMachine, GitLocalUpdateTargetMachine, GitRemoteHeadMachine, GlobalNpmRootMachine,
    InstallScope, PackageManagerConfig, PnpmGlobalListMachine,
};

/// JSON shape of the package-manager config the command argv depends on
/// (pi's `options.cwd` / `options.agentDir` / `settings.npmCommand`), parsed at
/// the boundary and mapped onto [`PackageManagerConfig`].
#[derive(Debug, serde::Deserialize)]
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
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParamsJson {
    config: Option<ConfigJson>,
    #[serde(default)]
    specs: Vec<String>,
    name: Option<String>,
    scope: Option<String>,
    package_name: Option<String>,
    target_dir: Option<String>,
    repo: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    #[serde(default)]
    fetch_args: Vec<String>,
    #[serde(default)]
    has_package_json: bool,
    installed_path: Option<String>,
    // Note: the `npm view` version probe (pi's getLatestNpmVersion) is
    // deliberately not an op here. pi's parseSource expands version ranges into
    // node-semver syntax (e.g. `>=1.0.0 <2.0.0-0`), which the machines'
    // Cargo-style `semver::VersionReq` cannot parse; the shim keeps that method
    // on pi's original rather than silently mis-select versions.
}

impl ParamsJson {
    fn config(&self) -> anyhow::Result<PackageManagerConfig> {
        self.config
            .as_ref()
            .map(|c| {
                PackageManagerConfig::new(c.cwd.clone(), c.agent_dir.clone(), c.npm_command.clone())
            })
            .ok_or_else(|| anyhow::anyhow!("missing `config` for op"))
    }

    fn scope(&self) -> anyhow::Result<InstallScope> {
        match self.scope.as_deref() {
            Some("user") => Ok(InstallScope::User),
            Some("project") => Ok(InstallScope::Project),
            other => Err(anyhow::anyhow!(
                "invalid or missing scope: {other:?} (expected \"user\" | \"project\")"
            )),
        }
    }

    fn require(&self, field: Option<&String>, name: &str) -> anyhow::Result<String> {
        field
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing `{name}` for op"))
    }
}

/// Build the boxed machine for `op` from its JSON params.
fn build_machine(
    op: &str,
    params: &ParamsJson,
) -> anyhow::Result<Box<dyn CommandFlowMachine + Send>> {
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
        other => Err(anyhow::anyhow!("unknown CommandCore op: {other}")),
    }
}

/// Serialize a [`CommandStep`] to the driver-loop JSON contract.
fn step_to_json(step: CommandStep) -> anyhow::Result<String> {
    let value = match step {
        CommandStep::Run { request } => {
            let request = serde_json::to_value(&request).map_err(anyhow::Error::from)?;
            serde_json::json!({ "type": "run", "request": request })
        }
        CommandStep::Done { result } => serde_json::json!({ "type": "done", "result": result }),
    };
    serde_json::to_string(&value).map_err(anyhow::Error::from)
}

/// The engine-backed implementation of the generated `CommandCore` contract.
/// Holds one boxed [`CommandFlowMachine`] behind a `Mutex`; the generated handle
/// class owns it as `Arc<CommandCoreImpl>` and delegates `start`/`advance`
/// straight through. The `Mutex` supplies the interior mutability the machine's
/// `&mut self` steps need behind the generated `&self` method receivers — a
/// single JS caller never contends, so the lock is uncontended in practice.
pub struct CommandCoreImpl {
    machine: std::sync::Mutex<Box<dyn CommandFlowMachine + Send>>,
}

impl crate::generated::CommandCoreCore for CommandCoreImpl {
    fn new(op: String, params_json: String) -> anyhow::Result<Self> {
        let params: ParamsJson = if params_json.trim().is_empty() {
            serde_json::from_str("{}")
        } else {
            serde_json::from_str(&params_json)
        }
        .map_err(|e| anyhow::anyhow!("invalid CommandCore params: {e}"))?;
        Ok(Self {
            machine: std::sync::Mutex::new(build_machine(&op, &params)?),
        })
    }

    fn start(&self) -> anyhow::Result<String> {
        let mut machine = self.machine.lock().unwrap();
        step_to_json(machine.start())
    }

    fn advance(&self, output_json: String) -> anyhow::Result<String> {
        let output: CommandOutput = serde_json::from_str(&output_json)
            .map_err(|e| anyhow::anyhow!("invalid CommandOutput: {e}"))?;
        let mut machine = self.machine.lock().unwrap();
        step_to_json(machine.advance(output))
    }
}

// --- faux provider surface (ai/src/providers/faux.ts) -----------------------
//
// The engine-backed implementation behind the generated `FauxCore` handle class
// (its ctor + 8 stream/query methods). Wraps one `pidgin_ai::providers::faux::
// FauxProvider` plus the settable `FakeClock` shared with it, reaching the SAME
// deterministic delta-streaming + prompt-cache/call-count logic the hand-written
// `#[napi]` class called before the fluessig swap. The provider carries its own
// interior mutability (`Mutex`-guarded call count, pending queue, and prompt
// cache), so — unlike `CommandCore`/`TuiCore` — no extra `Mutex` shim is needed:
// every method is already `&self`, matching the generated handle receivers. JSON
// crosses in/out exactly as the pre-swap class defined, and the hand-written
// parse-error messages (`invalid faux options: …`, `invalid context: …`,
// `invalid stream options: …`, `invalid model: …`, `invalid message: …`) are
// reproduced verbatim through `anyhow` (the generated wrapper throws
// `napi::Error::from_reason(e.to_string())`, and `anyhow::Error`'s `Display`
// forwards the wrapped error's own message), so the JS-visible behavior is
// byte-for-byte unchanged.
//
// straitjacket-allow-file:duplication is not needed here: the per-method parse /
// build-seams / call-provider / serialize shape mirrors pi's faux surface and is
// kept distinct, but lives in one impl block below.

use pidgin_ai::providers::faux::{FauxModelDefinition, FauxProvider, RegisterFauxProviderOptions};
use pidgin_ai::seams::clock::FakeClock;
use pidgin_ai::seams::provider::{AbortSignal, Provider};
use pidgin_ai::types::{AssistantMessage, Context, Modality, ModelCost, StreamOptions};

/// JSON shape of pi's `RegisterFauxProviderOptions` (`faux.ts:105-114`), parsed
/// at the boundary and mapped onto the builder options.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OptionsJson {
    api: Option<String>,
    provider: Option<String>,
    models: Option<Vec<ModelDefJson>>,
    tokens_per_second: Option<f64>,
    token_size: Option<TokenSizeJson>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TokenSizeJson {
    min: Option<u32>,
    max: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDefJson {
    id: String,
    name: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<Modality>>,
    cost: Option<ModelCost>,
    context_window: Option<u32>,
    max_tokens: Option<u32>,
}

fn build_faux_options(json: &str) -> anyhow::Result<RegisterFauxProviderOptions> {
    let parsed: OptionsJson = if json.trim().is_empty() {
        OptionsJson::default()
    } else {
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("invalid faux options: {e}"))?
    };
    let token_size = parsed.token_size.unwrap_or_default();
    Ok(RegisterFauxProviderOptions {
        api: parsed.api,
        provider: parsed.provider,
        models: parsed.models.map(|defs| {
            defs.into_iter()
                .map(|d| FauxModelDefinition {
                    id: d.id,
                    name: d.name,
                    reasoning: d.reasoning,
                    input: d.input,
                    cost: d.cost,
                    context_window: d.context_window.map(u64::from),
                    max_tokens: d.max_tokens.map(u64::from),
                })
                .collect()
        }),
        tokens_per_second: parsed.tokens_per_second,
        token_size_min: token_size.min.map(u64::from),
        token_size_max: token_size.max.map(u64::from),
    })
}

fn parse_faux_context(json: &str) -> anyhow::Result<Context> {
    serde_json::from_str(json).map_err(|e| anyhow::anyhow!("invalid context: {e}"))
}

fn parse_faux_options(json: Option<String>) -> anyhow::Result<Option<StreamOptions>> {
    match json {
        None => Ok(None),
        Some(s) if s.trim().is_empty() || s == "null" => Ok(None),
        Some(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid stream options: {e}")),
    }
}

/// The engine-backed implementation of the generated `FauxCore` contract. Holds
/// one `FauxProvider` (with its own interior mutability for the call count and
/// prompt cache) plus the `FakeClock` shared with it; the generated handle class
/// owns it as `Arc<FauxCoreImpl>` and delegates each `&self` method straight
/// through, so the JS-visible behavior is byte-for-byte unchanged from the
/// pre-swap hand-written class.
pub struct FauxCoreImpl {
    inner: FauxProvider,
    clock: FakeClock,
}

impl crate::generated::FauxCoreCore for FauxCoreImpl {
    fn new(options_json: String) -> anyhow::Result<Self> {
        let (inner, clock) = FauxProvider::with_fake_clock(build_faux_options(&options_json)?);
        Ok(Self { inner, clock })
    }

    fn set_now_ms(&self, now_ms: i64) {
        self.clock.set_now_ms(now_ms);
    }

    fn api(&self) -> String {
        self.inner.api().to_string()
    }

    fn models_json(&self) -> anyhow::Result<String> {
        serde_json::to_string(self.inner.models()).map_err(anyhow::Error::from)
    }

    fn get_model_json(&self, id: Option<String>) -> anyhow::Result<Option<String>> {
        match self.inner.get_model(id.as_deref()) {
            Some(model) => serde_json::to_string(&model)
                .map(Some)
                .map_err(anyhow::Error::from),
            None => Ok(None),
        }
    }

    fn bump_call_count(&self) -> i64 {
        self.inner.bump_call_count() as i64
    }

    fn call_count(&self) -> i64 {
        self.inner.call_count() as i64
    }

    fn stream_resolved(
        &self,
        model_json: String,
        context_json: String,
        options_json: Option<String>,
        message_json: String,
        aborted: bool,
    ) -> anyhow::Result<String> {
        let model =
            serde_json::from_str(&model_json).map_err(|e| anyhow::anyhow!("invalid model: {e}"))?;
        let context = parse_faux_context(&context_json)?;
        let options = parse_faux_options(options_json)?;
        let message: AssistantMessage = serde_json::from_str(&message_json)
            .map_err(|e| anyhow::anyhow!("invalid message: {e}"))?;
        let signal = if aborted {
            Some(AbortSignal::aborted())
        } else {
            None
        };
        let result = self.inner.stream_resolved(
            &model,
            &context,
            options.as_ref(),
            message,
            signal.as_ref(),
        );
        serde_json::to_string(&result).map_err(anyhow::Error::from)
    }

    fn empty_queue_result(
        &self,
        model_json: String,
        context_json: String,
        options_json: Option<String>,
    ) -> anyhow::Result<String> {
        let model =
            serde_json::from_str(&model_json).map_err(|e| anyhow::anyhow!("invalid model: {e}"))?;
        let context = parse_faux_context(&context_json)?;
        let options = parse_faux_options(options_json)?;
        let result = self
            .inner
            .empty_queue_result(&model, &context, options.as_ref());
        serde_json::to_string(&result).map_err(anyhow::Error::from)
    }
}

// --- tui stdin-buffer surface (tui/src/stdin-buffer.ts) ---------------------
//
// The engine-backed implementation behind the generated `StdinBufferCore` handle
// class (its ctor + `process`/`flush`/`getBuffer`/`clear` methods). Wraps one
// `pidgin_tui::StdinBuffer` — the SAME escape-sequence splitter / bracketed-paste
// / Kitty-dedup state machine the hand-written `#[napi]` class drove before the
// fluessig swap. The splitter's stepping methods take `&mut self`, so — like
// `CommandCore`/`TuiCore` — the core holds it behind a `Mutex` to supply the
// interior mutability the generated `&self` receivers need; a single JS caller
// never contends, so the lock is uncontended in practice. Strings/plain objects
// cross in/out exactly as the pre-swap class defined (the ctor is infallible and
// no method is fallible, so there are no error messages to reproduce), leaving
// the JS-visible behavior byte-for-byte unchanged.

use crate::generated::StdinEventJs;
use pidgin_tui::{StdinBuffer, StdinBufferOptions, StdinEvent};

impl From<StdinEvent> for StdinEventJs {
    fn from(event: StdinEvent) -> Self {
        match event {
            StdinEvent::Data(value) => Self {
                kind: "data".to_string(),
                value,
            },
            StdinEvent::Paste(value) => Self {
                kind: "paste".to_string(),
                value,
            },
        }
    }
}

/// The engine-backed implementation of the generated `StdinBufferCore` contract.
/// Holds one [`StdinBuffer`] behind a `Mutex`; the generated handle class owns it
/// as `Arc<StdinBufferCoreImpl>` and delegates each `&self` method straight
/// through, so the JS-visible behavior is byte-for-byte unchanged from the
/// pre-swap hand-written class.
pub struct StdinBufferCoreImpl {
    inner: std::sync::Mutex<StdinBuffer>,
}

impl crate::generated::StdinBufferCoreCore for StdinBufferCoreImpl {
    fn new(timeout_ms: Option<i64>) -> anyhow::Result<Self> {
        let options = match timeout_ms {
            Some(ms) => StdinBufferOptions {
                timeout_ms: ms.max(0) as u64,
            },
            None => StdinBufferOptions::default(),
        };
        Ok(Self {
            inner: std::sync::Mutex::new(StdinBuffer::new(options)),
        })
    }

    fn process(&self, data: String) -> Vec<StdinEventJs> {
        self.inner
            .lock()
            .unwrap()
            .process(&data)
            .into_iter()
            .map(StdinEventJs::from)
            .collect()
    }

    fn flush(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .flush()
            .into_iter()
            .map(|event| match event {
                StdinEvent::Data(value) | StdinEvent::Paste(value) => value,
            })
            .collect()
    }

    fn get_buffer(&self) -> String {
        self.inner.lock().unwrap().buffer().to_string()
    }

    fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}
