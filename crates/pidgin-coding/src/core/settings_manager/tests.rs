// straitjacket-allow-file:duplication — these tests are a faithful translation
// of pi's `settings-manager.test.ts` / `settings-manager-bug.test.ts` cases,
// each of which repeats the same fixture-setup + write + assert scaffolding per
// scenario. The parallel structure is the point (one case per behavior); it is
// not extractable slop.
//! Unit tests translated from pi's `settings-manager.test.ts` and
//! `settings-manager-bug.test.ts`. Each `#[test]` names the pi case it mirrors.
//!
//! Environment-shaped assertions are handled as noted:
//! * `externalEditor` precedence/platform: pi mutates `process.env` and
//!   `process.platform` per case. Rust cannot rebind `cfg!(windows)` at runtime
//!   and mutating process env races across parallel tests, so the pure
//!   [`resolve_external_editor`] seam is exercised directly for every case
//!   instead of the env-reading wrapper.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use tempfile::TempDir;

use pidgin_agent::types::ThinkingLevel;

use super::*;

struct Fixture {
    _root: TempDir,
    agent_dir: String,
    project_dir: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let agent = root.path().join("agent");
        let project = root.path().join("project");
        fs::create_dir_all(&agent).expect("mkdir agent");
        fs::create_dir_all(project.join(".pi")).expect("mkdir project/.pi");
        Fixture {
            agent_dir: agent.to_string_lossy().into_owned(),
            project_dir: project.to_string_lossy().into_owned(),
            _root: root,
        }
    }

    fn manager(&self) -> SettingsManager {
        SettingsManager::create(&self.project_dir, &self.agent_dir)
    }

    fn global_path(&self) -> PathBuf {
        Path::new(&self.agent_dir).join("settings.json")
    }

    fn project_path(&self) -> PathBuf {
        Path::new(&self.project_dir)
            .join(".pi")
            .join("settings.json")
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn write_raw(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

// -- preserves externally added settings ------------------------------------

#[test]
fn preserves_enabled_models_when_changing_thinking_level() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "theme": "dark", "defaultModel": "claude-sonnet" }),
    );

    let mut manager = fx.manager();

    // Simulate an external edit adding enabledModels.
    let mut current = read_json(&fx.global_path());
    current["enabledModels"] = json!(["claude-opus-4-5", "gpt-5.2-codex"]);
    write_json(&fx.global_path(), &current);

    manager.set_default_thinking_level(ThinkingLevel::High);
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(
        saved["enabledModels"],
        json!(["claude-opus-4-5", "gpt-5.2-codex"])
    );
    assert_eq!(saved["defaultThinkingLevel"], json!("high"));
    assert_eq!(saved["theme"], json!("dark"));
    assert_eq!(saved["defaultModel"], json!("claude-sonnet"));
}

#[test]
fn preserves_custom_settings_when_changing_theme() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "defaultModel": "claude-sonnet" }),
    );

    let mut manager = fx.manager();

    let mut current = read_json(&fx.global_path());
    current["shellPath"] = json!("/bin/zsh");
    current["extensions"] = json!(["/path/to/extension.ts"]);
    write_json(&fx.global_path(), &current);

    manager.set_theme("light");
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(saved["shellPath"], json!("/bin/zsh"));
    assert_eq!(saved["extensions"], json!(["/path/to/extension.ts"]));
    assert_eq!(saved["theme"], json!("light"));
}

#[test]
fn in_memory_changes_override_file_changes_for_same_key() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));

    let mut manager = fx.manager();

    let mut current = read_json(&fx.global_path());
    current["defaultThinkingLevel"] = json!("low");
    write_json(&fx.global_path(), &current);

    manager.set_default_thinking_level(ThinkingLevel::High);
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(saved["defaultThinkingLevel"], json!("high"));
}

// -- packages migration -----------------------------------------------------

#[test]
fn keeps_local_only_extensions_in_extensions_array() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "extensions": ["/local/ext.ts", "./relative/ext.ts"] }),
    );

    let manager = fx.manager();

    assert_eq!(manager.get_packages(), Vec::<Value>::new());
    assert_eq!(
        manager.get_extension_paths(),
        vec!["/local/ext.ts".to_string(), "./relative/ext.ts".to_string()]
    );
}

