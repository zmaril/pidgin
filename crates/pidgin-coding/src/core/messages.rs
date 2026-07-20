//! Coding-agent message types and their LLM transform.
//!
//! Ported from pi's `core/messages.ts`. The coding agent adds four message
//! roles on top of the base agent messages — bash executions (`!` command),
//! extension-injected custom messages, branch summaries, and compaction
//! summaries — and lowers all of them to plain user messages for LLM context.
//!
//! NOTE (seams):
//! - pi imports `AgentMessage` (pi-agent-core) and `Message` / `TextContent` /
//!   `ImageContent` (pi-ai). Those are unported, so this module defines the
//!   minimal mirrors it needs: [`AgentMessage`], [`LlmMessage`], [`Content`].
//!   The base roles pi passes through untouched (`user` / `assistant` /
//!   `toolResult`) are represented by the [`AgentMessage::PassThrough`]
//!   variant, since their bodies are already LLM-shaped.
//! - pi's `create*` constructors take an ISO timestamp string and call
//!   `new Date(ts).getTime()`. Parsing an ISO string is the caller/session
//!   layer's job (and would pull a datetime dependency here), so these ports
//!   take the epoch-millis value directly.

/// A single content part of a message. Minimal mirror of pi-ai's
/// `TextContent | ImageContent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// A text part.
    Text(String),
    /// An image part.
    Image(ImageContent),
}

/// An image content part. Minimal mirror of pi-ai's `ImageContent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageContent {
    /// The image MIME type (e.g. `"image/png"`).
    pub mime_type: String,
    /// Base64-encoded image data.
    pub data: String,
}

/// An LLM-compatible message. Minimal mirror of pi-ai's `Message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmMessage {
    /// The message role (`"user"`, `"assistant"`, `"toolResult"`, …).
    pub role: String,
    /// The message content parts.
    pub content: Vec<Content>,
    /// Epoch-millis timestamp.
    pub timestamp: i64,
}

impl LlmMessage {
    fn user(content: Vec<Content>, timestamp: i64) -> Self {
        LlmMessage {
            role: "user".to_string(),
            content,
            timestamp,
        }
    }

    fn user_text(text: String, timestamp: i64) -> Self {
        Self::user(vec![Content::Text(text)], timestamp)
    }
}

/// Prefix wrapping a compaction summary injected into LLM context.
pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// Suffix closing a compaction summary.
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// Prefix wrapping a branch summary injected into LLM context.
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// Suffix closing a branch summary.
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// A bash execution recorded via the `!` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashExecutionMessage {
    /// The command that was run.
    pub command: String,
    /// Captured combined output.
    pub output: String,
    /// Process exit code, if the process exited normally.
    pub exit_code: Option<i32>,
    /// Whether the command was cancelled.
    pub cancelled: bool,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Path to the full (untruncated) output, when truncated.
    pub full_output_path: Option<String>,
    /// Epoch-millis timestamp.
    pub timestamp: i64,
    /// When true, excluded from LLM context (`!!` prefix).
    pub exclude_from_context: bool,
}

/// Content of a [`CustomMessage`]: either a plain string or content parts.
/// Mirrors pi's `string | (TextContent | ImageContent)[]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomContent {
    /// A plain string body.
    Text(String),
    /// Structured content parts.
    Parts(Vec<Content>),
}

/// An extension-injected message via `sendMessage()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomMessage {
    /// Extension-defined type tag.
    pub custom_type: String,
    /// Message body.
    pub content: CustomContent,
    /// Whether to display the message in the transcript.
    pub display: bool,
    /// Opaque extension detail payload.
    pub details: Option<serde_json::Value>,
    /// Epoch-millis timestamp.
    pub timestamp: i64,
}

/// A summary of a branch this conversation returned from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummaryMessage {
    /// The summary text.
    pub summary: String,
    /// The entry id the branch originated from.
    pub from_id: String,
    /// Epoch-millis timestamp.
    pub timestamp: i64,
}

