//! Node-API surface for missing-session-cwd detection (the `session-cwd`
//! functions).
//!
//! This exposes the Rust [`pidgin_coding::core::session_cwd`] port — a faithful
//! port of pi's `session-cwd.ts`
//! (`vendor/pi/packages/coding-agent/src/core/session-cwd.ts`) — to pi's
//! `packages/coding-agent` `session-cwd.test.ts`. The Rust code owns the one
//! decision the module makes: whether a resumed session's stored working
//! directory is non-empty and absent on disk (`existsSync` → `Path::exists`),
//! and the exact error / prompt text it formats from that issue.
//!
//! # The seam: decisions in Rust, class identity in TS
//!
//! pi's `session-cwd.ts` public surface is three functions plus a
//! `MissingSessionCwdError` class and an `assertSessionCwdExists` throw-helper.
//! `getMissingSessionCwdIssue` reads a structural `SessionCwdSource` (an object
//! with `getCwd()` / `getSessionFile()` methods — pi's `SessionManager`); that
//! object's identity and its live methods are inherently JS-runtime and cannot
//! cross the addon boundary, so the shim calls those two methods JS-side and
//! passes only the resolved strings here. Likewise `MissingSessionCwdError`'s
//! class identity (`instanceof`, `.name`) is JS-inherent, so the shim keeps the
//! class shell in TS and routes its message through
//! [`format_missing_session_cwd_error`]. Everything that *decides* — the
//! filesystem probe, the empty-cwd guard, and both format strings — runs here.
//!
//! # Marshaling
//!
//! Everything crosses as strings / optional strings / a plain object. The issue
//! is `{ sessionFile?, sessionCwd, fallbackCwd }` — pi's exact `SessionCwdIssue`
//! shape; `getMissingSessionCwdIssue` returns `Option<SessionCwdIssueJs>`
//! (`None` → pi's `undefined`). No JS closures, streams, or stable object
//! identity are required across the boundary.

use napi_derive::napi;

use pidgin_coding::core::session_cwd::{
    self, SessionCwdIssue, SessionCwdSource as CoreSessionCwdSource,
};

/// pi's `SessionCwdIssue`: a resumed session whose stored working directory is
/// gone. `sessionFile` is the persisted session path, `sessionCwd` the stored
/// (now-missing) directory, `fallbackCwd` the directory to continue in.
#[napi(object)]
pub struct SessionCwdIssueJs {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

impl From<SessionCwdIssue> for SessionCwdIssueJs {
    fn from(issue: SessionCwdIssue) -> Self {
        Self {
            session_file: issue.session_file,
            session_cwd: issue.session_cwd,
            fallback_cwd: issue.fallback_cwd,
        }
    }
}

impl From<SessionCwdIssueJs> for SessionCwdIssue {
    fn from(issue: SessionCwdIssueJs) -> Self {
        Self {
            session_file: issue.session_file,
            session_cwd: issue.session_cwd,
            fallback_cwd: issue.fallback_cwd,
        }
    }
}

/// The two strings the shim reads from pi's `SessionCwdSource` (`getCwd()` /
/// `getSessionFile()`), adapting them to the Rust trait so the real port owns
/// the empty-cwd guard and filesystem probe — no logic is reimplemented here.
struct SourceArgs {
    cwd: String,
    session_file: Option<String>,
}

impl CoreSessionCwdSource for SourceArgs {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }
    fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }
}

/// pi's `getMissingSessionCwdIssue`. Given the source's stored cwd and optional
/// session file (read JS-side from `SessionCwdSource`) plus the fallback cwd,
/// report a [`SessionCwdIssueJs`] when the source is a persisted session whose
/// stored cwd is non-empty and absent on disk; otherwise `None` (pi's
/// `undefined`).
#[napi(js_name = "getMissingSessionCwdIssue")]
pub fn get_missing_session_cwd_issue(
    session_cwd: String,
    session_file: Option<String>,
    fallback_cwd: String,
) -> Option<SessionCwdIssueJs> {
    let source = SourceArgs {
        cwd: session_cwd,
        session_file,
    };
    session_cwd::get_missing_session_cwd_issue(&source, &fallback_cwd).map(SessionCwdIssueJs::from)
}

/// pi's `formatMissingSessionCwdError`: the human-readable error text for an
/// issue (backs the JS `MissingSessionCwdError` message).
#[napi(js_name = "formatMissingSessionCwdError")]
pub fn format_missing_session_cwd_error(issue: SessionCwdIssueJs) -> String {
    session_cwd::format_missing_session_cwd_error(&issue.into())
}

/// pi's `formatMissingSessionCwdPrompt`: the interactive prompt text for an
/// issue.
#[napi(js_name = "formatMissingSessionCwdPrompt")]
pub fn format_missing_session_cwd_prompt(issue: SessionCwdIssueJs) -> String {
    session_cwd::format_missing_session_cwd_prompt(&issue.into())
}
