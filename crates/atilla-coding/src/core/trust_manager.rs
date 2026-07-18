//! Project-trust persistence and project-resource detection.
//!
//! Ported from pi's `core/trust-manager.ts`. A [`ProjectTrustStore`] persists
//! per-directory trust decisions to a `trust.json` file, with child directories
//! inheriting the nearest ancestor's decision.
//! [`has_trust_requiring_project_resources`] reports whether a working directory
//! carries project-local resources (`.pi/settings.json`, `.pi/extensions`,
//! `.agents/skills`, …) that must be gated behind a trust decision.
//!
//! NOTE: pi guards `trust.json` reads/writes with a cross-process advisory lock
//! (`proper-lockfile`). That lock defends against concurrent pi processes, not
//! any behavior pi's own tests pin; it is intentionally omitted here rather than
//! pulling in a filesystem-locking dependency. `CONFIG_DIR_NAME` is likewise
//! duplicated as a local constant because pi sources it from `config.ts`, which
//! is outside this port's scope.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::utils::paths::{canonicalize_path, resolve_path, PathInputOptions};

/// pi's `CONFIG_DIR_NAME` (`pkg.piConfig?.configDir || ".pi"`), duplicated here
/// because `config.ts` is out of this port's scope.
const CONFIG_DIR_NAME: &str = ".pi";

/// Project-config entries that, when present under `<cwd>/.pi`, require the
/// project to be trusted before they are loaded.
const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: [&str; 7] = [
    "settings.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// A resolved trust decision: `Some(true)`/`Some(false)` for an explicit
/// decision, `None` for "no decision recorded" (pi's `boolean | null`).
pub type ProjectTrustDecision = Option<bool>;

/// The nearest recorded trust decision for a directory, plus the path it was
/// recorded against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustStoreEntry {
    /// Canonicalized path the decision is stored under.
    pub path: String,
    /// The stored decision.
    pub decision: bool,
}

/// A single mutation to apply to the trust store. `decision: None` removes the
/// entry (pi's `null`), letting the directory fall back to an ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustUpdate {
    /// Path to update (canonicalized before it is stored).
    pub path: String,
    /// New decision, or `None` to clear the entry.
    pub decision: ProjectTrustDecision,
}

/// A selectable trust choice presented to the user, with the mutations it would
/// persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustOption {
    /// Human-readable label.
    pub label: String,
    /// Whether choosing this option trusts the project.
    pub trusted: bool,
    /// Trust-store mutations to apply if selected.
    pub updates: Vec<ProjectTrustUpdate>,
    /// Path the decision is saved against, if any (session-only options save
    /// nothing).
    pub saved_path: Option<String>,
}

/// Error raised when the on-disk trust store cannot be read or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStoreError(pub String);

impl std::fmt::Display for TrustStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TrustStoreError {}

