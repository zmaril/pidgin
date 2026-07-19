// straitjacket-allow-file:duplication — `parse_frontmatter`, `resolve_kind`, and
// the diagnostic-shaped helpers are transcribed verbatim from pi, where
// prompt-templates.ts and skills.ts carry byte-identical copies of the same
// logic; the clone detector reads the two ports as duplicates by design.
//! Prompt-template loading and invocation formatting, mirroring
//! `packages/agent/src/harness/prompt-templates.ts`.
//!
//! # Faithful divergences from pi
//!
//! - **Synchronous.** pi's loaders are `async` over `Promise<Result<...>>`;
//!   this port drops `async` and reads through the synchronous
//!   [`ExecutionEnv`]/[`FileSystem`] contract in [`crate::harness::env`].
//! - **`localeCompare` → `str::cmp`.** Directory entries are sorted with Rust's
//!   lexicographic ordering rather than ICU locale collation. For the ASCII
//!   markdown filenames these loaders address the two orderings agree.
//! - **UTF-16 slices → byte slices.** pi slices frontmatter/body by UTF-16 code
//!   units; this port slices by UTF-8 bytes. The delimiters (`---`, newlines)
//!   are ASCII, so the boundaries coincide for real frontmatter.
//! - **Generic `mapPromptTemplate`.** pi's `loadSourcedPromptTemplates` accepts
//!   an optional structural-subtype mapper (`TPromptTemplate extends
//!   PromptTemplate`). Rust has no structural-subtype bound; the identity
//!   behavior is ported and callers compose their own mapping.

use serde::{Deserialize, Serialize};

use crate::harness::env::{ExecutionEnv, FileErrorCode, FileInfo, FileKind};

/// Prompt template that can be formatted into a prompt for explicit invocation.
/// Mirrors pi's `PromptTemplate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplate {
    /// Stable template name used for lookup or application command routing.
    pub name: String,
    /// Optional description for command lists or autocomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Template content. Argument placeholders are formatted by
    /// [`format_prompt_template_invocation`].
    pub content: String,
}

/// Severity of a harness diagnostic. pi currently only emits warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
}

/// Stable diagnostic code for prompt-template loading. Mirrors pi's
/// `PromptTemplateDiagnosticCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
}

/// Warning produced while loading prompt templates. Mirrors pi's
/// `PromptTemplateDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplateDiagnostic {
    /// Diagnostic severity. Currently only warnings are emitted.
    #[serde(rename = "type")]
    pub severity: DiagnosticSeverity,
    /// Stable diagnostic code.
    pub code: PromptTemplateDiagnosticCode,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Path associated with the diagnostic.
    pub path: String,
}

/// Result of [`load_prompt_templates`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPromptTemplates {
    pub prompt_templates: Vec<PromptTemplate>,
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

/// A prompt template tagged with its provenance source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplate<S> {
    pub prompt_template: PromptTemplate,
    pub source: S,
}

/// A diagnostic tagged with its provenance source (pi spreads `{...diagnostic,
/// source}`; the fields are reachable through [`Self::diagnostic`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplateDiagnostic<S> {
    pub diagnostic: PromptTemplateDiagnostic,
    pub source: S,
}

/// Result of [`load_sourced_prompt_templates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSourcedPromptTemplates<S> {
    pub prompt_templates: Vec<SourcedPromptTemplate<S>>,
    pub diagnostics: Vec<SourcedPromptTemplateDiagnostic<S>>,
}

/// Load prompt templates from one or more paths. Mirrors pi's
/// `loadPromptTemplates`.
///
/// Directory inputs load direct `.md` children non-recursively. File inputs load
/// explicit `.md` files. Missing paths and non-markdown files are skipped. Read
/// and parse failures are returned as diagnostics.
pub fn load_prompt_templates(env: &impl ExecutionEnv, paths: &[&str]) -> LoadedPromptTemplates {
    let mut out = LoadedPromptTemplates::default();
    for path in paths {
        let info = match env.file_info(path, None) {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    out.diagnostics.push(warning(
                        PromptTemplateDiagnosticCode::FileInfoFailed,
                        error.message,
                        (*path).to_string(),
                    ));
                }
                continue;
            }
        };
        match resolve_kind(env, &info, &mut out.diagnostics) {
            Some(FileKind::Directory) => {
                let result = load_templates_from_dir(env, &info.path);
                out.prompt_templates.extend(result.prompt_templates);
                out.diagnostics.extend(result.diagnostics);
            }
            Some(FileKind::File) if info.name.ends_with(".md") => {
                let result = load_template_from_file(env, &info.path);
                if let Some(template) = result.prompt_template {
                    out.prompt_templates.push(template);
                }
                out.diagnostics.extend(result.diagnostics);
            }
            _ => {}
        }
    }
    out
}