/// A summary produced by compacting earlier history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSummaryMessage {
    /// The summary text.
    pub summary: String,
    /// Token count before compaction.
    pub tokens_before: i64,
    /// Epoch-millis timestamp.
    pub timestamp: i64,
}

/// An agent message: one of the coding-agent-specific roles, or a base pi-ai
/// message passed through untouched. Mirror of pi's extended `AgentMessage`
/// union (see the module NOTE on the seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMessage {
    /// A bash execution (`!` command).
    BashExecution(BashExecutionMessage),
    /// An extension-injected custom message.
    Custom(CustomMessage),
    /// A branch summary.
    BranchSummary(BranchSummaryMessage),
    /// A compaction summary.
    CompactionSummary(CompactionSummaryMessage),
    /// A base pi-ai message (`user` / `assistant` / `toolResult`) already in
    /// LLM-compatible form; passed through by [`convert_to_llm`].
    PassThrough(LlmMessage),
}

/// Render a [`BashExecutionMessage`] as user-message text for LLM context.
/// Port of `bashExecutionToText`.
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if msg.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", msg.output));
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code {
        if code != 0 {
            text.push_str(&format!("\n\nCommand exited with code {code}"));
        }
    }
    if msg.truncated {
        if let Some(path) = &msg.full_output_path {
            text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
        }
    }
    text
}

/// Build a [`BranchSummaryMessage`]. Port of `createBranchSummaryMessage`
/// (see the module NOTE: takes epoch-millis, not an ISO string).
pub fn create_branch_summary_message(
    summary: impl Into<String>,
    from_id: impl Into<String>,
    timestamp_ms: i64,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        summary: summary.into(),
        from_id: from_id.into(),
        timestamp: timestamp_ms,
    }
}

/// Build a [`CompactionSummaryMessage`]. Port of
/// `createCompactionSummaryMessage`.
pub fn create_compaction_summary_message(
    summary: impl Into<String>,
    tokens_before: i64,
    timestamp_ms: i64,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        summary: summary.into(),
        tokens_before,
        timestamp: timestamp_ms,
    }
}

/// Build a [`CustomMessage`]. Port of `createCustomMessage`.
pub fn create_custom_message(
    custom_type: impl Into<String>,
    content: CustomContent,
    display: bool,
    details: Option<serde_json::Value>,
    timestamp_ms: i64,
) -> CustomMessage {
    CustomMessage {
        custom_type: custom_type.into(),
        content,
        display,
        details,
        timestamp: timestamp_ms,
    }
}

