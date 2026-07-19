//! SKILL.md discovery, frontmatter validation, and prompt formatting.
//!
//! Ported from pi's `core/skills.ts`. Discovers Agent Skills (per the
//! <https://agentskills.io> spec) by walking a directory tree for `SKILL.md`
//! files, parses and validates their YAML frontmatter, and renders the surviving
//! skills into the XML block pi injects into the system prompt.
//!
//! Discovery rules, mirroring pi exactly:
//! - if a directory contains `SKILL.md`, treat it as a skill root and do not
//!   recurse further into it;
//! - otherwise load direct `.md` children of the scan root, and
//! - recurse into subdirectories looking for nested `SKILL.md` files.
//!
//! Filesystem access is taken as directory parameters (a seam): callers pass the
//! roots to scan, so nothing here reads a real `HOME`. The `.gitignore`-style
//! filtering pi layers on top is ported faithfully via the [`ignore`] crate,
//! though pi's own test suite never exercises it.
//!
//! The [`SourceInfo`] and diagnostics types live here for now because pi's
//! sibling `source-info`/`diagnostics` modules are not yet ported; they mirror
//! those shapes so a later refactor can lift them out unchanged.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use serde_yaml::Value;

use crate::utils::frontmatter::parse_frontmatter;
use crate::utils::paths::{canonicalize_path, resolve_path, PathInputOptions};

/// Max name length per the Agent Skills spec.
const MAX_NAME_LENGTH: usize = 64;

/// Max description length per the Agent Skills spec.
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Config directory name (`CONFIG_DIR_NAME` in pi's `config.ts`).
const CONFIG_DIR_NAME: &str = ".pi";

/// Environment variable overriding the agent directory (`ENV_AGENT_DIR`).
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

/// Ignore files honored during discovery, in pi's order.
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Where a resource's scope-qualified origin sits (`SourceScope` in pi).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    /// User-global (`~/.pi/agent`) resource.
    User,
    /// Project-local (`.pi`) resource.
    Project,
    /// Ad-hoc / explicit-path resource.
    Temporary,
}

/// Whether a resource came from a package or a top-level entry (`SourceOrigin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOrigin {
    /// Provided by an installed package.
    Package,
    /// A top-level (directly configured) resource.
    TopLevel,
}

/// Provenance of a loaded resource, mirroring pi's `SourceInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// Absolute path the resource was loaded from.
    pub path: String,
    /// Free-form source label (e.g. `"local"`, `"test"`).
    pub source: String,
    /// Scope the resource applies to.
    pub scope: SourceScope,
    /// Origin classification.
    pub origin: SourceOrigin,
    /// Base directory the resource resolves relative paths against.
    pub base_dir: Option<String>,
}

/// Options for [`create_synthetic_source_info`], mirroring pi's optional bag.
#[derive(Debug, Clone, Default)]
pub struct SyntheticSourceOptions {
    /// Source label (required).
    pub source: String,
    /// Scope; defaults to [`SourceScope::Temporary`].
    pub scope: Option<SourceScope>,
    /// Origin; defaults to [`SourceOrigin::TopLevel`].
    pub origin: Option<SourceOrigin>,
    /// Base directory.
    pub base_dir: Option<String>,
}

/// Build a synthetic [`SourceInfo`], filling in pi's defaults for scope/origin.
pub fn create_synthetic_source_info(path: &str, options: SyntheticSourceOptions) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: options.source,
        scope: options.scope.unwrap_or(SourceScope::Temporary),
        origin: options.origin.unwrap_or(SourceOrigin::TopLevel),
        base_dir: options.base_dir,
    }
}

/// Kind of resource a collision refers to (`ResourceCollision.resourceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// An extension.
    Extension,
    /// A skill.
    Skill,
    /// A prompt.
    Prompt,
    /// A theme.
    Theme,
}

/// Details of a name collision between two resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceCollision {
    /// The kind of resource that collided.
    pub resource_type: ResourceType,
    /// The shared name.
    pub name: String,
    /// Path of the resource that won (loaded first).
    pub winner_path: String,
    /// Path of the resource that lost.
    pub loser_path: String,
    /// Optional source of the winner.
    pub winner_source: Option<String>,
    /// Optional source of the loser.
    pub loser_source: Option<String>,
}

