//! Pure result-shaping folds for extension hook dispatch — the language-agnostic
//! heart of pi's `ExtensionRunner.emit*` methods (`runner.ts`).
//!
//! pi's runner interleaves two concerns in each `emitXxx`: (1) the off-thread
//! call into a handler closure and (2) the *shaping* of that handler's result
//! into the accumulated outcome (chain / merge / short-circuit / replace). This
//! module isolates concern (2) as a set of small, deterministic **fold state
//! machines** that carry no runtime and no `deno_core` — so the tricky merge
//! rules are unit-tested here, in the default (V8-free) build, exactly mirroring
//! `runner.ts`.
//!
//! The JS half (concern 1) lives in `atilla-extensions`' `ExtensionRunner`: it
//! runs each registered JS handler over the `Affinity::OwnRuntime` rendezvous,
//! deserializes the shaped JSON result into the typed result structs here, and
//! feeds it to the matching fold. The fold decides what the accumulated outcome
//! is and (for `input`) whether to short-circuit the remaining handlers.
//!
//! Each fold mirrors one `emitXxx` in `runner.ts`:
//!
//! | fold | runner.ts method | shaping rule |
//! |------|------------------|--------------|
//! | [`InputFold`] | `emitInput` (`runner.ts:1174`) | chain transforms; `handled` short-circuits |
//! | [`ToolResultFold`] | `emitToolResult` (`runner.ts:860`) | merge partial `content`/`details`/`isError` patches |
//! | [`BeforeAgentStartFold`] | `emitBeforeAgentStart` (`runner.ts:1059`) | chain `systemPrompt`, collect `message`s |
//! | [`ContextFold`] | `emitContext` (`runner.ts:962`) | replace the message array |
//! | [`ProjectTrustFold`] | `emitProjectTrustEvent` (`runner.ts:201`) | skip `undecided`; first `yes`/`no` decision wins |
//!
//! `before_provider_headers` is not a fold: its handlers mutate the headers
//! object in place and their return value is ignored (`runner.ts:1028`), so the
//! `ExtensionRunner` threads the headers value directly ([`next_headers`]).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/runner.ts`.

// straitjacket-allow-file:duplication -- each fold is a deliberate parallel
// mirror of one emitXxx method's shaping block in runner.ts; the accumulate /
// merge / finish shape recurs per hook, so it is faithful-port duplication.

use serde::{Deserialize, Serialize};

use super::events::common::{AgentMessage, CustomMessage, ImageContent, ProviderHeaders};
use super::events::{
    BeforeAgentStartEventResult, ContextEventResult, InputEventResult, ProjectTrustEventDecision,
    ProjectTrustEventResult, ToolResultContent, ToolResultEventResult,
};

use serde_json::Value;

/// An error surfaced from a hook handler that threw (pi's `ExtensionError`, the
/// object passed to `emitError` / `onError`, `runner.ts:805`).
///
/// The `ExtensionRunner` isolates a thrown handler into one of these and routes
/// it to the registered `onError` listeners; dispatch of the remaining handlers
/// continues (advisory hooks fail open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionError {
    /// The entrypoint path of the extension whose handler threw.
    pub extension_path: String,
    /// The snake_case event name that was being dispatched (`"input"`, …).
    pub event: String,
    /// The thrown error's message.
    pub error: String,
    /// The thrown error's stack, when the handler threw an `Error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// The combined result of `emitBeforeAgentStart` (pi's
/// `BeforeAgentStartCombinedResult`, `runner.ts:1116`).
///
/// Distinct from the per-handler [`BeforeAgentStartEventResult`]: it carries the
/// collected injected messages and the final chained system prompt, each present
/// only when at least one handler contributed it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartCombinedResult {
    /// The injected custom messages, in handler order; `None` when none injected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<CustomMessage>>,
    /// The final chained system prompt; `None` when no handler modified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// Shaping fold for `emitInput` (`runner.ts:1174`): transforms chain across
