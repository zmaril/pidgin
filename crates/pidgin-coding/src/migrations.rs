//! One-time migrations that run on startup.
//!
//! Ported from pi-coding-agent's `migrations.ts`. Mirrors pi's migration driver
//! symbol-for-symbol: [`migrate_auth_to_auth_json`],
//! [`migrate_sessions_from_agent_root`], the extension-system migrations
//! (`commands/` → `prompts/`, deprecated-directory warnings), the `tools/` →
//! `bin/` binary move, the keybindings-config file rewrite, and the top-level
//! [`run_migrations`] driver.
//!
//! # Seams
//!
//! - pi colors console output with `chalk`; matching the neighboring ported
//!   files (see `utils/deprecation.rs`), this port strips color and writes the
//!   plain text. pi's migration messages use `console.log` (stdout), so this
//!   port uses `println!`.
//! - The keybinding-name migration itself lives in `core::keybindings` as
//!   [`crate::core::keybindings::migrate_keybindings_config`] (already ported);
//!   this module only ports the file-rewrite wrapper around it.
//! - [`show_deprecation_warnings`] waits for a keypress. pi toggles
//!   `process.stdin` into raw mode (a tui-level concern not ported yet); this
//!   port reads a single byte from stdin without raw mode. The printed output is
//!   faithful.
//! - The `.pi` config dir name is duplicated here as [`CONFIG_DIR_NAME`], as in
//!   the other coding-agent modules, because pi's `config.ts` is not fully
//!   ported.

// straitjacket-allow-file:duplication

use std::fs;
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::core::keybindings::migrate_keybindings_config;
use crate::core::skills::get_agent_dir;
use crate::utils::shell::get_bin_dir;

/// pi's `CONFIG_DIR_NAME` (`pkg.piConfig?.configDir || ".pi"`), duplicated here
/// while `config.ts` is unported.
const CONFIG_DIR_NAME: &str = ".pi";

const MIGRATION_GUIDE_URL: &str =
    "https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";
const EXTENSIONS_DOC_URL: &str =
    "https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/extensions.md";

/// Migrate legacy oauth.json and settings.json apiKeys to auth.json.
///
/// Returns the list of provider names that were migrated.
///
/// Mirrors `migrateAuthToAuthJson`. A non-object `oauth.json` is left untouched
/// (in practice the file is always a credential map; JS's `Object.entries` would
/// throw on `null`, matching this no-op).
pub fn migrate_auth_to_auth_json() -> Vec<String> {
    let agent_dir = get_agent_dir();
    let auth_path = Path::new(&agent_dir).join("auth.json");
    let oauth_path = Path::new(&agent_dir).join("oauth.json");
    let settings_path = Path::new(&agent_dir).join("settings.json");

    // Skip if auth.json already exists
    if auth_path.exists() {
        return Vec::new();
    }

    let mut migrated: Map<String, Value> = Map::new();
    let mut providers: Vec<String> = Vec::new();

    // Migrate oauth.json
    if oauth_path.exists() {
        if let Ok(content) = fs::read_to_string(&oauth_path) {
            if let Ok(Value::Object(oauth)) = serde_json::from_str::<Value>(&content) {
                for (provider, cred) in oauth {
                    // { type: "oauth", ...cred }
                    let mut entry: Map<String, Value> = Map::new();
                    entry.insert("type".to_string(), Value::String("oauth".to_string()));
                    if let Value::Object(cred_map) = cred {
                        for (key, value) in cred_map {
                            entry.insert(key, value);
                        }
                    }
                    migrated.insert(provider.clone(), Value::Object(entry));
                    providers.push(provider);
                }
                let migrated_path = format!("{}.migrated", oauth_path.display());
                let _ = fs::rename(&oauth_path, migrated_path);
            }
        }
    }

    // Migrate settings.json apiKeys
    if settings_path.exists() {
        if let Ok(content) = fs::read_to_string(&settings_path) {
            if let Ok(mut settings) = serde_json::from_str::<Value>(&content) {
                let api_keys = settings.get("apiKeys").and_then(Value::as_object).cloned();
                if let Some(api_keys) = api_keys {
                    for (provider, key) in &api_keys {
                        if !migrated.contains_key(provider) {
                            if let Value::String(key) = key {
                                let mut entry: Map<String, Value> = Map::new();
                                entry.insert(
                                    "type".to_string(),
                                    Value::String("api_key".to_string()),
                                );
                                entry.insert("key".to_string(), Value::String(key.clone()));
                                migrated.insert(provider.clone(), Value::Object(entry));
                                providers.push(provider.clone());
                            }
                        }
                    }
                    if let Value::Object(obj) = &mut settings {
                        obj.remove("apiKeys");
                    }
                    if let Ok(serialized) = serde_json::to_string_pretty(&settings) {
                        let _ = fs::write(&settings_path, serialized);
                    }
                }
            }
        }
    }

    if !migrated.is_empty() {
        if let Some(parent) = auth_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(serialized) = serde_json::to_string_pretty(&Value::Object(migrated)) {
            if fs::write(&auth_path, serialized).is_ok() {
                set_owner_only_permissions(&auth_path);
            }
        }
    }

    providers
}

