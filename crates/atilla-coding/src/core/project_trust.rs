//! Resolve whether a project directory is trusted.
//!
//! Ported from pi's `core/project-trust.ts`. [`resolve_project_trusted`] applies
//! pi's precedence: an explicit override wins; a project with no
//! trust-requiring resources is trusted implicitly; a `project_trust` extension
//! decision is consulted next; then the persisted [`ProjectTrustStore`]
//! decision; then the `default_project_trust` policy; and finally, only when a
//! UI is available, an interactive prompt.
//!
//! NOTE: pi's collaborators here are large and unported — the extension runner
//! (`emitProjectTrustEvent`), the settings manager (`DefaultProjectTrust`), and
//! the interactive UI. This port models each as a small trait/enum seam so the
//! decision logic is exercisable and self-contained. pi's version is `async`
//! (the UI and extension calls await); the port is synchronous, matching the
//! crate's blocking FFI surface — the async bridge lands with the real UI layer.
//! pi's extension decision is the string union `"yes" | "no" | "undecided"`;
//! the seam here surfaces a resolved `Option<bool>` (`None` == undecided).

use crate::core::trust_manager::{
    get_project_trust_options, has_trust_requiring_project_resources, ProjectTrustOption,
    ProjectTrustStore, TrustStoreError,
};

/// pi's `CONFIG_DIR_NAME`, duplicated (see `trust_manager`).
const CONFIG_DIR_NAME: &str = ".pi";

/// Application run mode (pi's `AppMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Interactive TUI session.
    Interactive,
    /// One-shot print mode.
    Print,
    /// Structured JSON output mode.
    Json,
    /// RPC server mode.
    Rpc,
}

/// Global `default_project_trust` policy (pi's `DefaultProjectTrust`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultProjectTrust {
    /// Prompt the user when no decision is recorded (pi's default).
    #[default]
    Ask,
    /// Trust every project by default.
    Always,
    /// Trust no project by default.
    Never,
}

/// The interactive context used to prompt for a trust decision (pi's
/// `ProjectTrustContext`, reduced to what this function needs).
pub trait ProjectTrustContext {
    /// Whether an interactive UI is available.
    fn has_ui(&self) -> bool;
    /// Present `prompt` with `options`, returning the chosen label (or `None` if
    /// dismissed).
    fn select(&self, prompt: &str, options: &[String]) -> Option<String>;
}

/// A resolved `project_trust` extension decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectTrustExtensionResult {
    /// Whether the extension trusts the project.
    pub trusted: bool,
    /// Whether the decision should be persisted to the trust store.
    pub remember: bool,
}

/// An error raised by a `project_trust` extension handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustExtensionError {
    /// Path of the extension that failed.
    pub extension_path: String,
    /// The error message.
    pub error: String,
}

/// Outcome of running the `project_trust` extension handlers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectTrustExtensionOutcome {
    /// The winning decision, if any handler decided (`None` == all undecided).
    pub result: Option<ProjectTrustExtensionResult>,
    /// Errors reported by handlers.
    pub errors: Vec<ProjectTrustExtensionError>,
}

/// Seam over pi's `emitProjectTrustEvent`: runs project-trust extension
/// handlers for `cwd`.
pub trait ProjectTrustExtensions {
    /// Emit the `project_trust` event and collect the outcome.
    fn emit_project_trust(&self, cwd: &str) -> ProjectTrustExtensionOutcome;
}

/// Inputs to [`resolve_project_trusted`] (pi's `ResolveProjectTrustedOptions`).
pub struct ResolveProjectTrustedOptions<'a> {
    /// Working directory whose trust is being resolved.
    pub cwd: &'a str,
    /// Persistent trust store.
    pub trust_store: &'a ProjectTrustStore,
    /// Explicit override that short-circuits all other logic.
    pub trust_override: Option<bool>,
    /// Policy applied when no decision is recorded.
    pub default_project_trust: Option<DefaultProjectTrust>,
    /// Optional project-trust extension seam.
    pub extensions: Option<&'a dyn ProjectTrustExtensions>,
    /// Interactive context used to prompt.
    pub context: &'a dyn ProjectTrustContext,
    /// Optional sink for extension error messages.
    pub on_extension_error: Option<&'a dyn Fn(&str)>,
}

