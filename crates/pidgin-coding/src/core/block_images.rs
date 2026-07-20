//! The `blockImages` message-conversion filter, ported from pi's
//! `convertToLlmWithBlockImages` (`packages/coding-agent/src/core/sdk.ts:250-285`).
//!
//! pi wraps the base `convertToLlm` in a closure that, when
//! `settingsManager.getBlockImages()` is enabled, replaces every `image` content
//! block in the CONVERTED output with a `{ type: "text", text: "Image reading is
//! disabled." }` placeholder, then dedupes CONSECUTIVE identical placeholders.
//! The setting is read live per call so a mid-session toggle takes effect.
//!
//! # Why this operates on the converted output
//!
//! The Agent's [`pidgin_agent::types::ConvertToLlm`] seam is
//! `Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync`: it already yields the
//! LLM-facing [`pidgin_ai::Message`] list. Running the filter there — after the
//! base [`pidgin_agent::harness::messages::convert_to_llm`] — keeps this a pure
//! transform on real `Message` values and avoids bridging the coding crate's
//! typed mirror messages (`core::messages`). Assistant messages are never
//! touched (pi filters only `user` / `toolResult`), matching the fact that the
//! model never authors image blocks.
//!
//! # Live setting without capturing the `!Send` SettingsManager
//!
//! [`SettingsManager`](crate::core::settings_manager::SettingsManager) is
//! `!Send`, so it cannot be captured by the `Send + Sync` converter closure. The
//! manager instead exposes a shared `Arc<AtomicBool>` mirror
//! (`block_images_flag`) that it keeps in sync with `getBlockImages()`; the
//! closure reads that atomic live, so `setBlockImages` mid-session is observed on
//! the very next conversion — matching pi's per-call `getBlockImages()` read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pidgin_agent::harness::messages::convert_to_llm;
use pidgin_agent::types::{AgentMessage, ConvertToLlm};
use pidgin_ai::{ContentBlock, Message, UserContent};

/// The placeholder text pi substitutes for every blocked image
/// (`sdk.ts:266`).
pub(crate) const IMAGE_DISABLED_TEXT: &str = "Image reading is disabled.";

/// Build the `convertToLlm` wrapper (pi's `convertToLlmWithBlockImages`,
/// `sdk.ts:251-285`).
///
/// Runs the base [`convert_to_llm`] and, when `block_images` is set, applies
/// [`filter_block_images`] to the converted output. The atomic is read live so a
/// mid-session `setBlockImages` toggle takes effect on the next call, mirroring
/// pi's per-call `settingsManager.getBlockImages()`.
pub(crate) fn block_images_converter(block_images: Arc<AtomicBool>) -> ConvertToLlm {
    Arc::new(move |messages: &[AgentMessage]| {
        let converted = convert_to_llm(messages);
        if block_images.load(Ordering::Relaxed) {
            filter_block_images(converted)
        } else {
            converted
        }
    })
}

/// Replace image blocks with the [`IMAGE_DISABLED_TEXT`] placeholder across every
/// `user` / `toolResult` message and dedupe consecutive placeholders (pi's
/// `converted.map(...)`, `sdk.ts:258-284`). `assistant` messages and string-form
/// user content pass through untouched (pi only rewrites array content on
/// `user` / `toolResult`).
pub(crate) fn filter_block_images(messages: Vec<Message>) -> Vec<Message> {
    messages.into_iter().map(filter_message).collect()
}

/// Apply the image filter to a single message. `user` array content and
/// `toolResult` content are rewritten; everything else is returned as-is.
fn filter_message(message: Message) -> Message {
    match message {
        Message::User(mut user) => {
            if let UserContent::Blocks(blocks) = &user.content {
                if let Some(filtered) = filter_blocks(blocks) {
                    user.content = UserContent::Blocks(filtered);
                }
            }
            Message::User(user)
        }
        Message::ToolResult(mut tool_result) => {
            if let Some(filtered) = filter_blocks(&tool_result.content) {
                tool_result.content = filtered;
            }
            Message::ToolResult(tool_result)
        }
        // pi leaves `assistant` messages untouched.
        other => other,
    }
}