/// Restrict `path` to `0o600` (owner read/write), matching pi's
/// `writeFileSync(..., { mode: 0o600 })`. No-op on non-Unix targets.
fn set_owner_only_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Migrate sessions from `~/.pi/agent/*.jsonl` to proper session directories.
///
/// Bug in v0.30.0: Sessions were saved to `~/.pi/agent/` instead of
/// `~/.pi/agent/sessions/<encoded-cwd>/`. This migration moves them to the
/// correct location based on the cwd in their session header.
///
/// See: <https://github.com/earendil-works/pi-mono/issues/320>
pub fn migrate_sessions_from_agent_root() {
    let agent_dir = get_agent_dir();

    // Find all .jsonl files directly in agentDir (not in subdirectories)
    let files: Vec<std::path::PathBuf> = match fs::read_dir(&agent_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().ends_with(".jsonl"))
            .map(|name| Path::new(&agent_dir).join(name))
            .collect(),
        Err(_) => return,
    };

    if files.is_empty() {
        return;
    }

    for file in files {
        // Read first line to get session header
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let first_line = content.split('\n').next().unwrap_or("");
        if first_line.trim().is_empty() {
            continue;
        }

        let Ok(header) = serde_json::from_str::<Value>(first_line) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        let Some(cwd) = header.get("cwd").and_then(Value::as_str) else {
            continue;
        };

        // Compute the correct session directory (same encoding as session-manager.ts)
        let safe_path = encode_session_dir(cwd);
        let correct_dir = Path::new(&agent_dir).join("sessions").join(&safe_path);

        // Create directory if needed
        if !correct_dir.exists() {
            let _ = fs::create_dir_all(&correct_dir);
        }

        // Move the file
        let Some(file_name) = file.file_name() else {
            continue;
        };
        let new_path = correct_dir.join(file_name);

        if new_path.exists() {
            continue; // Skip if target exists
        }

        let _ = fs::rename(&file, &new_path);
    }
}

/// Encode a cwd into a session directory name: strip a leading `/` or `\`, then
/// replace every `/`, `\`, and `:` with `-`, wrapped in `--`.
///
/// Mirrors the TS `` `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--` ``.
fn encode_session_dir(cwd: &str) -> String {
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let replaced: String = stripped
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{replaced}--")
}

/// Migrate `commands/` to `prompts/` if needed.
///
/// Works for both regular directories and symlinks. Returns whether a migration
/// occurred. Mirrors `migrateCommandsToPrompts`.
fn migrate_commands_to_prompts(base_dir: &Path, label: &str) -> bool {
    let commands_dir = base_dir.join("commands");
    let prompts_dir = base_dir.join("prompts");

    if commands_dir.exists() && !prompts_dir.exists() {
        match fs::rename(&commands_dir, &prompts_dir) {
            Ok(()) => {
                println!("Migrated {label} commands/ → prompts/");
                return true;
            }
            Err(err) => {
                println!("Warning: Could not migrate {label} commands/ to prompts/: {err}");
            }
        }
    }
    false
}

