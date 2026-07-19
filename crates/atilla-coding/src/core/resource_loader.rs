//! Resource discovery engine ported from pi's `core/resource-loader.ts`.
//!
//! pi's `DefaultResourceLoader` is a large orchestrator: it wires together the
//! extension loader, the skill/prompt/theme loaders, the settings manager and
//! the package manager, then decorates every resolved resource with provenance
//! and collision diagnostics. Most of those collaborators have not yet been
//! ported to Rust (`loadSkills`, `loadPromptTemplates`, the theme loader, the
//! extension runtime, and `SettingsManager` all still live only in
//! `vendor/pi`). What `resource-loader.ts` genuinely *owns* — as opposed to
//! delegates — is a cluster of pure discovery/precedence helpers:
//!
//! * project context-file discovery (`AGENTS.md` / `CLAUDE.md` ancestor walk),
//! * system/append-system prompt-file discovery (trust-gated),
//! * `file`-or-literal prompt-input resolution,
//! * path merging + canonical de-duplication (project-before-user precedence),
//! * [`SourceInfo`] construction and prefix-matched lookup,
//! * skill-path mapping (`dir` -> `dir/SKILL.md`),
//! * extension-path normalization (resolving `file://` URLs and base dirs),
//! * theme-file directory discovery, and
//! * name-collision detection for prompts, themes and extension tools/flags.
//!
//! This module ports exactly that engine, with the filesystem roots (`cwd`,
//! `agentDir`) and the project-trust flag injected as seams so it is unit
//! testable against self-contained temp-dir fixtures. The stateful
//! `DefaultResourceLoader` class, its `reload()`/`extendResources()` lifecycle,
//! and the integration tests in `resource-loader.test.ts` (which stand up the
//! whole loader plus `SettingsManager`, `ExtensionRunner`, `SessionManager`,
//! …) are deferred until those collaborators land; see the crate-level porting
//! notes.
//!
//! Reused from already-merged modules:
//! * [`crate::core::diagnostics`] — `ResourceDiagnostic` / `ResourceCollision`
//!   / `ResourceType` / `DiagnosticType`.
//! * [`crate::core::source_info`] — `SourceInfo` / `PathMetadata` /
//!   `SourceScope` / `SourceOrigin` / [`source_info::create`].
//! * [`crate::utils::paths`] — `resolve_path`, `canonicalize_path`.
//! * [`crate::core::package_manager::CONFIG_DIR_NAME`] — the `.pi` config dir.

use std::collections::HashSet;
use std::path::Path;

use crate::core::diagnostics::{
    DiagnosticType, ResourceCollision, ResourceDiagnostic, ResourceType,
};
use crate::core::package_manager::CONFIG_DIR_NAME;
use crate::core::source_info::{self, PathMetadata, SourceInfo, SourceOrigin, SourceScope};
use crate::utils::paths::{canonicalize_path, resolve_path, PathInputOptions};

/// A discovered project context file (`AGENTS.md` / `CLAUDE.md`) and its
/// contents. Port of pi's inline `{ path; content }` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    /// Absolute path of the context file.
    pub path: String,
    /// Full UTF-8 contents of the file.
    pub content: String,
}

/// A `{ path, metadata }` pair as accepted by [`ResourceLoader::extend_paths`],
/// mirroring the entries pi's `extendResources` / `normalizeExtensionPaths`
/// consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPathEntry {
    /// The (possibly `file://`, possibly relative) resource path.
    pub path: String,
    /// Provenance metadata for the path.
    pub metadata: PathMetadata,
}

/// The result of mapping an auto-discovered skill resource to its concrete
/// file, from [`ResourceLoader::map_skill_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPathMapping {
    /// The path the loader should hand to the skill loader (either the original
    /// directory/file, or `dir/SKILL.md` when that file exists).
    pub path: String,
    /// When a `SKILL.md` file was resolved, the `(path, metadata)` entry pi
    /// records in its `metadataByPath` map. The orchestrator inserts this into
    /// its provenance table (only when the path is not already present).
    pub extra_metadata: Option<(String, PathMetadata)>,
}

/// A minimal prompt record for [`dedupe_prompts`]. The full `PromptTemplate`
/// type is not yet ported; this carries only the fields the de-dup logic reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEntry {
    /// The prompt's invocation name (without the leading `/`).
    pub name: String,
    /// The path the prompt was loaded from.
    pub file_path: String,
}

/// A minimal theme record for [`dedupe_themes`]. The full `Theme` type is not
/// yet ported; this carries only the fields the de-dup logic reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    /// The theme's name; pi treats a missing name as `"unnamed"`.
    pub name: Option<String>,
    /// The `.json` file the theme was loaded from, if any.
    pub source_path: Option<String>,
}

/// A tool/flag-bearing extension for [`detect_extension_conflicts`]. The full
/// `Extension` type is not yet ported; this carries only the registered tool
/// and flag names plus the owning path that conflict detection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionConflictInput {
    /// The extension's source path (its identity for conflict reporting).
    pub path: String,
    /// Names of tools this extension registers.
    pub tools: Vec<String>,
    /// Names of flags this extension registers (without the `--`).
    pub flags: Vec<String>,
}

