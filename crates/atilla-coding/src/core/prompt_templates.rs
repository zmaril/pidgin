//! Prompt template loading, argument parsing, and placeholder substitution.
//!
//! Ported from pi's `core/prompt-templates.ts`. A prompt template is a markdown
//! file (optionally with YAML frontmatter) that a user invokes as a slash
//! command; its body supports bash-style argument interpolation.
//!
//! The three pure functions the conformance suite exercises are
//! [`parse_command_args`], [`substitute_args`], and [`expand_prompt_template`];
//! [`load_prompt_templates`] performs the filesystem discovery.
//!
//! NOTE: pi imports `CONFIG_DIR_NAME` from `../config.ts` and the source-info
//! helpers from `./source-info.ts`. Neither collaborator is ported yet, so this
//! module inlines the one constant it needs ([`CONFIG_DIR_NAME`]) and a minimal
//! [`SourceInfo`] mirror. When `config`/`source-info` land as their own modules
//! these should be re-exported from there instead.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::utils::frontmatter::parse_frontmatter;
use crate::utils::paths::{resolve_path, PathInputOptions};

/// Project-local config directory name.
///
/// NOTE: pi derives this from `package.json`'s `piConfig.configDir`, defaulting
/// to `.pi` (see `config.ts`). Inlined here until `config` is ported.
pub const CONFIG_DIR_NAME: &str = ".pi";

/// Scope of a resource relative to where it was discovered.
///
/// NOTE: minimal mirror of `core/source-info.ts`'s `SourceScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    /// A user-global resource (e.g. under the agent config dir).
    User,
    /// A project-local resource (e.g. under `.pi/`).
    Project,
    /// An ad-hoc resource with no fixed home.
    Temporary,
}

/// Whether a resource came from a package or a top-level location.
///
/// NOTE: minimal mirror of `core/source-info.ts`'s `SourceOrigin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceOrigin {
    /// Provided by an installed package.
    #[serde(rename = "package")]
    Package,
    /// Provided directly by the user/project.
    #[serde(rename = "top-level")]
    TopLevel,
}

/// Provenance of a loaded resource.
///
/// NOTE: minimal mirror of `core/source-info.ts`'s `SourceInfo`. Only the
/// fields the prompt-template loader populates are represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    /// Absolute path to the resource.
    pub path: String,
    /// Source identifier (e.g. `"local"`).
    pub source: String,
    /// Scope relative to discovery.
    pub scope: SourceScope,
    /// Package vs. top-level origin.
    pub origin: SourceOrigin,
    /// Base directory the resource was discovered under, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

impl SourceInfo {
    /// Build a synthetic source info, mirroring pi's `createSyntheticSourceInfo`
    /// (scope defaults to [`SourceScope::Temporary`], origin to
    /// [`SourceOrigin::TopLevel`]).
    pub fn synthetic(
        path: impl Into<String>,
        source: impl Into<String>,
        scope: Option<SourceScope>,
        base_dir: Option<String>,
    ) -> Self {
        SourceInfo {
            path: path.into(),
            source: source.into(),
            scope: scope.unwrap_or(SourceScope::Temporary),
            origin: SourceOrigin::TopLevel,
            base_dir,
        }
    }
}

/// A prompt template loaded from a markdown file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    /// Template name (file stem, without the `.md` extension).
    pub name: String,
    /// Short description (from frontmatter or the first non-empty body line).
    pub description: String,
    /// Optional argument hint from frontmatter (`argument-hint`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// Template body (frontmatter stripped).
    pub content: String,
    /// Provenance of the template file.
    pub source_info: SourceInfo,
    /// Absolute path to the template file.
    pub file_path: String,
}

/// Parse command arguments respecting quoted strings (bash-style).
///
/// Mirrors pi's `parseCommandArgs`: single/double quotes group whitespace,
/// unmatched or empty quotes are dropped, and there is no escape mechanism
/// (a backslash is a literal character).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in args_string.chars() {
        if let Some(quote) = in_quote {
            if ch == quote {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn substitute_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)")
            .expect("valid substitution regex")
    })
}