/// Rewrite the keybindings config file if any legacy names need migrating.
///
/// Mirrors `migrateKeybindingsConfigFile`. Delegates the key-name migration to
/// [`migrate_keybindings_config`].
fn migrate_keybindings_config_file() {
    let config_path = Path::new(&get_agent_dir()).join("keybindings.json");
    if !config_path.exists() {
        return;
    }

    let Ok(content) = fs::read_to_string(&config_path) else {
        return; // Ignore malformed files during migration
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
        return;
    };
    let Value::Object(map) = parsed else {
        return;
    };
    let raw: IndexMap<String, Value> = map.into_iter().collect();
    let (config, migrated) = migrate_keybindings_config(&raw);
    if !migrated {
        return;
    }
    if let Ok(serialized) = serde_json::to_string_pretty(&config) {
        let _ = fs::write(&config_path, format!("{serialized}\n"));
    }
}

/// Move fd/rg binaries from `tools/` to `bin/` if they exist.
///
/// Mirrors `migrateToolsToBin`.
fn migrate_tools_to_bin() {
    let agent_dir = get_agent_dir();
    let tools_dir = Path::new(&agent_dir).join("tools");
    let bin_dir = get_bin_dir();

    if !tools_dir.exists() {
        return;
    }

    let binaries = ["fd", "rg", "fd.exe", "rg.exe"];
    let mut moved_any = false;

    for bin in binaries {
        let old_path = tools_dir.join(bin);
        let new_path = bin_dir.join(bin);

        if old_path.exists() {
            if !bin_dir.exists() {
                let _ = fs::create_dir_all(&bin_dir);
            }
            if !new_path.exists() {
                if fs::rename(&old_path, &new_path).is_ok() {
                    moved_any = true;
                }
            } else {
                // Target exists, just delete the old one
                let _ = fs::remove_file(&old_path);
            }
        }
    }

    if moved_any {
        println!("Migrated managed binaries tools/ → bin/");
    }
}

/// Check for deprecated `hooks/` and `tools/` directories.
///
/// Note: `tools/` may contain fd/rg binaries extracted by pi, so only warn if it
/// has other files. Mirrors `checkDeprecatedExtensionDirs`.
fn check_deprecated_extension_dirs(base_dir: &Path, label: &str) -> Vec<String> {
    let hooks_dir = base_dir.join("hooks");
    let tools_dir = base_dir.join("tools");
    let mut warnings: Vec<String> = Vec::new();

    if hooks_dir.exists() {
        warnings.push(format!(
            "{label} hooks/ directory found. Hooks have been renamed to extensions."
        ));
    }

    if tools_dir.exists() {
        // Check if tools/ contains anything other than fd/rg (which are auto-extracted binaries)
        if let Ok(entries) = fs::read_dir(&tools_dir) {
            let custom_tools: Vec<String> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| {
                    let lower = name.to_lowercase();
                    lower != "fd"
                        && lower != "rg"
                        && lower != "fd.exe"
                        && lower != "rg.exe"
                        && !name.starts_with('.') // Ignore .DS_Store and other hidden files
                })
                .collect();
            if !custom_tools.is_empty() {
                warnings.push(format!(
                    "{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."
                ));
            }
        }
    }

    warnings
}

/// Run extension system migrations (commands→prompts) and collect warnings about
/// deprecated directories. Mirrors `migrateExtensionSystem`.
fn migrate_extension_system(cwd: &str) -> Vec<String> {
    let agent_dir = get_agent_dir();
    let agent_dir = Path::new(&agent_dir);
    let project_dir = Path::new(cwd).join(CONFIG_DIR_NAME);

    // Migrate commands/ to prompts/
    migrate_commands_to_prompts(agent_dir, "Global");
    migrate_commands_to_prompts(&project_dir, "Project");

    // Check for deprecated directories
    let mut warnings = check_deprecated_extension_dirs(agent_dir, "Global");
    warnings.extend(check_deprecated_extension_dirs(&project_dir, "Project"));

    warnings
}