#[test]
fn handles_packages_with_filtering_objects() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({
            "packages": [
                "npm:simple-pkg",
                { "source": "npm:shitty-extensions", "extensions": ["extensions/oracle.ts"], "skills": [] }
            ]
        }),
    );

    let manager = fx.manager();

    let packages = manager.get_packages();
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0], json!("npm:simple-pkg"));
    assert_eq!(
        packages[1],
        json!({ "source": "npm:shitty-extensions", "extensions": ["extensions/oracle.ts"], "skills": [] })
    );
}

// -- reload -----------------------------------------------------------------

#[test]
fn reloads_global_settings_from_disk() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "theme": "dark", "extensions": ["/before.ts"] }),
    );

    let mut manager = fx.manager();

    write_json(
        &fx.global_path(),
        &json!({ "theme": "light", "extensions": ["/after.ts"], "defaultModel": "claude-sonnet" }),
    );

    manager.reload();

    assert_eq!(manager.get_theme(), Some("light".to_string()));
    assert_eq!(manager.get_extension_paths(), vec!["/after.ts".to_string()]);
    assert_eq!(
        manager.get_default_model(),
        Some("claude-sonnet".to_string())
    );
}

#[test]
fn keeps_previous_settings_when_file_is_invalid() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));

    let mut manager = fx.manager();

    write_raw(&fx.global_path(), "{ invalid json");
    manager.reload();

    assert_eq!(manager.get_theme(), Some("dark".to_string()));
}

// -- theme setting ----------------------------------------------------------

#[test]
fn stores_slash_separated_automatic_theme_separately() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "light/dark" }));

    let mut manager = fx.manager();

    assert_eq!(manager.get_theme(), None);
    assert_eq!(manager.get_theme_setting(), Some("light/dark".to_string()));

    manager.set_theme("solarized-light/tokyo-night");
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(saved["theme"], json!("solarized-light/tokyo-night"));
}

// -- error tracking ---------------------------------------------------------

#[test]
fn collects_and_clears_load_errors_via_drain_errors() {
    let fx = Fixture::new();
    write_raw(&fx.global_path(), "{ invalid global json");
    write_raw(&fx.project_path(), "{ invalid project json");

    let mut manager = fx.manager();
    let errors = manager.drain_errors();

    assert_eq!(errors.len(), 2);
    let mut scopes: Vec<SettingsScope> = errors.iter().map(|e| e.scope).collect();
    scopes.sort_by_key(|s| match s {
        SettingsScope::Global => 0,
        SettingsScope::Project => 1,
    });
    assert_eq!(scopes, vec![SettingsScope::Global, SettingsScope::Project]);
    assert_eq!(manager.drain_errors(), Vec::new());
}

// -- project trust ----------------------------------------------------------

#[test]
fn skips_project_settings_when_project_is_not_trusted() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "global" }));
    write_json(&fx.project_path(), &json!({ "theme": "project" }));

    let manager = SettingsManager::create_with_options(
        &fx.project_dir,
        &fx.agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );

    assert!(!manager.is_project_trusted());
    assert_eq!(manager.get_theme(), Some("global".to_string()));
    assert_eq!(manager.get_project_settings(), Settings::empty());
}

#[test]
fn reloads_project_settings_after_trust_changes_to_true() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "global" }));
    write_json(&fx.project_path(), &json!({ "theme": "project" }));

    let mut manager = SettingsManager::create_with_options(
        &fx.project_dir,
        &fx.agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );

    manager.set_project_trusted(true);

    assert!(manager.is_project_trusted());
    assert_eq!(manager.get_theme(), Some("project".to_string()));
}

#[test]
fn fails_project_writes_when_project_is_not_trusted() {
    let fx = Fixture::new();
    write_json(&fx.project_path(), &json!({ "packages": ["npm:existing"] }));

    let mut manager = SettingsManager::create_with_options(
        &fx.project_dir,
        &fx.agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );

    let err = manager
        .set_project_packages(vec![json!("npm:new")])
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("Project is not trusted; refusing to write project settings"));
    manager.flush();

    assert_eq!(manager.get_project_settings(), Settings::empty());
    assert_eq!(
        read_json(&fx.project_path()),
        json!({ "packages": ["npm:existing"] })
    );
}

#[test]
fn reads_default_project_trust_from_global_only() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "defaultProjectTrust": "always" }),
    );
    write_json(
        &fx.project_path(),
        &json!({ "defaultProjectTrust": "never" }),
    );

    let manager = fx.manager();

    assert_eq!(
        manager.get_default_project_trust(),
        DefaultProjectTrust::Always
    );
}