/// A single detected extension conflict, mirroring pi's `{ path, message }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionConflict {
    /// The path of the extension whose registration conflicts.
    pub path: String,
    /// The human-readable conflict message.
    pub message: String,
}

// -- free-standing discovery helpers ----------------------------------------

fn default_options() -> PathInputOptions {
    PathInputOptions::default()
}

/// Resolve `path` the way pi's single-argument `resolvePath(x)` does: against
/// the process working directory. Absolute inputs are simply normalized.
fn resolve_against_cwd(path: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    resolve_path(path, &cwd, &default_options()).unwrap_or_else(|_| path.to_string())
}

/// Join a path segment onto `base` using POSIX semantics.
fn join(base: &str, segment: &str) -> String {
    Path::new(base).join(segment).to_string_lossy().into_owned()
}

/// Port of pi's `loadContextFileFromDir`: return the first of
/// `AGENTS.md`/`AGENTS.MD`/`CLAUDE.md`/`CLAUDE.MD` that exists in `dir`.
fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    const CANDIDATES: [&str; 4] = ["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];
    for filename in CANDIDATES {
        let file_path = join(dir, filename);
        if Path::new(&file_path).exists() {
            match std::fs::read_to_string(&file_path) {
                Ok(content) => {
                    return Some(ContextFile {
                        path: file_path,
                        content,
                    });
                }
                Err(error) => {
                    eprintln!("Warning: Could not read {file_path}: {error}");
                }
            }
        }
    }
    None
}

/// Port of pi's exported `loadProjectContextFiles`: collect the global context
/// file from `agent_dir`, then every distinct context file walking from `cwd`
/// up to the filesystem root, ordered root-most first and appended after the
/// global file.
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let resolved_cwd = resolve_against_cwd(cwd);
    let resolved_agent_dir = resolve_against_cwd(agent_dir);

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    if let Some(global) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.insert(global.path.clone());
        context_files.push(global);
    }

    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();
    let mut current_dir = resolved_cwd;
    loop {
        if let Some(context_file) = load_context_file_from_dir(&current_dir) {
            if !seen_paths.contains(&context_file.path) {
                seen_paths.insert(context_file.path.clone());
                ancestor_context_files.insert(0, context_file);
            }
        }

        let parent = match Path::new(&current_dir).parent() {
            Some(parent) => parent.to_string_lossy().into_owned(),
            None => break,
        };
        if parent == current_dir {
            break;
        }
        current_dir = parent;
    }

    context_files.extend(ancestor_context_files);
    context_files
}

/// Port of pi's `resolvePromptInput`: if `input` names an existing file, return
/// its contents; otherwise return `input` verbatim. `None` stays `None`.
pub fn resolve_prompt_input(input: Option<&str>, description: &str) -> Option<String> {
    let input = input?;
    if Path::new(input).exists() {
        match std::fs::read_to_string(input) {
            Ok(content) => return Some(content),
            Err(error) => {
                eprintln!("Warning: Could not read {description} file {input}: {error}");
                return Some(input.to_string());
            }
        }
    }
    Some(input.to_string())
}

// -- name-collision detection -----------------------------------------------

