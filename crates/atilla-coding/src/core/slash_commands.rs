//! Slash command metadata: sources and the built-in command list.
//!
//! Ported from pi's `core/slash-commands.ts`. This module is data plus type
//! definitions; the command *dispatch* lives elsewhere in pi and is not ported
//! here.
//!
//! NOTE: pi imports `APP_NAME` from `../config.ts` (defaulting to `"pi"`).
//! Inlined as [`APP_NAME`] until `config` is ported. [`SlashCommandInfo`]
//! reuses the [`SourceInfo`](crate::core::prompt_templates::SourceInfo) mirror
//! from `prompt_templates`.

use serde::{Deserialize, Serialize};

use crate::core::prompt_templates::SourceInfo;

/// Application name used in built-in command descriptions.
///
/// NOTE: seam for `config.ts`'s `APP_NAME` (default `"pi"`).
pub const APP_NAME: &str = "pi";

/// Where a discovered slash command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlashCommandSource {
    /// Provided by an extension.
    Extension,
    /// A user prompt template.
    Prompt,
    /// A skill.
    Skill,
}

/// A discovered (non-built-in) slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandInfo {
    /// Command name (without the leading `/`).
    pub name: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the command came from.
    pub source: SlashCommandSource,
    /// Provenance of the backing resource.
    pub source_info: SourceInfo,
}

/// A built-in slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinSlashCommand {
    /// Command name (without the leading `/`).
    pub name: &'static str,
    /// Description shown in the command menu.
    pub description: String,
    /// Optional argument hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<&'static str>,
}

impl BuiltinSlashCommand {
    fn new(
        name: &'static str,
        description: impl Into<String>,
        argument_hint: Option<&'static str>,
    ) -> Self {
        BuiltinSlashCommand {
            name,
            description: description.into(),
            argument_hint,
        }
    }
}

/// The built-in slash commands, mirroring pi's `BUILTIN_SLASH_COMMANDS`.
///
/// NOTE: pi exposes this as a module-level `const` array; because the `quit`
/// description interpolates [`APP_NAME`], the Rust equivalent is a builder
/// function rather than a `const`.
pub fn builtin_slash_commands() -> Vec<BuiltinSlashCommand> {
    vec![
        BuiltinSlashCommand::new("settings", "Open settings menu", None),
        BuiltinSlashCommand::new(
            "model",
            "Select model (opens selector UI)",
            Some("<provider/model>"),
        ),
        BuiltinSlashCommand::new(
            "scoped-models",
            "Enable/disable models for Ctrl+P cycling",
            None,
        ),
        BuiltinSlashCommand::new(
            "export",
            "Export session (HTML default, or specify path: .html/.jsonl)",
            None,
        ),
        BuiltinSlashCommand::new(
            "import",
            "Import and resume a session from a JSONL file",
            None,
        ),
        BuiltinSlashCommand::new("share", "Share session as a secret GitHub gist", None),
        BuiltinSlashCommand::new("copy", "Copy last agent message to clipboard", None),
        BuiltinSlashCommand::new("name", "Set session display name", None),
        BuiltinSlashCommand::new("session", "Show session info and stats", None),
        BuiltinSlashCommand::new("changelog", "Show changelog entries", None),
        BuiltinSlashCommand::new("hotkeys", "Show all keyboard shortcuts", None),
        BuiltinSlashCommand::new(
            "fork",
            "Create a new fork from a previous user message",
            None,
        ),
        BuiltinSlashCommand::new(
            "clone",
            "Duplicate the current session at the current position",
            None,
        ),
        BuiltinSlashCommand::new("tree", "Navigate session tree (switch branches)", None),
        BuiltinSlashCommand::new(
            "trust",
            "Save project trust decision for future sessions",
            None,
        ),
        BuiltinSlashCommand::new(
            "login",
            "Configure provider authentication",
            Some("<provider>"),
        ),
        BuiltinSlashCommand::new("logout", "Remove provider authentication", None),
        BuiltinSlashCommand::new("new", "Start a new session", None),
        BuiltinSlashCommand::new("compact", "Manually compact the session context", None),
        BuiltinSlashCommand::new("resume", "Resume a different session", None),
        BuiltinSlashCommand::new(
            "reload",
            "Reload keybindings, extensions, skills, prompts, themes, and context files",
            None,
        ),
        BuiltinSlashCommand::new("quit", format!("Quit {APP_NAME}"), None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_list_matches_pi_shape() {
        let commands = builtin_slash_commands();
        assert_eq!(commands.len(), 22);
        assert_eq!(commands.first().unwrap().name, "settings");

        let quit = commands.last().unwrap();
        assert_eq!(quit.name, "quit");
        assert_eq!(quit.description, format!("Quit {APP_NAME}"));

        let model = commands.iter().find(|c| c.name == "model").unwrap();
        assert_eq!(model.argument_hint, Some("<provider/model>"));
    }
}
