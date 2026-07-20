// straitjacket-allow-file:duplication — the per-`ResourceType` field accessors
// (`PiManifest::entries`, `PackageFilter::patterns`, `ResourceType::key`) are
// deliberately parallel 4-arm matches mirroring pi's `x[resourceType]` indexing.

//! Resolved-resource types, precedence ranking, and the resolution accumulator.
//!
//! Ports the pure-filesystem resolution data types from pi's
//! `core/package-manager.ts`: `ResolvedResource`, `ResolvedPaths`,
//! `ResourceAccumulator`, the `ResourceType` key set, the per-type file-name
//! patterns, `resourcePrecedenceRank`, and `toResolvedPaths` (precedence sort
//! plus canonical-path dedupe). The `PathMetadata` these carry is the canonical
//! one from [`crate::core::source_info`].

use crate::core::source_info::{PathMetadata, SourceOrigin};
use crate::utils::paths::canonicalize_path;
use indexmap::IndexMap;

/// A single resolved resource path plus its enabled state and provenance.
///
/// Mirrors pi's `ResolvedResource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResource {
    /// The resolved filesystem path.
    pub path: String,
    /// Whether the resource is enabled (survives pattern filtering).
    pub enabled: bool,
    /// Where the path came from.
    pub metadata: PathMetadata,
}

/// The four resolved resource collections. Mirrors pi's `ResolvedPaths`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedPaths {
    /// Resolved extension entry points.
    pub extensions: Vec<ResolvedResource>,
    /// Resolved skill paths.
    pub skills: Vec<ResolvedResource>,
    /// Resolved prompt-template paths.
    pub prompts: Vec<ResolvedResource>,
    /// Resolved theme paths.
    pub themes: Vec<ResolvedResource>,
}

/// The resource kinds resolved from packages and top-level settings. Mirrors
/// pi's `ResourceType` string-union and `RESOURCE_TYPES` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// `.ts` / `.js` extension modules.
    Extensions,
    /// `SKILL.md` (or root `.md`) skill files.
    Skills,
    /// `.md` prompt templates.
    Prompts,
    /// `.json` theme files.
    Themes,
}

/// The resource types in pi's fixed iteration order.
pub const RESOURCE_TYPES: [ResourceType; 4] = [
    ResourceType::Extensions,
    ResourceType::Skills,
    ResourceType::Prompts,
    ResourceType::Themes,
];

impl ResourceType {
    /// The manifest / settings key for this type (`"extensions"`, ...).
    pub fn key(self) -> &'static str {
        match self {
            ResourceType::Extensions => "extensions",
            ResourceType::Skills => "skills",
            ResourceType::Prompts => "prompts",
            ResourceType::Themes => "themes",
        }
    }

    /// Whether a file name matches this type's `FILE_PATTERNS` regex.
    pub fn matches_file(self, name: &str) -> bool {
        match self {
            ResourceType::Extensions => name.ends_with(".ts") || name.ends_with(".js"),
            ResourceType::Skills => name.ends_with(".md"),
            ResourceType::Prompts => name.ends_with(".md"),
            ResourceType::Themes => name.ends_with(".json"),
        }
    }
}

/// A pi manifest (`package.json`'s `pi` field). Mirrors pi's `PiManifest`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct PiManifest {
    /// Extension entries / patterns.
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    /// Skill entries / patterns.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// Prompt entries / patterns.
    #[serde(default)]
    pub prompts: Option<Vec<String>>,
    /// Theme entries / patterns.
    #[serde(default)]
    pub themes: Option<Vec<String>>,
}

impl PiManifest {
    /// The entry list for `resource_type`, mirroring `manifest[resourceType]`.
    pub fn entries(&self, resource_type: ResourceType) -> Option<&Vec<String>> {
        match resource_type {
            ResourceType::Extensions => self.extensions.as_ref(),
            ResourceType::Skills => self.skills.as_ref(),
            ResourceType::Prompts => self.prompts.as_ref(),
            ResourceType::Themes => self.themes.as_ref(),
        }
    }
}

/// A per-package resource filter (the object form of a `PackageSource`).
///
/// Mirrors pi's `PackageFilter`: an optional `autoload` flag plus optional
/// per-type pattern arrays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageFilter {
    /// `autoload: false` selects delta-over-global semantics.
    pub autoload: Option<bool>,
    /// Extension patterns (absent = default discovery).
    pub extensions: Option<Vec<String>>,
    /// Skill patterns.
    pub skills: Option<Vec<String>>,
    /// Prompt patterns.
    pub prompts: Option<Vec<String>>,
    /// Theme patterns.
    pub themes: Option<Vec<String>>,
}