/// Positional index (1-based, bash-style) resolved against `args`, or `None`
/// when the token does not name a present argument.
fn positional(args: &[String], token: &str) -> Option<String> {
    token
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| args.get(i))
        .cloned()
}

/// Join `args[start..]` (optionally limited to `len` items), with JS
/// `Array.slice` clamping semantics.
fn slice_join(args: &[String], start: usize, len: Option<usize>) -> String {
    if start >= args.len() {
        return String::new();
    }
    let end = match len {
        Some(l) => start.saturating_add(l).min(args.len()),
        None => args.len(),
    };
    args[start..end].join(" ")
}

/// Substitute argument placeholders in template content.
///
/// Mirrors pi's `substituteArgs`. Supports `$1`, `$@`/`$ARGUMENTS`,
/// `${N:-default}`, `${@:-default}`/`${ARGUMENTS:-default}`, `${@:N}`, and
/// `${@:N:L}`. Replacement runs once over the template only; patterns appearing
/// in argument or default values are never re-expanded.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    substitute_regex()
        .replace_all(content, |caps: &Captures<'_>| -> String {
            if let Some(default_target) = caps.get(1) {
                let default_value = caps.get(2).map_or("", |m| m.as_str());
                let value = if matches!(default_target.as_str(), "@" | "ARGUMENTS") {
                    Some(all_args.clone())
                } else {
                    positional(args, default_target.as_str())
                };
                // JS `value ? value : defaultValue`: empty strings are falsy.
                match value {
                    Some(v) if !v.is_empty() => v,
                    _ => default_value.to_string(),
                }
            } else if let Some(slice_start) = caps.get(3) {
                // 1-indexed input; bash treats 0 as 1, so clamp at 0.
                let start = slice_start
                    .as_str()
                    .parse::<usize>()
                    .ok()
                    .map(|n| n.saturating_sub(1))
                    .unwrap_or(0);
                let len = caps.get(4).and_then(|m| m.as_str().parse::<usize>().ok());
                slice_join(args, start, len)
            } else {
                let simple = caps.get(5).map_or("", |m| m.as_str());
                if matches!(simple, "ARGUMENTS" | "@") {
                    all_args.clone()
                } else {
                    positional(args, simple).unwrap_or_default()
                }
            }
        })
        .into_owned()
}

fn frontmatter_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn load_template_from_file(file_path: &str, source_info: SourceInfo) -> Option<PromptTemplate> {
    let raw_content = fs::read_to_string(file_path).ok()?;
    let (frontmatter, body) = parse_frontmatter(&raw_content).ok()?;

    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let name = file_name
        .strip_suffix(".md")
        .unwrap_or(file_name)
        .to_string();

    // Description from frontmatter, else first non-empty body line (truncated).
    let mut description = frontmatter_str(&frontmatter, "description").unwrap_or_default();
    if description.is_empty() {
        if let Some(first_line) = body.split('\n').find(|line| !line.trim().is_empty()) {
            description = first_line.chars().take(60).collect();
            if first_line.chars().count() > 60 {
                description.push_str("...");
            }
        }
    }

    let argument_hint = frontmatter_str(&frontmatter, "argument-hint").filter(|s| !s.is_empty());

    Some(PromptTemplate {
        name,
        description,
        argument_hint,
        content: body,
        source_info,
        file_path: file_path.to_string(),
    })
}

/// Scan a directory for `.md` files (non-recursive) and load them as templates.
fn load_templates_from_dir(
    dir: &str,
    get_source_info: &dyn Fn(&str) -> SourceInfo,
) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();

    let Ok(entries) = fs::read_dir(dir) else {
        return templates;
    };

    for entry in entries.flatten() {
        let full_path = entry.path();
        let Some(full_path_str) = full_path.to_str() else {
            continue;
        };

        // `metadata` follows symlinks; a broken symlink errors and is skipped.
        let Ok(metadata) = fs::metadata(&full_path) else {
            continue;
        };

        if metadata.is_file() && full_path_str.ends_with(".md") {
            if let Some(template) =
                load_template_from_file(full_path_str, get_source_info(full_path_str))
            {
                templates.push(template);
            }
        }
    }

    templates
}