/// handlers and a `handled` result short-circuits the rest.
#[derive(Debug, Clone)]
pub struct InputFold {
    orig_text: String,
    orig_images: Option<Vec<ImageContent>>,
    current_text: String,
    current_images: Option<Vec<ImageContent>>,
    handled: bool,
}

impl InputFold {
    /// Start a fold from the initial input `text` and `images`.
    pub fn new(text: impl Into<String>, images: Option<Vec<ImageContent>>) -> Self {
        let text = text.into();
        Self {
            orig_text: text.clone(),
            orig_images: images.clone(),
            current_text: text,
            current_images: images,
            handled: false,
        }
    }

    /// The current input text — what the next handler's `event.text` carries.
    pub fn current_text(&self) -> &str {
        &self.current_text
    }

    /// The current input images — what the next handler's `event.images` carries.
    pub fn current_images(&self) -> Option<&Vec<ImageContent>> {
        self.current_images.as_ref()
    }

    /// Fold one handler's result in. Returns `true` when the input was `handled`
    /// and the remaining handlers must be skipped (pi's early `return result`).
    pub fn apply(&mut self, result: Option<InputEventResult>) -> bool {
        match result {
            Some(InputEventResult::Handled) => {
                self.handled = true;
                true
            }
            Some(InputEventResult::Transform { text, images }) => {
                self.current_text = text;
                // `result.images ?? currentImages`: only replace when supplied.
                if let Some(images) = images {
                    self.current_images = Some(images);
                }
                false
            }
            // `continue` or no result: leave the accumulated state unchanged.
            Some(InputEventResult::Continue) | None => false,
        }
    }

    /// Finish the fold into the `emitInput` return value: `handled` short-circuits
    /// to itself, an unchanged input yields `continue`, otherwise a `transform`
    /// carrying the chained text/images.
    pub fn finish(self) -> InputEventResult {
        if self.handled {
            return InputEventResult::Handled;
        }
        if self.current_text != self.orig_text || self.current_images != self.orig_images {
            InputEventResult::Transform {
                text: self.current_text,
                images: self.current_images,
            }
        } else {
            InputEventResult::Continue
        }
    }
}

/// Shaping fold for `emitToolResult` (`runner.ts:860`): each handler returns a
/// partial patch and the fold merges `content` / `details` / `isError`, so a
/// later handler that patches only one field preserves the others.
#[derive(Debug, Clone)]
pub struct ToolResultFold {
    content: Vec<ToolResultContent>,
    details: Value,
    is_error: bool,
    modified: bool,
}

impl ToolResultFold {
    /// Start a fold from the tool result's initial `content` / `details` /
    /// `is_error` (the `currentEvent` seed in pi, `runner.ts:862`).
    pub fn new(content: Vec<ToolResultContent>, details: Value, is_error: bool) -> Self {
        Self {
            content,
            details,
            is_error,
            modified: false,
        }
    }

    /// The current content — what the next handler's `event.content` carries.
    pub fn content(&self) -> &[ToolResultContent] {
        &self.content
    }

    /// The current details — what the next handler's `event.details` carries.
    pub fn details(&self) -> &Value {
        &self.details
    }

    /// The current error flag — what the next handler's `event.isError` carries.
    pub fn is_error(&self) -> bool {
        self.is_error
    }

    /// Merge one handler's partial patch in (only present fields overwrite).
    pub fn apply(&mut self, patch: Option<ToolResultEventResult>) {
        let Some(patch) = patch else { return };
        if let Some(content) = patch.content {
            self.content = content;
            self.modified = true;
        }
        if let Some(details) = patch.details {
            self.details = details;
            self.modified = true;
        }
        if let Some(is_error) = patch.is_error {
            self.is_error = is_error;
            self.modified = true;
        }
    }

