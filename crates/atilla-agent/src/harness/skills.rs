// straitjacket-allow-file:duplication — `parse_frontmatter`, `resolve_kind`, and
// the diagnostic-shaped helpers are transcribed verbatim from pi, where
// skills.ts and prompt-templates.ts carry byte-identical copies of the same
// logic; the clone detector reads the two ports as duplicates by design.
//! Skill loading and invocation formatting, mirroring
//! `packages/agent/src/harness/skills.ts`.
//!
//! # Faithful divergences from pi
//!
//! - **Synchronous.** pi's loaders are `async` over `Promise<Result<...>>`;
//!   this port drops `async` and reads through the synchronous
//!   [`ExecutionEnv`]/[`FileSystem`] contract in [`crate::harness::env`].
//! - **Ignore matcher.** pi uses the npm `ignore` package's incremental
//!   `add`/`ignores` API. [`IgnoreMatcher`] wraps the `ignore` crate's
//!   `GitignoreBuilder` to reproduce that accumulate-then-query behavior;
//!   pattern-match fidelity is bounded by that crate. pi appends a trailing
//!   `/` to a directory path before querying; this port threads an explicit
//!   `is_dir` flag instead.
//! - **`localeCompare` → `str::cmp`**, and **UTF-16 lengths → char counts**, as
//!   in [`crate::harness::prompt_templates`]. The addressed skill files use
//!   ASCII names, so the orderings and length checks agree.
//! - **Generic `mapSkill`.** pi's `loadSourcedSkills` accepts an optional
//!   structural-subtype mapper; Rust has no structural-subtype bound, so the
//!   identity behavior is ported and callers compose their own mapping.

use serde::{Deserialize, Serialize};

use crate::harness::env::{ExecutionEnv, FileErrorCode, FileInfo, FileKind};
use crate::harness::prompt_templates::{
    basename_env_path, frontmatter_string, parse_frontmatter, DiagnosticSeverity,
};

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Skill loaded from a `SKILL.md` file or provided by an application. Mirrors
/// pi's `Skill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// Stable skill name used for lookup and model-visible listings.
    pub name: String,
    /// Short model-visible description of when to use the skill.
    pub description: String,
    /// Full skill instructions.
    pub content: String,
    /// Absolute path to the skill file. Used for model-visible location and
    /// resolving relative references.
    pub file_path: String,
    /// Exclude this skill from model-visible skill lists while still allowing
    /// explicit application invocation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disable_model_invocation: bool,
}

/// Stable diagnostic code for skill loading. Mirrors pi's `SkillDiagnosticCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
    InvalidMetadata,
}

/// Warning produced while loading skills. Mirrors pi's `SkillDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDiagnostic {
    /// Diagnostic severity. Currently only warnings are emitted.
    #[serde(rename = "type")]
    pub severity: DiagnosticSeverity,
    /// Stable diagnostic code.
    pub code: SkillDiagnosticCode,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Path associated with the diagnostic.
    pub path: String,
}

/// Result of [`load_skills`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedSkills {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// A skill tagged with its provenance source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkill<S> {
    pub skill: Skill,
    pub source: S,
}

/// A diagnostic tagged with its provenance source (pi spreads `{...diagnostic,
/// source}`; the fields are reachable through [`Self::diagnostic`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkillDiagnostic<S> {
    pub diagnostic: SkillDiagnostic,
    pub source: S,
}

/// Result of [`load_sourced_skills`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSourcedSkills<S> {
    pub skills: Vec<SourcedSkill<S>>,
    pub diagnostics: Vec<SourcedSkillDiagnostic<S>>,
}

/// Format a skill invocation prompt, optionally appending additional user
/// instructions. Mirrors pi's `formatSkillInvocation`.
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        dirname_env_path(&skill.file_path),
        skill.content,
    );
    match additional_instructions {
        Some(extra) => format!("{skill_block}\n\n{extra}"),
        None => skill_block,
    }
}