/// Options for [`load_prompt_templates`].
#[derive(Debug, Clone)]
pub struct LoadPromptTemplatesOptions {
    /// Working directory for project-local templates.
    pub cwd: String,
    /// Agent config directory for global templates.
    pub agent_dir: String,
    /// Explicit prompt template paths (files or directories).
    pub prompt_paths: Vec<String>,
    /// Include the default prompt directories.
    pub include_defaults: bool,
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string()
}

fn is_dir(path: &str) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// `path.resolve(root)` — resolve a single path against the current directory.
fn resolve_single(path: &str) -> String {
    resolve_path(path, &current_dir(), &PathInputOptions::default())
        .unwrap_or_else(|_| path.to_string())
}

fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = resolve_single(root);
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

/// Load all prompt templates from the global directory, the project directory,
/// and any explicit prompt paths.
///
/// Mirrors pi's `loadPromptTemplates`.
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> Vec<PromptTemplate> {
    let default_opts = PathInputOptions::default();
    let resolved_cwd = resolve_single(&options.cwd);
    let resolved_agent_dir = resolve_single(&options.agent_dir);

    let mut templates: Vec<PromptTemplate> = Vec::new();

    let global_prompts_dir = format!("{resolved_agent_dir}/prompts");
    let project_prompts_dir = resolve_path(
        &format!("{CONFIG_DIR_NAME}/prompts"),
        &resolved_cwd,
        &default_opts,
    )
    .unwrap_or_else(|_| format!("{resolved_cwd}/{CONFIG_DIR_NAME}/prompts"));

    let get_source_info = |resolved_path: &str| -> SourceInfo {
        if is_under_path(resolved_path, &global_prompts_dir) {
            return SourceInfo::synthetic(
                resolved_path,
                "local",
                Some(SourceScope::User),
                Some(global_prompts_dir.clone()),
            );
        }
        if is_under_path(resolved_path, &project_prompts_dir) {
            return SourceInfo::synthetic(
                resolved_path,
                "local",
                Some(SourceScope::Project),
                Some(project_prompts_dir.clone()),
            );
        }
        let base_dir = if is_dir(resolved_path) {
            resolved_path.to_string()
        } else {
            dirname(resolved_path)
        };
        SourceInfo::synthetic(resolved_path, "local", None, Some(base_dir))
    };

    if options.include_defaults {
        templates.extend(load_templates_from_dir(
            &global_prompts_dir,
            &get_source_info,
        ));
        templates.extend(load_templates_from_dir(
            &project_prompts_dir,
            &get_source_info,
        ));
    }

    let trim_opts = PathInputOptions {
        trim: true,
        ..PathInputOptions::default()
    };

    for raw_path in &options.prompt_paths {
        let Ok(resolved_path) = resolve_path(raw_path, &resolved_cwd, &trim_opts) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&resolved_path) else {
            continue;
        };

        if metadata.is_dir() {
            templates.extend(load_templates_from_dir(&resolved_path, &get_source_info));
        } else if metadata.is_file() && resolved_path.ends_with(".md") {
            if let Some(template) =
                load_template_from_file(&resolved_path, get_source_info(&resolved_path))
            {
                templates.push(template);
            }
        }
    }

    templates
}

fn expand_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/([^\s]+)(?:\s+([\s\S]*))?$").expect("valid expansion regex"))
}