/// Severity/kind of a diagnostic (`ResourceDiagnostic.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A non-fatal warning.
    Warning,
    /// An error.
    Error,
    /// A name collision.
    Collision,
}

/// A diagnostic emitted while loading resources, mirroring `ResourceDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDiagnostic {
    /// The diagnostic kind.
    pub kind: DiagnosticKind,
    /// Human-readable message.
    pub message: String,
    /// Path the diagnostic concerns, if any.
    pub path: Option<String>,
    /// Collision details, when [`DiagnosticKind::Collision`].
    pub collision: Option<ResourceCollision>,
}

/// A discovered, validated skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill name (from frontmatter, or the parent directory name).
    pub name: String,
    /// Skill description (required; skills without one are skipped).
    pub description: String,
    /// Absolute path to the `SKILL.md` (or `.md`) file.
    pub file_path: String,
    /// Directory containing the skill file.
    pub base_dir: String,
    /// Provenance information.
    pub source_info: SourceInfo,
    /// When true, the skill is excluded from the model-visible prompt.
    pub disable_model_invocation: bool,
}

/// Result of a skill load: the skills plus any diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadSkillsResult {
    /// Loaded skills.
    pub skills: Vec<Skill>,
    /// Validation and collision diagnostics.
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// Options for [`load_skills_from_dir`].
#[derive(Debug, Clone)]
pub struct LoadSkillsFromDirOptions {
    /// Directory to scan for skills.
    pub dir: String,
    /// Source identifier for these skills.
    pub source: String,
}

/// Validate a skill name per the Agent Skills spec, returning error messages.
fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let len = name.chars().count();

    if len > MAX_NAME_LENGTH {
        errors.push(format!("name exceeds {MAX_NAME_LENGTH} characters ({len})"));
    }

    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }

    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }

    errors
}

/// Validate a skill description per the Agent Skills spec.
fn validate_description(description: Option<&str>) -> Vec<String> {
    match description {
        Some(d) if !d.trim().is_empty() => {
            let len = d.chars().count();
            if len > MAX_DESCRIPTION_LENGTH {
                vec![format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({len})"
                )]
            } else {
                Vec::new()
            }
        }
        _ => vec!["description is required".to_string()],
    }
}

/// Render a path as an owned lossy string.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build a warning diagnostic tied to a path.
fn warning(path: &Path, message: String) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: DiagnosticKind::Warning,
        message,
        path: Some(path_string(path)),
        collision: None,
    }
}

/// Build the [`SourceInfo`] for a discovered skill, keyed by its `source` label.
fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    let (source_label, scope) = match source {
        "user" => ("local", Some(SourceScope::User)),
        "project" => ("local", Some(SourceScope::Project)),
        "path" => ("local", None),
        other => (other, None),
    };
    create_synthetic_source_info(
        file_path,
        SyntheticSourceOptions {
            source: source_label.to_string(),
            scope,
            origin: None,
            base_dir: Some(base_dir.to_string()),
        },
    )
}

/// Parse and validate a single skill file. Returns `(skill, diagnostics)` where
/// `skill` is `None` if the file failed to parse or lacks a description.
fn load_skill_from_file(
    file_path: &Path,
    source: &str,
) -> (Option<Skill>, Vec<ResourceDiagnostic>) {
    let mut diagnostics = Vec::new();

    let raw_content = match fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(warning(file_path, error.to_string()));
            return (None, diagnostics);
        }
    };

    let frontmatter = match parse_frontmatter(&raw_content) {
        Ok((value, _body)) => value,
        Err(error) => {
            diagnostics.push(warning(file_path, error.to_string()));
            return (None, diagnostics);
        }
    };

    let skill_dir = file_path.parent().unwrap_or(file_path);
    let parent_dir_name = skill_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let description = frontmatter
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);

    for error in validate_description(description.as_deref()) {
        diagnostics.push(warning(file_path, error));
    }

    // Use name from frontmatter, or fall back to the parent directory name.
    let name = match frontmatter.get("name").and_then(Value::as_str) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => parent_dir_name,
    };

    for error in validate_name(&name) {
        diagnostics.push(warning(file_path, error));
    }

    // Load the skill even with warnings, unless the description is missing.
    let description = match description {
        Some(d) if !d.trim().is_empty() => d,
        _ => return (None, diagnostics),
    };

    let file_path_str = path_string(file_path);
    let base_dir = path_string(skill_dir);
    let skill = Skill {
        name,
        description,
        source_info: create_skill_source_info(&file_path_str, &base_dir, source),
        disable_model_invocation: matches!(
            frontmatter.get("disable-model-invocation"),
            Some(Value::Bool(true))
        ),
        file_path: file_path_str,
        base_dir,
    };

    (Some(skill), diagnostics)
}