/// Rewrite a content-block list, or `None` when it holds no images (pi skips the
/// rebuild via `hasImages`, `sdk.ts:262-263`, returning the message unchanged).
///
/// When images are present, each image becomes an [`IMAGE_DISABLED_TEXT`] text
/// block (`sdk.ts:265-267`); the result is then deduped so a run of consecutive
/// placeholders collapses to one (`sdk.ts:268-278`). The dedup compares against
/// the *mapped* predecessor, exactly as pi's `.filter((c, i, arr) => ...)`
/// inspects `arr[i - 1]` of the post-`map` array.
fn filter_blocks(blocks: &[ContentBlock]) -> Option<Vec<ContentBlock>> {
    if !blocks.iter().any(is_image) {
        return None;
    }
    let mapped: Vec<ContentBlock> = blocks
        .iter()
        .map(|block| {
            if is_image(block) {
                placeholder_block()
            } else {
                block.clone()
            }
        })
        .collect();
    let deduped = mapped
        .iter()
        .enumerate()
        .filter(|(index, block)| {
            !(is_placeholder(block) && *index > 0 && is_placeholder(&mapped[index - 1]))
        })
        .map(|(_, block)| block.clone())
        .collect();
    Some(deduped)
}

/// Whether a block is an image (pi's `c.type === "image"`).
fn is_image(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Image { .. })
}

/// The placeholder text block pi substitutes for a blocked image.
fn placeholder_block() -> ContentBlock {
    ContentBlock::Text {
        text: IMAGE_DISABLED_TEXT.to_string(),
        text_signature: None,
    }
}

