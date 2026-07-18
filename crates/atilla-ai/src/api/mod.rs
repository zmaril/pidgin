//! Per-provider wire-dialect drivers, mirroring pi-ai's `src/api`
//! (`packages/ai/src/api`).
//!
//! Each driver turns a provider's streaming wire format into atilla-ai's uniform
//! [`crate::types::AssistantMessageEvent`] stream. Stage 2 ports the Anthropic
//! Messages SSE parsing path; a later stage ports the Google Generative AI and
//! Google Vertex dialects (sharing one decode/build core in [`google_shared`]).

pub mod anthropic;
pub mod azure_openai_responses;
pub mod google_generative_ai;
pub mod google_shared;
pub mod google_vertex;
pub mod mistral;
pub mod openai_completions;
pub mod openai_responses;
pub mod openai_responses_shared;
