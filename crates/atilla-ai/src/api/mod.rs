//! Per-provider wire-dialect drivers, mirroring pi-ai's `src/api`
//! (`packages/ai/src/api`).
//!
//! Each driver turns a provider's streaming wire format into atilla-ai's uniform
//! [`crate::types::AssistantMessageEvent`] stream. Stage 2 ports the Anthropic
//! Messages SSE parsing path.

pub mod anthropic;
pub mod openai_completions;