/// Generic port of pi's `dedupePrompts`/`dedupeThemes` de-dup-by-name pass:
/// keep the first item seen for each name and emit a `collision`
/// [`ResourceDiagnostic`] for every later item that shadows it. The winner is
/// the earlier item, matching pi's project-before-user ordering.
///
/// `name_of` extracts the collision key, `path_of` the (optional) source path,
/// `message_of` builds the diagnostic message from the name, and `default_path`
/// substitutes for a missing path in the collision record (pi uses
/// `"<builtin>"` for themes).
pub fn dedupe_named<T>(
    items: Vec<T>,
    resource_type: ResourceType,
    name_of: impl Fn(&T) -> String,
    path_of: impl Fn(&T) -> Option<String>,
    message_of: impl Fn(&str) -> String,
    default_path: &str,
) -> (Vec<T>, Vec<ResourceDiagnostic>) {
    let mut kept: Vec<T> = Vec::new();
    let mut winner_by_name: Vec<(String, Option<String>)> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    for item in items {
        let name = name_of(&item);
        let existing = winner_by_name.iter().find(|(n, _)| *n == name);
        match existing {
            Some((_, winner_path)) => {
                let loser_path = path_of(&item);
                diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: DiagnosticType::Collision,
                    message: message_of(&name),
                    path: loser_path.clone(),
                    collision: Some(ResourceCollision {
                        resource_type,
                        name: name.clone(),
                        winner_path: winner_path
                            .clone()
                            .unwrap_or_else(|| default_path.to_string()),
                        loser_path: loser_path.unwrap_or_else(|| default_path.to_string()),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            }
            None => {
                winner_by_name.push((name, path_of(&item)));
                kept.push(item);
            }
        }
    }

    (kept, diagnostics)
}

/// Port of pi's `dedupePrompts`: de-dup prompts by name, emitting
/// `name "/{name}" collision` diagnostics for shadowed entries.
pub fn dedupe_prompts(prompts: Vec<PromptEntry>) -> (Vec<PromptEntry>, Vec<ResourceDiagnostic>) {
    dedupe_named(
        prompts,
        ResourceType::Prompt,
        |p| p.name.clone(),
        |p| Some(p.file_path.clone()),
        |name| format!("name \"/{name}\" collision"),
        "",
    )
}

/// Port of pi's `dedupeThemes`: de-dup themes by name (missing name -> `"unnamed"`),
/// emitting `name "{name}" collision` diagnostics with `"<builtin>"` standing
/// in for a missing source path.
pub fn dedupe_themes(themes: Vec<ThemeEntry>) -> (Vec<ThemeEntry>, Vec<ResourceDiagnostic>) {
    dedupe_named(
        themes,
        ResourceType::Theme,
        |t| t.name.clone().unwrap_or_else(|| "unnamed".to_string()),
        |t| t.source_path.clone(),
        |name| format!("name \"{name}\" collision"),
        "<builtin>",
    )
}

/// Port of pi's `detectExtensionConflicts`: flag a conflict whenever two
/// different extensions register a tool or flag of the same name. The first
/// registrant wins ownership; later ones report the conflict. All extensions
/// stay loaded (precedence is handled by load order elsewhere).
pub fn detect_extension_conflicts(extensions: &[ExtensionConflictInput]) -> Vec<ExtensionConflict> {
    let mut conflicts: Vec<ExtensionConflict> = Vec::new();
    let mut tool_owners: Vec<(String, String)> = Vec::new();
    let mut flag_owners: Vec<(String, String)> = Vec::new();

    let register = |owners: &mut Vec<(String, String)>,
                    conflicts: &mut Vec<ExtensionConflict>,
                    name: &str,
                    ext_path: &str,
                    message: String| {
        match owners.iter().find(|(n, _)| n == name) {
            Some((_, existing_owner)) if existing_owner != ext_path => {
                conflicts.push(ExtensionConflict {
                    path: ext_path.to_string(),
                    message,
                });
            }
            Some(_) => {}
            None => owners.push((name.to_string(), ext_path.to_string())),
        }
    };

    for ext in extensions {
        for tool_name in &ext.tools {
            let owner = tool_owners
                .iter()
                .find(|(n, _)| n == tool_name)
                .map(|(_, o)| o.clone());
            let message = owner
                .as_deref()
                .map(|o| format!("Tool \"{tool_name}\" conflicts with {o}"))
                .unwrap_or_default();
            register(
                &mut tool_owners,
                &mut conflicts,
                tool_name,
                &ext.path,
                message,
            );
        }
        for flag_name in &ext.flags {
            let owner = flag_owners
                .iter()
                .find(|(n, _)| n == flag_name)
                .map(|(_, o)| o.clone());
            let message = owner
                .as_deref()
                .map(|o| format!("Flag \"--{flag_name}\" conflicts with {o}"))
                .unwrap_or_default();
            register(
                &mut flag_owners,
                &mut conflicts,
                flag_name,
                &ext.path,
                message,
            );
        }
    }

    conflicts
}

// -- the injected-root discovery engine -------------------------------------

/// The pure discovery/precedence engine extracted from pi's
/// `DefaultResourceLoader`, with its filesystem roots and project-trust flag
/// injected rather than read from a `SettingsManager`.
#[derive(Debug, Clone)]
pub struct ResourceLoader {
    cwd: String,
    agent_dir: String,
    project_trusted: bool,
}

impl ResourceLoader {
    /// Construct a loader rooted at `cwd`/`agent_dir` (resolved against the
    /// process working directory, as pi's constructor does). `project_trusted`
    /// stands in for `settingsManager.isProjectTrusted()`.
    pub fn new(cwd: &str, agent_dir: &str, project_trusted: bool) -> Self {
        Self {
            cwd: resolve_against_cwd(cwd),
            agent_dir: resolve_against_cwd(agent_dir),
            project_trusted,
        }
    }

    /// The resolved current working directory root.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The resolved agent-config directory root.
    pub fn agent_dir(&self) -> &str {
        &self.agent_dir
    }

    /// Port of pi's `resolveResourcePath`: resolve `p` against `cwd`, trimming
    /// surrounding whitespace first.
    pub fn resolve_resource_path(&self, p: &str) -> String {
        let options = PathInputOptions {
            trim: true,
            ..PathInputOptions::default()
        };
        resolve_path(p, &self.cwd, &options).unwrap_or_else(|_| p.to_string())
    }

    /// Port of pi's `mergePaths`: resolve each path in `primary` then
    /// `additional`, de-duplicating by canonical (symlink-resolved) form while
    /// preserving the resolved (non-canonical) representative and first-seen
    /// order. Because callers pass project paths before user paths, the project
    /// alias of a shared directory is the surviving representative.
    pub fn merge_paths(&self, primary: &[String], additional: &[String]) -> Vec<String> {
        let mut merged: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for p in primary.iter().chain(additional.iter()) {
            let resolved = self.resolve_resource_path(p);
            let canonical = canonicalize_path(&resolved);
            if seen.contains(&canonical) {
                continue;
            }
            seen.insert(canonical);
            merged.push(resolved);
        }

        merged
    }

    /// Port of pi's `normalizeExtensionPaths`: resolve each entry's path
    /// (handling `file://` URLs and relative paths) and, when present, its
    /// metadata `base_dir`.
    pub fn normalize_extension_paths(
        &self,
        entries: &[ExtensionPathEntry],
    ) -> Vec<ExtensionPathEntry> {
        entries
            .iter()
            .map(|entry| {
                let metadata = match &entry.metadata.base_dir {
                    Some(base_dir) => PathMetadata {
                        base_dir: Some(self.resolve_resource_path(base_dir)),
                        ..entry.metadata.clone()
                    },
                    None => entry.metadata.clone(),
                };
                ExtensionPathEntry {
                    path: self.resolve_resource_path(&entry.path),
                    metadata,
                }
            })
            .collect()
    }

    /// Port of pi's `mapSkillPath`: when an auto-discovered (or package-origin)
    /// skill resource is a directory containing `SKILL.md`, map it to that file
    /// and surface the `(SKILL.md, metadata)` provenance entry the orchestrator
    /// records. Otherwise return the path unchanged.
    pub fn map_skill_path(&self, path: &str, metadata: &PathMetadata) -> SkillPathMapping {
        if metadata.source != "auto" && metadata.origin != SourceOrigin::Package {
            return SkillPathMapping {
                path: path.to_string(),
                extra_metadata: None,
            };
        }
        let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
        if !is_dir {
            return SkillPathMapping {
                path: path.to_string(),
                extra_metadata: None,
            };
        }
        let skill_file = join(path, "SKILL.md");
        if Path::new(&skill_file).exists() {
            return SkillPathMapping {
                path: skill_file.clone(),
                extra_metadata: Some((skill_file, metadata.clone())),
            };
        }
        SkillPathMapping {
            path: path.to_string(),
            extra_metadata: None,
        }
    }

    /// Port of pi's `getDefaultSourceInfoForPath`: classify `file_path` by which
    /// known config root (agent-user or project) it lives under, falling back to
    /// a temporary/top-level source rooted at the path (or its parent).
    /// Synthetic `<source:...>` paths become a temporary source named after the
    /// bracketed prefix.
    pub fn default_source_info_for_path(&self, file_path: &str) -> SourceInfo {
        if file_path.starts_with('<') && file_path.ends_with('>') {
            let inner = &file_path[1..file_path.len() - 1];
            let source = inner
                .split(':')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("temporary");
            return SourceInfo {
                path: file_path.to_string(),
                source: source.to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            };
        }

        let normalized = resolve_against_cwd(file_path);

        let agent_roots = [
            join(&self.agent_dir, "skills"),
            join(&self.agent_dir, "prompts"),
            join(&self.agent_dir, "themes"),
            join(&self.agent_dir, "extensions"),
        ];
        for root in &agent_roots {
            if Self::is_under_path(&normalized, root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: SourceScope::User,
                    origin: SourceOrigin::TopLevel,
                    base_dir: Some(resolve_against_cwd(root)),
                };
            }
        }

        let project_config = join(&self.cwd, CONFIG_DIR_NAME);
        let project_roots = [
            join(&project_config, "skills"),
            join(&project_config, "prompts"),
            join(&project_config, "themes"),
            join(&project_config, "extensions"),
        ];
        for root in &project_roots {
            if Self::is_under_path(&normalized, root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: SourceScope::Project,
                    origin: SourceOrigin::TopLevel,
                    base_dir: Some(resolve_against_cwd(root)),
                };
            }
        }

        let base_dir = if std::fs::metadata(&normalized)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            normalized.clone()
        } else {
            Path::new(&normalized)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| normalized.clone())
        };

        SourceInfo {
            path: file_path.to_string(),
            source: "local".to_string(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(base_dir),
        }
    }

    /// Port of pi's `findSourceInfoForPath`: locate provenance for
    /// `resource_path`, first among `extra_source_infos` (extension-supplied,
    /// exact-or-prefix match, keeping the extension's info but the resource's
    /// own path), then among `metadata_by_path` (exact then prefix match, built
    /// via [`source_info::create`]). Bracketed synthetic paths resolve through
    /// [`Self::default_source_info_for_path`]. Both maps are ordered slices so
    /// prefix-match precedence is deterministic, mirroring JS `Map` iteration.
    pub fn find_source_info_for_path(
        &self,
        resource_path: &str,
        extra_source_infos: &[(String, SourceInfo)],
        metadata_by_path: &[(String, PathMetadata)],
    ) -> Option<SourceInfo> {
        if resource_path.is_empty() {
            return None;
        }
        if resource_path.starts_with('<') {
            return Some(self.default_source_info_for_path(resource_path));
        }

        let normalized_resource = resolve_against_cwd(resource_path);

        for (source_path, source_info) in extra_source_infos {
            let normalized_source = resolve_against_cwd(source_path);
            if Self::path_matches_prefix(&normalized_resource, &normalized_source) {
                return Some(SourceInfo {
                    path: resource_path.to_string(),
                    ..source_info.clone()
                });
            }
        }

        let exact = metadata_by_path
            .iter()
            .find(|(k, _)| *k == normalized_resource || *k == resource_path)
            .map(|(_, m)| m.clone());
        if let Some(metadata) = exact {
            return Some(source_info::create(resource_path, metadata));
        }

        for (source_path, metadata) in metadata_by_path {
            let normalized_source = resolve_against_cwd(source_path);
            if Self::path_matches_prefix(&normalized_resource, &normalized_source) {
                return Some(source_info::create(resource_path, metadata.clone()));
            }
        }

        None
    }

    /// Return the trust-gated system-prompt file: the project `.pi/SYSTEM.md`
    /// when the project is trusted and it exists, otherwise the global
    /// `agent_dir/SYSTEM.md` when it exists. Port of `discoverSystemPromptFile`.
    pub fn discover_system_prompt_file(&self) -> Option<String> {
        self.discover_prompt_file("SYSTEM.md")
    }

    /// Same as [`Self::discover_system_prompt_file`] for `APPEND_SYSTEM.md`.
    /// Port of `discoverAppendSystemPromptFile`.
    pub fn discover_append_system_prompt_file(&self) -> Option<String> {
        self.discover_prompt_file("APPEND_SYSTEM.md")
    }

    fn discover_prompt_file(&self, filename: &str) -> Option<String> {
        let project_path = join(&join(&self.cwd, CONFIG_DIR_NAME), filename);
        if self.project_trusted && Path::new(&project_path).exists() {
            return Some(project_path);
        }
        let global_path = join(&self.agent_dir, filename);
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }
        None
    }

    /// Port of pi's `isUnderPath`: whether `target` equals `root` (both
    /// resolved) or lies beneath it.
    fn is_under_path(target: &str, root: &str) -> bool {
        let normalized_root = resolve_against_cwd(root);
        Self::path_matches_prefix(target, &normalized_root)
    }

    /// Whether `target` equals `prefix` or begins with `prefix` followed by a
    /// path separator.
    fn path_matches_prefix(target: &str, prefix: &str) -> bool {
        if target == prefix {
            return true;
        }
        let boundary = if prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{prefix}/")
        };
        target.starts_with(&boundary)
    }
}

