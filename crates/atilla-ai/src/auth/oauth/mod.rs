//! OAuth flows, ported from pi-ai's `packages/ai/src/auth/oauth/` at pinned
//! commit `3da591ab`.
//!
//! Foundation modules ([`flow`], [`pkce`], [`device_code`], [`oauth_page`],
//! [`load`]) are fully ported; [`anthropic`] is the fully-ported reference
//! provider, while the remaining four provider flow modules ([`xai`],
//! [`github_copilot`], [`openai_codex`], [`radius`]) are stubs that lay down the
//! public constants and [`crate::auth::types::OAuthAuth`] signatures for the
//! per-provider workers to fill.
//!
//! [`flow`] defines the blessed state-machine contract ([`flow::Step`],
//! [`flow::StepInput`], [`flow::OAuthFlowMachine`]) that carries multi-step
//! OAuth flows across the one-way napi boundary, plus the pure-Rust
//! [`flow::run_flow`] driver and the [`flow::run_login`] / [`flow::run_refresh`]
//! convenience wrappers.

pub mod bridge;
pub mod device_code;
pub mod flow;
pub mod load;
pub mod oauth_page;
pub mod pkce;

pub mod anthropic;
pub mod github_copilot;
pub mod openai_codex;
pub mod radius;
pub mod xai;

pub use bridge::{oauth_flow_for, OAuthFlowMode};
pub use device_code::{DeviceCodePollMachine, DevicePollInput, DevicePollStep};
pub use flow::{run_flow, run_login, run_refresh, OAuthFlowMachine, Step, StepInput};
