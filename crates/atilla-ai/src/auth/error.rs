//! Auth error types, ported from pi-ai's `packages/ai/src/auth/resolve.ts`
//! (`ModelsError` / `ModelsErrorCode`) at pinned commit `3da591ab`.
//!
//! pi models auth failures with a single `ModelsError` carrying a stable
//! `ModelsErrorCode` discriminant (`resolve.ts:14,21-29`). The codes are
//! asserted directly by pi's tests, so the serde wire values are transcribed
//! verbatim. A second, lighter error — [`AuthFlowError`] — represents the plain
//! `Error`s pi throws inside login/refresh/`toAuth` flows (which `resolve` then
//! re-wraps as a `ModelsError`).

use std::fmt;

/// Stable error-category discriminant (`resolve.ts:14`).
///
/// The serde values (`"model_source"`, `"model_validation"`, `"provider"`,
/// `"stream"`, `"auth"`, `"oauth"`) are the exact strings pi's tests assert on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsErrorCode {
    /// Model-source resolution failure.
    ModelSource,
    /// Model-validation failure.
    ModelValidation,
    /// Provider-level failure.
    Provider,
    /// Streaming failure.
    Stream,
    /// Generic auth failure (`resolve.ts` wraps store read/modify + api-key
    /// resolution failures under this code).
    Auth,
    /// OAuth-specific failure (refresh / auth-derivation).
    Oauth,
}

impl ModelsErrorCode {
    /// The serde wire value, matching pi's `ModelsErrorCode` union members.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelsErrorCode::ModelSource => "model_source",
            ModelsErrorCode::ModelValidation => "model_validation",
            ModelsErrorCode::Provider => "provider",
            ModelsErrorCode::Stream => "stream",
            ModelsErrorCode::Auth => "auth",
            ModelsErrorCode::Oauth => "oauth",
        }
    }
}

/// pi's `ModelsError` (`resolve.ts:21-29`): a coded error message. The optional
/// `cause` mirrors pi's `{ cause }` option — kept as a rendered string so the
/// type stays `Clone`/`PartialEq` for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsError {
    /// The error category.
    pub code: ModelsErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Rendered upstream cause, if any (pi's `{ cause }`).
    pub cause: Option<String>,
}

impl ModelsError {
    /// Construct a `ModelsError` with `code` and `message`.
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    /// Attach a rendered cause (builder style), mirroring pi's `{ cause }`.
    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    /// A `model_source` error.
    pub fn model_source(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::ModelSource, message)
    }

    /// A `model_validation` error.
    pub fn model_validation(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::ModelValidation, message)
    }

    /// A `provider` error.
    pub fn provider(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::Provider, message)
    }

    /// A `stream` error.
    pub fn stream(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::Stream, message)
    }

    /// An `auth` error.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::Auth, message)
    }

    /// An `oauth` error.
    pub fn oauth(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::Oauth, message)
    }
}

impl fmt::Display for ModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModelsError[{}]: {}", self.code.as_str(), self.message)?;
        if let Some(cause) = &self.cause {
            write!(f, " (cause: {cause})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ModelsError {}

/// The plain error pi throws inside OAuth/api-key login, refresh, and `toAuth`
/// flows (device-code failures, "Login cancelled", "Missing authorization
/// code", token-exchange failures, ...). `resolve` re-wraps these into a
/// [`ModelsError`] with the appropriate code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFlowError {
    /// The error message.
    pub message: String,
}

impl AuthFlowError {
    /// Construct an `AuthFlowError` from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AuthFlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AuthFlowError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_to_pi_wire_values() {
        for (code, wire) in [
            (ModelsErrorCode::ModelSource, "model_source"),
            (ModelsErrorCode::ModelValidation, "model_validation"),
            (ModelsErrorCode::Provider, "provider"),
            (ModelsErrorCode::Stream, "stream"),
            (ModelsErrorCode::Auth, "auth"),
            (ModelsErrorCode::Oauth, "oauth"),
        ] {
            assert_eq!(code.as_str(), wire);
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::Value::String(wire.to_string())
            );
        }
    }

    #[test]
    fn constructors_set_code_and_message() {
        let err = ModelsError::oauth("boom").with_cause("network down");
        assert_eq!(err.code, ModelsErrorCode::Oauth);
        assert_eq!(err.message, "boom");
        assert_eq!(err.cause.as_deref(), Some("network down"));
    }
}