/// Print deprecation warnings and wait for a keypress.
///
/// Mirrors `showDeprecationWarnings`. See the module seam note on stdin handling.
pub fn show_deprecation_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }

    for warning in warnings {
        println!("Warning: {warning}");
    }
    println!("\nMove your extensions to the extensions/ directory.");
    println!("Migration guide: {MIGRATION_GUIDE_URL}");
    println!("Documentation: {EXTENSIONS_DOC_URL}");
    println!("\nPress any key to continue...");

    use std::io::Read;
    let mut byte = [0u8; 1];
    let _ = std::io::stdin().read(&mut byte);
    println!();
}

/// Aggregated results of [`run_migrations`].
///
/// Mirrors the object returned by pi's `runMigrations`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationResults {
    /// Provider names migrated into `auth.json`.
    pub migrated_auth_providers: Vec<String>,
    /// Deprecation warnings collected from the extension-system migration.
    pub deprecation_warnings: Vec<String>,
}

/// Run all migrations. Called once on startup.
///
/// Mirrors `runMigrations`.
pub fn run_migrations(cwd: &str) -> MigrationResults {
    let migrated_auth_providers = migrate_auth_to_auth_json();
    migrate_sessions_from_agent_root();
    migrate_tools_to_bin();
    migrate_keybindings_config_file();
    let deprecation_warnings = migrate_extension_system(cwd);
    MigrationResults {
        migrated_auth_providers,
        deprecation_warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    // get_agent_dir()/get_bin_dir() read process-global env vars; serialize the
    // tests that set them so they do not race under parallel execution.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn migrate_auth_happy_path_oauth_and_api_keys() {
        let _guard = lock_env();
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        fs::create_dir_all(&agent).unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", &agent);

        fs::write(
            agent.join("oauth.json"),
            r#"{"anthropic":{"access":"tok","refresh":"r"}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("settings.json"),
            r#"{"apiKeys":{"openai":"sk-123","anthropic":"should-be-ignored"},"theme":"dark"}"#,
        )
        .unwrap();

        let mut providers = migrate_auth_to_auth_json();
        providers.sort();
        assert_eq!(
            providers,
            vec!["anthropic".to_string(), "openai".to_string()]
        );

        // auth.json written with the migrated credentials.
        let auth: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["anthropic"]["type"], "oauth");
        assert_eq!(auth["anthropic"]["access"], "tok");
        // oauth wins over settings for the same provider (already migrated).
        assert!(auth["anthropic"].get("key").is_none());
        assert_eq!(auth["openai"]["type"], "api_key");
        assert_eq!(auth["openai"]["key"], "sk-123");

        // oauth.json renamed and apiKeys stripped from settings.json.
        assert!(!agent.join("oauth.json").exists());
        assert!(agent.join("oauth.json.migrated").exists());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("settings.json")).unwrap())
                .unwrap();
        assert!(settings.get("apiKeys").is_none());
        assert_eq!(settings["theme"], "dark");

        std::env::remove_var("PI_CODING_AGENT_DIR");
    }

    #[test]
    fn migrate_auth_no_op_when_auth_json_exists() {
        let _guard = lock_env();
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        fs::create_dir_all(&agent).unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", &agent);

        fs::write(agent.join("auth.json"), r#"{"existing":{"type":"oauth"}}"#).unwrap();
        fs::write(
            agent.join("oauth.json"),
            r#"{"anthropic":{"access":"tok"}}"#,
        )
        .unwrap();

        let providers = migrate_auth_to_auth_json();
        assert!(providers.is_empty());

        // Nothing touched: oauth.json still present, auth.json unchanged.
        assert!(agent.join("oauth.json").exists());
        assert!(!agent.join("oauth.json.migrated").exists());
        let auth: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["existing"]["type"], "oauth");

        std::env::remove_var("PI_CODING_AGENT_DIR");
    }

    #[test]
    fn encode_session_dir_matches_ts_encoding() {
        assert_eq!(encode_session_dir("/home/user/proj"), "--home-user-proj--");
        // Both the ':' and the '\' are replaced, so "C:\" becomes "C--".
        assert_eq!(
            encode_session_dir("C:\\Users\\me\\proj"),
            "--C--Users-me-proj--"
        );
    }
}