/// Load prompt templates from source-tagged paths. Mirrors pi's
/// `loadSourcedPromptTemplates` (identity mapping; see the module note on the
/// dropped `mapPromptTemplate` generic).
///
/// Source values are preserved exactly and attached to every loaded prompt
/// template and diagnostic.
pub fn load_sourced_prompt_templates<S: Clone>(
    env: &impl ExecutionEnv,
    inputs: &[(&str, S)],
) -> LoadedSourcedPromptTemplates<S> {
    let mut out = LoadedSourcedPromptTemplates {
        prompt_templates: Vec::new(),
        diagnostics: Vec::new(),
    };
    for (path, source) in inputs {
        let result = load_prompt_templates(env, &[path]);
        for prompt_template in result.prompt_templates {
            out.prompt_templates.push(SourcedPromptTemplate {
                prompt_template,
                source: source.clone(),
            });
        }
        for diagnostic in result.diagnostics {
            out.diagnostics.push(SourcedPromptTemplateDiagnostic {
                diagnostic,
                source: source.clone(),
            });
        }
    }
    out
}

fn load_templates_from_dir(env: &impl ExecutionEnv, dir: &str) -> LoadedPromptTemplates {
    let mut out = LoadedPromptTemplates::default();
    let mut entries = match env.list_dir(dir, None) {
        Ok(entries) => entries,
        Err(error) => {
            out.diagnostics.push(warning(
                PromptTemplateDiagnosticCode::ListFailed,
                error.message,
                dir.to_string(),
            ));
            return out;
        }
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let kind = resolve_kind(env, &entry, &mut out.diagnostics);
        if kind != Some(FileKind::File) || !entry.name.ends_with(".md") {
            continue;
        }
        let result = load_template_from_file(env, &entry.path);
        if let Some(template) = result.prompt_template {
            out.prompt_templates.push(template);
        }
        out.diagnostics.extend(result.diagnostics);
    }
    out
}

struct LoadedTemplate {
    prompt_template: Option<PromptTemplate>,
    diagnostics: Vec<PromptTemplateDiagnostic>,
}

fn load_template_from_file(env: &impl ExecutionEnv, file_path: &str) -> LoadedTemplate {
    let mut diagnostics = Vec::new();
    let raw_content = match env.read_text_file(file_path, None) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(warning(
                PromptTemplateDiagnosticCode::ReadFailed,
                error.message,
                file_path.to_string(),
            ));
            return LoadedTemplate {
                prompt_template: None,
                diagnostics,
            };
        }
    };

    let (frontmatter, body) = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(warning(
                PromptTemplateDiagnosticCode::ParseFailed,
                message,
                file_path.to_string(),
            ));
            return LoadedTemplate {
                prompt_template: None,
                diagnostics,
            };
        }
    };

    // pi: `firstLine = body.split("\n").find((line) => line.trim())`.
    let first_line = body.split('\n').find(|line| !line.trim().is_empty());
    let mut description = frontmatter_string(&frontmatter, "description").unwrap_or_default();
    if description.is_empty() {
        if let Some(first_line) = first_line {
            // pi: `firstLine.slice(0, 60)` then append "..." when longer.
            description = first_line.chars().take(60).collect();
            if first_line.chars().count() > 60 {
                description.push_str("...");
            }
        }
    }

    LoadedTemplate {
        prompt_template: Some(PromptTemplate {
            name: strip_md_suffix(&basename_env_path(file_path)),
            description: Some(description),
            content: body,
        }),
        diagnostics,
    }
}

