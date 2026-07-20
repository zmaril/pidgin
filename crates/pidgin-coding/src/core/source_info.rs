//! Provenance metadata for resolved extension/skill/prompt/theme sources.
//!
//! Ported from pi's `core/source-info.ts`. A [`SourceInfo`] records where a
//! resolved resource path came from: which install source produced it, whether
//! it is user- or project-scoped, and whether it sits inside an installed
//! package or at the top level of a source tree.
//!
//! NOTE: A sibling `prompt-templates` port defined a minimal local `SourceInfo`
//! mirror to avoid a cross-module dependency. This module is the canonical port
//! of `source-info.ts`; the two should be unified once both land on `main`.

use serde::{Deserialize, Serialize};

/// Whether a source is scoped to the user, the project, or a temporary
/// (synthetic) context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    /// User-level configuration (e.g. `~/.pi`).
    User,
    /// Project-level configuration (e.g. `./.pi`).
    Project,
    /// A synthetic, in-memory source with no persistent home.
    Temporary,
}

/// Whether a resolved path lives inside an installed package or at the top
/// level of a source tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOrigin {
    /// The resource came from an installed package.
    Package,
    /// The resource sits at the top level of a source tree.
    TopLevel,
}

/// Provenance for a single resolved resource path.
///
/// Mirrors pi's `SourceInfo` interface. `base_dir` is optional, matching the
/// optional `baseDir?` field upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// The resolved filesystem path of the resource.
    pub path: String,
    /// The install source string this path was resolved from.
    pub source: String,
    /// User / project / temporary scope.
    pub scope: SourceScope,
    /// Package or top-level origin.
    pub origin: SourceOrigin,
    /// Optional base directory the path is relative to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

/// The subset of package-manager path metadata consumed by [`create`].
///
/// NOTE: This mirrors pi's `PathMetadata` (from the unported `package-manager`
/// module) with only the fields `source-info.ts` reads. It becomes a re-export
/// once `package-manager` is ported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMetadata {
    /// The install source string.
    pub source: String,
    /// User / project / temporary scope.
    pub scope: SourceScope,
    /// Package or top-level origin.
    pub origin: SourceOrigin,
    /// Optional base directory.
    pub base_dir: Option<String>,
}

/// Options for [`create_synthetic`], mirroring pi's optional-field object.
///
/// `scope` and `origin` default to [`SourceScope::Temporary`] and
/// [`SourceOrigin::TopLevel`] respectively when left `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyntheticOptions {
    /// The install source string (required upstream).
    pub source: String,
    /// Scope override; defaults to `Temporary`.
    pub scope: Option<SourceScope>,
    /// Origin override; defaults to `TopLevel`.
    pub origin: Option<SourceOrigin>,
    /// Optional base directory.
    pub base_dir: Option<String>,
}

/// Build a [`SourceInfo`] from a resolved `path` and its package-manager
/// `metadata`. Port of `createSourceInfo`.
pub fn create(path: impl Into<String>, metadata: PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.into(),
        source: metadata.source,
        scope: metadata.scope,
        origin: metadata.origin,
        base_dir: metadata.base_dir,
    }
}

/// Build a synthetic [`SourceInfo`] for a `path` with the given `options`,
/// defaulting scope to `Temporary` and origin to `TopLevel`. Port of
/// `createSyntheticSourceInfo`.
pub fn create_synthetic(path: impl Into<String>, options: SyntheticOptions) -> SourceInfo {
    SourceInfo {
        path: path.into(),
        source: options.source,
        scope: options.scope.unwrap_or(SourceScope::Temporary),
        origin: options.origin.unwrap_or(SourceOrigin::TopLevel),
        base_dir: options.base_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_metadata() -> PathMetadata {
        PathMetadata {
            source: "npm:foo".to_string(),
            scope: SourceScope::Project,
            origin: SourceOrigin::Package,
            base_dir: Some("/pkg".to_string()),
        }
    }

    fn base_synthetic() -> SyntheticOptions {
        SyntheticOptions {
            source: "inline".to_string(),
            scope: None,
            origin: None,
            base_dir: None,
        }
    }

    #[test]
    fn create_copies_all_metadata_fields() {
        let info = create("/pkg/ext.ts", base_metadata());
        assert_eq!(
            info,
            SourceInfo {
                path: "/pkg/ext.ts".to_string(),
                source: "npm:foo".to_string(),
                scope: SourceScope::Project,
                origin: SourceOrigin::Package,
                base_dir: Some("/pkg".to_string()),
            }
        );
    }

    #[test]
    fn synthetic_applies_temporary_top_level_defaults() {
        let info = create_synthetic("/tmp/x.ts", base_synthetic());
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(info.origin, SourceOrigin::TopLevel);
        assert_eq!(info.base_dir, None);
        assert_eq!(info.source, "inline");
    }

    #[test]
    fn synthetic_honors_explicit_overrides() {
        let info = create_synthetic(
            "/home/x.ts",
            SyntheticOptions {
                scope: Some(SourceScope::User),
                origin: Some(SourceOrigin::Package),
                base_dir: Some("/home".to_string()),
                ..base_synthetic()
            },
        );
        assert_eq!(info.scope, SourceScope::User);
        assert_eq!(info.origin, SourceOrigin::Package);
        assert_eq!(info.base_dir.as_deref(), Some("/home"));
    }

    #[test]
    fn origin_serializes_kebab_case() {
        let json = serde_json::to_string(&SourceOrigin::TopLevel).unwrap();
        assert_eq!(json, "\"top-level\"");
        let scope = serde_json::to_string(&SourceScope::Temporary).unwrap();
        assert_eq!(scope, "\"temporary\"");
    }

    #[test]
    fn source_info_omits_none_base_dir() {
        let info = create_synthetic("/tmp/x.ts", base_synthetic());
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("base_dir"), "got: {json}");
    }
}
