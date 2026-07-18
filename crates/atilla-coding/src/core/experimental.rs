//! Experimental-feature gating.
//!
//! Ported from pi's `core/experimental.ts`. Experimental features are enabled
//! when the `PI_EXPERIMENTAL` environment variable is exactly `"1"`.

/// Whether experimental features are enabled for this process.
pub fn are_experimental_features_enabled() -> bool {
    std::env::var("PI_EXPERIMENTAL").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn enabled_only_when_env_is_exactly_one() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("PI_EXPERIMENTAL", "1");
        assert!(are_experimental_features_enabled());
        std::env::set_var("PI_EXPERIMENTAL", "0");
        assert!(!are_experimental_features_enabled());
        std::env::set_var("PI_EXPERIMENTAL", "true");
        assert!(!are_experimental_features_enabled());
        std::env::remove_var("PI_EXPERIMENTAL");
        assert!(!are_experimental_features_enabled());
    }
}
