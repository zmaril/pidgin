// straitjacket-allow-file:duplication — the `#[cfg(test)]` fixtures (the
// `AssistantMessage` / `ToolResultMessage` / `Usage` builders) and the
// header-assertion blocks transcribe the same shapes the sibling
// `openai_completions`/`openai_responses` test fixtures use; the clone detector
// reads that mirrored, load-bearing scaffolding as duplication by design. This
// module is alphabetically first among the clone set.
//! GitHub Copilot dynamic request headers, ported from pi-ai's
//! `packages/ai/src/api/github-copilot-headers.ts` at pinned commit `f8f9feb`.
//!
//! A `github-copilot` request carries two header layers. The *static* layer
//! (`COPILOT_STATIC_HEADERS` in pi's model catalog: `User-Agent`,
//! `Editor-Version`, `Editor-Plugin-Version`, `Copilot-Integration-Id`) rides on
//! each copilot model's `model.headers` and flows through the assembly seams'
//! existing `model.headers` merge unchanged — it is a model-catalog concern, not
//! computed here. This module ports the *dynamic* layer pi computes per request
//! from the conversation (`buildCopilotDynamicHeaders`): the initiator, the
//! intent, and the vision flag.
//!
//! pi's `createClient` merges these over `model.headers` and under
//! `optionsHeaders` (its `Object.assign` / `mergeHeaders` order), so a caller
//! header still wins; each assembly seam injects [`build_copilot_dynamic_headers`]
//! at exactly that point.
//!
//! # Header-name casing
//!
//! pi emits `X-Initiator` / `Openai-Intent` / `Copilot-Vision-Request`; the
//! transport seam lowercases every header key (see `merge_into`), so this module
//! emits the lowercase forms directly. HTTP header names are case-insensitive and
//! the rest of the crate normalizes identically (e.g. the anthropic port's
//! `user-agent`), so this is on-the-wire faithful.

use std::collections::BTreeMap;

use crate::types::{ContentBlock, Message, UserContent};

/// pi's `inferCopilotInitiator` (`github-copilot-headers.ts:5`): a request is
/// `agent`-initiated when the last message is not a user turn — i.e. a follow-up
/// after assistant/tool messages — and `user` otherwise. An empty history is
/// `user`, mirroring pi's `last && last.role !== "user"` being falsy when `last`
/// is `undefined`.
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        None | Some(Message::User(_)) => "user",
        Some(_) => "agent",
    }
}

/// True when a content block is an image (`c.type === "image"`).
fn is_image_block(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Image { .. })
}

/// pi's `hasCopilotVisionInput` (`github-copilot-headers.ts:11`): true when any
/// user or tool-result message carries an image block. A bare-string user message
/// never matches, mirroring pi's `Array.isArray(msg.content)` guard (a
/// [`UserContent::Text`] is not a block list); assistant messages never match.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User(user) => match &user.content {
            UserContent::Blocks(blocks) => blocks.iter().any(is_image_block),
            UserContent::Text(_) => false,
        },
        Message::ToolResult(result) => result.content.iter().any(is_image_block),
        Message::Assistant(_) => false,
    })
}