// -- theme-file discovery ----------------------------------------------------

/// The candidate theme files discovered under a set of paths, plus any warning
/// diagnostics raised while scanning. Port of the discovery half of pi's
/// `loadThemes` (the JSON-parse half belongs to the not-yet-ported theme
/// module).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeDiscovery {
    /// Absolute paths of discovered `.json` theme files, in scan order.
    pub files: Vec<String>,
    /// Warnings for paths that were missing or not JSON files.
    pub diagnostics: Vec<ResourceDiagnostic>,
}

fn warning(message: &str, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic {
        diagnostic_type: DiagnosticType::Warning,
        message: message.to_string(),
        path: Some(path.to_string()),
        collision: None,
    }
}

impl ResourceLoader {
    /// Port of the discovery portion of pi's `loadThemes`: for each input path,
    /// scan a directory for `.json` files (following symlinks) or accept a
    /// single `.json` file, recording warning diagnostics for missing paths and
    /// non-JSON files. Returns candidate file paths for the theme loader to
    /// parse.
    pub fn discover_theme_files(&self, paths: &[String]) -> ThemeDiscovery {
        let mut discovery = ThemeDiscovery::default();

        for p in paths {
            let resolved = self.resolve_resource_path(p);
            if !Path::new(&resolved).exists() {
                discovery
                    .diagnostics
                    .push(warning("theme path does not exist", &resolved));
                continue;
            }

            match std::fs::metadata(&resolved) {
                Ok(stats) if stats.is_dir() => {
                    self.scan_theme_dir(&resolved, &mut discovery);
                }
                Ok(stats) if stats.is_file() && resolved.ends_with(".json") => {
                    discovery.files.push(resolved);
                }
                Ok(_) => {
                    discovery
                        .diagnostics
                        .push(warning("theme path is not a json file", &resolved));
                }
                Err(error) => {
                    discovery
                        .diagnostics
                        .push(warning(&error.to_string(), &resolved));
                }
            }
        }

        discovery
    }

