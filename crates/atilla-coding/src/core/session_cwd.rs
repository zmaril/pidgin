//! Missing-session-cwd detection, ported from
//! `packages/coding-agent/src/core/session-cwd.ts`.
//!
//! When a session is resumed but the working directory recorded in its header
//! no longer exists, pi surfaces a recoverable issue rather than failing. The
//! detection reads only whether a directory exists; it never touches the
//! session file.

use std::error::Error;
use std::fmt;
use std::path::Path;

/// A resumed session whose stored working directory is gone. Mirrors
/// `SessionCwdIssue`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

/// The read-only handle inspected for a missing cwd. Mirrors the structural
/// `SessionCwdSource` in pi; the coding-agent `SessionManager` satisfies it.
pub trait SessionCwdSource {
    fn get_cwd(&self) -> &str;
    fn get_session_file(&self) -> Option<&str>;
}

/// Report a missing-cwd issue if the source is a persisted session whose stored
/// cwd is non-empty and absent on disk. Mirrors `getMissingSessionCwdIssue`.
pub fn get_missing_session_cwd_issue(
    source: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = source.get_session_file()?;
    let session_cwd = source.get_cwd();
    if session_cwd.is_empty() || Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file.to_string()),
        session_cwd: session_cwd.to_string(),
        fallback_cwd: fallback_cwd.to_string(),
    })
}

/// The human-readable error text. Mirrors `formatMissingSessionCwdError`.
pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = match &issue.session_file {
        Some(path) => format!("\nSession file: {path}"),
        None => String::new(),
    };
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// The interactive prompt text. Mirrors `formatMissingSessionCwdPrompt`.
pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// The error raised by [`assert_session_cwd_exists`]. Mirrors
/// `MissingSessionCwdError`.
#[derive(Clone, Debug, PartialEq)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_missing_session_cwd_error(&self.issue))
    }
}

impl Error for MissingSessionCwdError {}

/// Raise [`MissingSessionCwdError`] when the source's stored cwd is missing.
/// Mirrors `assertSessionCwdExists`.
pub fn assert_session_cwd_exists(
    source: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(source, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}

impl SessionCwdSource for super::session_manager::SessionManager {
    fn get_cwd(&self) -> &str {
        super::session_manager::SessionManager::get_cwd(self)
    }
    fn get_session_file(&self) -> Option<&str> {
        super::session_manager::SessionManager::get_session_file(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource {
        cwd: String,
        session_file: Option<String>,
    }

    impl SessionCwdSource for FakeSource {
        fn get_cwd(&self) -> &str {
            &self.cwd
        }
        fn get_session_file(&self) -> Option<&str> {
            self.session_file.as_deref()
        }
    }

    fn source(cwd: &str, session_file: Option<&str>) -> FakeSource {
        FakeSource {
            cwd: cwd.to_string(),
            session_file: session_file.map(String::from),
        }
    }

    #[test]
    fn no_issue_without_a_session_file() {
        let src = source("/definitely/missing/dir", None);
        assert!(get_missing_session_cwd_issue(&src, "/now").is_none());
    }

    #[test]
    fn no_issue_when_cwd_exists() {
        // The filesystem root always exists.
        let src = source("/", Some("/sessions/s.jsonl"));
        assert!(get_missing_session_cwd_issue(&src, "/now").is_none());
    }

    #[test]
    fn reports_issue_when_stored_cwd_is_missing() {
        let src = source("/definitely/missing/dir", Some("/sessions/s.jsonl"));
        let issue = get_missing_session_cwd_issue(&src, "/now").unwrap();
        assert_eq!(issue.session_cwd, "/definitely/missing/dir");
        assert_eq!(issue.fallback_cwd, "/now");
        assert_eq!(issue.session_file.as_deref(), Some("/sessions/s.jsonl"));

        let err = assert_session_cwd_exists(&src, "/now").unwrap_err();
        assert_eq!(err.issue, issue);
    }

    #[test]
    fn formats_error_and_prompt_text() {
        let issue = SessionCwdIssue {
            session_file: Some("/sessions/s.jsonl".to_string()),
            session_cwd: "/gone".to_string(),
            fallback_cwd: "/now".to_string(),
        };
        assert_eq!(
            format_missing_session_cwd_error(&issue),
            "Stored session working directory does not exist: /gone\nSession file: /sessions/s.jsonl\nCurrent working directory: /now"
        );
        assert_eq!(
            format_missing_session_cwd_prompt(&issue),
            "cwd from session file does not exist\n/gone\n\ncontinue in current cwd\n/now"
        );
    }
}
