//! HTTP idle-timeout parsing/formatting helpers.
//!
//! Ported from the pure slice of pi's `core/http-dispatcher.ts`: the idle-timeout
//! choices and their parse/format helpers. pi's undici global-dispatcher install
//! plumbing (`configureHttpDispatcher`, the undici `Client`/`Pool` factories,
//! proxy env wiring) is Node-runtime-specific and deliberately not ported here.

/// Default HTTP idle timeout in milliseconds (`http-dispatcher.ts:4`).
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;

/// A selectable idle-timeout preset (`http-dispatcher.ts:6`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpIdleTimeoutChoice {
    /// Human-readable label shown in the picker.
    pub label: &'static str,
    /// The timeout in milliseconds (`0` means disabled).
    pub timeout_ms: u64,
}

/// The idle-timeout presets, in menu order (`http-dispatcher.ts:6`).
pub const HTTP_IDLE_TIMEOUT_CHOICES: &[HttpIdleTimeoutChoice] = &[
    HttpIdleTimeoutChoice {
        label: "30 sec",
        timeout_ms: 30_000,
    },
    HttpIdleTimeoutChoice {
        label: "1 min",
        timeout_ms: 60_000,
    },
    HttpIdleTimeoutChoice {
        label: "2 min",
        timeout_ms: 120_000,
    },
    HttpIdleTimeoutChoice {
        label: "5 min",
        timeout_ms: 300_000,
    },
    HttpIdleTimeoutChoice {
        label: "disabled",
        timeout_ms: 0,
    },
];

/// Parse a string idle-timeout value (`http-dispatcher.ts:17`).
///
/// `"disabled"` (case-insensitive) → `Some(0)`; empty/whitespace → `None`;
/// otherwise the string is parsed as a number and floored via
/// [`parse_http_idle_timeout_num`]. A non-numeric string yields `None`, matching
/// pi's `Number("abc")` → `NaN` → `undefined`.
pub fn parse_http_idle_timeout_ms(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("disabled") {
        return Some(0);
    }
    if trimmed.is_empty() {
        return None;
    }
    parse_http_idle_timeout_num(trimmed.parse::<f64>().ok()?)
}

/// Normalize a numeric idle-timeout value (`http-dispatcher.ts:29`).
///
/// Non-finite or negative values yield `None`; otherwise the value is floored.
pub fn parse_http_idle_timeout_num(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.floor() as u64)
}

/// Format an idle timeout for display (`http-dispatcher.ts:35`).
///
/// Returns a preset label when one matches, else `"<seconds> sec"`.
pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|c| c.timeout_ms == timeout_ms)
    {
        return choice.label.to_string();
    }
    format!("{} sec", timeout_ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disabled_and_empty() {
        assert_eq!(parse_http_idle_timeout_ms("disabled"), Some(0));
        assert_eq!(parse_http_idle_timeout_ms("  DISABLED  "), Some(0));
        assert_eq!(parse_http_idle_timeout_ms(""), None);
        assert_eq!(parse_http_idle_timeout_ms("   "), None);
    }

    #[test]
    fn parse_numeric_strings() {
        assert_eq!(parse_http_idle_timeout_ms("300000"), Some(300_000));
        assert_eq!(parse_http_idle_timeout_ms(" 12.9 "), Some(12));
        assert_eq!(parse_http_idle_timeout_ms("-1"), None);
        assert_eq!(parse_http_idle_timeout_ms("abc"), None);
    }

    #[test]
    fn parse_num_edges() {
        assert_eq!(parse_http_idle_timeout_num(f64::NAN), None);
        assert_eq!(parse_http_idle_timeout_num(f64::INFINITY), None);
        assert_eq!(parse_http_idle_timeout_num(-0.5), None);
        assert_eq!(parse_http_idle_timeout_num(90_000.0), Some(90_000));
    }

    #[test]
    fn format_uses_labels_then_seconds() {
        assert_eq!(format_http_idle_timeout_ms(0), "disabled");
        assert_eq!(format_http_idle_timeout_ms(300_000), "5 min");
        assert_eq!(format_http_idle_timeout_ms(30_000), "30 sec");
        assert_eq!(format_http_idle_timeout_ms(90_000), "90 sec");
        assert_eq!(format_http_idle_timeout_ms(1_500), "1.5 sec");
    }

    #[test]
    fn parse_format_round_trips_presets() {
        for choice in HTTP_IDLE_TIMEOUT_CHOICES {
            assert_eq!(format_http_idle_timeout_ms(choice.timeout_ms), choice.label);
            // The label "disabled" and each numeric ms round-trip back through parse.
            assert_eq!(
                parse_http_idle_timeout_ms(&choice.timeout_ms.to_string()),
                Some(choice.timeout_ms)
            );
        }
        assert_eq!(parse_http_idle_timeout_ms("disabled"), Some(0));
    }
}
