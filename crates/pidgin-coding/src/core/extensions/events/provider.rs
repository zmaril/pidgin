//! Provider-request extension events.
//!
//! Faithful port of the provider-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`:
//! `BeforeProviderRequestEvent`, `BeforeProviderHeadersEvent`, and
//! `AfterProviderResponseEvent`, plus the `before_provider_request` result alias.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::ProviderHeaders;

/// Fired before a provider request is sent; can replace the payload (pi's
/// `BeforeProviderRequestEvent`, `types.ts:664`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeProviderRequestEvent {
    /// The outgoing provider payload (pi types this `unknown`).
    pub payload: Value,
}

/// Result of a `before_provider_request` handler (pi's
/// `BeforeProviderRequestEventResult` = `unknown`, `types.ts:1055`).
///
/// A replacement payload; when the handler returns nothing the payload is left
/// unchanged.
pub type BeforeProviderRequestEventResult = Value;

/// Fired after request headers are assembled, before the HTTP call (pi's
/// `BeforeProviderHeadersEvent`, `types.ts:674`). Handlers mutate `headers` in
/// place; a `null` value deletes that header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeProviderHeadersEvent {
    /// The assembled provider headers.
    pub headers: ProviderHeaders,
}

/// Fired after a provider response is received, before its stream is consumed
/// (pi's `AfterProviderResponseEvent`, `types.ts:680`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterProviderResponseEvent {
    /// The HTTP status code.
    pub status: i64,
    /// The response headers.
    pub headers: HashMap<String, String>,
}