/// Load skills from one or more directories. Mirrors pi's `loadSkills`.
///
/// Traverses directories recursively, loads `SKILL.md` files, loads direct root
/// `.md` files as skills, honors ignore files, and returns diagnostics for
/// invalid skill files. Missing input directories are skipped.
pub fn load_skills(env: &impl ExecutionEnv, dirs: &[&str]) -> LoadedSkills {
    let mut out = LoadedSkills::default();
    for dir in dirs {
        let root_info = match env.file_info(dir, None) {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    out.diagnostics.push(warning(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.message,
                        (*dir).to_string(),
                    ));
                }
                continue;
            }
        };
        if resolve_kind(env, &root_info, &mut out.diagnostics) != Some(FileKind::Directory) {
            continue;
        }
        let mut ignore_matcher = IgnoreMatcher::new(&root_info.path);
        let result = load_skills_from_dir_internal(
            env,
            &root_info.path,
            true,
            &mut ignore_matcher,
            &root_info.path,
        );
        out.skills.extend(result.skills);
        out.diagnostics.extend(result.diagnostics);
    }
    out
}

/// Load skills from source-tagged directories. Mirrors pi's `loadSourcedSkills`
/// (identity mapping; see the module note on the dropped `mapSkill` generic).
///
/// Source values are preserved exactly and attached to every loaded skill and
/// diagnostic.
pub fn load_sourced_skills<S: Clone>(
    env: &impl ExecutionEnv,
    inputs: &[(&str, S)],
) -> LoadedSourcedSkills<S> {
    let mut out = LoadedSourcedSkills {
        skills: Vec::new(),
        diagnostics: Vec::new(),
    };
    for (path, source) in inputs {
        let result = load_skills(env, &[path]);
        for skill in result.skills {
            out.skills.push(SourcedSkill {
                skill,
                source: source.clone(),
            });
        }
        for diagnostic in result.diagnostics {
            out.diagnostics.push(SourcedSkillDiagnostic {
                diagnostic,
                source: source.clone(),
            });
        }
    }
    out
}

fn load_skills_from_dir_internal(
    env: &impl ExecutionEnv,
    dir: &str,
    include_root_files: bool,
    ignore_matcher: &mut IgnoreMatcher,
    root_dir: &str,
) -> LoadedSkills {
    let mut out = LoadedSkills::default();

    let dir_info = match env.file_info(dir, None) {
        Ok(info) => info,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                out.diagnostics.push(warning(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message,
                    dir.to_string(),
                ));
            }
            return out;
        }
    };
    if resolve_kind(env, &dir_info, &mut out.diagnostics) != Some(FileKind::Directory) {
        return out;
    }

    add_ignore_rules(env, ignore_matcher, dir, root_dir, &mut out.diagnostics);

    let entries = match env.list_dir(dir, None) {
        Ok(entries) => entries,
        Err(error) => {
            out.diagnostics.push(warning(
                SkillDiagnosticCode::ListFailed,
                error.message,
                dir.to_string(),
            ));
            return out;
        }
    };

    // pi: a `SKILL.md` at this level makes the directory a single skill and short
    // circuits the rest of the traversal.
    for entry in &entries {
        if entry.name != "SKILL.md" {
            continue;
        }
        let full_path = &entry.path;
        if resolve_kind(env, entry, &mut out.diagnostics) != Some(FileKind::File) {
            continue;
        }
        let rel_path = relative_env_path(root_dir, full_path);
        if ignore_matcher.ignores(&rel_path, false) {
            continue;
        }
        let result = load_skill_from_file(env, full_path);
        if let Some(skill) = result.skill {
            out.skills.push(skill);
        }
        out.diagnostics.extend(result.diagnostics);
        return out;
    }

    let mut sorted = entries;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in sorted {
        if entry.name.starts_with('.') || entry.name == "node_modules" {
            continue;
        }
        let full_path = &entry.path;
        let Some(kind) = resolve_kind(env, &entry, &mut out.diagnostics) else {
            continue;
        };

        let rel_path = relative_env_path(root_dir, full_path);
        // pi: `ignorePath = kind === "directory" ? `${relPath}/` : relPath`.
        let is_dir = kind == FileKind::Directory;
        if ignore_matcher.ignores(&rel_path, is_dir) {
            continue;
        }

        if kind == FileKind::Directory {
            let result =
                load_skills_from_dir_internal(env, full_path, false, ignore_matcher, root_dir);
            out.skills.extend(result.skills);
            out.diagnostics.extend(result.diagnostics);
            continue;
        }

        if kind != FileKind::File || !include_root_files || !entry.name.ends_with(".md") {
            continue;
        }
        let result = load_skill_from_file(env, full_path);
        if let Some(skill) = result.skill {
            out.skills.push(skill);
        }
        out.diagnostics.extend(result.diagnostics);
    }

    out
}

