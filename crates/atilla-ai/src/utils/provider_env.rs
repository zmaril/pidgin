//! Provider environment-variable resolution, ported from pi-ai's
//! `packages/ai/src/utils/provider-env.ts` at pinned commit `3da591ab`.
//!
//! [`get_provider_env_value`] resolves a provider credential/config variable
//! from, in order: caller-supplied scoped overrides ([`ProviderEnv`]), then the
//! process environment. pi's third fallback — reading `/proc/self/environ` to
//! work around a Bun compiled-binary bug where `process.env` is empty inside a
//! Linux sandbox (oven-sh/bun#27802) — has no analogue here: Rust's
//! [`std::env::var`] reads the real process environment directly and is never
//! blanked out the way Bun's `process.env` can be, so the port stops at the
//! standard environment lookup.
//!
//! Parity note: pi chains the candidates with JS `||`, which treats the empty
//! string as falsy. The port reproduces that — an override or variable whose
//! value is `""` is skipped in favour of the next candidate, and an all-empty
//! chain yields `None` (pi's `undefined`).

use std::collections::BTreeMap;

/// Scoped provider environment overrides (`types.ts:104`,
/// `ProviderEnv = Record<string, string>`).
pub type ProviderEnv = BTreeMap<String, String>;

/// Resolve a provider env value from scoped overrides, then the process
/// environment (`provider-env.ts:45`).
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(value) = env.and_then(|env| env.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn scoped_override_takes_precedence() {
        let env = env_of(&[("SCOPED_ONLY_KEY_XYZ", "from-scope")]);
        assert_eq!(
            get_provider_env_value("SCOPED_ONLY_KEY_XYZ", Some(&env)),
            Some("from-scope".to_string())
        );
    }

    #[test]
    fn empty_scoped_value_falls_through_to_process_env() {
        std::env::set_var("ATILLA_TEST_EMPTY_FALLTHROUGH", "from-process");
        let env = env_of(&[("ATILLA_TEST_EMPTY_FALLTHROUGH", "")]);
        assert_eq!(
            get_provider_env_value("ATILLA_TEST_EMPTY_FALLTHROUGH", Some(&env)),
            Some("from-process".to_string())
        );
        std::env::remove_var("ATILLA_TEST_EMPTY_FALLTHROUGH");
    }

    #[test]
    fn reads_process_env_when_no_override() {
        std::env::set_var("ATILLA_TEST_PROCESS_ONLY", "value");
        assert_eq!(
            get_provider_env_value("ATILLA_TEST_PROCESS_ONLY", None),
            Some("value".to_string())
        );
        std::env::remove_var("ATILLA_TEST_PROCESS_ONLY");
    }

    #[test]
    fn missing_everywhere_is_none() {
        assert_eq!(
            get_provider_env_value("ATILLA_TEST_DEFINITELY_UNSET_KEY", None),
            None
        );
    }

    #[test]
    fn empty_process_env_value_is_none() {
        std::env::set_var("ATILLA_TEST_EMPTY_PROCESS", "");
        assert_eq!(
            get_provider_env_value("ATILLA_TEST_EMPTY_PROCESS", None),
            None
        );
        std::env::remove_var("ATILLA_TEST_EMPTY_PROCESS");
    }
}