#[test]
fn defaults_invalid_project_trust_to_ask() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "defaultProjectTrust": "sometimes" }),
    );

    let manager = fx.manager();

    assert_eq!(
        manager.get_default_project_trust(),
        DefaultProjectTrust::Ask
    );
}

// -- project settings directory creation ------------------------------------

#[test]
fn does_not_create_pi_folder_when_only_reading() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));

    let project_pi = Path::new(&fx.project_dir).join(".pi");
    fs::remove_dir_all(&project_pi).unwrap();

    let manager = fx.manager();

    assert!(!project_pi.exists());
    assert_eq!(manager.get_theme(), Some("dark".to_string()));
}

#[test]
fn creates_pi_folder_when_writing_project_settings() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));

    let project_pi = Path::new(&fx.project_dir).join(".pi");
    fs::remove_dir_all(&project_pi).unwrap();

    let mut manager = fx.manager();
    assert!(!project_pi.exists());

    manager
        .set_project_packages(vec![json!({ "source": "npm:test-pkg" })])
        .unwrap();
    manager.flush();

    assert!(project_pi.exists());
    assert!(project_pi.join("settings.json").exists());
}

// -- httpIdleTimeoutMs ------------------------------------------------------

#[test]
fn http_idle_timeout_defaults_to_five_minutes() {
    let fx = Fixture::new();
    let manager = fx.manager();
    assert_eq!(
        manager.get_http_idle_timeout_ms().unwrap(),
        crate::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS
    );
}

#[test]
fn http_idle_timeout_uses_merged_global_and_project() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "httpIdleTimeoutMs": 300000 }));
    write_json(&fx.project_path(), &json!({ "httpIdleTimeoutMs": 0 }));

    let manager = fx.manager();

    assert_eq!(manager.get_http_idle_timeout_ms().unwrap(), 0);
}

#[test]
fn http_idle_timeout_rejects_invalid_values() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "httpIdleTimeoutMs": -1 }));
    let manager = fx.manager();

    let err = manager.get_http_idle_timeout_ms().unwrap_err();
    assert!(err
        .to_string()
        .contains("Invalid httpIdleTimeoutMs setting"));
}

// -- externalEditor (pure seam) ---------------------------------------------

#[test]
fn resolves_editor_commands_by_precedence() {
    // Configured setting wins over env.
    assert_eq!(
        resolve_external_editor(Some("code --wait"), Some("vim"), Some("nano"), false),
        "code --wait"
    );
    // VISUAL wins over EDITOR when no configured setting.
    assert_eq!(
        resolve_external_editor(None, Some("vim"), Some("nano"), false),
        "vim"
    );
    // Falls through to EDITOR when VISUAL is absent.
    assert_eq!(
        resolve_external_editor(None, None, Some("emacs"), false),
        "emacs"
    );
    // Blank configured setting is ignored.
    assert_eq!(
        resolve_external_editor(Some("   "), None, Some("emacs"), false),
        "emacs"
    );
}

#[test]
fn falls_back_to_platform_defaults() {
    assert_eq!(resolve_external_editor(None, None, None, true), "notepad");
    assert_eq!(resolve_external_editor(None, None, None, false), "nano");
    // Empty env strings are treated as unset.
    assert_eq!(
        resolve_external_editor(None, Some(""), Some(""), false),
        "nano"
    );
}

// -- outputPad --------------------------------------------------------------

#[test]
fn output_pad_defaults_to_one_and_persists_binary_values() {
    let fx = Fixture::new();
    let mut manager = fx.manager();

    assert_eq!(manager.get_output_pad(), 1);

    manager.set_output_pad(0);
    manager.flush();

    assert_eq!(manager.get_output_pad(), 0);
    let saved = read_json(&fx.global_path());
    assert_eq!(saved["outputPad"], json!(0));
}

#[test]
fn treats_unsupported_output_pad_values_as_default_padding() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "outputPad": 2 }));

    let manager = fx.manager();

    assert_eq!(manager.get_output_pad(), 1);
}

// -- shellCommandPrefix -----------------------------------------------------