fn add_ignore_rules(
    env: &impl ExecutionEnv,
    ig: &mut IgnoreMatcher,
    dir: &str,
    root_dir: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let relative_dir = relative_env_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{relative_dir}/")
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = join_env_path(dir, filename);
        let info = match env.file_info(&ignore_path, None) {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(warning(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.message,
                        ignore_path,
                    ));
                }
                continue;
            }
        };
        if info.kind != FileKind::File {
            continue;
        }
        let content = match env.read_text_file(&ignore_path, None) {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(warning(
                    SkillDiagnosticCode::ReadFailed,
                    error.message,
                    ignore_path,
                ));
                continue;
            }
        };
        // pi: split on `/\r?\n/`, prefix each pattern, drop empties.
        let patterns: Vec<String> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter_map(|line| prefix_ignore_pattern(line, &prefix))
            .collect();
        if !patterns.is_empty() {
            ig.add(patterns);
        }
    }
}

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
        // pi drops the escaping backslash, leaving a literal `!` pattern.
        pattern = format!("!{rest}");
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

struct LoadedSkill {
    skill: Option<Skill>,
    diagnostics: Vec<SkillDiagnostic>,
}

fn load_skill_from_file(env: &impl ExecutionEnv, file_path: &str) -> LoadedSkill {
    let mut diagnostics = Vec::new();
    let raw_content = match env.read_text_file(file_path, None) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(warning(
                SkillDiagnosticCode::ReadFailed,
                error.message,
                file_path.to_string(),
            ));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };

    let (frontmatter, body) = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(warning(
                SkillDiagnosticCode::ParseFailed,
                message,
                file_path.to_string(),
            ));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };

    let skill_dir = dirname_env_path(file_path);
    let parent_dir_name = basename_env_path(&skill_dir);
    let description = frontmatter_string(&frontmatter, "description");

    for error in validate_description(description.as_deref()) {
        diagnostics.push(warning(
            SkillDiagnosticCode::InvalidMetadata,
            error,
            file_path.to_string(),
        ));
    }

    // pi: `frontmatterName || parentDirName` — an empty string falls back.
    let name = frontmatter_string(&frontmatter, "name")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| parent_dir_name.clone());
    for error in validate_name(&name, &parent_dir_name) {
        diagnostics.push(warning(
            SkillDiagnosticCode::InvalidMetadata,
            error,
            file_path.to_string(),
        ));
    }

    // pi: `if (!description || description.trim() === "") return skill null`.
    let description = match description {
        Some(value) if !value.trim().is_empty() => value,
        _ => {
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };

    LoadedSkill {
        skill: Some(Skill {
            name,
            description,
            content: body,
            file_path: file_path.to_string(),
            disable_model_invocation: frontmatter_bool(&frontmatter, "disable-model-invocation"),
        }),
        diagnostics,
    }
}

fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    let name_len = name.chars().count();
    if name_len > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({name_len})"
        ));
    }
    // pi: `/^[a-z0-9-]+$/` — non-empty and lowercase alphanumerics or hyphens.
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

fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_string()),
        Some(value) if value.trim().is_empty() => {
            errors.push("description is required".to_string());
        }
        Some(value) => {
            let len = value.chars().count();
            if len > MAX_DESCRIPTION_LENGTH {
                errors.push(format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({len})"
                ));
            }
        }
    }
    errors
}

fn resolve_kind(
    env: &impl ExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind);
    }
    let canonical_path = match env.canonical_path(&info.path, None) {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(warning(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message,
                    info.path.clone(),
                ));
            }
            return None;
        }
    };
    let target = match env.file_info(&canonical_path, None) {
        Ok(target) => target,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(warning(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message,
                    info.path.clone(),
                ));
            }
            return None;
        }
    };
    if target.kind == FileKind::File || target.kind == FileKind::Directory {
        Some(target.kind)
    } else {
        None
    }
}

fn frontmatter_bool(frontmatter: &serde_yaml::Value, key: &str) -> bool {
    // pi: `frontmatter["disable-model-invocation"] === true`.
    frontmatter.get(key).and_then(serde_yaml::Value::as_bool) == Some(true)
}

fn warning(code: SkillDiagnosticCode, message: String, path: String) -> SkillDiagnostic {
    SkillDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code,
        message,
        path,
    }
}

fn join_env_path(base: &str, child: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        child.trim_start_matches('/')
    )
}

fn dirname_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    match normalized.rfind('/') {
        // pi: `slashIndex <= 0 ? "/" : normalized.slice(0, slashIndex)`.
        Some(index) if index > 0 => normalized[..index].to_string(),
        _ => "/".to_string(),
    }
}

fn relative_env_path(root: &str, path: &str) -> String {
    let normalized_root = root.trim_end_matches('/');
    let normalized_path = path.trim_end_matches('/');
    if normalized_path == normalized_root {
        return String::new();
    }
    let prefix = format!("{normalized_root}/");
    match normalized_path.strip_prefix(&prefix) {
        Some(rest) => rest.to_string(),
        None => normalized_path.trim_start_matches('/').to_string(),
    }
}

/// Accumulating gitignore matcher mirroring the npm `ignore` package's
/// `add`/`ignores` surface, backed by the `ignore` crate. Patterns accumulate as
/// the traversal descends; querying rebuilds the compiled matcher (a no-op fast
/// path returns `false` while no patterns have been added, which is the only
/// state the ported test fixtures reach).
struct IgnoreMatcher {
    root: String,
    lines: Vec<String>,
}

impl IgnoreMatcher {
    fn new(root: &str) -> Self {
        Self {
            root: root.to_string(),
            lines: Vec::new(),
        }
    }

    fn add(&mut self, patterns: Vec<String>) {
        self.lines.extend(patterns);
    }

