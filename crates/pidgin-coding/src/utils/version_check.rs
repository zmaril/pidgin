//! Semantic version comparison for update checks.
//!
//! Ported from pi's `utils/version-check.ts`. Only the pure comparison helpers
//! are ported here, backed by the `semver` crate (which preserves prerelease
//! ordering).
//!
//! Deferred: `getLatestPiRelease` / `getLatestPiVersion` / `checkForNewPiVersion`
//! perform an HTTP fetch against `https://pi.dev/api/latest-version`. Those
//! require an HTTP client and are out of scope for this pure-utilities PR; the
//! fetch/mock tests from pi are intentionally not ported.

use semver::Version;
use std::cmp::Ordering;

/// Compare two package versions. Returns `None` when either string is not a
/// valid semantic version, mirroring pi's `comparePackageVersions`.
pub fn compare_package_versions(left_version: &str, right_version: &str) -> Option<Ordering> {
    let left = Version::parse(left_version.trim()).ok()?;
    let right = Version::parse(right_version.trim()).ok()?;
    Some(left.cmp(&right))
}

/// Returns true when `candidate_version` is strictly newer than
/// `current_version`. When either is not valid semver, falls back to a trimmed
/// string inequality, mirroring pi's `isNewerPackageVersion`.
pub fn is_newer_package_version(candidate_version: &str, current_version: &str) -> bool {
    match compare_package_versions(candidate_version, current_version) {
        Some(ordering) => ordering == Ordering::Greater,
        None => candidate_version.trim() != current_version.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_package_versions() {
        assert_eq!(
            compare_package_versions("0.70.6", "0.70.5"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_package_versions("0.70.5", "0.70.5"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare_package_versions("0.70.4", "0.70.5"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_package_versions("5.0.0-beta.20", "5.0.0-beta.9"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn strictly_newer_semantics() {
        assert!(!is_newer_package_version("0.70.5", "0.70.5"));
        assert!(is_newer_package_version("0.70.6", "0.70.5"));
    }

    #[test]
    fn falls_back_to_string_inequality_for_invalid_semver() {
        assert_eq!(compare_package_versions("not-semver", "1.0.0"), None);
        assert!(is_newer_package_version("branch-a", "branch-b"));
        assert!(!is_newer_package_version("branch-a", "branch-a"));
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            compare_package_versions(" 1.0.0 ", "1.0.0"),
            Some(Ordering::Equal)
        );
    }
}