/// Expand a prompt template if `text` matches a template name.
///
/// Returns the expanded content, or the original text when it is not a slash
/// command or names no known template. Mirrors pi's `expandPromptTemplate`.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }

    let Some(caps) = expand_regex().captures(text) else {
        return text.to_string();
    };

    let template_name = caps.get(1).map_or("", |m| m.as_str());
    let args_string = caps.get(2).map_or("", |m| m.as_str());

    if let Some(template) = templates.iter().find(|t| t.name == template_name) {
        let args = parse_command_args(args_string);
        return substitute_args(&template.content, &args);
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- substitute_args -----------------------------------------------------

    fn sub(content: &str, args: &[&str]) -> String {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        substitute_args(content, &owned)
    }

    /// Assert each `(template, args, expected)` row substitutes as expected.
    fn check_all(cases: &[(&str, &[&str], &str)]) {
        for (content, args, expected) in cases {
            assert_eq!(
                sub(content, args),
                *expected,
                "content={content:?} args={args:?}"
            );
        }
    }

    #[test]
    fn substitutes_all_args_and_positionals() {
        check_all(&[
            ("Test: $ARGUMENTS", &["a", "b", "c"], "Test: a b c"),
            ("Test: $@", &["a", "b", "c"], "Test: a b c"),
            (
                "$1: $ARGUMENTS",
                &["prefix", "a", "b"],
                "prefix: prefix a b",
            ),
            ("$1: $@", &["prefix", "a", "b"], "prefix: prefix a b"),
            ("$1 $2 $3", &["a", "b", "c"], "a b c"),
            ("$1 $2 $@", &["a", "b", "c"], "a b a b c"),
            ("Just plain text", &["a", "b"], "Just plain text"),
            (
                "$1: $@ ($ARGUMENTS)",
                &["first", "second", "third"],
                "first: first second third (first second third)",
            ),
        ]);
    }

    #[test]
    fn substitutes_edge_cases() {
        check_all(&[
            ("Test: $ARGUMENTS", &[], "Test: "),
            ("Test: $@", &[], "Test: "),
            ("Test: $1", &[], "Test: "),
            ("$ARGUMENTS and $ARGUMENTS", &["a", "b"], "a b and a b"),
            ("$@ and $@", &["a", "b"], "a b and a b"),
            ("$@ and $ARGUMENTS", &["a", "b"], "a b and a b"),
            ("$1 $2 $3 $4 $5", &["a", "b"], "a b   "),
            ("$1$2", &["a", "b"], "ab"),
            ("$0", &["a", "b"], ""),
            ("$1.5", &["a"], "a.5"),
            ("pre$ARGUMENTS", &["a", "b"], "prea b"),
            ("pre$@", &["a", "b"], "prea b"),
            ("$ARGUMENTS", &["a", "", "c"], "a  c"),
            (
                "Prefix $ARGUMENTS suffix",
                &["ARGUMENTS"],
                "Prefix ARGUMENTS suffix",
            ),
            ("$A $$ $ $ARGS", &["a"], "$A $$ $ $ARGS"),
            (
                "$arguments $Arguments $ARGUMENTS",
                &["a", "b"],
                "$arguments $Arguments a b",
            ),
            ("Price: \\$100", &[], "Price: \\"),
        ]);
    }

    #[test]
    fn substitutes_without_recursive_expansion() {
        check_all(&[
            ("$ARGUMENTS", &["$1", "$ARGUMENTS"], "$1 $ARGUMENTS"),
            ("$@", &["$100", "$1"], "$100 $1"),
            ("$ARGUMENTS", &["$100", "$1"], "$100 $1"),
            (
                "$1 $2: $ARGUMENTS",
                &["arg100", "@user"],
                "arg100 @user: arg100 @user",
            ),
        ]);
    }

    #[test]
    fn substitutes_unicode_and_whitespace() {
        check_all(&[
            (
                "$ARGUMENTS",
                &["日本語", "\u{1F389}", "café"],
                "日本語 \u{1F389} café",
            ),
            (
                "$1 $2",
                &["line1\nline2", "tab\tthere"],
                "line1\nline2 tab\tthere",
            ),
            (
                "$ARGUMENTS",
                &["first arg", "second arg"],
                "first arg second arg",
            ),
            (
                "$ARGUMENTS",
                &["  leading  ", "trailing  "],
                "  leading   trailing  ",
            ),
        ]);
    }

    #[test]
    fn substitutes_multi_digit_placeholders() {
        let many: Vec<String> = (0..15).map(|i| format!("val{i}")).collect();
        let many_ref: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(sub("$10 $12 $15", &many_ref), "val9 val11 val14");

        let hundred: Vec<String> = (0..100).map(|i| format!("arg{i}")).collect();
        let hundred_ref: Vec<&str> = hundred.iter().map(String::as_str).collect();
        assert_eq!(sub("$ARGUMENTS", &hundred_ref), hundred.join(" "));
    }

    #[test]
    fn substitutes_positional_defaults() {
        check_all(&[
            (
                "List exactly ${1:-7} next steps",
                &[],
                "List exactly 7 next steps",
            ),
            (
                "List exactly ${1:-7} next steps",
                &["3"],
                "List exactly 3 next steps",
            ),
            ("Mode: ${1:-brief}", &[""], "Mode: brief"),
            ("${1:-7} ${2:-brief}", &[], "7 brief"),
            ("${1:-7} ${2:-brief}", &["3"], "3 brief"),
            ("${1:-7} ${2:-brief}", &["3", "verbose"], "3 verbose"),
            ("${1:-7}", &["$ARGUMENTS"], "$ARGUMENTS"),
            ("${1:-7}", &["$1"], "$1"),
            ("${1:-$ARGUMENTS}", &["a", "b"], "a"),
            ("${3:-$ARGUMENTS}", &["a", "b"], "$ARGUMENTS"),
            ("${1:-seven steps}", &[], "seven steps"),
            ("${3:-fallback}", &["a", "b"], "fallback"),
            ("$1 ${2:-x} $ARGUMENTS", &["a"], "a x a"),
            (
                "${@:-default}\n${ARGUMENTS:-default}",
                &[],
                "default\ndefault",
            ),
            (
                "${@:-default}\n${ARGUMENTS:-default}",
                &["This", "would", "be", "the", "arguments"],
                "This would be the arguments\nThis would be the arguments",
            ),
        ]);
    }

    #[test]
    fn substitutes_array_slices() {
        check_all(&[
            ("${@:2}", &["a", "b", "c", "d"], "b c d"),
            ("${@:1}", &["a", "b", "c"], "a b c"),
            ("${@:3}", &["a", "b", "c", "d"], "c d"),
            ("${@:2:2}", &["a", "b", "c", "d"], "b c"),
            ("${@:1:1}", &["a", "b", "c"], "a"),
            ("${@:3:1}", &["a", "b", "c", "d"], "c"),
            ("${@:2:3}", &["a", "b", "c", "d", "e"], "b c d"),
            ("${@:99}", &["a", "b"], ""),
            ("${@:5}", &["a", "b"], ""),
            ("${@:10:5}", &["a", "b"], ""),
            ("${@:2:0}", &["a", "b", "c"], ""),
            ("${@:1:0}", &["a", "b"], ""),
            ("${@:2:99}", &["a", "b", "c"], "b c"),
            ("${@:1:10}", &["a", "b"], "a b"),
            ("${@:0}", &["a", "b", "c"], "a b c"),
            ("${@:2}", &[], ""),
            ("${@:1}", &["only"], "only"),
            ("${@:2}", &["only"], ""),
        ]);
    }

    #[test]
    fn substitutes_slices_mixed_with_other_placeholders() {
        check_all(&[
            ("${@:2} vs $@", &["a", "b", "c"], "b c vs a b c"),
            (
                "First: ${@:1:1}, All: $@",
                &["x", "y", "z"],
                "First: x, All: x y z",
            ),
            ("$1: ${@:2}", &["cmd", "arg1", "arg2"], "cmd: arg1 arg2"),
            ("$1 $2 ${@:3}", &["a", "b", "c", "d"], "a b c d"),
            (
                "Process ${@:2} with $1",
                &["tool", "file1", "file2"],
                "Process file1 file2 with tool",
            ),
            ("${@:1:1} and ${@:2}", &["a", "b", "c"], "a and b c"),
            (
                "${@:1:2} vs ${@:3:2}",
                &["a", "b", "c", "d", "e"],
                "a b vs c d",
            ),
            ("prefix${@:2}suffix", &["a", "b", "c"], "prefixb csuffix"),
            (
                "Run $1 on ${@:2:2}, then process $@",
                &["eslint", "file1.ts", "file2.ts", "file3.ts"],
                "Run eslint on file1.ts file2.ts, then process eslint file1.ts file2.ts file3.ts",
            ),
        ]);
    }

    #[test]
    fn substitutes_slices_without_recursive_expansion() {
        check_all(&[
            ("${@:1}", &["${@:2}", "test"], "${@:2} test"),
            ("${@:2}", &["a", "${@:3}", "c"], "${@:3} c"),
            (
                "${@:2}",
                &["cmd", "first arg", "second arg"],
                "first arg second arg",
            ),
            (
                "${@:2}",
                &["cmd", "$100", "@user", "#tag"],
                "$100 @user #tag",
            ),
            (
                "${@:1}",
                &["日本語", "\u{1F389}", "café"],
                "日本語 \u{1F389} café",
            ),
        ]);
    }

    #[test]
    fn substitutes_large_slice_lengths() {
        let args: Vec<String> = (1..=10).map(|i| format!("arg{i}")).collect();
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(
            sub("${@:5:100}", &args_ref),
            "arg5 arg6 arg7 arg8 arg9 arg10"
        );
    }

    // --- parse_command_args --------------------------------------------------

    /// Assert each `(input, expected_args)` row parses as expected.
    fn check_parse(cases: &[(&str, &[&str])]) {
        for (input, expected) in cases {
            let got = parse_command_args(input);
            let expected_owned: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
            assert_eq!(got, expected_owned, "input={input:?}");
        }
    }

    #[test]
    fn parses_command_arguments() {
        check_parse(&[
            ("a b c", &["a", "b", "c"]),
            ("\"first arg\" second", &["first arg", "second"]),
            ("'first arg' second", &["first arg", "second"]),
            (
                "\"double\" 'single' \"double again\"",
                &["double", "single", "double again"],
            ),
            ("", &[]),
            ("a  b   c", &["a", "b", "c"]),
            ("a\tb\tc", &["a", "b", "c"]),
            ("\"\" \" \"", &[" "]),
            ("$100 @user #tag", &["$100", "@user", "#tag"]),
            ("日本語 \u{1F389} café", &["日本語", "\u{1F389}", "café"]),
            ("\"line1\nline2\" second", &["line1\nline2", "second"]),
            (
                "label-2\n\nHere is some description #2.",
                &["label-2", "Here", "is", "some", "description", "#2."],
            ),
            ("a\n\n\tb  c", &["a", "b", "c"]),
            ("\"quoted \\\"text\\\"\"", &["quoted \\text\\"]),
            ("a b c   ", &["a", "b", "c"]),
            ("   a b c", &["a", "b", "c"]),
        ]);
    }

    // --- expand_prompt_template ----------------------------------------------

    fn base_template() -> PromptTemplate {
        PromptTemplate {
            name: "arg-test".to_string(),
            description: "test".to_string(),
            argument_hint: None,
            content: String::new(),
            source_info: SourceInfo::synthetic(
                "/tmp/arg-test.md",
                "local",
                Some(SourceScope::Temporary),
                None,
            ),
            file_path: "/tmp/arg-test.md".to_string(),
        }
    }

    #[test]
    fn expands_template_splitting_args_on_newlines() {
        let templates = vec![PromptTemplate {
            content: "- arg1: $1\n- rest: ${@:2}".to_string(),
            ..base_template()
        }];
        let result = expand_prompt_template(
            "/arg-test label-2\n\nHere is some description #2.",
            &templates,
        );
        assert_eq!(
            result,
            "- arg1: label-2\n- rest: Here is some description #2."
        );
    }

    #[test]
    fn expands_template_with_command_and_args_split_by_newline() {
        let templates = vec![PromptTemplate {
            content: "arg1: $1".to_string(),
            ..base_template()
        }];
        assert_eq!(
            expand_prompt_template("/arg-test\nlabel-2", &templates),
            "arg1: label-2"
        );
    }

    #[test]
    fn expands_returns_original_for_non_command_or_unknown() {
        let templates = vec![base_template()];
        assert_eq!(
            expand_prompt_template("plain text", &templates),
            "plain text"
        );
        assert_eq!(
            expand_prompt_template("/unknown foo", &templates),
            "/unknown foo"
        );
    }

    // --- parse + substitute integration --------------------------------------

    #[test]
    fn parses_and_substitutes_together() {
        let cases = [
            (
                "Button \"onClick handler\" \"disabled support\"",
                "Create component $1 with features: $ARGUMENTS",
                "Create component Button with features: Button onClick handler disabled support",
            ),
            (
                "Button \"onClick handler\" \"disabled support\"",
                "Create a React component named $1 with features: $ARGUMENTS",
                "Create a React component named Button with features: Button onClick handler disabled support",
            ),
        ];
        for (input, template, expected) in cases {
            let args = parse_command_args(input);
            assert_eq!(substitute_args(template, &args), expected);
        }
    }

    // --- load_prompt_templates -----------------------------------------------

    /// A self-cleaning temp directory holding template `.md` files.
    struct TempPromptDir {
        path: PathBuf,
    }

    impl TempPromptDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "atilla-prompts-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            TempPromptDir { path }
        }

        /// Write `<name>.md`, load templates from this dir, and return the one
        /// named `name`.
        fn load(&self, name: &str, content: &str) -> PromptTemplate {
            fs::write(self.path.join(format!("{name}.md")), content).unwrap();
            let templates = load_prompt_templates(&LoadPromptTemplatesOptions {
                cwd: current_dir(),
                agent_dir: self.path.to_str().unwrap().to_string(),
                prompt_paths: vec![self.path.to_str().unwrap().to_string()],
                include_defaults: false,
            });
            templates
                .into_iter()
                .find(|t| t.name == name)
                .expect("template loaded")
        }
    }

    impl Drop for TempPromptDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn loads_argument_hint_and_description_from_frontmatter() {
        let dir = TempPromptDir::new();
        let cases = [
            (
                "pr",
                "---\ndescription: Review PRs from URLs with structured issue and code analysis\nargument-hint: \"<PR-URL>\"\n---\nYou are given one or more GitHub PR URLs: $@",
                Some("<PR-URL>"),
                Some("Review PRs from URLs with structured issue and code analysis"),
            ),
            (
                "wr",
                "---\ndescription: Finish the current task end-to-end with changelog, commit, and push\nargument-hint: \"[instructions]\"\n---\nWrap it. Additional instructions: $ARGUMENTS",
                Some("[instructions]"),
                Some("Finish the current task end-to-end with changelog, commit, and push"),
            ),
            (
                "is",
                "---\ndescription: Analyze GitHub issues (bugs or feature requests)\nargument-hint: \"<issue>\"\n---\nAnalyze GitHub issue(s): $ARGUMENTS",
                Some("<issue>"),
                Some("Analyze GitHub issues (bugs or feature requests)"),
            ),
        ];
        for (name, content, hint, desc) in cases {
            let template = dir.load(name, content);
            assert_eq!(template.argument_hint.as_deref(), hint, "hint for {name}");
            assert_eq!(Some(template.description.as_str()), desc, "desc for {name}");
        }
    }

    #[test]
    fn omits_missing_or_empty_argument_hint() {
        let dir = TempPromptDir::new();
        let cases = [
            (
                "cl",
                "---\ndescription: Audit changelog entries before release\n---\nAudit changelog entries for all commits since the last release.",
            ),
            (
                "empty-hint",
                "---\ndescription: A command with empty hint\nargument-hint: \"\"\n---\nDo something",
            ),
        ];
        for (name, content) in cases {
            let template = dir.load(name, content);
            assert_eq!(template.argument_hint, None, "hint for {name}");
        }
    }
}
