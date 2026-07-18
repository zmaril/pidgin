//! System prompt construction and project context assembly.
//!
//! Ported from pi's `core/system-prompt.ts`. [`build_system_prompt`] assembles
//! the agent's system prompt from the selected tools, guideline bullets,
//! project context files, and skills, in the exact order pi's tests pin.
//!
//! NOTE: pi computes the pi documentation paths at runtime via
//! `getReadmePath`/`getDocsPath`/`getExamplesPath` (from `config.ts`) and
//! formats skills via `formatSkillsForPrompt` (from `skills.ts`). Neither
//! collaborator is ported yet, so the doc paths are taken as an input
//! ([`PiDocPaths`]) and the pure skill-formatting slice is inlined here
//! ([`format_skills_for_prompt`]) against a minimal [`SystemPromptSkill`] input.
//! When `config`/`skills` land these seams should delegate to them.

use std::collections::HashSet;

/// Absolute paths to the bundled pi documentation, examples, and README.
///
/// NOTE: seam for `config.ts`'s `getReadmePath`/`getDocsPath`/`getExamplesPath`.
#[derive(Debug, Clone, Default)]
pub struct PiDocPaths {
    /// Path to the main README.
    pub readme: String,
    /// Path to the docs directory.
    pub docs: String,
    /// Path to the examples directory.
    pub examples: String,
}

/// A pre-loaded project context file.
#[derive(Debug, Clone)]
pub struct ContextFile {
    /// Path shown in the `<project_instructions>` tag.
    pub path: String,
    /// File contents.
    pub content: String,
}

/// Minimal skill input for prompt formatting.
///
/// NOTE: mirrors the fields of `skills.ts`'s `Skill` that
/// `formatSkillsForPrompt` reads. The full skill loader lands separately.
#[derive(Debug, Clone)]
pub struct SystemPromptSkill {
    /// Skill name.
    pub name: String,
    /// Skill description.
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub file_path: String,
    /// When true the skill is hidden from the prompt (invoke-only).
    pub disable_model_invocation: bool,
}

/// Options for [`build_system_prompt`].
#[derive(Debug, Clone, Default)]
pub struct BuildSystemPromptOptions {
    /// Custom system prompt that replaces the default preamble.
    pub custom_prompt: Option<String>,
    /// Tools to include. Defaults to `[read, bash, edit, write]` when `None`.
    pub selected_tools: Option<Vec<String>>,
    /// One-line tool snippets keyed by tool name; a tool is only listed when it
    /// has a snippet.
    pub tool_snippets: Vec<(String, String)>,
    /// Additional guideline bullets appended to the defaults.
    pub prompt_guidelines: Vec<String>,
    /// Text appended after the main prompt body.
    pub append_system_prompt: Option<String>,
    /// Working directory (rendered with `/` separators).
    pub cwd: String,
    /// Pre-loaded project context files.
    pub context_files: Vec<ContextFile>,
    /// Pre-loaded skills.
    pub skills: Vec<SystemPromptSkill>,
    /// Bundled pi documentation paths (see [`PiDocPaths`]).
    pub doc_paths: PiDocPaths,
}

fn snippet_for<'a>(snippets: &'a [(String, String)], name: &str) -> Option<&'a str> {
    snippets
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Escape the five XML metacharacters, mirroring `skills.ts`'s `escapeXml`.
fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Format skills as an `<available_skills>` block for the system prompt.
///
/// Skills with `disable_model_invocation == true` are excluded. Returns an
/// empty string when no visible skills remain. Mirrors `skills.ts`'s
/// `formatSkillsForPrompt`.
pub fn format_skills_for_prompt(skills: &[SystemPromptSkill]) -> String {
    let visible: Vec<&SystemPromptSkill> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();

    if visible.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = vec![
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

/// Append the `<project_context>` block for `context_files` to `prompt`.
fn append_context_files(prompt: &mut String, context_files: &[ContextFile]) {
    if context_files.is_empty() {
        return;
    }
    prompt.push_str("\n\n<project_context>\n\n");
    prompt.push_str("Project-specific instructions and guidelines:\n\n");
    for file in context_files {
        prompt.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            file.path, file.content
        ));
    }
    prompt.push_str("</project_context>\n");
}

