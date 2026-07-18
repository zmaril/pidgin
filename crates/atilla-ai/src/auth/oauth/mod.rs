//! OAuth flows, ported from pi-ai's `packages/ai/src/auth/oauth/` at pinned
//! commit `3da591ab`.
//!
//! Foundation modules ([`pkce`], [`device_code`], [`oauth_page`], [`load`]) are
//! fully ported; the five provider flow modules ([`anthropic`], [`xai`],
//! [`github_copilot`], [`openai_codex`], [`radius`]) are stubs that lay down the
//! public constants and [`crate::auth::types::OAuthAuth`] signatures for the
//! per-provider workers to fill.

pub mod device_code;
pub mod load;
pub mod oauth_page;
pub mod pkce;

pub mod anthropic;
pub mod github_copilot;
pub mod openai_codex;
pub mod radius;
pub mod xai;