#[test]
fn loads_shell_command_prefix_from_settings() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "shellCommandPrefix": "shopt -s expand_aliases" }),
    );

    let manager = fx.manager();

    assert_eq!(
        manager.get_shell_command_prefix(),
        Some("shopt -s expand_aliases".to_string())
    );
}

#[test]
fn returns_none_when_shell_command_prefix_not_set() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));

    let manager = fx.manager();

    assert_eq!(manager.get_shell_command_prefix(), None);
}

#[test]
fn preserves_shell_command_prefix_when_saving_unrelated_settings() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "shellCommandPrefix": "shopt -s expand_aliases" }),
    );

    let mut manager = fx.manager();
    manager.set_theme("light");
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(
        saved["shellCommandPrefix"],
        json!("shopt -s expand_aliases")
    );
    assert_eq!(saved["theme"], json!("light"));
}

// -- getSessionDir ----------------------------------------------------------

#[test]
fn session_dir_returns_none_when_not_set() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));
    let manager = fx.manager();
    assert_eq!(manager.get_session_dir(), None);
}

#[test]
fn session_dir_returns_global_value() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "sessionDir": "/tmp/sessions" }));
    let manager = fx.manager();
    assert_eq!(manager.get_session_dir(), Some("/tmp/sessions".to_string()));
}

#[test]
fn session_dir_project_overrides_global() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "sessionDir": "/global/sessions" }),
    );
    write_json(&fx.project_path(), &json!({ "sessionDir": "./sessions" }));
    let manager = fx.manager();
    assert_eq!(manager.get_session_dir(), Some("./sessions".to_string()));
}

#[test]
fn session_dir_expands_tilde() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "sessionDir": "~/sessions" }));
    let manager = fx.manager();
    assert_eq!(
        manager.get_session_dir(),
        Some(format!("{}/sessions", home()))
    );
}

// -- getShellPath -----------------------------------------------------------

#[test]
fn shell_path_returns_none_when_not_set() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "theme": "dark" }));
    let manager = fx.manager();
    assert_eq!(manager.get_shell_path(), None);
}

#[test]
fn shell_path_returns_absolute_unchanged() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "shellPath": "/bin/zsh" }));
    let manager = fx.manager();
    assert_eq!(manager.get_shell_path(), Some("/bin/zsh".to_string()));
}

#[test]
fn shell_path_expands_tilde() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "shellPath": "~/.local/bin/agent-shell-sandbox" }),
    );
    let manager = fx.manager();
    assert_eq!(
        manager.get_shell_path(),
        Some(format!("{}/.local/bin/agent-shell-sandbox", home()))
    );
}

#[test]
fn shell_path_expands_bare_tilde() {
    let fx = Fixture::new();
    write_json(&fx.global_path(), &json!({ "shellPath": "~" }));
    let manager = fx.manager();
    assert_eq!(manager.get_shell_path(), Some(home()));
}

// -- external edit preservation (settings-manager-bug.test.ts) --------------

#[test]
fn preserves_file_changes_to_packages_array_when_changing_unrelated_setting() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "theme": "dark", "packages": ["npm:pi-mcp-adapter"] }),
    );

    let mut manager = fx.manager();
    assert_eq!(manager.get_packages(), vec![json!("npm:pi-mcp-adapter")]);

    // User externally edits the file to remove the package.
    let mut current = read_json(&fx.global_path());
    current["packages"] = json!([]);
    write_json(&fx.global_path(), &current);
    assert_eq!(read_json(&fx.global_path())["packages"], json!([]));

    // Changing an unrelated setting triggers a save.
    manager.set_theme("light");
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(saved["packages"], json!([]));
    assert_eq!(saved["theme"], json!("light"));
}

#[test]
fn preserves_file_changes_to_extensions_array_when_changing_unrelated_setting() {
    let fx = Fixture::new();
    write_json(
        &fx.global_path(),
        &json!({ "theme": "dark", "extensions": ["/old/extension.ts"] }),
    );

    let mut manager = fx.manager();

    let mut current = read_json(&fx.global_path());
    current["extensions"] = json!(["/new/extension.ts"]);
    write_json(&fx.global_path(), &current);

    manager.set_default_thinking_level(ThinkingLevel::High);
    manager.flush();

    let saved = read_json(&fx.global_path());
    assert_eq!(saved["extensions"], json!(["/new/extension.ts"]));
}

