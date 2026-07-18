//! Install-telemetry opt-in resolution.
//!
//! Ported from pi's `core/telemetry.ts`. The `PI_TELEMETRY` environment
//! variable, when set, overrides the persisted setting; otherwise the
//! settings-manager value decides.

/// Read-only view of the settings the telemetry check consults.
///
/// NOTE: This is a minimal seam over the unported `SettingsManager`. It exposes
/// only `getEnableInstallTelemetry()`; the full manager implements this trait
/// once ported.
pub trait TelemetrySettings {
    /// Whether install telemetry is enabled in persisted settings.
    fn enable_install_telemetry(&self) -> bool;
}

/// Port of pi's `isTruthyEnvFlag`: `"1"`, or `"true"`/`"yes"` in any case.
pub fn is_truthy_env_flag(value: Option<&str>) -> bool {
    match value {
        None | Some("") => false,
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"),
    }
}

/// Resolve whether install telemetry is enabled. Port of
/// `isInstallTelemetryEnabled`: a set `telemetry_env` overrides settings;
/// otherwise the settings value is used.
///
/// Pass `std::env::var("PI_TELEMETRY").ok().as_deref()` for `telemetry_env` at
/// the call site to mirror pi's `process.env.PI_TELEMETRY` default.
pub fn is_install_telemetry_enabled(
    settings: &impl TelemetrySettings,
    telemetry_env: Option<&str>,
) -> bool {
    match telemetry_env {
        Some(env) => is_truthy_env_flag(Some(env)),
        None => settings.enable_install_telemetry(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSettings {
        enabled: bool,
    }

    impl TelemetrySettings for FakeSettings {
        fn enable_install_telemetry(&self) -> bool {
            self.enabled
        }
    }

    #[test]
    fn truthy_flag_recognizes_accepted_values() {
        let cases = [
            (None, false),
            (Some(""), false),
            (Some("1"), true),
            (Some("0"), false),
            (Some("true"), true),
            (Some("TRUE"), true),
            (Some("Yes"), true),
            (Some("no"), false),
            (Some("2"), false),
        ];
        for (input, want) in cases {
            assert_eq!(is_truthy_env_flag(input), want, "input {input:?}");
        }
    }

    #[test]
    fn env_override_beats_settings() {
        let on = FakeSettings { enabled: true };
        let off = FakeSettings { enabled: false };
        // Env wins in both directions regardless of the persisted setting.
        assert!(is_install_telemetry_enabled(&off, Some("1")));
        assert!(!is_install_telemetry_enabled(&on, Some("no")));
    }

    #[test]
    fn falls_back_to_settings_when_env_unset() {
        assert!(is_install_telemetry_enabled(
            &FakeSettings { enabled: true },
            None
        ));
        assert!(!is_install_telemetry_enabled(
            &FakeSettings { enabled: false },
            None
        ));
    }
}