/// Build the system prompt with tools, guidelines, and context.
///
/// Mirrors pi's `buildSystemPrompt`, preserving the assembly order the tests
/// pin: preamble/custom prompt, appended text, project context, skills, then
/// the trailing working-directory line.
pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let prompt_cwd = options.cwd.replace('\\', "/");
    let append_section = options
        .append_system_prompt
        .as_ref()
        .map(|text| format!("\n\n{text}"))
        .unwrap_or_default();

    // Custom prompt short-circuits the default preamble.
    if let Some(custom_prompt) = &options.custom_prompt {
        let mut prompt = custom_prompt.clone();
        prompt.push_str(&append_section);
        append_context_files(&mut prompt, &options.context_files);

        let has_read = options
            .selected_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| t == "read"));
        if has_read {
            prompt.push_str(&format_skills_for_prompt(&options.skills));
        }

        prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));
        return prompt;
    }

    let default_tools = ["read", "bash", "edit", "write"];
    let tools: Vec<String> = options
        .selected_tools
        .clone()
        .unwrap_or_else(|| default_tools.iter().map(|s| s.to_string()).collect());

    // A tool is listed only when the caller provides a one-line snippet.
    let visible_lines: Vec<String> = tools
        .iter()
        .filter_map(|name| {
            snippet_for(&options.tool_snippets, name).map(|s| format!("- {name}: {s}"))
        })
        .collect();
    let tools_list = if visible_lines.is_empty() {
        "(none)".to_string()
    } else {
        visible_lines.join("\n")
    };

    // Guidelines, deduplicated in insertion order.
    let mut guidelines_list: Vec<String> = Vec::new();
    let mut guidelines_set: HashSet<String> = HashSet::new();
    let mut add_guideline = |guideline: String| {
        if guidelines_set.insert(guideline.clone()) {
            guidelines_list.push(guideline);
        }
    };

    let has = |name: &str| tools.iter().any(|t| t == name);
    let has_read = has("read");

    if has("bash") && !has("grep") && !has("find") && !has("ls") {
        add_guideline("Use bash for file operations like ls, rg, find".to_string());
    }

    for guideline in &options.prompt_guidelines {
        let normalized = guideline.trim();
        if !normalized.is_empty() {
            add_guideline(normalized.to_string());
        }
    }

    add_guideline("Be concise in your responses".to_string());
    add_guideline("Show file paths clearly when working with files".to_string());

    let guidelines = guidelines_list
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");

    let PiDocPaths {
        readme,
        docs,
        examples,
    } = &options.doc_paths;

    let mut prompt = format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
{tools_list}

In addition to the tools above, you may have access to other custom tools depending on the project.

Guidelines:
{guidelines}

Pi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {readme}
- Additional docs: {docs}
- Examples: {examples} (extensions, custom tools, SDK)
- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md)
- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing
- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)"
    );

    prompt.push_str(&append_section);
    append_context_files(&mut prompt, &options.context_files);

    if has_read {
        prompt.push_str(&format_skills_for_prompt(&options.skills));
    }

    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_opts() -> BuildSystemPromptOptions {
        BuildSystemPromptOptions {
            cwd: ".".to_string(),
            ..Default::default()
        }
    }

    fn snippets(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn tools(names: &[&str]) -> Option<Vec<String>> {
        Some(names.iter().map(|s| (*s).to_string()).collect())
    }

    fn assert_all_contain(prompt: &str, needles: &[&str]) {
        for needle in needles {
            assert!(
                prompt.contains(needle),
                "expected prompt to contain {needle:?}"
            );
        }
    }

    #[test]
    fn empty_tools_show_none_and_default_guideline() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: tools(&[]),
            ..base_opts()
        });
        assert_all_contain(
            &prompt,
            &["Available tools:\n(none)", "Show file paths clearly"],
        );
    }

    #[test]
    fn lists_default_tools_when_snippets_provided() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            tool_snippets: snippets(&[
                ("read", "Read file contents"),
                ("bash", "Execute bash commands"),
                ("edit", "Make surgical edits"),
                ("write", "Create or overwrite files"),
            ]),
            ..base_opts()
        });
        assert_all_contain(&prompt, &["- read:", "- bash:", "- edit:", "- write:"]);
    }

    #[test]
    fn instructs_resolving_pi_docs_under_base_paths() {
        let prompt = build_system_prompt(&base_opts());
        assert!(prompt.contains(
            "- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory"
        ));
    }

    #[test]
    fn includes_custom_tool_only_with_snippet() {
        let with_snippet = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: tools(&["read", "dynamic_tool"]),
            tool_snippets: snippets(&[("dynamic_tool", "Run dynamic test behavior")]),
            ..base_opts()
        });
        assert!(with_snippet.contains("- dynamic_tool: Run dynamic test behavior"));

        let without_snippet = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: tools(&["read", "dynamic_tool"]),
            ..base_opts()
        });
        assert!(!without_snippet.contains("dynamic_tool"));
    }

    #[test]
    fn appends_prompt_guidelines() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: tools(&["read", "dynamic_tool"]),
            prompt_guidelines: vec!["Use dynamic_tool for project summaries.".to_string()],
            ..base_opts()
        });
        assert!(prompt.contains("- Use dynamic_tool for project summaries."));
    }

    #[test]
    fn deduplicates_and_trims_prompt_guidelines() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: tools(&["read", "dynamic_tool"]),
            prompt_guidelines: vec![
                "Use dynamic_tool for summaries.".to_string(),
                "  Use dynamic_tool for summaries.  ".to_string(),
                "   ".to_string(),
            ],
            ..base_opts()
        });
        let count = prompt.matches("- Use dynamic_tool for summaries.").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn formats_visible_skills_only() {
        let skills = vec![
            SystemPromptSkill {
                name: "alpha".to_string(),
                description: "First skill".to_string(),
                file_path: "/skills/alpha/SKILL.md".to_string(),
                disable_model_invocation: false,
            },
            SystemPromptSkill {
                name: "hidden".to_string(),
                description: "Invoke-only".to_string(),
                file_path: "/skills/hidden/SKILL.md".to_string(),
                disable_model_invocation: true,
            },
        ];
        let formatted = format_skills_for_prompt(&skills);
        assert_all_contain(
            &formatted,
            &[
                "<available_skills>",
                "<name>alpha</name>",
                "<location>/skills/alpha/SKILL.md</location>",
            ],
        );
        assert!(!formatted.contains("hidden"));
        assert!(format_skills_for_prompt(&[]).is_empty());
    }
}