/// Transform agent messages (including the coding-agent-specific roles) into
/// LLM-compatible messages. Bash executions flagged `exclude_from_context`
/// (`!!` prefix) are dropped. Port of `convertToLlm`.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<LlmMessage> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::BashExecution(b) => {
                if b.exclude_from_context {
                    None
                } else {
                    Some(LlmMessage::user_text(
                        bash_execution_to_text(b),
                        b.timestamp,
                    ))
                }
            }
            AgentMessage::Custom(c) => {
                let content = match &c.content {
                    CustomContent::Text(text) => vec![Content::Text(text.clone())],
                    CustomContent::Parts(parts) => parts.clone(),
                };
                Some(LlmMessage::user(content, c.timestamp))
            }
            AgentMessage::BranchSummary(b) => Some(LlmMessage::user_text(
                format!(
                    "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                    b.summary
                ),
                b.timestamp,
            )),
            AgentMessage::CompactionSummary(c) => Some(LlmMessage::user_text(
                format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    c.summary
                ),
                c.timestamp,
            )),
            AgentMessage::PassThrough(m) => Some(m.clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_bash() -> BashExecutionMessage {
        BashExecutionMessage {
            command: "ls".to_string(),
            output: "a\nb".to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: 5,
            exclude_from_context: false,
        }
    }

    #[test]
    fn bash_text_wraps_output_in_a_fence() {
        let text = bash_execution_to_text(&base_bash());
        assert_eq!(text, "Ran `ls`\n```\na\nb\n```");
    }

    #[test]
    fn bash_text_reports_no_output() {
        let msg = BashExecutionMessage {
            output: String::new(),
            ..base_bash()
        };
        assert_eq!(bash_execution_to_text(&msg), "Ran `ls`\n(no output)");
    }

    #[test]
    fn bash_text_reports_nonzero_exit() {
        let msg = BashExecutionMessage {
            exit_code: Some(2),
            ..base_bash()
        };
        assert!(bash_execution_to_text(&msg).ends_with("Command exited with code 2"));
    }

    #[test]
    fn bash_text_omits_exit_note_when_missing_or_zero() {
        let zero = bash_execution_to_text(&base_bash());
        assert!(!zero.contains("Command exited"));
        let none = bash_execution_to_text(&BashExecutionMessage {
            exit_code: None,
            ..base_bash()
        });
        assert!(!none.contains("Command exited"));
    }

    #[test]
    fn bash_text_prefers_cancelled_note_over_exit_code() {
        let msg = BashExecutionMessage {
            cancelled: true,
            exit_code: Some(2),
            ..base_bash()
        };
        let text = bash_execution_to_text(&msg);
        assert!(text.ends_with("(command cancelled)"));
        assert!(!text.contains("Command exited"));
    }

    #[test]
    fn bash_text_appends_truncation_pointer() {
        let msg = BashExecutionMessage {
            truncated: true,
            full_output_path: Some("/tmp/out.txt".to_string()),
            ..base_bash()
        };
        assert!(
            bash_execution_to_text(&msg).ends_with("[Output truncated. Full output: /tmp/out.txt]")
        );
    }

    #[test]
    fn bash_text_skips_truncation_pointer_without_path() {
        let msg = BashExecutionMessage {
            truncated: true,
            full_output_path: None,
            ..base_bash()
        };
        assert!(!bash_execution_to_text(&msg).contains("Output truncated"));
    }

    #[test]
    fn convert_drops_excluded_bash_executions() {
        let messages = vec![
            AgentMessage::BashExecution(BashExecutionMessage {
                exclude_from_context: true,
                ..base_bash()
            }),
            AgentMessage::BashExecution(base_bash()),
        ];
        let out = convert_to_llm(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].timestamp, 5);
    }

    #[test]
    fn convert_wraps_branch_and_compaction_summaries() {
        let messages = vec![
            AgentMessage::BranchSummary(create_branch_summary_message("did X", "id-1", 7)),
            AgentMessage::CompactionSummary(create_compaction_summary_message("history", 42, 8)),
        ];
        let out = convert_to_llm(&messages);
        assert_eq!(
            out[0].content,
            vec![Content::Text(format!(
                "{BRANCH_SUMMARY_PREFIX}did X{BRANCH_SUMMARY_SUFFIX}"
            ))]
        );
        assert_eq!(
            out[1].content,
            vec![Content::Text(format!(
                "{COMPACTION_SUMMARY_PREFIX}history{COMPACTION_SUMMARY_SUFFIX}"
            ))]
        );
    }

    #[test]
    fn convert_lifts_string_custom_content_to_a_text_part() {
        let msg = create_custom_message(
            "note",
            CustomContent::Text("hello".to_string()),
            true,
            None,
            9,
        );
        let out = convert_to_llm(&[AgentMessage::Custom(msg)]);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content, vec![Content::Text("hello".to_string())]);
    }

    #[test]
    fn convert_preserves_structured_custom_parts() {
        let parts = vec![
            Content::Text("look".to_string()),
            Content::Image(ImageContent {
                mime_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            }),
        ];
        let msg =
            create_custom_message("shot", CustomContent::Parts(parts.clone()), true, None, 10);
        let out = convert_to_llm(&[AgentMessage::Custom(msg)]);
        assert_eq!(out[0].content, parts);
    }

    #[test]
    fn convert_passes_base_messages_through_unchanged() {
        let base = LlmMessage {
            role: "assistant".to_string(),
            content: vec![Content::Text("hi".to_string())],
            timestamp: 3,
        };
        let out = convert_to_llm(&[AgentMessage::PassThrough(base.clone())]);
        assert_eq!(out, vec![base]);
    }
}
