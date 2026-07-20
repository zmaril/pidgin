//! Build the `User-Agent` string pi sends to pi.dev.
//!
//! Ported from pi's `utils/pi-user-agent.ts`. pi's format is
//! `pi/<version> (<platform>; node|bun/<runtime-version>; <arch>)`.
//!
//! Intentional delta: this crate is not a Node/Bun runtime, so the middle
//! runtime segment reports the Rust toolchain. Where pi emits `node/<ver>` or
//! `bun/<ver>`, this port emits `rust/<ver>` using the compiler version
//! captured at build time (falling back to the static string `rust` if that
//! information is unavailable). The `<platform>` and `<arch>` segments use
//! `std::env::consts::OS` / `ARCH`, which mirror the values pi reads from
//! `process.platform` / `process.arch` closely enough for server-side
//! bookkeeping.

/// Rust compiler version, if the build environment exposed it.
const RUSTC_VERSION: Option<&str> = option_env!("CARGO_PKG_RUST_VERSION");

/// Build the pi user-agent string for `version`.
pub fn pi_user_agent(version: &str) -> String {
    let runtime = match RUSTC_VERSION {
        Some(v) if !v.is_empty() => format!("rust/{v}"),
        _ => "rust".to_string(),
    };
    format!(
        "pi/{version} ({}; {runtime}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn formats_the_expected_shape() {
        let runtime = match RUSTC_VERSION {
            Some(v) if !v.is_empty() => format!("rust/{v}"),
            _ => "rust".to_string(),
        };
        let user_agent = pi_user_agent("1.2.3");
        assert_eq!(
            user_agent,
            format!(
                "pi/1.2.3 ({}; {runtime}; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
    }

    #[test]
    fn matches_the_user_agent_pattern() {
        let re = Regex::new(r"^pi/[^\s()]+ \([^;()]+;\s*[^;()]+;\s*[^()]+\)$").unwrap();
        assert!(re.is_match(&pi_user_agent("1.2.3")));
    }
}