impl PackageFilter {
    /// The pattern list for `resource_type`, mirroring `filter[resourceType]`.
    pub fn patterns(&self, resource_type: ResourceType) -> Option<&Vec<String>> {
        match resource_type {
            ResourceType::Extensions => self.extensions.as_ref(),
            ResourceType::Skills => self.skills.as_ref(),
            ResourceType::Prompts => self.prompts.as_ref(),
            ResourceType::Themes => self.themes.as_ref(),
        }
    }
}

/// Compute a numeric precedence rank for a resource based on its metadata.
///
/// Lower rank = higher precedence. Port of pi's `resourcePrecedenceRank`:
///   0  project + settings entry (`source: "local"`, `scope: "project"`)
///   1  project + auto-discovered (`source: "auto"`, `scope: "project"`)
///   2  user + settings entry (`source: "local"`, `scope: "user"`)
///   3  user + auto-discovered (`source: "auto"`, `scope: "user"`)
///   4  package resource (`origin: "package"`)
pub fn resource_precedence_rank(metadata: &PathMetadata) -> i32 {
    if metadata.origin == SourceOrigin::Package {
        return 4;
    }
    let scope_base = if metadata.scope == crate::core::source_info::SourceScope::Project {
        0
    } else {
        2
    };
    scope_base + if metadata.source == "local" { 0 } else { 1 }
}

/// One accumulator entry: provenance plus enabled state.
#[derive(Debug, Clone)]
pub struct ResourceEntry {
    /// Where the path came from.
    pub metadata: PathMetadata,
    /// Whether the path is enabled.
    pub enabled: bool,
}

/// Path-keyed maps collecting resolved resources before precedence sorting.
///
/// Mirrors pi's `ResourceAccumulator`. Uses [`IndexMap`] so insertion order is
/// preserved, matching JS `Map` iteration order (which the stable precedence
/// sort in [`Self::to_resolved_paths`] relies on for tie-breaks).
#[derive(Debug, Default)]
pub struct ResourceAccumulator {
    /// Extension entries keyed by path.
    pub extensions: IndexMap<String, ResourceEntry>,
    /// Skill entries keyed by path.
    pub skills: IndexMap<String, ResourceEntry>,
    /// Prompt entries keyed by path.
    pub prompts: IndexMap<String, ResourceEntry>,
    /// Theme entries keyed by path.
    pub themes: IndexMap<String, ResourceEntry>,
}

impl ResourceAccumulator {
    /// A fresh empty accumulator. Port of pi's `createAccumulator`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The map for `resource_type`. Port of pi's `getTargetMap`.
    pub fn target_map(
        &mut self,
        resource_type: ResourceType,
    ) -> &mut IndexMap<String, ResourceEntry> {
        match resource_type {
            ResourceType::Extensions => &mut self.extensions,
            ResourceType::Skills => &mut self.skills,
            ResourceType::Prompts => &mut self.prompts,
            ResourceType::Themes => &mut self.themes,
        }
    }

    /// Insert a resource, keeping the first entry for a given exact path.
    ///
    /// Port of pi's `addResource` (`if (!map.has(path)) map.set(...)`).
    pub fn add_resource(
        map: &mut IndexMap<String, ResourceEntry>,
        path: &str,
        metadata: &PathMetadata,
        enabled: bool,
    ) {
        if path.is_empty() {
            return;
        }
        if !map.contains_key(path) {
            map.insert(
                path.to_string(),
                ResourceEntry {
                    metadata: metadata.clone(),
                    enabled,
                },
            );
        }
    }

    /// Sort each map by precedence and drop canonical-path duplicates, keeping
    /// the highest-precedence survivor. Port of pi's `toResolvedPaths`.
    pub fn to_resolved_paths(&self) -> ResolvedPaths {
        ResolvedPaths {
            extensions: map_to_resolved(&self.extensions),
            skills: map_to_resolved(&self.skills),
            prompts: map_to_resolved(&self.prompts),
            themes: map_to_resolved(&self.themes),
        }
    }
}

fn map_to_resolved(entries: &IndexMap<String, ResourceEntry>) -> Vec<ResolvedResource> {
    let mut resolved: Vec<ResolvedResource> = entries
        .iter()
        .map(|(path, entry)| ResolvedResource {
            path: path.clone(),
            enabled: entry.enabled,
            metadata: entry.metadata.clone(),
        })
        .collect();

    // Stable sort by precedence rank (JS `Array.prototype.sort` is stable).
    resolved.sort_by_key(|entry| resource_precedence_rank(&entry.metadata));

    let mut seen = std::collections::HashSet::new();
    resolved
        .into_iter()
        .filter(|entry| {
            let canonical = canonicalize_path(&entry.path);
            seen.insert(canonical)
        })
        .collect()
}
