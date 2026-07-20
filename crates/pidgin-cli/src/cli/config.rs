//! App configuration constants and user-config paths.
//!
//! Mirrors the pieces of pi's `packages/coding-agent/src/config.ts` that the
//! CLI shell needs. pi derives these from the coding-agent `package.json`
//! (`piConfig`); the pinned pi (0.80.10) has `piConfig.configDir = ".pi"` and
//! no `piConfig.name`, so `APP_NAME` falls back to `"pi"`. The env-var names
//! are derived from `APP_NAME` exactly as pi does, so the black-box tests'
//! `PI_CODING_AGENT_DIR` / `PI_CODING_AGENT_SESSION_DIR` line up.

/// pi's `APP_NAME` (`piConfig.name || "pi"`). Used in help text and env vars.
pub const APP_NAME: &str = "pi";

/// pi's `CONFIG_DIR_NAME` (`piConfig.configDir || ".pi"`).
pub const CONFIG_DIR_NAME: &str = ".pi";

/// `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR` = `PI_CODING_AGENT_DIR`.
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

/// `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`.
pub const ENV_SESSION_DIR: &str = "PI_CODING_AGENT_SESSION_DIR";

/// The version reported by `--version`.
///
/// This is the pidgin crate's own semver (the pidgin binary's version), which
/// satisfies pi's `--version` contract (`/^\d+\.\d+\.\d+/`). It is intentionally
/// not pinned to pi's package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