/// pi's `buildCopilotDynamicHeaders` (`github-copilot-headers.ts:23`): the
/// per-request `X-Initiator` + `Openai-Intent` pair, plus `Copilot-Vision-Request`
/// when the conversation carries image input. Keys are lowercased per the module
/// note.
pub fn build_copilot_dynamic_headers(messages: &[Message]) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-initiator".to_string(),
        infer_copilot_initiator(messages).to_string(),
    );
    headers.insert(
        "openai-intent".to_string(),
        "conversation-edits".to_string(),
    );
    if has_copilot_vision_input(messages) {
        headers.insert("copilot-vision-request".to_string(), "true".to_string());
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, AssistantRole, StopReason, ToolResultMessage, ToolResultRole, Usage,
        UsageCost, UserMessage, UserRole,
    };

    fn user_text(text: &str) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp: 0,
        })
    }

    fn user_blocks(blocks: Vec<ContentBlock>) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(blocks),
            timestamp: 0,
        })
    }

    fn image_block() -> ContentBlock {
        ContentBlock::Image {
            data: "aGVsbG8=".to_string(),
            mime_type: "image/png".to_string(),
        }
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
            text_signature: None,
        }
    }

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![text_block(text)],
            api: "openai-completions".to_string(),
            provider: "github-copilot".to_string(),
            model: "gpt-5".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: zero_usage(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        })
    }

    fn tool_result(blocks: Vec<ContentBlock>) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call_1".to_string(),
            tool_name: "screenshot".to_string(),
            content: blocks,
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })
    }

    fn zero_usage() -> Usage {
        Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        }
    }

    // -- inferCopilotInitiator -------------------------------------------------

    #[test]
    fn initiator_is_user_for_empty_history() {
        assert_eq!(infer_copilot_initiator(&[]), "user");
    }

    #[test]
    fn initiator_is_user_when_last_message_is_user() {
        let messages = vec![assistant("hi"), user_text("hello")];
        assert_eq!(infer_copilot_initiator(&messages), "user");
    }

    #[test]
    fn initiator_is_agent_when_last_message_is_assistant() {
        let messages = vec![user_text("hello"), assistant("hi")];
        assert_eq!(infer_copilot_initiator(&messages), "agent");
    }

    #[test]
    fn initiator_is_agent_when_last_message_is_tool_result() {
        let messages = vec![user_text("hello"), tool_result(vec![text_block("ok")])];
        assert_eq!(infer_copilot_initiator(&messages), "agent");
    }

    // -- hasCopilotVisionInput -------------------------------------------------

    #[test]
    fn vision_false_for_text_only_conversation() {
        let messages = vec![user_text("describe"), assistant("sure")];
        assert!(!has_copilot_vision_input(&messages));
    }

    #[test]
    fn vision_false_for_bare_string_user_content() {
        // pi's `Array.isArray(msg.content)` guard: a bare-string user message is
        // never inspected for images.
        assert!(!has_copilot_vision_input(&[user_text("just text")]));
    }

    #[test]
    fn vision_true_for_user_image_block() {
        let messages = vec![user_blocks(vec![text_block("look"), image_block()])];
        assert!(has_copilot_vision_input(&messages));
    }

    #[test]
    fn vision_true_for_tool_result_image_block() {
        let messages = vec![user_text("run it"), tool_result(vec![image_block()])];
        assert!(has_copilot_vision_input(&messages));
    }

    // -- buildCopilotDynamicHeaders --------------------------------------------

    #[test]
    fn headers_without_images_carry_initiator_and_intent_only() {
        let headers = build_copilot_dynamic_headers(&[user_text("hello")]);
        assert_eq!(headers.get("x-initiator").map(String::as_str), Some("user"));
        assert_eq!(
            headers.get("openai-intent").map(String::as_str),
            Some("conversation-edits"),
        );
        assert!(!headers.contains_key("copilot-vision-request"));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn headers_with_images_add_vision_flag() {
        let messages = vec![user_blocks(vec![image_block()])];
        let headers = build_copilot_dynamic_headers(&messages);
        assert_eq!(headers.get("x-initiator").map(String::as_str), Some("user"));
        assert_eq!(
            headers.get("openai-intent").map(String::as_str),
            Some("conversation-edits"),
        );
        assert_eq!(
            headers.get("copilot-vision-request").map(String::as_str),
            Some("true"),
        );
        assert_eq!(headers.len(), 3);
    }

    #[test]
    fn headers_reflect_agent_initiator_after_tool_result() {
        let messages = vec![user_text("run it"), tool_result(vec![image_block()])];
        let headers = build_copilot_dynamic_headers(&messages);
        assert_eq!(
            headers.get("x-initiator").map(String::as_str),
            Some("agent"),
        );
        assert_eq!(
            headers.get("copilot-vision-request").map(String::as_str),
            Some("true"),
        );
    }
}