/// Accumulates prefixed ignore rules while a directory tree is walked, matching
/// paths relative to the original scan root (pi's shared `ignore()` matcher).
struct IgnoreState {
    root: PathBuf,
    builder: GitignoreBuilder,
}

impl IgnoreState {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            builder: GitignoreBuilder::new(root),
        }
    }

    /// The `relative(root, dir)` prefix (POSIX, trailing slash) for `dir`.
    fn dir_prefix(&self, dir: &Path) -> String {
        let rel = rel_posix(&self.root, dir);
        if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        }
    }

    /// Read this directory's ignore files and add their prefixed patterns.
    fn add_rules_from_dir(&mut self, dir: &Path) {
        let prefix = self.dir_prefix(dir);
        for filename in IGNORE_FILE_NAMES {
            let Ok(content) = fs::read_to_string(dir.join(filename)) else {
                continue;
            };
            for raw_line in content.split('\n') {
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                    let _ = self.builder.add_line(None, &pattern);
                }
            }
        }
    }

    /// Whether `rel_path` (relative to the root) is ignored.
    fn ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        match self.builder.build() {
            Ok(gitignore) => gitignore.matched(rel_path, is_dir).is_ignore(),
            Err(_) => false,
        }
    }
}

/// Rewrite one ignore-file line so it applies relative to the scan root.
/// Returns `None` for blank lines and comments (mirroring pi's helper).
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line.to_string();
    let mut negated = false;

    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest.to_string();
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest.to_string();
    }

    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest.to_string();
    }

    let prefixed = if prefix.is_empty() {
        pattern
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// POSIX path of `path` relative to `root`, falling back to `path` itself.
fn rel_posix(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
        Err(_) => path_string(path),
    }
}

/// Resolve a directory entry's real kind, following symlinks. Returns
/// `(is_file, is_dir)`, or `None` for a broken symlink that cannot be stat-ed.
fn entry_kind(path: &Path, file_type: &fs::FileType) -> Option<(bool, bool)> {
    if file_type.is_symlink() {
        match fs::metadata(path) {
            Ok(meta) => Some((meta.is_file(), meta.is_dir())),
            Err(_) => None,
        }
    } else {
        Some((file_type.is_file(), file_type.is_dir()))
    }
}

/// Full path plus resolved `(is_file, is_dir)` for an entry, or `None` to skip.
fn classify_entry(entry: &fs::DirEntry) -> Option<(PathBuf, bool, bool)> {
    let full = entry.path();
    let file_type = entry.file_type().ok()?;
    let (is_file, is_dir) = entry_kind(&full, &file_type)?;
    Some((full, is_file, is_dir))
}

/// Append a `load_skill_from_file` result into `out`.
fn push_skill_result(out: &mut LoadSkillsResult, result: (Option<Skill>, Vec<ResourceDiagnostic>)) {
    let (skill, diagnostics) = result;
    if let Some(skill) = skill {
        out.skills.push(skill);
    }
    out.diagnostics.extend(diagnostics);
}

/// Load skills from a directory (see the module docs for discovery rules).
pub fn load_skills_from_dir(options: LoadSkillsFromDirOptions) -> LoadSkillsResult {
    let dir = PathBuf::from(&options.dir);
    let mut state = IgnoreState::new(&dir);
    load_skills_from_dir_internal(&dir, &options.source, true, &mut state, &dir)
}