    /// Finish into the `emitToolResult` return value: `None` when no handler
    /// modified anything, otherwise the merged `{content, details, isError}`.
    pub fn finish(self) -> Option<ToolResultEventResult> {
        if !self.modified {
            return None;
        }
        Some(ToolResultEventResult {
            content: Some(self.content),
            details: Some(self.details),
            is_error: Some(self.is_error),
        })
    }
}

/// Shaping fold for `emitBeforeAgentStart` (`runner.ts:1059`): the system prompt
/// chains across handlers (each sees the running value via `ctx.getSystemPrompt()`
/// and `event.systemPrompt`) and injected messages are collected in order.
#[derive(Debug, Clone)]
pub struct BeforeAgentStartFold {
    current_system_prompt: String,
    messages: Vec<CustomMessage>,
    system_prompt_modified: bool,
}

impl BeforeAgentStartFold {
    /// Start a fold from the base system prompt.
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            current_system_prompt: system_prompt.into(),
            messages: Vec::new(),
            system_prompt_modified: false,
        }
    }

    /// The running system prompt — what both `ctx.getSystemPrompt()` and the next
    /// handler's `event.systemPrompt` must report (pi keeps them in sync).
    pub fn current_system_prompt(&self) -> &str {
        &self.current_system_prompt
    }

    /// Fold one handler's result in: push any injected message, and chain the
    /// system prompt when the handler returned a new one.
    pub fn apply(&mut self, result: Option<BeforeAgentStartEventResult>) {
        let Some(result) = result else { return };
        if let Some(message) = result.message {
            self.messages.push(message);
        }
        if let Some(system_prompt) = result.system_prompt {
            self.current_system_prompt = system_prompt;
            self.system_prompt_modified = true;
        }
    }

    /// Finish into the combined result, or `None` when no handler contributed a
    /// message or a system-prompt change.
    pub fn finish(self) -> Option<BeforeAgentStartCombinedResult> {
        if self.messages.is_empty() && !self.system_prompt_modified {
            return None;
        }
        Some(BeforeAgentStartCombinedResult {
            messages: if self.messages.is_empty() {
                None
            } else {
                Some(self.messages)
            },
            system_prompt: if self.system_prompt_modified {
                Some(self.current_system_prompt)
            } else {
                None
            },
        })
    }
}

/// Shaping fold for `emitContext` (`runner.ts:962`): each handler may replace the
/// message array wholesale, and the next handler sees the replacement.
#[derive(Debug, Clone)]
pub struct ContextFold {
    current_messages: Vec<AgentMessage>,
}

impl ContextFold {
    /// Start a fold from the initial message array.
    pub fn new(messages: Vec<AgentMessage>) -> Self {
        Self {
            current_messages: messages,
        }
    }

    /// The current messages — what the next handler's `event.messages` carries.
    pub fn current_messages(&self) -> &[AgentMessage] {
        &self.current_messages
    }

    /// Fold one handler's result in: replace the array when the handler returned
    /// a non-empty `messages` (pi's `if (result.messages)` truthiness).
    pub fn apply(&mut self, result: Option<ContextEventResult>) {
        if let Some(ContextEventResult {
            messages: Some(messages),
        }) = result
        {
            self.current_messages = messages;
        }
    }

    /// Finish into the final message array.
    pub fn finish(self) -> Vec<AgentMessage> {
        self.current_messages
    }
}

/// Shaping fold for `emitProjectTrustEvent` (`runner.ts:201`): each handler
/// returns a `{trusted, remember}` decision, an `undecided` result is skipped and
/// the fold falls through to the next handler, and the first `yes`/`no` decision
/// wins and short-circuits the rest. When no handler decides, the fold yields
/// `None` (pi returns `{ errors }` with no `result`, and the caller applies its
/// own default).
#[derive(Debug, Clone, Default)]
pub struct ProjectTrustFold {
    decision: Option<ProjectTrustEventResult>,
}

