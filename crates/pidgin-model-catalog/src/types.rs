//! Serde types mirroring pi's canonical `Model<TApi>` shape
//! (see `vendor/pi/packages/ai/src/types.ts`).
//!
//! JSON produced by pi's generator is camelCase; these structs use idiomatic
//! snake_case Rust fields remapped via `#[serde(rename_all = "camelCase")]`.
//! Every model-shaped struct carries an `extra` catch-all so that fields added
//! upstream in future pin bumps deserialize successfully instead of erroring —
//! this crate is deliberately tolerant of forward-compatible schema growth.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Per-token cost rates, all expressed in US dollars per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    /// Cost per million input tokens.
    pub input: f64,
    /// Cost per million output tokens.
    pub output: f64,
    /// Cost per million cache-read tokens.
    pub cache_read: f64,
    /// Cost per million cache-write tokens.
    pub cache_write: f64,
}

/// A pricing tier that applies once the cumulative input token count exceeds
/// `input_tokens_above`. Extends [`ModelCostRates`] with the threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    /// Rates charged above the threshold.
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Input-token count above which this tier's rates apply.
    pub input_tokens_above: u64,
}

/// A model's full cost: base [`ModelCostRates`] plus optional volume tiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// Base rates applied below the first tier threshold.
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Optional volume-based pricing tiers, in ascending threshold order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// An accepted input modality.
///
/// Uses a catch-all `Other` variant so that modalities added upstream (e.g.
/// `audio`, `video`) deserialize into `Other` rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    /// Textual input.
    Text,
    /// Image input.
    Image,
    /// Any modality not yet known to this crate.
    #[serde(other)]
    Other,
}

/// A single model entry, mirroring pi's `Model<TApi>`.
///
/// `api` and `provider` are kept as plain [`String`]s because in pi they are
/// open unions (`KnownApi | string` / `ProviderId | string`): providers and
/// wire APIs can be added upstream without a code change here. `compat` is kept
/// as a raw [`serde_json::Value`] because its shape depends on `api`
/// (OpenAICompletionsCompat / OpenAIResponsesCompat / AnthropicMessagesCompat);
/// a consumer such as pidgin-ai can strongly-type it per-api later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// Stable model identifier (unique within its provider).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Wire API family (open union, e.g. `anthropic-messages`, `openai-completions`).
    pub api: String,
    /// Owning provider id (open union, e.g. `anthropic`, `openai`).
    pub provider: String,
    /// Base URL for the provider endpoint serving this model.
    pub base_url: String,
    /// Whether the model supports reasoning / thinking.
    pub reasoning: bool,
    /// Optional mapping from a normalized thinking level to a provider-specific
    /// value (or `null` to disable that level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<BTreeMap<String, Option<String>>>,
    /// Accepted input modalities.
    pub input: Vec<Modality>,
    /// Pricing information.
    pub cost: ModelCost,
    /// Maximum context window in tokens.
    pub context_window: u64,
    /// Maximum output tokens per request.
    pub max_tokens: u64,
    /// Optional extra HTTP headers required by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// API-dependent compatibility flags, kept untyped (see struct docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<serde_json::Value>,
    /// Catch-all for fields added upstream but not yet modeled here.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