fn load_skills_from_dir_internal(
    dir: &Path,
    source: &str,
    include_root_files: bool,
    state: &mut IgnoreState,
    root: &Path,
) -> LoadSkillsResult {
    let mut out = LoadSkillsResult::default();

    if !dir.exists() {
        return out;
    }

    state.add_rules_from_dir(dir);

    let Ok(read_dir) = fs::read_dir(dir) else {
        return out;
    };
    let entries: Vec<fs::DirEntry> = read_dir.filter_map(Result::ok).collect();

    // First pass: a directory containing SKILL.md is a skill root; load it and
    // do not recurse further.
    for entry in &entries {
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let Some((full, is_file, _)) = classify_entry(entry) else {
            continue;
        };
        let rel = rel_posix(root, &full);
        if !is_file || state.ignores(&rel, false) {
            continue;
        }
        push_skill_result(&mut out, load_skill_from_file(&full, source));
        return out;
    }

    // Second pass: recurse into subdirectories and load direct `.md` children.
    for entry in &entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Some((full, is_file, is_dir)) = classify_entry(entry) else {
            continue;
        };
        let rel = rel_posix(root, &full);
        let ignore_path = if is_dir { format!("{rel}/") } else { rel };
        if state.ignores(&ignore_path, is_dir) {
            continue;
        }

        if is_dir {
            let sub = load_skills_from_dir_internal(&full, source, false, state, root);
            out.skills.extend(sub.skills);
            out.diagnostics.extend(sub.diagnostics);
            continue;
        }

        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }
        push_skill_result(&mut out, load_skill_from_file(&full, source));
    }

    out
}

/// Escape XML text content (order matches pi's `escapeXml`).
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Format skills for inclusion in a system prompt, using the Agent Skills XML
/// format. Skills with `disable_model_invocation` are excluded; an empty result
/// yields an empty string.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();

    if visible.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Options for [`load_skills`].
#[derive(Debug, Clone)]
pub struct LoadSkillsOptions {
    /// Working directory for project-local skills.
    pub cwd: String,
    /// Agent config directory for global skills. Empty falls back to the
    /// environment-derived default (see [`get_agent_dir`]).
    pub agent_dir: String,
    /// Explicit skill paths (files or directories).
    pub skill_paths: Vec<String>,
    /// Whether to include the default user/project skills directories.
    pub include_defaults: bool,
}