/// In-memory representation of `trust.json`: path -> `Some(bool)` or `None`
/// (explicit JSON `null`).
type TrustFile = BTreeMap<String, Option<bool>>;

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path.dirname` for POSIX absolute paths: the parent, or the path itself at a
/// filesystem root.
fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => {
            if path.starts_with('/') {
                "/".to_string()
            } else {
                ".".to_string()
            }
        }
    }
}

fn join(base: &str, components: &[&str]) -> String {
    let mut path = PathBuf::from(base);
    for component in components {
        path.push(component);
    }
    path.to_string_lossy().into_owned()
}

fn resolve(input: &str) -> String {
    resolve_path(input, &current_dir(), &PathInputOptions::default())
        .unwrap_or_else(|_| input.to_string())
}

fn normalize_cwd(cwd: &str) -> String {
    canonicalize_path(&resolve(cwd))
}

fn find_nearest_trust_entry(data: &TrustFile, cwd: &str) -> Option<ProjectTrustStoreEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        if let Some(Some(value)) = data.get(&current_dir) {
            return Some(ProjectTrustStoreEntry {
                path: current_dir,
                decision: *value,
            });
        }
        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }
}

/// The nearest ancestor path of `cwd`, or `None` when `cwd` is a filesystem
/// root.
pub fn get_project_trust_parent_path(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd(cwd);
    let parent_dir = dirname(&trust_path);
    if parent_dir == trust_path {
        None
    } else {
        Some(parent_dir)
    }
}

/// Build the ordered list of trust options for `cwd`. When `include_session_only`
/// is set, the session-only (persist-nothing) variants are included.
pub fn get_project_trust_options(cwd: &str, include_session_only: bool) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd(cwd);
    let mut options = vec![ProjectTrustOption {
        label: "Trust".to_string(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];

    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }

    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_string(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    options.push(ProjectTrustOption {
        label: "Do not trust".to_string(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path),
    });

    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_string(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    options
}

fn read_trust_file(path: &str) -> Result<TrustFile, TrustStoreError> {
    if !Path::new(path).exists() {
        return Ok(TrustFile::new());
    }

    let contents = std::fs::read_to_string(path)
        .map_err(|error| TrustStoreError(format!("Failed to read trust store {path}: {error}")))?;
    let parsed: Value = serde_json::from_str(&contents)
        .map_err(|error| TrustStoreError(format!("Failed to read trust store {path}: {error}")))?;

    let Value::Object(map) = parsed else {
        return Err(TrustStoreError(format!(
            "Invalid trust store {path}: expected an object"
        )));
    };

    let mut data = TrustFile::new();
    for (key, value) in map {
        let decision = match value {
            Value::Bool(b) => Some(b),
            Value::Null => None,
            _ => {
                let key_json = serde_json::to_string(&key).unwrap_or_else(|_| format!("{key:?}"));
                return Err(TrustStoreError(format!(
                    "Invalid trust store {path}: value for {key_json} must be true, false, or null"
                )));
            }
        };
        data.insert(key, decision);
    }
    Ok(data)
}

fn write_trust_file(path: &str, data: &TrustFile) -> Result<(), TrustStoreError> {
    // BTreeMap already yields sorted keys, matching pi's `Object.keys(data).sort()`.
    let mut object = serde_json::Map::new();
    for (key, value) in data {
        let json_value = match value {
            Some(b) => Value::Bool(*b),
            None => Value::Null,
        };
        object.insert(key.clone(), json_value);
    }
    let serialized = serde_json::to_string_pretty(&Value::Object(object))
        .map_err(|error| TrustStoreError(format!("Failed to serialize trust store: {error}")))?;

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            TrustStoreError(format!("Failed to create trust store directory: {error}"))
        })?;
    }
    std::fs::write(path, format!("{serialized}\n"))
        .map_err(|error| TrustStoreError(format!("Failed to write trust store {path}: {error}")))?;
    Ok(())
}

/// Returns true when `cwd` has project-local resources that must be gated by
/// project trust: trust-requiring entries under `cwd/.pi`, or `.agents/skills`
/// in `cwd` or one of its ancestors. The user/global `~/.agents/skills`
/// directory is always treated as a trusted user resource and ignored, even
/// when `cwd` is `$HOME`.
pub fn has_trust_requiring_project_resources(cwd: &str) -> bool {
    let home = std::env::var("HOME").ok().filter(|value| !value.is_empty());
    let home = home.unwrap_or_default();
    has_trust_requiring_project_resources_with_home(cwd, &home)
}

/// [`has_trust_requiring_project_resources`] with an explicit home directory, so
/// callers (and tests) can supply `$HOME` without mutating process-global env.
pub fn has_trust_requiring_project_resources_with_home(cwd: &str, home_dir: &str) -> bool {
    let home = canonicalize_path(&resolve(home_dir));
    let user_agents_skills_dir = join(&home, &[".agents", "skills"]);
    let mut current_dir = canonicalize_path(&resolve(cwd));

    let config_dir = join(&current_dir, &[CONFIG_DIR_NAME]);
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| Path::new(&join(&config_dir, &[entry])).exists())
    {
        return true;
    }

    loop {
        let agents_skills_dir = join(&current_dir, &[".agents", "skills"]);
        if agents_skills_dir != user_agents_skills_dir && Path::new(&agents_skills_dir).exists() {
            return true;
        }

        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            return false;
        }
        current_dir = parent_dir;
    }
}

/// Persists per-directory project-trust decisions to `<agent_dir>/trust.json`.
pub struct ProjectTrustStore {
    trust_path: String,
}

impl ProjectTrustStore {
    /// Create a store backed by `<agent_dir>/trust.json`.
    pub fn new(agent_dir: &str) -> Self {
        let trust_path = join(&resolve(agent_dir), &["trust.json"]);
        Self { trust_path }
    }

    /// The nearest recorded decision for `cwd`, or `None` if none applies.
    pub fn get(&self, cwd: &str) -> Result<ProjectTrustDecision, TrustStoreError> {
        Ok(self.get_entry(cwd)?.map(|entry| entry.decision))
    }

    /// The nearest recorded trust entry for `cwd`, or `None`.
    pub fn get_entry(&self, cwd: &str) -> Result<Option<ProjectTrustStoreEntry>, TrustStoreError> {
        let data = read_trust_file(&self.trust_path)?;
        Ok(find_nearest_trust_entry(&data, cwd))
    }

    /// Record a single decision for `cwd` (`None` clears it).
    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), TrustStoreError> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_string(),
            decision,
        }])
    }

    /// Apply a batch of trust updates atomically (relative to a single
    /// read-modify-write of the file).
    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), TrustStoreError> {
        let mut data = read_trust_file(&self.trust_path)?;
        for update in decisions {
            let key = normalize_cwd(&update.path);
            match update.decision {
                None => {
                    data.remove(&key);
                }
                Some(decision) => {
                    data.insert(key, Some(decision));
                }
            }
        }
        write_trust_file(&self.trust_path, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{s, scratch_dir};

    #[test]
    fn stores_decisions_and_inherits_from_parent_directories() {
        let temp = scratch_dir("store");
        let agent_dir = temp.join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let store = ProjectTrustStore::new(&s(&agent_dir));

        let parent_dir = temp.join("trusted-parent");
        let child_dir = parent_dir.join("project");
        std::fs::create_dir_all(&child_dir).unwrap();

        assert_eq!(store.get(&s(&child_dir)).unwrap(), None);
        store.set(&s(&parent_dir), Some(true)).unwrap();
        assert_eq!(store.get(&s(&child_dir)).unwrap(), Some(true));
        store.set(&s(&child_dir), Some(false)).unwrap();
        assert_eq!(store.get(&s(&child_dir)).unwrap(), Some(false));
        store.set(&s(&child_dir), None).unwrap();
        assert_eq!(store.get(&s(&child_dir)).unwrap(), Some(true));

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn detects_trust_requiring_project_resources() {
        // NOTE: pi's test mutates `process.env.HOME`; the port injects `$HOME`
        // via `_with_home` so the assertions run without touching process-global
        // env (which would race other parallel tests). Behavior is identical.
        let temp = scratch_dir("resources");
        let home = s(&temp);
        let cwd = temp.join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        std::fs::create_dir_all(temp.join(".pi").join("agent")).unwrap();
        std::fs::create_dir_all(temp.join(".agents").join("skills")).unwrap();
        assert!(!has_trust_requiring_project_resources_with_home(
            &s(&temp),
            &home
        ));
        assert!(!has_trust_requiring_project_resources_with_home(
            &s(&cwd),
            &home
        ));

        std::fs::write(temp.join(".pi").join("settings.json"), "{}").unwrap();
        assert!(has_trust_requiring_project_resources_with_home(
            &s(&temp),
            &home
        ));
        std::fs::remove_file(temp.join(".pi").join("settings.json")).unwrap();

        std::fs::create_dir_all(cwd.join(".pi")).unwrap();
        std::fs::write(cwd.join(".pi").join("settings.json"), "{}").unwrap();
        assert!(has_trust_requiring_project_resources_with_home(
            &s(&cwd),
            &home
        ));

        std::fs::remove_dir_all(cwd.join(".pi")).unwrap();
        std::fs::create_dir_all(cwd.join(".agents").join("skills")).unwrap();
        assert!(has_trust_requiring_project_resources_with_home(
            &s(&cwd),
            &home
        ));

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn trust_options_include_parent_and_session_variants() {
        let temp = scratch_dir("options");
        let cwd = temp.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd_norm = normalize_cwd(&s(&cwd));
        let parent = get_project_trust_parent_path(&s(&cwd)).unwrap();

        let options = get_project_trust_options(&s(&cwd), true);
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Trust",
                format!("Trust parent folder ({parent})").as_str(),
                "Trust (this session only)",
                "Do not trust",
                "Do not trust (this session only)",
            ]
        );

        // "Trust" saves the cwd decision.
        assert_eq!(options[0].updates.len(), 1);
        assert_eq!(options[0].updates[0].path, cwd_norm);
        assert_eq!(options[0].updates[0].decision, Some(true));
        assert_eq!(options[0].saved_path.as_deref(), Some(cwd_norm.as_str()));

        // "Trust parent" trusts the parent and clears the cwd entry.
        assert_eq!(options[1].updates.len(), 2);
        assert_eq!(options[1].updates[0].decision, Some(true));
        assert_eq!(options[1].updates[1].decision, None);

        // Session-only variants persist nothing.
        assert!(options[2].updates.is_empty());
        assert!(options[2].saved_path.is_none());

        // Without session-only, only the two persisting options remain.
        let short = get_project_trust_options(&s(&cwd), false);
        assert_eq!(short.len(), 3); // Trust, Trust parent, Do not trust
        assert!(short.iter().all(|o| !o.label.contains("session only")));

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn write_then_read_round_trips_sorted_json() {
        let temp = scratch_dir("json");
        let agent_dir = temp.join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let store = ProjectTrustStore::new(&s(&agent_dir));
        let a = temp.join("a");
        let b = temp.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        store.set(&s(&b), Some(false)).unwrap();
        store.set(&s(&a), Some(true)).unwrap();

        let raw = std::fs::read_to_string(agent_dir.join("trust.json")).unwrap();
        assert!(raw.ends_with("}\n"));
        // Keys are written sorted: the "a" path sorts before the "b" path.
        let a_key = normalize_cwd(&s(&a));
        let b_key = normalize_cwd(&s(&b));
        assert!(raw.find(&a_key).unwrap() < raw.find(&b_key).unwrap());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn rejects_non_object_trust_store() {
        let temp = scratch_dir("invalid");
        let agent_dir = temp.join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("trust.json"), "[]").unwrap();
        let store = ProjectTrustStore::new(&s(&agent_dir));
        let err = store.get(&s(&temp)).unwrap_err();
        assert!(err.to_string().contains("expected an object"));

        std::fs::write(agent_dir.join("trust.json"), "{\"/x\": 3}").unwrap();
        let err = store.get(&s(&temp)).unwrap_err();
        assert!(err.to_string().contains("must be true, false, or null"));

        std::fs::remove_dir_all(&temp).ok();
    }
}