#[test]
fn preserves_external_project_changes_when_updating_unrelated_project_field() {
    let fx = Fixture::new();
    write_json(
        &fx.project_path(),
        &json!({ "extensions": ["./old-extension.ts"], "prompts": ["./old-prompt.md"] }),
    );

    let mut manager = fx.manager();

    let mut current = read_json(&fx.project_path());
    current["prompts"] = json!(["./new-prompt.md"]);
    write_json(&fx.project_path(), &current);

    manager
        .set_project_extension_paths(vec!["./updated-extension.ts".to_string()])
        .unwrap();
    manager.flush();

    let saved = read_json(&fx.project_path());
    assert_eq!(saved["prompts"], json!(["./new-prompt.md"]));
    assert_eq!(saved["extensions"], json!(["./updated-extension.ts"]));
}

#[test]
fn in_memory_project_changes_override_external_for_same_field() {
    let fx = Fixture::new();
    write_json(
        &fx.project_path(),
        &json!({ "extensions": ["./initial-extension.ts"] }),
    );

    let mut manager = fx.manager();

    let mut current = read_json(&fx.project_path());
    current["extensions"] = json!(["./external-extension.ts"]);
    write_json(&fx.project_path(), &current);

    manager
        .set_project_extension_paths(vec!["./in-memory-extension.ts".to_string()])
        .unwrap();
    manager.flush();

    let saved = read_json(&fx.project_path());
    assert_eq!(saved["extensions"], json!(["./in-memory-extension.ts"]));
}

// -- in-memory manager ------------------------------------------------------

#[test]
fn in_memory_manager_reads_configured_external_editor() {
    // Mirrors pi's `SettingsManager.inMemory({ externalEditor: ... })` path:
    // the configured value takes precedence over any environment editor.
    let manager = SettingsManager::in_memory(
        Settings::from_map(
            json!({ "externalEditor": "code --wait" })
                .as_object()
                .unwrap()
                .clone(),
        ),
        SettingsManagerCreateOptions::default(),
    );
    assert_eq!(manager.get_external_editor_command(), "code --wait");
}

// -- blockImages setting (pi `test/block-images.test.ts`, SettingsManager) ---

#[test]
fn block_images_defaults_to_false() {
    let manager = SettingsManager::in_memory(
        Settings::from_map(Map::new()),
        SettingsManagerCreateOptions::default(),
    );
    assert!(!manager.get_block_images());
}

#[test]
fn block_images_returns_true_when_set() {
    let manager = SettingsManager::in_memory(
        Settings::from_map(
            json!({ "images": { "blockImages": true } })
                .as_object()
                .unwrap()
                .clone(),
        ),
        SettingsManagerCreateOptions::default(),
    );
    assert!(manager.get_block_images());
}

#[test]
fn block_images_persists_via_set_block_images() {
    let mut manager = SettingsManager::in_memory(
        Settings::from_map(Map::new()),
        SettingsManagerCreateOptions::default(),
    );
    assert!(!manager.get_block_images());

    manager.set_block_images(true);
    assert!(manager.get_block_images());

    manager.set_block_images(false);
    assert!(!manager.get_block_images());
}

#[test]
fn block_images_coexists_with_auto_resize() {
    let manager = SettingsManager::in_memory(
        Settings::from_map(
            json!({ "images": { "autoResize": true, "blockImages": true } })
                .as_object()
                .unwrap()
                .clone(),
        ),
        SettingsManagerCreateOptions::default(),
    );
    assert!(manager.get_image_auto_resize());
    assert!(manager.get_block_images());
}

// The shared live mirror handed to the Agent's `convertToLlm` closure tracks
// `getBlockImages()` across construction and `setBlockImages`, so a mid-session
// toggle is observed live (pi reads the setting per call).
#[test]
fn block_images_flag_mirrors_the_setting_live() {
    use std::sync::atomic::Ordering;

    let mut manager = SettingsManager::in_memory(
        Settings::from_map(
            json!({ "images": { "blockImages": true } })
                .as_object()
                .unwrap()
                .clone(),
        ),
        SettingsManagerCreateOptions::default(),
    );
    let flag = manager.block_images_flag();
    assert!(flag.load(Ordering::Relaxed), "initialized from the setting");

    manager.set_block_images(false);
    assert!(
        !flag.load(Ordering::Relaxed),
        "same handle tracks the toggle"
    );

    manager.set_block_images(true);
    assert!(flag.load(Ordering::Relaxed));
}