    /// Port of pi's `loadThemesFromDir`: collect `.json` entries in `dir`,
    /// resolving symlinks to check they point at files. Missing dirs are
    /// skipped; read errors become warnings.
    fn scan_theme_dir(&self, dir: &str, discovery: &mut ThemeDiscovery) {
        if !Path::new(dir).exists() {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                discovery.diagnostics.push(warning(&error.to_string(), dir));
                return;
            }
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let full_path = join(dir, &name);
            let is_file = if file_type.is_symlink() {
                match std::fs::metadata(&full_path) {
                    Ok(target) => target.is_file(),
                    Err(_) => continue,
                }
            } else {
                file_type.is_file()
            };
            if !is_file || !name.ends_with(".json") {
                continue;
            }
            discovery.files.push(full_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{s, scratch_dir, write};
    use std::fs;

    fn meta(source: &str, scope: SourceScope, origin: SourceOrigin) -> PathMetadata {
        PathMetadata {
            source: source.to_string(),
            scope,
            origin,
            base_dir: None,
        }
    }

    /// Create a scratch root with `project/` (cwd) and `agent/` subdirs,
    /// returning both as strings. Shared setup for the many tests that root a
    /// [`ResourceLoader`] at a project + agent-config pair.
    fn project_agent(tag: &str) -> (String, String) {
        let root = scratch_dir(tag);
        let cwd = s(&root.join("project"));
        let agent = s(&root.join("agent"));
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        (cwd, agent)
    }

    // -- context files ------------------------------------------------------

    #[test]
    fn discovers_agents_md_from_cwd() {
        let (cwd, agent) = project_agent("rl-ctx-cwd");
        write(&join(&cwd, "AGENTS.md"), "# Project Guidelines");

        let files = load_project_context_files(&cwd, &agent);
        assert!(files.iter().any(|f| f.path.contains("AGENTS.md")));
    }

    #[test]
    fn context_walk_orders_global_then_ancestors_rootmost_first() {
        let root = scratch_dir("rl-ctx-walk");
        let agent = s(&root.join("agent"));
        let parent = s(&root.join("project"));
        let cwd = s(&root.join("project").join("nested"));
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        write(&join(&agent, "AGENTS.md"), "global");
        write(&join(&parent, "AGENTS.md"), "parent");
        write(&join(&cwd, "AGENTS.md"), "child");

        let files = load_project_context_files(&cwd, &agent);
        // Global first, then ancestors root-most first (parent before child).
        assert_eq!(files[0].content, "global");
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        let parent_idx = contents.iter().position(|c| *c == "parent").unwrap();
        let child_idx = contents.iter().position(|c| *c == "child").unwrap();
        assert!(parent_idx < child_idx);
    }

    #[test]
    fn context_file_candidate_precedence_prefers_agents_md() {
        let (cwd, agent) = project_agent("rl-ctx-prec");
        write(&join(&cwd, "AGENTS.md"), "agents");
        write(&join(&cwd, "CLAUDE.md"), "claude");

        let files = load_project_context_files(&cwd, &agent);
        // Only one context file per dir; AGENTS.md wins over CLAUDE.md.
        let from_cwd: Vec<&ContextFile> =
            files.iter().filter(|f| f.path.starts_with(&cwd)).collect();
        assert_eq!(from_cwd.len(), 1);
        assert_eq!(from_cwd[0].content, "agents");
    }

    // -- prompt input resolution -------------------------------------------

    #[test]
    fn resolve_prompt_input_reads_existing_file() {
        let root = scratch_dir("rl-prompt-input");
        let file = join(&s(&root), "prompt.md");
        write(&file, "file contents");
        assert_eq!(
            resolve_prompt_input(Some(&file), "system prompt"),
            Some("file contents".to_string())
        );
    }

    #[test]
    fn resolve_prompt_input_passes_through_literal_and_none() {
        assert_eq!(
            resolve_prompt_input(Some("literal text"), "system prompt"),
            Some("literal text".to_string())
        );
        assert_eq!(resolve_prompt_input(None, "system prompt"), None);
    }

    // -- system / append prompt discovery ----------------------------------

    #[test]
    fn discovers_project_system_md_when_trusted() {
        let (cwd, agent) = project_agent("rl-sys-project");
        let project_path = join(&join(&cwd, ".pi"), "SYSTEM.md");
        write(&project_path, "project system");
        write(&join(&agent, "SYSTEM.md"), "global system");

        let loader = ResourceLoader::new(&cwd, &agent, true);
        assert_eq!(
            loader.discover_system_prompt_file(),
            Some(resolve_against_cwd(&project_path))
        );
    }

    #[test]
    fn falls_back_to_global_system_md_when_untrusted() {
        let (cwd, agent) = project_agent("rl-sys-global");
        write(&join(&join(&cwd, ".pi"), "SYSTEM.md"), "project system");
        let global = join(&agent, "SYSTEM.md");
        write(&global, "global system");

        let loader = ResourceLoader::new(&cwd, &agent, false);
        assert_eq!(
            loader.discover_system_prompt_file(),
            Some(resolve_against_cwd(&global))
        );
    }

    #[test]
    fn discovers_append_system_md() {
        let (cwd, agent) = project_agent("rl-append");
        let project_path = join(&join(&cwd, ".pi"), "APPEND_SYSTEM.md");
        write(&project_path, "append");

        let loader = ResourceLoader::new(&cwd, &agent, true);
        assert_eq!(
            loader.discover_append_system_prompt_file(),
            Some(resolve_against_cwd(&project_path))
        );
    }

    // -- merge_paths --------------------------------------------------------

    #[test]
    fn merge_paths_dedupes_symlinked_alias_keeping_first_seen() {
        let root = scratch_dir("rl-merge");
        let shared = s(&root.join("shared"));
        fs::create_dir_all(&shared).unwrap();
        let project_alias = s(&root.join("project-link"));
        let user_alias = s(&root.join("user-link"));
        std::os::unix::fs::symlink(&shared, &project_alias).unwrap();
        std::os::unix::fs::symlink(&shared, &user_alias).unwrap();

        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        // Project paths passed first -> project alias survives.
        let merged = loader.merge_paths(
            std::slice::from_ref(&project_alias),
            std::slice::from_ref(&user_alias),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], resolve_against_cwd(&project_alias));
    }

    #[test]
    fn merge_paths_preserves_distinct_paths() {
        let root = scratch_dir("rl-merge-distinct");
        let a = s(&root.join("a"));
        let b = s(&root.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let merged = loader.merge_paths(std::slice::from_ref(&a), std::slice::from_ref(&b));
        assert_eq!(merged.len(), 2);
    }

    // -- source info --------------------------------------------------------

    #[test]
    fn default_source_info_classifies_agent_user_root() {
        let (cwd, agent) = project_agent("rl-si-agent");
        let loader = ResourceLoader::new(&cwd, &agent, true);
        let skill = join(&join(&agent, "skills"), "my-skill");
        let info = loader.default_source_info_for_path(&skill);
        assert_eq!(info.source, "local");
        assert_eq!(info.scope, SourceScope::User);
        assert_eq!(info.origin, SourceOrigin::TopLevel);
    }

    #[test]
    fn default_source_info_classifies_project_root() {
        let (cwd, agent) = project_agent("rl-si-project");
        let loader = ResourceLoader::new(&cwd, &agent, true);
        let prompt = join(&join(&join(&cwd, ".pi"), "prompts"), "commit.md");
        let info = loader.default_source_info_for_path(&prompt);
        assert_eq!(info.scope, SourceScope::Project);
    }

    #[test]
    fn default_source_info_synthetic_bracket_path() {
        let loader = ResourceLoader::new("/cwd", "/agent", true);
        let info = loader.default_source_info_for_path("<inline:extra>");
        assert_eq!(info.source, "inline");
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(info.base_dir, None);
    }

    #[test]
    fn default_source_info_temporary_fallback_uses_parent_dir_for_file() {
        let root = scratch_dir("rl-si-temp");
        let file = join(&s(&root), "loose.md");
        write(&file, "x");
        let loader = ResourceLoader::new("/nowhere/cwd", "/nowhere/agent", true);
        let info = loader.default_source_info_for_path(&file);
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(info.base_dir.as_deref(), Some(s(&root).as_str()));
    }

    #[test]
    fn find_source_info_prefers_extra_infos_by_prefix() {
        let loader = ResourceLoader::new("/cwd", "/agent", true);
        let base = SourceInfo {
            path: "/ext/base".to_string(),
            source: "extension:extra".to_string(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/ext".to_string()),
        };
        let extra = vec![("/ext".to_string(), base)];
        let found = loader
            .find_source_info_for_path("/ext/skills/SKILL.md", &extra, &[])
            .unwrap();
        assert_eq!(found.source, "extension:extra");
        // The resource's own path is kept, not the extra info's path.
        assert_eq!(found.path, "/ext/skills/SKILL.md");
    }

    #[test]
    fn find_source_info_exact_metadata_match() {
        let loader = ResourceLoader::new("/cwd", "/agent", true);
        let md = meta("npm:foo", SourceScope::Project, SourceOrigin::Package);
        let table = vec![("/pkg/skill/SKILL.md".to_string(), md)];
        let found = loader
            .find_source_info_for_path("/pkg/skill/SKILL.md", &[], &table)
            .unwrap();
        assert_eq!(found.source, "npm:foo");
        assert_eq!(found.origin, SourceOrigin::Package);
    }

    #[test]
    fn find_source_info_returns_none_when_unknown() {
        let loader = ResourceLoader::new("/cwd", "/agent", true);
        assert!(loader
            .find_source_info_for_path("/unknown/x", &[], &[])
            .is_none());
        assert!(loader.find_source_info_for_path("", &[], &[]).is_none());
    }

    // -- normalize_extension_paths -----------------------------------------

    #[test]
    fn normalize_extension_paths_resolves_file_url_and_base_dir() {
        let root = scratch_dir("rl-norm");
        let skill_dir = s(&root.join("extra skills").join("file-url-skill"));
        fs::create_dir_all(&skill_dir).unwrap();
        let url = format!("file://{skill_dir}");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let entries = vec![ExtensionPathEntry {
            path: url,
            metadata: PathMetadata {
                source: "extension:file-url".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: Some(skill_dir.clone()),
            },
        }];
        let normalized = loader.normalize_extension_paths(&entries);
        assert_eq!(normalized[0].path, resolve_against_cwd(&skill_dir));
        assert_eq!(
            normalized[0].metadata.base_dir.as_deref(),
            Some(resolve_against_cwd(&skill_dir).as_str())
        );
    }

    // -- map_skill_path -----------------------------------------------------

    #[test]
    fn map_skill_path_maps_auto_dir_to_skill_md() {
        let root = scratch_dir("rl-mapskill");
        let dir = s(&root.join("skill"));
        let skill_md = join(&dir, "SKILL.md");
        write(&skill_md, "---\nname: x\n---\n");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let md = meta("auto", SourceScope::User, SourceOrigin::TopLevel);
        let mapping = loader.map_skill_path(&dir, &md);
        assert_eq!(mapping.path, skill_md);
        assert_eq!(
            mapping.extra_metadata.as_ref().map(|(p, _)| p.clone()),
            Some(skill_md)
        );
    }

    #[test]
    fn map_skill_path_leaves_non_auto_non_package_untouched() {
        let root = scratch_dir("rl-mapskill-local");
        let dir = s(&root.join("skill"));
        write(&join(&dir, "SKILL.md"), "x");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let md = meta("local", SourceScope::User, SourceOrigin::TopLevel);
        let mapping = loader.map_skill_path(&dir, &md);
        assert_eq!(mapping.path, dir);
        assert!(mapping.extra_metadata.is_none());
    }

    // -- dedupe / collisions -----------------------------------------------

    #[test]
    fn dedupe_prompts_keeps_first_and_reports_collision() {
        let prompts = vec![
            PromptEntry {
                name: "commit".to_string(),
                file_path: "/project/commit.md".to_string(),
            },
            PromptEntry {
                name: "commit".to_string(),
                file_path: "/user/commit.md".to_string(),
            },
        ];
        let (kept, diagnostics) = dedupe_prompts(prompts);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].file_path, "/project/commit.md");
        assert_eq!(diagnostics.len(), 1);
        let diag = &diagnostics[0];
        assert_eq!(diag.diagnostic_type, DiagnosticType::Collision);
        assert_eq!(diag.message, "name \"/commit\" collision");
        let collision = diag.collision.as_ref().unwrap();
        assert_eq!(collision.resource_type, ResourceType::Prompt);
        assert_eq!(collision.winner_path, "/project/commit.md");
        assert_eq!(collision.loser_path, "/user/commit.md");
    }

    #[test]
    fn dedupe_themes_defaults_name_and_builtin_path() {
        let themes = vec![
            ThemeEntry {
                name: Some("dark".to_string()),
                source_path: Some("/project/dark.json".to_string()),
            },
            ThemeEntry {
                name: Some("dark".to_string()),
                source_path: None,
            },
        ];
        let (kept, diagnostics) = dedupe_themes(themes);
        assert_eq!(kept.len(), 1);
        assert_eq!(diagnostics.len(), 1);
        let collision = diagnostics[0].collision.as_ref().unwrap();
        assert_eq!(collision.resource_type, ResourceType::Theme);
        assert_eq!(collision.winner_path, "/project/dark.json");
        assert_eq!(collision.loser_path, "<builtin>");
        assert_eq!(diagnostics[0].message, "name \"dark\" collision");
    }

    #[test]
    fn dedupe_themes_treats_missing_name_as_unnamed() {
        let themes = vec![
            ThemeEntry {
                name: None,
                source_path: Some("/a.json".to_string()),
            },
            ThemeEntry {
                name: None,
                source_path: Some("/b.json".to_string()),
            },
        ];
        let (kept, diagnostics) = dedupe_themes(themes);
        assert_eq!(kept.len(), 1);
        assert_eq!(diagnostics[0].message, "name \"unnamed\" collision");
    }

    // -- extension conflicts -----------------------------------------------

    #[test]
    fn detect_tool_conflict_between_extensions() {
        let extensions = vec![
            ExtensionConflictInput {
                path: "/ext1".to_string(),
                tools: vec!["duplicate-tool".to_string()],
                flags: vec![],
            },
            ExtensionConflictInput {
                path: "/ext2".to_string(),
                tools: vec!["duplicate-tool".to_string()],
                flags: vec![],
            },
        ];
        let conflicts = detect_extension_conflicts(&extensions);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "/ext2");
        assert_eq!(
            conflicts[0].message,
            "Tool \"duplicate-tool\" conflicts with /ext1"
        );
    }