    fn ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        if self.lines.is_empty() {
            return false;
        }
        let mut builder = ignore::gitignore::GitignoreBuilder::new(&self.root);
        for line in &self.lines {
            let _ = builder.add_line(None, line);
        }
        match builder.build() {
            Ok(gitignore) => gitignore.matched(rel_path, is_dir).is_ignore(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    // Port of `test/harness/skills.test.ts` plus the `formatSkillInvocation`
    // case from `test/harness/resource-formatting.test.ts`. pi drives a real
    // `NodeExecutionEnv` over a temp dir; here the same trees are seeded into a
    // `MemoryExecutionEnv` with absolute paths, so loaded `filePath`s match the
    // seeded absolute paths (as pi's `join(root, ...)` expectations do).
    use super::*;
    use crate::harness::env::MemoryExecutionEnv;

    #[test]
    fn loads_skill_md_files_through_the_execution_environment() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/.agents/skills")
            .with_dir("/root/.agents/skills/example")
            .with_file(
                "/root/.agents/skills/example/SKILL.md",
                "---\nname: example\ndescription: Example skill\ndisable-model-invocation: true\n---\nUse this skill.\n",
            );

        let result = load_skills(&env, &["/root/.agents/skills"]);

        assert_eq!(result.diagnostics, vec![]);
        assert_eq!(
            result.skills,
            vec![Skill {
                name: "example".to_string(),
                description: "Example skill".to_string(),
                content: "Use this skill.".to_string(),
                file_path: "/root/.agents/skills/example/SKILL.md".to_string(),
                disable_model_invocation: true,
            }]
        );
    }

    #[test]
    fn loads_skills_through_symlinked_directories() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/actual")
            .with_dir("/root/actual/example")
            .with_file(
                "/root/actual/example/SKILL.md",
                "---\nname: example\ndescription: Example skill\n---\nUse this skill.",
            )
            .with_symlink("/root/skills-link", "/root/actual");

        let result = load_skills(&env, &["/root/skills-link"]);

        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["example"]);
        assert_eq!(
            result.skills[0].file_path,
            "/root/skills-link/example/SKILL.md"
        );
    }

    #[test]
    fn preserves_source_info_for_sourced_skills() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/user")
            .with_dir("/root/user/example")
            .with_file(
                "/root/user/example/SKILL.md",
                "---\nname: example\ndescription: Example skill\n---\nUse this skill.",
            );

        let result = load_sourced_skills(&env, &[("/root/user", "user")]);

        assert_eq!(result.diagnostics, vec![]);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(
            result.skills[0].skill,
            Skill {
                name: "example".to_string(),
                description: "Example skill".to_string(),
                content: "Use this skill.".to_string(),
                file_path: "/root/user/example/SKILL.md".to_string(),
                disable_model_invocation: false,
            }
        );
        assert_eq!(result.skills[0].source, "user");
    }

    #[test]
    fn attaches_source_info_to_diagnostics() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/user")
            .with_dir("/root/user/broken")
            .with_file(
                "/root/user/broken/SKILL.md",
                "---\nname: broken\n---\nMissing description.",
            );

        let result = load_sourced_skills(&env, &[("/root/user", "user")]);

        assert_eq!(result.skills.len(), 0);
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.diagnostic.severity, DiagnosticSeverity::Warning);
        assert_eq!(
            diagnostic.diagnostic.code,
            SkillDiagnosticCode::InvalidMetadata
        );
        assert_eq!(diagnostic.diagnostic.message, "description is required");
        assert_eq!(diagnostic.diagnostic.path, "/root/user/broken/SKILL.md");
        assert_eq!(diagnostic.source, "user");
    }

    #[test]
    fn loads_direct_markdown_children_only_from_the_root_directory() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/skills")
            .with_dir("/root/skills/nested")
            .with_file(
                "/root/skills/root.md",
                "---\ndescription: Root skill\n---\nRoot content",
            )
            .with_file(
                "/root/skills/nested/ignored.md",
                "---\ndescription: Ignored\n---\nIgnored content",
            );

        let result = load_skills(&env, &["/root/skills"]);

        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["skills"]);
        assert_eq!(result.skills[0].content, "Root content");
    }

    #[test]
    fn formats_skill_invocations_with_additional_instructions() {
        // From resource-formatting.test.ts.
        let skill = Skill {
            name: "inspect".to_string(),
            description: "Inspect things".to_string(),
            content: "Use inspection tools.".to_string(),
            file_path: "/project/.pi/skills/inspect/SKILL.md".to_string(),
            disable_model_invocation: false,
        };

        assert_eq!(
            format_skill_invocation(&skill, Some("Check errors.")),
            "<skill name=\"inspect\" location=\"/project/.pi/skills/inspect/SKILL.md\">\nReferences are relative to /project/.pi/skills/inspect.\n\nUse inspection tools.\n</skill>\n\nCheck errors."
        );
    }
}
