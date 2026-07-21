//! Selection- and input-group extension events.
//!
//! Faithful port of the selection/input-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`: `ModelSelectEvent`,
//! `ThinkingLevelSelectEvent`, `UserBashEvent`, `InputEvent`, plus the
//! `user_bash` and `input` result shapes.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use serde::{Deserialize, Serialize};

use super::common::{BashOperations, BashResult, ImageContent, Model, ThinkingLevel};

/// How a model selection was triggered (pi's `ModelSelectSource`,
/// `types.ts:779`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSelectSource {
    /// A direct `set` of the model.
    Set,
    /// A `cycle` through the model list.
    Cycle,
    /// A `restore` of a previously selected model.
    Restore,
}

/// Fired when a new model is selected (pi's `ModelSelectEvent`, `types.ts:782`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectEvent {
    /// The newly selected model.
    pub model: Model,
    /// The previously selected model, if any.
    pub previous_model: Option<Model>,
    /// How the selection was triggered.
    pub source: ModelSelectSource,
}

/// Fired when a new thinking level is selected (pi's `ThinkingLevelSelectEvent`,
/// `types.ts:790`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelSelectEvent {
    /// The newly selected thinking level.
    pub level: ThinkingLevel,
    /// The previous thinking level.
    pub previous_level: ThinkingLevel,
}

/// Fired when the user executes a bash command via the `!` or `!!` prefix (pi's
/// `UserBashEvent`, `types.ts:801`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBashEvent {
    /// The command to execute.
    pub command: String,
    /// True if the `!!` prefix was used (excluded from LLM context).
    pub exclude_from_context: bool,
    /// The current working directory.
    pub cwd: String,
}

/// Result of a `user_bash` handler (pi's `UserBashEventResult`,
/// `types.ts:1064`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserBashEventResult {
    /// Custom operations to use for execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operations: Option<BashOperations>,
    /// Full replacement result: the extension handled execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<BashResult>,
}

/// The origin of a piece of user input (pi's `InputSource`, `types.ts:816`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSource {
    /// Interactive terminal input.
    Interactive,
    /// Input over the RPC channel.
    Rpc,
    /// Input injected by an extension.
    Extension,
}

/// How input will be delivered during streaming (pi inline union,
/// `types.ts:828`). Also reused by the agent input-delivery options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    /// Steer the in-flight turn.
    Steer,
    /// Queue as a follow-up after the current turn.
    FollowUp,
}

/// Fired when user input is received, before agent processing (pi's `InputEvent`,
/// `types.ts:819`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    /// The input text.
    pub text: String,
    /// Attached images, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    /// Where the input came from.
    pub source: InputSource,
    /// How the input will be delivered during streaming, or `None` when idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<StreamingBehavior>,
}

/// Result of an `input` handler (pi's `InputEventResult`, `types.ts:832`).
///
/// A discriminated union on `action`: continue unchanged, transform the text
/// (optionally replacing images), or mark the input handled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputEventResult {
    /// Leave the input unchanged.
    Continue,
    /// Replace the input text and, optionally, its images.
    Transform {
        /// The replacement text.
        text: String,
        /// Replacement images, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    /// The extension fully handled the input; skip agent processing.
    Handled,
}
