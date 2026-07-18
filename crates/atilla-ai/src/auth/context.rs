//! Default [`AuthContext`], ported from pi-ai's
//! `packages/ai/src/auth/context.ts` at pinned commit `3da591ab`.
//!
//! pi's `defaultProviderAuthContext` reads env vars from `process.env`
//! (returning a value only when it is a non-empty trimmed string) and checks
//! file existence via `node:fs`, expanding a leading `~` through `os.homedir()`
//! (`context.ts:23-44`).
//!
//! # Sync port deviations
//!
//! The async pi signatures become synchronous, and env/file access is routed
//! through the [`ExecutionEnv`] storage seam instead of `process.env` /
//! `node:fs`. There is no `os.homedir()` in the seam, so `~` expansion reads the
//! `HOME` environment variable through the same seam (the standard Unix home
//! source); when `HOME` is unset the `~` is left intact, mirroring pi's
//! best-effort "return false on any error" behavior.

use crate::seams::storage::ExecutionEnv;

use super::types::AuthContext;

/// Default auth context backed by an [`ExecutionEnv`] seam (`context.ts:23-44`).
///
/// `E` is typically [`crate::seams::storage::SystemEnv`] in production or
/// [`crate::seams::storage::MemoryEnv`] in tests.
pub struct DefaultAuthContext<E> {
    env: E,
}

impl<E: ExecutionEnv> DefaultAuthContext<E> {
    /// Build a default auth context over `env`.
    pub fn new(env: E) -> Self {
        Self { env }
    }

    /// Expand a leading `~` using the seam's `HOME` value, mirroring pi's
    /// `os.homedir()` prefix substitution (`context.ts:34-37`).
    fn expand_home(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix('~') {
            if let Some(home) = self.env.env_var("HOME") {
                return format!("{home}{rest}");
            }
        }
        path.to_string()
    }
}

impl<E: ExecutionEnv> AuthContext for DefaultAuthContext<E> {
    fn env(&self, name: &str) -> Option<String> {
        // pi: `typeof value === "string" && value.trim().length > 0 ? value : undefined`
        // — the original (untrimmed) value is returned when its trim is non-empty.
        self.env
            .env_var(name)
            .filter(|value| !value.trim().is_empty())
    }

    fn file_exists(&self, path: &str) -> bool {
        let resolved = self.expand_home(path);
        self.env.exists(std::path::Path::new(&resolved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::storage::MemoryEnv;

    #[test]
    fn env_returns_non_empty_trimmed_values_only() {
        let env = MemoryEnv::new()
            .with_env("SET", "value")
            .with_env("BLANK", "   ");
        let ctx = DefaultAuthContext::new(env);
        assert_eq!(ctx.env("SET").as_deref(), Some("value"));
        assert_eq!(ctx.env("BLANK"), None);
        assert_eq!(ctx.env("MISSING"), None);
    }

    #[test]
    fn env_returns_original_untrimmed_value() {
        // pi returns the original value when its trim is non-empty.
        let env = MemoryEnv::new().with_env("PADDED", "  value  ");
        let ctx = DefaultAuthContext::new(env);
        assert_eq!(ctx.env("PADDED").as_deref(), Some("  value  "));
    }

    #[test]
    fn file_exists_expands_leading_tilde_via_home() {
        let env = MemoryEnv::new()
            .with_env("HOME", "/home/zack")
            .with_file("/home/zack/.aws/credentials", "[default]\n");
        let ctx = DefaultAuthContext::new(env);
        assert!(ctx.file_exists("~/.aws/credentials"));
        assert!(!ctx.file_exists("~/.aws/missing"));
    }

    #[test]
    fn file_exists_checks_absolute_paths() {
        let env = MemoryEnv::new().with_file("/etc/hosts", "127.0.0.1 localhost\n");
        let ctx = DefaultAuthContext::new(env);
        assert!(ctx.file_exists("/etc/hosts"));
        assert!(!ctx.file_exists("/etc/nope"));
    }

    #[test]
    fn tilde_left_intact_when_home_unset() {
        let env = MemoryEnv::new().with_file("~/literal", "x");
        let ctx = DefaultAuthContext::new(env);
        // With no HOME, `~` is not expanded, so the literal path is checked.
        assert!(ctx.file_exists("~/literal"));
    }
}