/// Whether a block is the [`IMAGE_DISABLED_TEXT`] placeholder (pi's dedup guard,
/// `c.type === "text" && c.text === "Image reading is disabled."`).
fn is_placeholder(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Text { text, .. } if text == IMAGE_DISABLED_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    use pidgin_ai::{
        AssistantMessage, AssistantRole, StopReason, ToolResultMessage, ToolResultRole, Usage,
        UsageCost, UserMessage, UserRole,
    };

    fn image() -> ContentBlock {
        ContentBlock::Image {
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
        }
    }

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_string(),
            text_signature: None,
        }
    }

    fn user(content: UserContent) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content,
            timestamp: 0,
        })
    }

    fn tool_result(content: Vec<ContentBlock>) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content,
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })
    }

    fn assistant(content: Vec<ContentBlock>) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: "chat".to_string(),
            provider: "faux".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0,
                cost: UsageCost::default(),
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        })
    }

    fn blocks_of(message: &Message) -> &[ContentBlock] {
        match message {
            Message::User(UserMessage {
                content: UserContent::Blocks(blocks),
                ..
            }) => blocks,
            Message::ToolResult(ToolResultMessage { content, .. }) => content,
            _ => panic!("expected block content"),
        }
    }

    /// A converter over a fresh flag initialized to `block`, plus a one-message
    /// `AgentMessage` fixture whose user content is `content` (raw JSON blocks).
    fn converter_fixture(
        block: bool,
        content: serde_json::Value,
    ) -> (Arc<AtomicBool>, ConvertToLlm, Vec<AgentMessage>) {
        let flag = Arc::new(AtomicBool::new(block));
        let converter = block_images_converter(Arc::clone(&flag));
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": content,
            "timestamp": 0,
        })];
        (flag, converter, messages)
    }

    fn text_and_image_content() -> serde_json::Value {
        serde_json::json!([
            { "type": "text", "text": "hello" },
            { "type": "image", "data": "AAAA", "mimeType": "image/png" },
        ])
    }

    // pi `sdk.ts:265-267`: an image block in a user message becomes the exact
    // placeholder text.
    #[test]
    fn block_on_replaces_user_image_with_exact_text() {
        let out = filter_block_images(vec![user(UserContent::Blocks(vec![
            text("look at this"),
            image(),
        ]))]);
        assert_eq!(
            blocks_of(&out[0]),
            &[text("look at this"), text(IMAGE_DISABLED_TEXT)]
        );
    }

    // The same replacement fires for tool-result content.
    #[test]
    fn block_on_replaces_tool_result_image_with_exact_text() {
        let out = filter_block_images(vec![tool_result(vec![text("note"), image()])]);
        assert_eq!(
            blocks_of(&out[0]),
            &[text("note"), text(IMAGE_DISABLED_TEXT)]
        );
    }

    // pi `sdk.ts:268-278`: consecutive placeholders collapse to a single one,
    // while a non-placeholder between two images is preserved.
    #[test]
    fn block_on_dedupes_consecutive_placeholders() {
        let out = filter_block_images(vec![tool_result(vec![
            image(),
            image(),
            text("between"),
            image(),
        ])]);
        assert_eq!(
            blocks_of(&out[0]),
            &[
                text(IMAGE_DISABLED_TEXT),
                text("between"),
                text(IMAGE_DISABLED_TEXT),
            ]
        );
    }

    // An existing placeholder immediately before a converted image is also
    // deduped, since the dedup compares the mapped predecessor.
    #[test]
    fn block_on_dedupes_across_preexisting_placeholder() {
        let out = filter_block_images(vec![tool_result(vec![text(IMAGE_DISABLED_TEXT), image()])]);
        assert_eq!(blocks_of(&out[0]), &[text(IMAGE_DISABLED_TEXT)]);
    }

    // pi's wrapper only rewrites `user` / `toolResult`; an assistant message is
    // returned unchanged even if it (hypothetically) carried an image.
    #[test]
    fn block_on_leaves_assistant_messages_untouched() {
        let original = assistant(vec![text("answer"), image()]);
        let out = filter_block_images(vec![original.clone()]);
        assert_eq!(out[0], original);
    }

    // pi's `Array.isArray(content)` guard: string-form user content is not an
    // array, so it passes through untouched.
    #[test]
    fn block_on_leaves_string_user_content_untouched() {
        let original = user(UserContent::Text("just text".to_string()));
        let out = filter_block_images(vec![original.clone()]);
        assert_eq!(out[0], original);
    }

    // pi `sdk.ts:262-263`: a message with no images is returned as-is (the
    // `hasImages` short-circuit), leaving an existing lone placeholder alone.
    #[test]
    fn block_on_passes_through_messages_without_images() {
        let original = tool_result(vec![text("plain"), text(IMAGE_DISABLED_TEXT)]);
        let out = filter_block_images(vec![original.clone()]);
        assert_eq!(out[0], original);
    }

    // pi `sdk.ts:254-256`: when the setting is off the converter returns the base
    // conversion unchanged. The `block_images_converter` off path is a pure
    // passthrough of `convert_to_llm`.
    #[test]
    fn converter_off_passes_conversion_through_unchanged() {
        let (_flag, converter, messages) = converter_fixture(false, text_and_image_content());
        let out = converter(&messages);
        assert_eq!(
            blocks_of(&out[0]),
            &[text("hello"), image()],
            "off path must not touch images"
        );
    }

    // pi `sdk.ts:251-285` end to end: with the setting on, the converter runs the
    // base conversion and then blocks images.
    #[test]
    fn converter_on_blocks_images_in_converted_output() {
        let (_flag, converter, messages) = converter_fixture(true, text_and_image_content());
        let out = converter(&messages);
        assert_eq!(
            blocks_of(&out[0]),
            &[text("hello"), text(IMAGE_DISABLED_TEXT)]
        );
    }

    // pi reads `getBlockImages()` live per call (`sdk.ts:253-254`): flipping the
    // shared atomic between calls flips the behavior with no rebuild.
    #[test]
    fn converter_reads_the_flag_live_per_call() {
        let (flag, converter, messages) = converter_fixture(
            false,
            serde_json::json!([{ "type": "image", "data": "AAAA", "mimeType": "image/png" }]),
        );

        assert_eq!(blocks_of(&converter(&messages)[0]), &[image()]);

        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            blocks_of(&converter(&messages)[0]),
            &[text(IMAGE_DISABLED_TEXT)]
        );

        flag.store(false, Ordering::Relaxed);
        assert_eq!(blocks_of(&converter(&messages)[0]), &[image()]);
    }
}