fn resolve_kind(
    env: &impl ExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind);
    }
    let canonical_path = match env.canonical_path(&info.path, None) {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(warning(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
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
                    PromptTemplateDiagnosticCode::FileInfoFailed,
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

fn warning(
    code: PromptTemplateDiagnosticCode,
    message: String,
    path: String,
) -> PromptTemplateDiagnostic {
    PromptTemplateDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code,
        message,
        path,
    }
}

/// Parse YAML frontmatter, returning `(frontmatter, body)`. Mirrors pi's
/// `parseFrontmatter`.
pub(crate) fn parse_frontmatter(content: &str) -> Result<(serde_yaml::Value, String), String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let empty = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    if !normalized.starts_with("---") {
        return Ok((empty, normalized));
    }
    // pi: `normalized.indexOf("\n---", 3)`.
    let end_index = match normalized[3..].find("\n---") {
        Some(offset) => offset + 3,
        None => return Ok((empty, normalized)),
    };
    let yaml_string = normalized.get(4..end_index).unwrap_or("");
    let body = normalized
        .get(end_index + 4..)
        .unwrap_or("")
        .trim()
        .to_string();
    match serde_yaml::from_str::<serde_yaml::Value>(yaml_string) {
        // pi: `parse(yamlString) ?? {}` — a null/empty document is an empty map.
        Ok(serde_yaml::Value::Null) => Ok((empty, body)),
        Ok(value) => Ok((value, body)),
        Err(error) => Err(error.to_string()),
    }
}

/// Extract a string-valued frontmatter field, mirroring pi's `typeof x ===
/// "string"` guard (non-string values read as absent).
pub(crate) fn frontmatter_string(frontmatter: &serde_yaml::Value, key: &str) -> Option<String> {
    frontmatter
        .get(key)
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
}

pub(crate) fn basename_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    match normalized.rfind('/') {
        Some(index) => normalized[index + 1..].to_string(),
        None => normalized.to_string(),
    }
}

/// Strip a trailing `.md`/`.MD` suffix, mirroring pi's `.replace(/\.md$/i, "")`.
fn strip_md_suffix(name: &str) -> String {
    if name.len() >= 3 && name[name.len() - 3..].eq_ignore_ascii_case(".md") {
        name[..name.len() - 3].to_string()
    } else {
        name.to_string()
    }
}

/// Parse an argument string using simple shell-style single and double quotes.
/// Mirrors pi's `parseCommandArgs`.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for char in args_string.chars() {
        if let Some(quote) = in_quote {
            if char == quote {
                in_quote = None;
            } else {
                current.push(char);
            }
        } else if char == '"' || char == '\'' {
            in_quote = Some(char);
        } else if char == ' ' || char == '\t' {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(char);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Substitute prompt template placeholders (`$1`, `$@`, `$ARGUMENTS`, `${@:N}`,
/// `${@:N:L}`) with command arguments. Mirrors pi's `substituteArgs`, applying
/// the four replacement passes in the same order.
pub fn substitute_args(content: &str, args: &[&str]) -> String {
    let result = substitute_positional(content, args);
    let result = substitute_range(&result, args);
    let all_args = args.join(" ");
    // pi: `$ARGUMENTS` then `$@`, both replaced with the joined argument list.
    result
        .replace("$ARGUMENTS", &all_args)
        .replace("$@", &all_args)
}

// pi pass 1: `/\$(\d+)/g` → `args[parseInt(num) - 1] ?? ""`.
fn substitute_positional(content: &str, args: &[&str]) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            let mut j = i + 1;
            let mut num = 0usize;
            while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                num = num * 10 + (chars[j] as usize - '0' as usize);
                j += 1;
            }
            // `args[num - 1] ?? ""`: index 0 (num == 0) and out-of-range read as "".
            if let Some(arg) = num.checked_sub(1).and_then(|idx| args.get(idx)) {
                out.push_str(arg);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn substitute_range(content: &str, args: &[&str]) -> String {
    // pi pass 2: `/\$\{@:(\d+)(?::(\d+))?\}/g`.
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some((replacement, next)) = match_range_placeholder(&chars, i, args) {
            out.push_str(&replacement);
            i = next;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn match_range_placeholder(chars: &[char], start: usize, args: &[&str]) -> Option<(String, usize)> {
    // Match the literal prefix `${@:`.
    if chars.get(start) != Some(&'$')
        || chars.get(start + 1) != Some(&'{')
        || chars.get(start + 2) != Some(&'@')
        || chars.get(start + 3) != Some(&':')
    {
        return None;
    }
    let mut i = start + 4;
    let n_start = i;
    let mut n = 0usize;
    while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
        n = n * 10 + (chars[i] as usize - '0' as usize);
        i += 1;
    }
    if i == n_start {
        return None; // pi regex requires at least one digit after `@:`.
    }
    let mut length: Option<usize> = None;
    if chars.get(i) == Some(&':') && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
        let mut l = 0usize;
        while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
            l = l * 10 + (chars[i] as usize - '0' as usize);
            i += 1;
        }
        length = Some(l);
    }
    if chars.get(i) != Some(&'}') {
        return None;
    }
    i += 1;

    // pi: `let start = parseInt(startStr) - 1; if (start < 0) start = 0;`.
    let slice_start = n.saturating_sub(1).min(args.len());
    let slice_end = match length {
        Some(len) => (slice_start + len).min(args.len()),
        None => args.len(),
    };
    Some((args[slice_start..slice_end].join(" "), i))
}

/// Format a prompt template invocation with positional arguments. Mirrors pi's
/// `formatPromptTemplateInvocation`.
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[&str]) -> String {
    substitute_args(&template.content, args)
}