/// The agent directory: `$PI_CODING_AGENT_DIR` if set, else `~/.pi/agent`.
///
/// Environment-shaped: only used when [`LoadSkillsOptions::agent_dir`] is empty.
pub fn get_agent_dir() -> String {
    if let Ok(dir) = std::env::var(ENV_AGENT_DIR) {
        if !dir.is_empty() {
            return dir;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/{CONFIG_DIR_NAME}/agent")
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether `target` is `root` itself or lies beneath it.
fn is_under_path(target: &str, root: &str) -> bool {
    let base = current_dir_string();
    let normalized_root = resolve_path(root, &base, &PathInputOptions::default())
        .unwrap_or_else(|_| root.to_string());
    if target == normalized_root {
        return true;
    }
    let prefix = if normalized_root.ends_with('/') {
        normalized_root
    } else {
        format!("{normalized_root}/")
    };
    target.starts_with(&prefix)
}

/// Collects skills across sources with symlink de-duplication and first-wins
/// collision handling (pi's inner `addSkills`).
struct SkillCollector {
    skills: Vec<Skill>,
    real_paths: HashSet<String>,
    diagnostics: Vec<ResourceDiagnostic>,
    collisions: Vec<ResourceDiagnostic>,
}

impl SkillCollector {
    fn new() -> Self {
        Self {
            skills: Vec::new(),
            real_paths: HashSet::new(),
            diagnostics: Vec::new(),
            collisions: Vec::new(),
        }
    }

    fn add(&mut self, result: LoadSkillsResult) {
        self.diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            // Resolve symlinks to detect the same file loaded twice.
            let real_path = canonicalize_path(&skill.file_path);
            if self.real_paths.contains(&real_path) {
                continue;
            }

            if let Some(existing) = self.skills.iter().find(|s| s.name == skill.name) {
                self.collisions.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Collision,
                    message: format!("name \"{}\" collision", skill.name),
                    path: Some(skill.file_path.clone()),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceType::Skill,
                        name: skill.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: skill.file_path.clone(),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            } else {
                self.real_paths.insert(real_path);
                self.skills.push(skill);
            }
        }
    }

    fn finish(mut self) -> LoadSkillsResult {
        self.diagnostics.extend(self.collisions);
        LoadSkillsResult {
            skills: self.skills,
            diagnostics: self.diagnostics,
        }
    }
}

/// Load skills from all configured locations: the default user/project skills
/// directories (when `include_defaults`) plus any explicit `skill_paths`.
pub fn load_skills(options: LoadSkillsOptions) -> LoadSkillsResult {
    let default_opts = PathInputOptions::default();
    let base = current_dir_string();

    let resolved_cwd =
        resolve_path(&options.cwd, &base, &default_opts).unwrap_or_else(|_| options.cwd.clone());
    let agent_dir_raw = if options.agent_dir.is_empty() {
        get_agent_dir()
    } else {
        options.agent_dir.clone()
    };
    let resolved_agent_dir =
        resolve_path(&agent_dir_raw, &base, &default_opts).unwrap_or(agent_dir_raw);

    let user_skills_dir = join_path(&resolved_agent_dir, "skills");
    let project_skills_dir = join_path(&join_path(&resolved_cwd, CONFIG_DIR_NAME), "skills");

    let mut collector = SkillCollector::new();

    if options.include_defaults {
        collector.add(load_dir(&user_skills_dir, "user"));
        collector.add(load_dir(&project_skills_dir, "project"));
    }

    let get_source = |resolved_path: &str| -> &'static str {
        if !options.include_defaults {
            if is_under_path(resolved_path, &user_skills_dir) {
                return "user";
            }
            if is_under_path(resolved_path, &project_skills_dir) {
                return "project";
            }
        }
        "path"
    };

    let path_opts = PathInputOptions {
        trim: true,
        ..PathInputOptions::default()
    };

    for raw_path in &options.skill_paths {
        let resolved_path = match resolve_path(raw_path, &resolved_cwd, &path_opts) {
            Ok(p) => p,
            Err(_) => raw_path.clone(),
        };
        let resolved = Path::new(&resolved_path);
        if !resolved.exists() {
            collector.diagnostics.push(ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: "skill path does not exist".to_string(),
                path: Some(resolved_path.clone()),
                collision: None,
            });
            continue;
        }

        let source = get_source(&resolved_path);
        match fs::metadata(resolved) {
            Ok(meta) if meta.is_dir() => collector.add(load_dir(&resolved_path, source)),
            Ok(meta) if meta.is_file() && resolved_path.ends_with(".md") => {
                let (skill, diagnostics) = load_skill_from_file(resolved, source);
                match skill {
                    Some(skill) => collector.add(LoadSkillsResult {
                        skills: vec![skill],
                        diagnostics,
                    }),
                    None => collector.diagnostics.extend(diagnostics),
                }
            }
            Ok(_) => collector.diagnostics.push(ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: "skill path is not a markdown file".to_string(),
                path: Some(resolved_path.clone()),
                collision: None,
            }),
            Err(error) => collector.diagnostics.push(ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: error.to_string(),
                path: Some(resolved_path.clone()),
                collision: None,
            }),
        }
    }

    collector.finish()
}

/// Join `base` and `segment` with a POSIX separator (paths are already
/// resolved/normalized at this point).
fn join_path(base: &str, segment: &str) -> String {
    format!("{}/{segment}", base.trim_end_matches('/'))
}

