//! Default agent configuration constants.
//!
//! Ported from pi's `core/defaults.ts`.
//!
//! NOTE: pi types this as `ThinkingLevel` from `@earendil-works/pi-agent-core`,
//! a union that is not yet ported into this workspace. Until that enum lands the
//! default is carried as its string tag, which is the value pi actually stores.

/// Default thinking level applied when the agent config does not specify one.
pub const DEFAULT_THINKING_LEVEL: &str = "medium";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thinking_level_is_medium() {
        assert_eq!(DEFAULT_THINKING_LEVEL, "medium");
    }
}