#[cfg(test)]
mod tests {
    // Port of `test/harness/prompt-templates.test.ts`. pi drives a real
    // `NodeExecutionEnv` over a temp dir; here the same tree is seeded into a
    // `MemoryExecutionEnv` with absolute paths, so the loaded `filePath`/paths
    // match the seeded absolute paths (as pi's `join(root, ...)` expectations do).
    use super::*;
    use crate::harness::env::MemoryExecutionEnv;

    fn template(name: &str, description: Option<&str>, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: description.map(str::to_string),
            content: content.to_string(),
        }
    }

    #[test]
    fn loads_markdown_templates_non_recursively_from_one_or_more_dirs() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/a")
            .with_dir("/root/a/nested")
            .with_dir("/root/b")
            .with_file(
                "/root/a/one.md",
                "---\ndescription: One template\n---\nHello $1",
            )
            .with_file("/root/a/nested/ignored.md", "Ignored")
            .with_file("/root/b/two.md", "First line description\nBody");

        let result = load_prompt_templates(&env, &["/root/a", "/root/b"]);

        assert_eq!(result.diagnostics, vec![]);
        assert_eq!(
            result.prompt_templates,
            vec![
                template("one", Some("One template"), "Hello $1"),
                template(
                    "two",
                    Some("First line description"),
                    "First line description\nBody"
                ),
            ]
        );
    }

    #[test]
    fn preserves_source_info_for_sourced_prompt_templates() {
        let env = MemoryExecutionEnv::new("/root")
            .with_dir("/root/prompts")
            .with_file(
                "/root/prompts/example.md",
                "---\ndescription: Example\n---\nExample body",
            );

        let result = load_sourced_prompt_templates(&env, &[("/root/prompts", "project")]);

        assert_eq!(result.diagnostics, vec![]);
        assert_eq!(result.prompt_templates.len(), 1);
        assert_eq!(
            result.prompt_templates[0].prompt_template,
            template("example", Some("Example"), "Example body")
        );
        assert_eq!(result.prompt_templates[0].source, "project");
    }

    #[test]
    fn attaches_source_info_to_diagnostics() {
        let env = MemoryExecutionEnv::new("/root").with_file(
            "/root/broken.md",
            "---\ndescription: [unterminated\n---\nBody",
        );

        let result = load_sourced_prompt_templates(&env, &[("/root/broken.md", "user")]);

        assert_eq!(result.prompt_templates.len(), 0);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].diagnostic.severity,
            DiagnosticSeverity::Warning
        );
        assert_eq!(result.diagnostics[0].diagnostic.path, "/root/broken.md");
        assert_eq!(result.diagnostics[0].source, "user");
    }

    #[test]
    fn loads_explicit_markdown_files_and_symlinked_files() {
        let env = MemoryExecutionEnv::new("/root")
            .with_file(
                "/root/target.md",
                "---\ndescription: Target\n---\nTarget body",
            )
            .with_symlink("/root/link.md", "/root/target.md");

        let result = load_prompt_templates(&env, &["/root/target.md", "/root/link.md"]);

        assert_eq!(
            result.prompt_templates,
            vec![
                template("target", Some("Target"), "Target body"),
                template("link", Some("Target"), "Target body"),
            ]
        );
    }

    #[test]
    fn format_prompt_template_invocation_substitutes_command_arguments() {
        // pi: `content = "$1 $" + "{@:2} $ARGUMENTS"`.
        let content = "$1 ${@:2} $ARGUMENTS";
        assert_eq!(
            format_prompt_template_invocation(
                &template("one", None, content),
                &["hello world", "test"]
            ),
            "hello world test hello world test"
        );
    }

    #[test]
    fn format_prompt_template_invocation_with_positional_arguments() {
        // From resource-formatting.test.ts.
        assert_eq!(
            format_prompt_template_invocation(
                &template("review", None, "Review $1 with $ARGUMENTS"),
                &["a.ts", "care"]
            ),
            "Review a.ts with a.ts care"
        );
    }
}
