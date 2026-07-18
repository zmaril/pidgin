//! Emit deprecation warnings, deduplicated by message.
//!
//! Ported from pi's `utils/deprecation.ts`. Each unique message is warned at
//! most once. pi colors the output with `chalk`; this port writes plain text to
//! stderr and exposes [`warn_deprecation_returning`] so tests (and callers that
//! want to route the message elsewhere) can observe whether a warning would be
//! emitted without depending on captured stderr.

use std::collections::HashSet;
use std::sync::Mutex;

static EMITTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Record `message` as emitted if it has not been seen before. Returns `true`
/// when this is the first time the message is warned.
fn record(message: &str) -> bool {
    let mut guard = EMITTED.lock().expect("deprecation set mutex poisoned");
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(message.to_string())
}

/// Warn once per unique `message`, writing to stderr.
pub fn warn_deprecation(message: &str) {
    if warn_deprecation_returning(message) {
        eprintln!("Deprecation warning: {message}");
    }
}

/// Like [`warn_deprecation`] but returns whether the message is newly warned
/// (and performs no output), so callers and tests can observe dedup behavior.
pub fn warn_deprecation_returning(message: &str) -> bool {
    record(message)
}

/// Clear the deduplication state. Exposed for tests.
pub fn clear_deprecation_warnings_for_tests() {
    let mut guard = EMITTED.lock().expect("deprecation set mutex poisoned");
    if let Some(set) = guard.as_mut() {
        set.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A single test covers dedup, distinct messages, and clearing. These share
    // process-global state, so they are exercised sequentially in one test to
    // avoid cross-test interference under parallel execution.
    #[test]
    fn dedup_distinct_and_clear_behavior() {
        clear_deprecation_warnings_for_tests();

        let msg = "unique-message-for-dedup-test";
        assert!(warn_deprecation_returning(msg), "first call should warn");
        assert!(
            !warn_deprecation_returning(msg),
            "second call for same message should not warn"
        );

        assert!(warn_deprecation_returning("message-b-distinct"));

        clear_deprecation_warnings_for_tests();
        assert!(
            warn_deprecation_returning(msg),
            "clearing state should re-enable warning"
        );
    }
}