/// Scan a directory string for skills with the given source label.
fn load_dir(dir: &str, source: &str) -> LoadSkillsResult {
    load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: dir.to_string(),
        source: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute path to pi's `test/fixtures/skills` tree (the pinned spec).
    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/pi/packages/coding-agent/test/fixtures/skills")
    }

    fn fixtures_dir_string() -> String {
        fixtures_dir().to_string_lossy().into_owned()
    }

    /// Load a single fixture subdirectory with the `"test"` source.
    fn load_fixture(name: &str) -> LoadSkillsResult {
        load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: fixtures_dir().join(name).to_string_lossy().into_owned(),
            source: "test".to_string(),
        })
    }

    fn has_message(diagnostics: &[ResourceDiagnostic], needle: &str) -> bool {
        diagnostics.iter().any(|d| d.message.contains(needle))
    }

    /// Build a synthetic skill for the formatting tests.
    fn test_skill(name: &str, description: &str, file_path: &str, disable: bool) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            file_path: file_path.to_string(),
            base_dir: file_path.trim_end_matches("/SKILL.md").to_string(),
            source_info: create_synthetic_source_info(
                file_path,
                SyntheticSourceOptions {
                    source: "test".to_string(),
                    ..SyntheticSourceOptions::default()
                },
            ),
            disable_model_invocation: disable,
        }
    }

    #[test]
    fn loads_valid_skill() {
        let result = load_fixture("valid-skill");
        assert_eq!(result.skills.len(), 1);
        let skill = &result.skills[0];
        assert_eq!(skill.name, "valid-skill");
        assert_eq!(skill.description, "A valid skill for testing purposes.");
        assert_eq!(skill.source_info.source, "test");
        // disableModelInvocation defaults to false when not specified.
        assert!(!skill.disable_model_invocation);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn allows_name_not_matching_parent_directory() {
        let result = load_fixture("name-mismatch");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "different-name");
        assert!(!has_message(
            &result.diagnostics,
            "does not match parent directory"
        ));
    }

    #[test]
    fn name_and_description_diagnostics() {
        // (fixture, expected skill count, a diagnostic substring that must appear)
        let cases = [
            ("invalid-name-chars", 1, "invalid characters"),
            ("long-name", 1, "exceeds 64 characters"),
            ("consecutive-hyphens", 1, "consecutive hyphens"),
            ("missing-description", 0, "description is required"),
            ("no-frontmatter", 0, "description is required"),
            ("invalid-yaml", 0, "at line"),
        ];
        for (fixture, count, needle) in cases {
            let result = load_fixture(fixture);
            assert_eq!(result.skills.len(), count, "{fixture} skill count");
            assert!(
                has_message(&result.diagnostics, needle),
                "{fixture} expected diagnostic containing {needle:?}"
            );
        }
    }

    #[test]
    fn ignores_unknown_frontmatter_fields() {
        let result = load_fixture("unknown-field");
        assert_eq!(result.skills.len(), 1);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn loads_nested_skills_recursively() {
        let result = load_fixture("nested");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "child-skill");
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn prefers_root_skill_over_nested() {
        // The root SKILL.md has no `name`, so it falls back to the directory
        // name, and the nested child is not visited.
        let result = load_fixture("root-skill-preferred");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "root-skill-preferred");
        assert_eq!(result.skills[0].description, "Root skill should win.");
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn preserves_multiline_description() {
        let result = load_fixture("multiline-description");
        assert_eq!(result.skills.len(), 1);
        let description = &result.skills[0].description;
        assert!(description.contains('\n'));
        assert!(description.contains("This is a multiline description."));
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn parses_disable_model_invocation() {
        let result = load_fixture("disable-model-invocation");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "disable-model-invocation");
        assert!(result.skills[0].disable_model_invocation);
        assert!(!has_message(
            &result.diagnostics,
            "unknown frontmatter field"
        ));
    }

    #[test]
    fn loads_all_skills_from_fixtures_directory() {
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: fixtures_dir_string(),
            source: "test".to_string(),
        });
        // Every fixture with a description loads (even those with warnings);
        // missing-description and no-frontmatter are skipped.
        assert!(result.skills.len() >= 6, "got {}", result.skills.len());
    }

    #[test]
    fn returns_empty_for_nonexistent_directory() {
        let result = load_skills_from_dir(LoadSkillsFromDirOptions {
            dir: "/non/existent/path".to_string(),
            source: "test".to_string(),
        });
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn format_empty_and_all_disabled_return_empty_string() {
        assert_eq!(format_skills_for_prompt(&[]), "");
        let hidden = vec![test_skill(
            "hidden",
            "A hidden skill.",
            "/p/hidden/SKILL.md",
            true,
        )];
        assert_eq!(format_skills_for_prompt(&hidden), "");
    }

    #[test]
    fn formats_single_skill_with_intro_and_xml() {
        let skills = vec![test_skill(
            "test-skill",
            "A test skill.",
            "/path/to/skill/SKILL.md",
            false,
        )];
        let result = format_skills_for_prompt(&skills);

        let xml_start = result.find("<available_skills>").expect("xml block");
        let intro = &result[..xml_start];
        assert!(intro.contains("The following skills provide specialized instructions"));
        assert!(intro.contains("Use the read tool to load a skill's file"));

        assert!(result.contains("</available_skills>"));
        assert!(result.contains("<skill>"));
        assert!(result.contains("<name>test-skill</name>"));
        assert!(result.contains("<description>A test skill.</description>"));
        assert!(result.contains("<location>/path/to/skill/SKILL.md</location>"));
    }

    #[test]
    fn escapes_xml_special_characters() {
        let skills = vec![test_skill(
            "test-skill",
            "A skill with <special> & \"characters\".",
            "/path/to/skill/SKILL.md",
            false,
        )];
        let result = format_skills_for_prompt(&skills);
        assert!(result.contains("&lt;special&gt;"));
        assert!(result.contains("&amp;"));
        assert!(result.contains("&quot;characters&quot;"));
    }

    #[test]
    fn formats_multiple_skills() {
        let skills = vec![
            test_skill("skill-one", "First skill.", "/path/one/SKILL.md", false),
            test_skill("skill-two", "Second skill.", "/path/two/SKILL.md", false),
        ];
        let result = format_skills_for_prompt(&skills);
        assert!(result.contains("<name>skill-one</name>"));
        assert!(result.contains("<name>skill-two</name>"));
        assert_eq!(result.matches("<skill>").count(), 2);
    }

    #[test]
    fn excludes_disabled_skills_from_prompt() {
        let skills = vec![
            test_skill(
                "visible-skill",
                "A visible skill.",
                "/p/visible/SKILL.md",
                false,
            ),
            test_skill(
                "hidden-skill",
                "A hidden skill.",
                "/p/hidden/SKILL.md",
                true,
            ),
        ];
        let result = format_skills_for_prompt(&skills);
        assert!(result.contains("<name>visible-skill</name>"));
        assert!(!result.contains("<name>hidden-skill</name>"));
        assert_eq!(result.matches("<skill>").count(), 1);
    }

    /// The empty agent/cwd fixtures do not exist, so defaults contribute nothing.
    fn empty_dir(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/pi/packages/coding-agent/test/fixtures")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn load_skills_with_paths(skill_paths: Vec<String>) -> LoadSkillsResult {
        load_skills(LoadSkillsOptions {
            agent_dir: empty_dir("empty-agent"),
            cwd: empty_dir("empty-cwd"),
            skill_paths,
            include_defaults: true,
        })
    }

    #[test]
    fn load_skills_from_explicit_path() {
        let path = fixtures_dir()
            .join("valid-skill")
            .to_string_lossy()
            .into_owned();
        let result = load_skills_with_paths(vec![path]);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].source_info.scope, SourceScope::Temporary);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn load_skills_warns_on_missing_path() {
        let result = load_skills_with_paths(vec!["/non/existent/path".to_string()]);
        assert!(result.skills.is_empty());
        assert!(has_message(&result.diagnostics, "does not exist"));
    }

    // Note: pi's "should expand ~ in skillPaths" test is environment-shaped (it
    // reads the real `homedir()` and compares against `~/.pi/agent/skills`), so
    // it is intentionally not ported here. Tilde expansion itself is covered by
    // `crate::utils::paths` tests.

    #[test]
    fn detects_name_collisions_keeping_first() {
        let collision_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/pi/packages/coding-agent/test/fixtures/skills-collision");
        let load = |sub: &str, source: &str| {
            load_skills_from_dir(LoadSkillsFromDirOptions {
                dir: collision_dir.join(sub).to_string_lossy().into_owned(),
                source: source.to_string(),
            })
        };

        let first = load("first", "first");
        let second = load("second", "second");

        // Mirror loadSkills' first-wins collision resolution.
        let mut collector = SkillCollector::new();
        collector.add(first);
        collector.add(second);
        let result = collector.finish();

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "calendar");
        assert_eq!(result.skills[0].source_info.source, "first");
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| d.kind == DiagnosticKind::Collision)
                .count(),
            1
        );
    }
}