fn format_project_trust_prompt(cwd: &str) -> String {
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows pi to load {CONFIG_DIR_NAME} settings and \
         resources, install missing project packages, and execute project extensions."
    )
}

fn select_project_trust_option(
    cwd: &str,
    ctx: &dyn ProjectTrustContext,
) -> Option<ProjectTrustOption> {
    let options = get_project_trust_options(cwd, true);
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    let selected = ctx.select(&format_project_trust_prompt(cwd), &labels)?;
    options.into_iter().find(|option| option.label == selected)
}

fn save_project_trust_prompt_result(
    trust_store: &ProjectTrustStore,
    result: &ProjectTrustOption,
) -> Result<(), TrustStoreError> {
    if !result.updates.is_empty() {
        trust_store.set_many(&result.updates)?;
    }
    Ok(())
}

/// Resolve whether `options.cwd` is trusted, following pi's precedence.
pub fn resolve_project_trusted(
    options: &ResolveProjectTrustedOptions,
) -> Result<bool, TrustStoreError> {
    if let Some(override_value) = options.trust_override {
        return Ok(override_value);
    }
    if !has_trust_requiring_project_resources(options.cwd) {
        return Ok(true);
    }

    if let Some(extensions) = options.extensions {
        let outcome = extensions.emit_project_trust(options.cwd);
        for error in &outcome.errors {
            if let Some(sink) = options.on_extension_error {
                sink(&format!(
                    "Extension \"{}\" project_trust error: {}",
                    error.extension_path, error.error
                ));
            }
        }
        if let Some(result) = outcome.result {
            if result.remember {
                options.trust_store.set(options.cwd, Some(result.trusted))?;
            }
            return Ok(result.trusted);
        }
    }

    if let Some(decision) = options.trust_store.get(options.cwd)? {
        return Ok(decision);
    }

    match options.default_project_trust.unwrap_or_default() {
        DefaultProjectTrust::Always => return Ok(true),
        DefaultProjectTrust::Never => return Ok(false),
        DefaultProjectTrust::Ask => {}
    }

    if !options.context.has_ui() {
        return Ok(false);
    }

    if let Some(selected) = select_project_trust_option(options.cwd, options.context) {
        save_project_trust_prompt_result(options.trust_store, &selected)?;
        return Ok(selected.trusted);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    // NOTE: pi has no dedicated `project-trust.test.ts`; `trust-manager.test.ts`
    // covers the store/resource collaborators. These tests exercise the
    // decision precedence directly against stub seams, which pi's suite does not
    // pin but which keep the port self-contained and regression-guarded.
    use super::*;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    struct StubContext {
        has_ui: bool,
        pick: Option<String>,
        last_prompt: RefCell<Option<String>>,
        last_options: RefCell<Vec<String>>,
    }

    impl ProjectTrustContext for StubContext {
        fn has_ui(&self) -> bool {
            self.has_ui
        }
        fn select(&self, prompt: &str, options: &[String]) -> Option<String> {
            *self.last_prompt.borrow_mut() = Some(prompt.to_string());
            *self.last_options.borrow_mut() = options.to_vec();
            self.pick.clone()
        }
    }

    struct StubExtensions(ProjectTrustExtensionOutcome);
    impl ProjectTrustExtensions for StubExtensions {
        fn emit_project_trust(&self, _cwd: &str) -> ProjectTrustExtensionOutcome {
            self.0.clone()
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "atilla-project-trust-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn s(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// A project directory that carries a trust-requiring resource (`.pi/settings.json`).
    fn trust_requiring_project(temp: &Path) -> PathBuf {
        let cwd = temp.join("project");
        std::fs::create_dir_all(cwd.join(".pi")).unwrap();
        std::fs::write(cwd.join(".pi").join("settings.json"), "{}").unwrap();
        cwd
    }

    fn context(has_ui: bool, pick: Option<&str>) -> StubContext {
        StubContext {
            has_ui,
            pick: pick.map(str::to_string),
            last_prompt: RefCell::new(None),
            last_options: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn override_short_circuits_everything() {
        let temp = scratch_dir("override");
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);
        let opts = ResolveProjectTrustedOptions {
            cwd: "/nonexistent/path/xyz",
            trust_store: &store,
            trust_override: Some(true),
            default_project_trust: None,
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(resolve_project_trusted(&opts).unwrap());
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn project_without_trust_resources_is_trusted() {
        let temp = scratch_dir("noresources");
        let bare = temp.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&bare),
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(resolve_project_trusted(&opts).unwrap());
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn extension_decision_is_honored_and_can_be_remembered() {
        let temp = scratch_dir("ext");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);
        let extensions = StubExtensions(ProjectTrustExtensionOutcome {
            result: Some(ProjectTrustExtensionResult {
                trusted: true,
                remember: true,
            }),
            errors: vec![],
        });
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            extensions: Some(&extensions),
            context: &ctx,
            on_extension_error: None,
        };
        assert!(resolve_project_trusted(&opts).unwrap());
        // remember == true persisted the decision.
        assert_eq!(store.get(&s(&cwd)).unwrap(), Some(true));
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn extension_errors_are_forwarded_to_the_sink() {
        let temp = scratch_dir("exterr");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);
        let extensions = StubExtensions(ProjectTrustExtensionOutcome {
            result: None,
            errors: vec![ProjectTrustExtensionError {
                extension_path: "ext.ts".to_string(),
                error: "boom".to_string(),
            }],
        });
        let messages = RefCell::new(Vec::new());
        let sink = |message: &str| messages.borrow_mut().push(message.to_string());
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some(DefaultProjectTrust::Never),
            extensions: Some(&extensions),
            context: &ctx,
            on_extension_error: Some(&sink),
        };
        // Undecided extension -> falls through to default policy (Never).
        assert!(!resolve_project_trusted(&opts).unwrap());
        assert_eq!(
            messages.borrow().as_slice(),
            &["Extension \"ext.ts\" project_trust error: boom".to_string()]
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn stored_decision_beats_default_policy() {
        let temp = scratch_dir("stored");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        store.set(&s(&cwd), Some(false)).unwrap();
        let ctx = context(true, Some("Trust"));
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some(DefaultProjectTrust::Always),
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(!resolve_project_trusted(&opts).unwrap());
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn default_policy_applies_when_no_decision_recorded() {
        let temp = scratch_dir("policy");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);

        for (policy, expected) in [
            (DefaultProjectTrust::Always, true),
            (DefaultProjectTrust::Never, false),
        ] {
            let opts = ResolveProjectTrustedOptions {
                cwd: &s(&cwd),
                trust_store: &store,
                trust_override: None,
                default_project_trust: Some(policy),
                extensions: None,
                context: &ctx,
                on_extension_error: None,
            };
            assert_eq!(resolve_project_trusted(&opts).unwrap(), expected);
        }
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn ask_without_ui_defaults_to_untrusted() {
        let temp = scratch_dir("noui");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(false, None);
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some(DefaultProjectTrust::Ask),
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(!resolve_project_trusted(&opts).unwrap());
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn ask_with_ui_prompts_and_persists_selection() {
        let temp = scratch_dir("prompt");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(true, Some("Trust"));
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some(DefaultProjectTrust::Ask),
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(resolve_project_trusted(&opts).unwrap());
        // The "Trust" option persisted a positive decision.
        assert_eq!(store.get(&s(&cwd)).unwrap(), Some(true));
        // The prompt offered the session-only variants.
        assert!(ctx
            .last_options
            .borrow()
            .iter()
            .any(|label| label == "Trust (this session only)"));
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn ask_with_ui_dismissed_defaults_to_untrusted() {
        let temp = scratch_dir("dismiss");
        let cwd = trust_requiring_project(&temp);
        let store = ProjectTrustStore::new(&s(&temp.join("agent")));
        let ctx = context(true, None);
        let opts = ResolveProjectTrustedOptions {
            cwd: &s(&cwd),
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some(DefaultProjectTrust::Ask),
            extensions: None,
            context: &ctx,
            on_extension_error: None,
        };
        assert!(!resolve_project_trusted(&opts).unwrap());
        std::fs::remove_dir_all(&temp).ok();
    }
}