    #[test]
    fn detect_flag_conflict_between_extensions() {
        let extensions = vec![
            ExtensionConflictInput {
                path: "/ext1".to_string(),
                tools: vec![],
                flags: vec!["verbose".to_string()],
            },
            ExtensionConflictInput {
                path: "/ext2".to_string(),
                tools: vec![],
                flags: vec!["verbose".to_string()],
            },
        ];
        let conflicts = detect_extension_conflicts(&extensions);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].message,
            "Flag \"--verbose\" conflicts with /ext1"
        );
    }

    #[test]
    fn no_conflict_for_same_extension_or_distinct_names() {
        let extensions = vec![
            ExtensionConflictInput {
                path: "/ext1".to_string(),
                tools: vec!["a".to_string(), "b".to_string()],
                flags: vec![],
            },
            ExtensionConflictInput {
                path: "/ext2".to_string(),
                tools: vec!["c".to_string()],
                flags: vec![],
            },
        ];
        assert!(detect_extension_conflicts(&extensions).is_empty());
    }

    // -- theme discovery ----------------------------------------------------

    #[test]
    fn discover_theme_files_scans_dir_for_json() {
        let root = scratch_dir("rl-theme-dir");
        let dir = s(&root.join("themes"));
        write(&join(&dir, "dark.json"), "{}");
        write(&join(&dir, "notes.txt"), "ignore me");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let discovery = loader.discover_theme_files(std::slice::from_ref(&dir));
        assert_eq!(discovery.files.len(), 1);
        assert!(discovery.files[0].ends_with("dark.json"));
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn discover_theme_files_warns_on_missing_and_non_json() {
        let root = scratch_dir("rl-theme-warn");
        let missing = join(&s(&root), "nope.json");
        let text_file = join(&s(&root), "plain.txt");
        write(&text_file, "x");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let discovery = loader.discover_theme_files(&[missing, text_file]);
        assert!(discovery.files.is_empty());
        assert_eq!(discovery.diagnostics.len(), 2);
        assert!(discovery
            .diagnostics
            .iter()
            .any(|d| d.message == "theme path does not exist"));
        assert!(discovery
            .diagnostics
            .iter()
            .any(|d| d.message == "theme path is not a json file"));
    }

    #[test]
    fn discover_theme_files_accepts_single_json_file() {
        let root = scratch_dir("rl-theme-file");
        let file = join(&s(&root), "solo.json");
        write(&file, "{}");
        let loader = ResourceLoader::new(&s(&root), &s(&root), true);
        let discovery = loader.discover_theme_files(std::slice::from_ref(&file));
        assert_eq!(discovery.files, vec![resolve_against_cwd(&file)]);
    }
}
