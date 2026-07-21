//! The extension-facing turn behaviors, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts`).
//!
//! This module carries the parts of a prompt turn that route through the
//! extension runner, on top of the turn spine in [`super::turn`]:
//!
//! * [`AgentSession::try_execute_extension_command`] — the `/`-command shortcut
//!   (pi `_tryExecuteExtensionCommand`, L1258): resolve a registered command,
//!   build a command context, run the handler, isolate errors to the runner.
//! * [`AgentSession::expand_prompt_input`] / [`AgentSession::expand_skill_command`]
//!   — skill-command (`/skill:name args`) and prompt-template (`/template args`)
//!   expansion (pi `_expandSkillCommand`, L1289, plus `expandPromptTemplate`).
//! * [`AgentSession::apply_before_agent_start`] — inject the custom messages an
//!   extension `before_agent_start` handler returns and apply / reset the
//!   per-turn system-prompt override (pi L1218-1241).
//!
//! The four `bindCore` host-trait impls live in the sibling [`super::host`]
//! module. Source of truth:
//! `vendor/pi/packages/coding-agent/src/core/agent-session.ts`.

use std::fs;

use serde_json::{json, Value};

use pidgin_agent::types::AgentMessage;

use crate::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use crate::core::prompt_templates::expand_prompt_template;
use crate::utils::frontmatter::strip_frontmatter;

use super::session::AgentSession;
use super::turn::now_ms;

impl AgentSession {
    /// Try to execute an extension command (pi's `_tryExecuteExtensionCommand`,
    /// L1258). Returns `true` when a command was found and dispatched (whether it
    /// succeeded or its handler threw — a thrown error is reported via the runner
    /// and the command is still considered handled), `false` when no command with
    /// that name is registered.
    ///
    /// `text` is the raw prompt including the leading `/`. The command name is the
    /// text up to the first space; everything after is the raw argument string.
    pub(super) fn try_execute_extension_command(&self, text: &str) -> bool {
        let without_slash = &text[1..];
        let (command_name, args) = match without_slash.split_once(' ') {
            Some((name, args)) => (name, args),
            None => (without_slash, ""),
        };

        let Some(command) = self.extension_runner().get_command(command_name) else {
            return false;
        };

        // The command context carries the session-control methods bound through
        // `bind_core` (pi `createCommandContext`).
        let ctx = self.extension_runner().create_command_context();
        if let Err(error) = (command.command.handler)(args, ctx.as_ref()) {
            self.extension_runner().emit_error(ExtensionError {
                extension_path: format!("command:{command_name}"),
                event: "command".to_string(),
                error: error.to_string(),
                stack: None,
            });
        }
        true
    }

    /// Expand skill commands and prompt templates in `text` (pi L1141-1143): first
    /// `_expandSkillCommand`, then `expandPromptTemplate` over the loaded prompt
    /// templates. Non-command text passes through unchanged.
    pub(super) fn expand_prompt_input(&self, text: &str) -> String {
        let expanded = self.expand_skill_command(text);
        let templates = self.resource_loader.get_prompts().prompts;
        expand_prompt_template(&expanded, &templates)
    }

    /// Expand a `/skill:name args` command to its full content (pi's
    /// `_expandSkillCommand`, L1289).
    ///
    /// Returns the original text when it is not a skill command or the skill is
    /// unknown. On a read failure the error is reported via the runner and the
    /// original text is returned.
    pub(super) fn expand_skill_command(&self, text: &str) -> String {
        let Some(after_prefix) = text.strip_prefix("/skill:") else {
            return text.to_string();
        };
        let (skill_name, args) = match after_prefix.split_once(' ') {
            Some((name, args)) => (name, args.trim()),
            None => (after_prefix, ""),
        };

        let skills = self.resource_loader.get_skills().skills;
        let Some(skill) = skills.iter().find(|skill| skill.name == skill_name) else {
            return text.to_string(); // Unknown skill, pass through.
        };

        match fs::read_to_string(&skill.file_path) {
            Ok(content) => {
                let body = strip_frontmatter(&content);
                let body = body.trim();
                let skill_block = format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
                    skill.name, skill.file_path, skill.base_dir, body
                );
                if args.is_empty() {
                    skill_block
                } else {
                    format!("{skill_block}\n\n{args}")
                }
            }
            Err(error) => {
                self.extension_runner().emit_error(ExtensionError {
                    extension_path: skill.file_path.clone(),
                    event: "skill_expansion".to_string(),
                    error: error.to_string(),
                    stack: None,
                });
                text.to_string()
            }
        }
    }

    /// Apply the result of a `before_agent_start` emit (pi L1218-1241): append any
    /// custom messages the handler injected onto `messages`, then set or reset the
    /// per-turn system-prompt override on both the session state and the agent.
    ///
    /// A returned `system_prompt` becomes the override for this turn; its absence
    /// restores the base prompt (undoing any override left by a prior turn).
    pub(super) fn apply_before_agent_start(
        &self,
        result: Option<BeforeAgentStartCombinedResult>,
        base_system_prompt: &str,
        messages: &mut Vec<AgentMessage>,
    ) {
        let (custom_messages, system_prompt) = match result {
            Some(result) => (result.messages, result.system_prompt),
            None => (None, None),
        };

        if let Some(custom_messages) = custom_messages {
            for message in custom_messages {
                messages.push(json!({
                    "role": "custom",
                    "customType": message.get("customType").cloned().unwrap_or(Value::Null),
                    // Untyped extensions can pass null/missing content; normalize.
                    "content": message.get("content").cloned().unwrap_or_else(|| json!([])),
                    "display": message.get("display").cloned().unwrap_or(Value::Null),
                    "details": message.get("details").cloned().unwrap_or(Value::Null),
                    "timestamp": now_ms(),
                }));
            }
        }

        match system_prompt {
            Some(system_prompt) => {
                *self.system_prompt_override.lock().unwrap() = Some(system_prompt.clone());
                self.agent.set_system_prompt(system_prompt);
            }
            None => {
                *self.system_prompt_override.lock().unwrap() = None;
                self.agent.set_system_prompt(base_system_prompt.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests;