impl ProjectTrustFold {
    /// Start an undecided fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one handler's result in. Returns `true` when a decisive `yes`/`no`
    /// was reached and the remaining handlers must be skipped (pi's early
    /// `return { result, errors }`). An `undecided` result — or no result (the
    /// JS `undefined` a handler returns) — falls through (`continue`).
    pub fn apply(&mut self, result: Option<ProjectTrustEventResult>) -> bool {
        match result {
            Some(result) if result.trusted != ProjectTrustEventDecision::Undecided => {
                self.decision = Some(result);
                true
            }
            // `undecided` (pi's `continue`) or no result: fall through.
            _ => false,
        }
    }

    /// Finish into the decisive `{trusted, remember}`, or `None` when no handler
    /// reached a `yes`/`no` decision.
    pub fn finish(self) -> Option<ProjectTrustEventResult> {
        self.decision
    }
}

/// Thread one `before_provider_headers` handler's in-place mutation through
/// (`runner.ts:1028`). The handler receives the current headers as
/// `event.headers`, mutates them, and its return value is ignored — so the
/// runner simply carries whatever headers object the handler produced forward.
///
/// This is a free function rather than a fold because there is no accumulation
/// beyond passing the (possibly mutated) headers to the next handler.
pub fn next_headers(mutated_event_headers: ProviderHeaders) -> ProviderHeaders {
    mutated_event_headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- InputFold (emitInput) -----------------------------------------

    #[test]
    fn input_no_handlers_continues() {
        let fold = InputFold::new("x", None);
        assert_eq!(fold.finish(), InputEventResult::Continue);
    }

    #[test]
    fn input_undefined_and_explicit_continue_stay_continue() {
        let mut fold = InputFold::new("x", None);
        assert!(!fold.apply(None));
        assert!(!fold.apply(Some(InputEventResult::Continue)));
        assert_eq!(fold.finish(), InputEventResult::Continue);
    }

    #[test]
    fn input_transform_preserves_images_when_omitted() {
        let imgs = vec![json!({ "type": "image", "data": "orig", "mimeType": "image/png" })];
        let mut fold = InputFold::new("hi", Some(imgs.clone()));
        // Handler: { action: "transform", text: "T:" + e.text }
        assert!(!fold.apply(Some(InputEventResult::Transform {
            text: format!("T:{}", fold.current_text()),
            images: None,
        })));
        assert_eq!(
            fold.finish(),
            InputEventResult::Transform {
                text: "T:hi".into(),
                images: Some(imgs),
            }
        );
    }

    #[test]
    fn input_transform_replaces_images_when_provided() {
        let orig = vec![json!({ "type": "image", "data": "orig", "mimeType": "image/png" })];
        let new = vec![json!({ "type": "image", "data": "new", "mimeType": "image/jpeg" })];
        let mut fold = InputFold::new("hi", Some(orig));
        assert!(!fold.apply(Some(InputEventResult::Transform {
            text: "X".into(),
            images: Some(new.clone()),
        })));
        assert_eq!(
            fold.finish(),
            InputEventResult::Transform {
                text: "X".into(),
                images: Some(new),
            }
        );
    }

    #[test]
    fn input_chains_transforms_across_handlers() {
        let mut fold = InputFold::new("X", None);
        // e.text + "[1]"
        assert!(!fold.apply(Some(InputEventResult::Transform {
            text: format!("{}[1]", fold.current_text()),
            images: None,
        })));
        // e.text + "[2]" (sees the chained text)
        assert!(!fold.apply(Some(InputEventResult::Transform {
            text: format!("{}[2]", fold.current_text()),
            images: None,
        })));
        assert_eq!(
            fold.finish(),
            InputEventResult::Transform {
                text: "X[1][2]".into(),
                images: None,
            }
        );
    }

    #[test]
    fn input_handled_short_circuits() {
        let mut fold = InputFold::new("X", None);
        // First handler returns handled -> stop.
        assert!(fold.apply(Some(InputEventResult::Handled)));
        // The second handler must not be applied by the runner; finish is handled.
        assert_eq!(fold.finish(), InputEventResult::Handled);
    }

    // ---- ToolResultFold (emitToolResult) -------------------------------

    #[test]
    fn tool_result_chains_content_across_handlers() {
        let base = vec![json!({ "type": "text", "text": "base" })];
        let mut fold = ToolResultFold::new(base, json!({ "initial": true }), false);
        // ext1: [...event.content, {text: ext1}]
        let mut c1 = fold.content().to_vec();
        c1.push(json!({ "type": "text", "text": "ext1" }));
        fold.apply(Some(ToolResultEventResult {
            content: Some(c1),
            details: None,
            is_error: None,
        }));
        // ext2: [...event.content, {text: ext2}] (sees ext1's change)
        let mut c2 = fold.content().to_vec();
        c2.push(json!({ "type": "text", "text": "ext2" }));
        fold.apply(Some(ToolResultEventResult {
            content: Some(c2),
            details: None,
            is_error: None,
        }));
        let result = fold.finish().expect("modified");
        let content = result.content.expect("content");
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], json!({ "type": "text", "text": "base" }));
        let mut appended: Vec<String> = content[1..]
            .iter()
            .map(|c| c["text"].as_str().unwrap().to_string())
            .collect();
        appended.sort();
        assert_eq!(appended, vec!["ext1", "ext2"]);
    }

    #[test]
    fn tool_result_preserves_previous_on_partial_patch() {
        let base = vec![json!({ "type": "text", "text": "base" })];
        let mut fold = ToolResultFold::new(base, json!({ "initial": true }), false);
        // ext1: content + details
        fold.apply(Some(ToolResultEventResult {
            content: Some(vec![json!({ "type": "text", "text": "first" })]),
            details: Some(json!({ "source": "ext1" })),
            is_error: None,
        }));
        // ext2: only isError
        fold.apply(Some(ToolResultEventResult {
            content: None,
            details: None,
            is_error: Some(true),
        }));
        let result = fold.finish().expect("modified");
        assert_eq!(
            result,
            ToolResultEventResult {
                content: Some(vec![json!({ "type": "text", "text": "first" })]),
                details: Some(json!({ "source": "ext1" })),
                is_error: Some(true),
            }
        );
    }

    #[test]
    fn tool_result_unmodified_is_none() {
        let fold = ToolResultFold::new(vec![], Value::Null, false);
        assert!(fold.finish().is_none());
    }

    // ---- BeforeAgentStartFold (emitBeforeAgentStart) -------------------

    #[test]
    fn before_agent_start_chains_system_prompt() {
        let mut fold = BeforeAgentStartFold::new("base");
        // ext1: ctx.getSystemPrompt() + "\nfirst"
        let sp1 = format!("{}\nfirst", fold.current_system_prompt());
        fold.apply(Some(BeforeAgentStartEventResult {
            message: None,
            system_prompt: Some(sp1),
        }));
        // ext2: ctx.getSystemPrompt() + "\nsecond" (sees the chained value)
        let sp2 = format!("{}\nsecond", fold.current_system_prompt());
        fold.apply(Some(BeforeAgentStartEventResult {
            message: None,
            system_prompt: Some(sp2),
        }));
        assert_eq!(
            fold.finish(),
            Some(BeforeAgentStartCombinedResult {
                messages: None,
                system_prompt: Some("base\nfirst\nsecond".into()),
            })
        );
    }

    #[test]
    fn before_agent_start_no_change_is_none() {
        let mut fold = BeforeAgentStartFold::new("base");
        fold.apply(None);
        fold.apply(Some(BeforeAgentStartEventResult::default()));
        assert_eq!(fold.finish(), None);
    }

    #[test]
    fn before_agent_start_collects_messages() {
        let mut fold = BeforeAgentStartFold::new("base");
        fold.apply(Some(BeforeAgentStartEventResult {
            message: Some(json!({ "customType": "note" })),
            system_prompt: None,
        }));
        let result = fold.finish().expect("has message");
        assert_eq!(result.messages.unwrap().len(), 1);
        assert_eq!(result.system_prompt, None);
    }

    // ---- ContextFold (emitContext) -------------------------------------

    #[test]
    fn context_replaces_message_array() {
        let mut fold = ContextFold::new(vec![json!({ "role": "user" })]);
        fold.apply(Some(ContextEventResult {
            messages: Some(vec![json!({ "role": "system" })]),
        }));
        assert_eq!(fold.finish(), vec![json!({ "role": "system" })]);
    }

    #[test]
    fn context_empty_result_keeps_messages() {
        let mut fold = ContextFold::new(vec![json!({ "role": "user" })]);
        fold.apply(None);
        fold.apply(Some(ContextEventResult { messages: None }));
        assert_eq!(fold.finish(), vec![json!({ "role": "user" })]);
    }

    // ---- ProjectTrustFold (emitProjectTrustEvent) ----------------------

    fn decision(
        trusted: ProjectTrustEventDecision,
        remember: Option<bool>,
    ) -> ProjectTrustEventResult {
        ProjectTrustEventResult { trusted, remember }
    }

    #[test]
    fn project_trust_no_handler_defaults_to_none() {
        let fold = ProjectTrustFold::new();
        assert_eq!(fold.finish(), None);
    }

    #[test]
    fn project_trust_skips_undecided_and_no_result() {
        let mut fold = ProjectTrustFold::new();
        // Handler returns undefined -> no result.
        assert!(!fold.apply(None));
        // Handler returns { trusted: "undecided" } -> falls through.
        assert!(!fold.apply(Some(decision(
            ProjectTrustEventDecision::Undecided,
            Some(true)
        ))));
        assert_eq!(fold.finish(), None);
    }

    #[test]
    fn project_trust_first_decision_wins_and_short_circuits() {
        let mut fold = ProjectTrustFold::new();
        // First decisive handler: { trusted: "no", remember: true } short-circuits.
        assert!(fold.apply(Some(decision(ProjectTrustEventDecision::No, Some(true)))));
        // A later decisive handler must not be applied by the runner; even if it
        // were, the first decision has already been recorded.
        assert_eq!(
            fold.finish(),
            Some(decision(ProjectTrustEventDecision::No, Some(true))),
        );
    }

    #[test]
    fn project_trust_undecided_then_decided_returns_the_decision() {
        // Mirrors extensions-runner.test.ts: an undecided handler falls through
        // to a decided one, and the fold yields { trusted: "no", remember: true }.
        let mut fold = ProjectTrustFold::new();
        assert!(!fold.apply(Some(decision(
            ProjectTrustEventDecision::Undecided,
            Some(true)
        ))));
        assert!(fold.apply(Some(decision(ProjectTrustEventDecision::No, Some(true)))));
        let result = fold.finish().expect("decided");
        assert_eq!(result, decision(ProjectTrustEventDecision::No, Some(true)));
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "trusted": "no", "remember": true }),
        );
    }

    // ---- headers + ExtensionError --------------------------------------

    #[test]
    fn headers_thread_through() {
        let mutated = json!({ "User-Agent": "x", "X-Turn-Index": "3" });
        assert_eq!(next_headers(mutated.clone()), mutated);
    }

    #[test]
    fn extension_error_serializes_camel_case() {
        let err = ExtensionError {
            extension_path: "/repo/.pi/extensions/x.ts".into(),
            event: "context".into(),
            error: "boom".into(),
            stack: None,
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({
                "extensionPath": "/repo/.pi/extensions/x.ts",
                "event": "context",
                "error": "boom",
            }),
        );
    }
}
