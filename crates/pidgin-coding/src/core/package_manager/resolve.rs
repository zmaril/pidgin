//! The pure-filesystem package resolver.
//!
//! Ports the resolution concern of pi's `DefaultPackageManager`
//! (`core/package-manager.ts`): `resolve` / `resolveExtensionSources`,
//! `resolvePackageSources`, `dedupePackages`, `findAutoloadDeltaBase`,
//! `resolveLocalExtensionSource`, `collectPackageResources` and its filter
//! helpers, `resolveLocalEntries`, `addAutoDiscoveredResources`, the accumulator
//! wiring, and the install-path calculations. The npm/git *install* machinery
//! (pi's `installParsedSource`) is out of scope and sits behind the
//! [`InstallFallback`] seam, which by default reports every missing source as
//! "not found" so a pure-FS resolve simply skips absent packages.

use super::config::{join_path, CONFIG_DIR_NAME};
use super::discovery::{
    collect_ancestor_agents_skill_dirs, collect_auto_extension_entries,
    collect_auto_prompt_entries, collect_auto_theme_entries, collect_resource_files,
    collect_skill_entries, read_pi_manifest_file, SkillDiscoveryMode,
};
use super::patterns::{
    apply_autoload_disabled_patterns, apply_patterns, dirname, glob_sync, has_glob_pattern,
    is_override_pattern, split_patterns,
};
use super::resource::{
    PackageFilter, PiManifest, ResolvedPaths, ResourceAccumulator, ResourceType, RESOURCE_TYPES,
};
use crate::core::source_info::{PathMetadata, SourceOrigin, SourceScope};
use crate::utils::git_url::parse_git_url;
use crate::utils::paths::{is_local_path, resolve_path, PathInputOptions};
use std::path::Path;

/// What to do when a configured package source is missing. Port of pi's
/// `MissingSourceAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSourceAction {
    /// Attempt to install the source (delegated to [`InstallFallback`]).
    Install,
    /// Skip the source silently.
    Skip,
    /// Treat the missing source as an error.
    Error,
}

/// Seam for the out-of-scope npm/git install machinery (pi's
/// `installParsedSource`). The resolver calls [`InstallFallback::install`] when
/// a configured npm/git source is absent (or stale); a `true` result means the
/// source is now present on disk and resolution should re-read its path.
pub trait InstallFallback {
    /// Try to make `source` (already parsed as npm/git) present at `scope`.
    /// Returns `Ok(true)` if it is now installed, `Ok(false)` to skip.
    fn install(&self, source: &str, scope: SourceScope) -> Result<bool, String>;
}

/// Default [`InstallFallback`]: never installs, so absent packages are skipped.
pub struct NoInstall;

impl InstallFallback for NoInstall {
    fn install(&self, _source: &str, _scope: SourceScope) -> Result<bool, String> {
        Ok(false)
    }
}

/// The resource arrays for one settings scope. Mirrors the fields of pi's
/// `Settings` that `resolve` reads.
#[derive(Debug, Clone, Default)]
pub struct ScopeResources {
    /// The `packages` array (string or filter-object entries).
    pub packages: Vec<serde_json::Value>,
    /// Top-level `extensions` entries / patterns.
    pub extensions: Vec<String>,
    /// Top-level `skills` entries / patterns.
    pub skills: Vec<String>,
    /// Top-level `prompts` entries / patterns.
    pub prompts: Vec<String>,
    /// Top-level `themes` entries / patterns.
    pub themes: Vec<String>,
}

impl ScopeResources {
    fn typed(&self, resource_type: ResourceType) -> &[String] {
        match resource_type {
            ResourceType::Extensions => &self.extensions,
            ResourceType::Skills => &self.skills,
            ResourceType::Prompts => &self.prompts,
            ResourceType::Themes => &self.themes,
        }
    }
}

/// The settings snapshot `resolve` consumes: the global and project scopes plus
/// project trust. Constructed directly in tests, or from a `SettingsManager` by
/// the orchestrator.
#[derive(Debug, Clone, Default)]
pub struct ResolveSettings {
    /// User-global scope.
    pub global: ScopeResources,
    /// Project scope (empty when untrusted).
    pub project: ScopeResources,
    /// Whether the project scope is trusted (gates project auto-discovery).
    pub project_trusted: bool,
}

/// A parsed package source: the source string plus its optional filter object.
#[derive(Debug, Clone)]
struct PackageSpec {
    source: String,
    filter: Option<PackageFilter>,
}

fn string_array(value: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    value.get(key)?.as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

fn parse_package_spec(value: &serde_json::Value) -> Option<PackageSpec> {
    if let Some(source) = value.as_str() {
        return Some(PackageSpec {
            source: source.to_string(),
            filter: None,
        });
    }
    if value.is_object() {
        let source = value.get("source")?.as_str()?.to_string();
        let filter = PackageFilter {
            autoload: value.get("autoload").and_then(serde_json::Value::as_bool),
            extensions: string_array(value, "extensions"),
            skills: string_array(value, "skills"),
            prompts: string_array(value, "prompts"),
            themes: string_array(value, "themes"),
        };
        return Some(PackageSpec {
            source,
            filter: Some(filter),
        });
    }
    None
}

/// A parsed source, mirroring pi's `ParsedSource` union.
enum ParsedSource {
    Npm {
        name: String,
    },
    Git {
        host: String,
        path: String,
        pinned: bool,
    },
    Local {
        path: String,
    },
}

/// The pure-filesystem package resolver. Holds the same three inputs pi's
/// `DefaultPackageManager` needs for resolution (`cwd`, `agentDir`, and the home
/// directory used for `.agents` discovery) plus the install seam.
pub struct PackageResolver {
    cwd: String,
    agent_dir: String,
    home_dir: String,
    offline: bool,
    install_fallback: Box<dyn InstallFallback>,
}

impl PackageResolver {
    /// Build a resolver, resolving `cwd` / `agent_dir` the way pi does
    /// (`resolvePath`). `home_dir` defaults to `$HOME`; the install fallback
    /// defaults to [`NoInstall`]; offline mode follows `PI_OFFLINE`.
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        let home_dir = std::env::var("HOME").unwrap_or_default();
        Self {
            cwd: normalize_resolve(cwd),
            agent_dir: normalize_resolve(agent_dir),
            home_dir,
            offline: is_offline_env(),
            install_fallback: Box::new(NoInstall),
        }
    }

    /// Override the home directory used for `~/.agents` discovery (injected in
    /// tests to avoid mutating the process environment).
    pub fn with_home_dir(mut self, home_dir: &str) -> Self {
        self.home_dir = home_dir.to_string();
        self
    }

    /// Install a custom [`InstallFallback`] seam.
    pub fn with_install_fallback(mut self, fallback: Box<dyn InstallFallback>) -> Self {
        self.install_fallback = fallback;
        self
    }

    /// Force offline mode (skips the install fallback for absent sources).
    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Resolve all configured packages and top-level / auto-discovered resources
    /// into [`ResolvedPaths`]. Port of pi's `resolve`.
    pub fn resolve(
        &self,
        settings: &ResolveSettings,
        on_missing: Option<&dyn Fn(&str) -> MissingSourceAction>,
    ) -> ResolvedPaths {
        let mut acc = ResourceAccumulator::new();

        // Project first so cwd resources win collisions.
        let mut all_packages: Vec<(PackageSpec, SourceScope)> = Vec::new();
        for pkg in &settings.project.packages {
            if let Some(spec) = parse_package_spec(pkg) {
                all_packages.push((spec, SourceScope::Project));
            }
        }
        for pkg in &settings.global.packages {
            if let Some(spec) = parse_package_spec(pkg) {
                all_packages.push((spec, SourceScope::User));
            }
        }

        let package_sources = self.dedupe_packages(all_packages);
        self.resolve_package_sources(&package_sources, &mut acc, on_missing);

        let global_base = self.agent_dir.clone();
        let project_base = join_path(&self.cwd, &[CONFIG_DIR_NAME]);

        for resource_type in RESOURCE_TYPES {
            self.resolve_local_entries(
                settings.project.typed(resource_type),
                resource_type,
                &mut acc,
                &meta("local", SourceScope::Project, SourceOrigin::TopLevel, None),
                &project_base,
            );
            self.resolve_local_entries(
                settings.global.typed(resource_type),
                resource_type,
                &mut acc,
                &meta("local", SourceScope::User, SourceOrigin::TopLevel, None),
                &global_base,
            );
        }

        self.add_auto_discovered_resources(&mut acc, settings, &global_base, &project_base);

        acc.to_resolved_paths()
    }

    /// Resolve explicit extension sources (pi's `resolveExtensionSources`).
    pub fn resolve_extension_sources(
        &self,
        sources: &[String],
        local: bool,
        temporary: bool,
    ) -> ResolvedPaths {
        let mut acc = ResourceAccumulator::new();
        let scope = if temporary {
            SourceScope::Temporary
        } else if local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        let specs: Vec<(PackageSpec, SourceScope)> = sources
            .iter()
            .map(|source| {
                (
                    PackageSpec {
                        source: source.clone(),
                        filter: None,
                    },
                    scope,
                )
            })
            .collect();
        self.resolve_package_sources(&specs, &mut acc, None);
        acc.to_resolved_paths()
    }

    fn resolve_package_sources(
        &self,
        sources: &[(PackageSpec, SourceScope)],
        acc: &mut ResourceAccumulator,
        on_missing: Option<&dyn Fn(&str) -> MissingSourceAction>,
    ) {
        for (spec, scope) in sources {
            let source_str = spec.source.clone();
            let filter = spec.filter.as_ref();
            let delta = self.find_autoload_delta_base(spec, *scope, sources);
            let (resolved_source, resolved_scope) = match delta {
                Some((src, sc)) => (src, sc),
                None => (source_str.clone(), *scope),
            };
            let parsed = self.parse_source(&resolved_source);
            let mut metadata = meta(&source_str, *scope, SourceOrigin::Package, None);

            match parsed {
                ParsedSource::Local { path } => {
                    let base = self.base_dir_for_scope(resolved_scope);
                    self.resolve_local_extension_source(&path, acc, filter, &mut metadata, &base);
                }
                ParsedSource::Npm { name } => {
                    let mut installed_path = self.npm_install_path(&name, resolved_scope);
                    let needs_install = !Path::new(&installed_path).exists()
                        || !self.installed_npm_matches(&resolved_source, &installed_path);
                    if needs_install {
                        if !self.install_missing(&resolved_source, resolved_scope, on_missing) {
                            continue;
                        }
                        installed_path = self.npm_install_path(&name, resolved_scope);
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, acc, filter, &metadata);
                }
                ParsedSource::Git { host, path, pinned } => {
                    let installed_path = self.git_install_path(&host, &path, resolved_scope);
                    if !Path::new(&installed_path).exists() {
                        if !self.install_missing(&resolved_source, resolved_scope, on_missing) {
                            continue;
                        }
                    } else if resolved_scope == SourceScope::Temporary && !pinned && !self.offline {
                        // pi refreshes temporary git checkouts here; that is the
                        // out-of-scope command concern, so we leave the checkout
                        // as-is and resolve its files.
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, acc, filter, &metadata);
                }
            }
        }
    }

    fn install_missing(
        &self,
        source: &str,
        scope: SourceScope,
        on_missing: Option<&dyn Fn(&str) -> MissingSourceAction>,
    ) -> bool {
        if self.offline {
            return false;
        }
        let action = match on_missing {
            Some(cb) => cb(source),
            None => MissingSourceAction::Install,
        };
        match action {
            MissingSourceAction::Skip | MissingSourceAction::Error => false,
            MissingSourceAction::Install => self
                .install_fallback
                .install(source, scope)
                .unwrap_or(false),
        }
    }

    fn find_autoload_delta_base(
        &self,
        spec: &PackageSpec,
        scope: SourceScope,
        sources: &[(PackageSpec, SourceScope)],
    ) -> Option<(String, SourceScope)> {
        if scope != SourceScope::Project {
            return None;
        }
        let filter = spec.filter.as_ref()?;
        if filter.autoload != Some(false) {
            return None;
        }
        let identity = self.package_identity(&spec.source, Some(scope));
        sources
            .iter()
            .find(|(entry, entry_scope)| {
                *entry_scope == SourceScope::User
                    && self.package_identity(&entry.source, Some(SourceScope::User)) == identity
            })
            .map(|(entry, _)| (entry.source.clone(), SourceScope::User))
    }

    fn resolve_local_extension_source(
        &self,
        source_path: &str,
        acc: &mut ResourceAccumulator,
        filter: Option<&PackageFilter>,
        metadata: &mut PathMetadata,
        base_dir: &str,
    ) {
        let resolved = self.resolve_path_from_base(source_path, base_dir);
        let path = Path::new(&resolved);
        let Ok(meta_fs) = std::fs::metadata(&resolved) else {
            return;
        };
        if meta_fs.is_file() {
            metadata.base_dir = Some(dirname(&resolved));
            ResourceAccumulator::add_resource(&mut acc.extensions, &resolved, metadata, true);
            return;
        }
        if meta_fs.is_dir() && path.exists() {
            metadata.base_dir = Some(resolved.clone());
            let has_resources = self.collect_package_resources(&resolved, acc, filter, metadata);
            if !has_resources {
                ResourceAccumulator::add_resource(&mut acc.extensions, &resolved, metadata, true);
            }
        }
    }

    fn collect_package_resources(
        &self,
        package_root: &str,
        acc: &mut ResourceAccumulator,
        filter: Option<&PackageFilter>,
        metadata: &PathMetadata,
    ) -> bool {
        if let Some(filter) = filter {
            for resource_type in RESOURCE_TYPES {
                let patterns = filter.patterns(resource_type);
                if filter.autoload == Some(false) {
                    self.apply_package_delta_filter(
                        package_root,
                        patterns.map(Vec::as_slice).unwrap_or(&[]),
                        resource_type,
                        acc,
                        metadata,
                    );
                } else if let Some(patterns) = patterns {
                    self.apply_package_filter(package_root, patterns, resource_type, acc, metadata);
                } else {
                    self.collect_default_resources(package_root, resource_type, acc, metadata);
                }
            }
            return true;
        }

        if let Some(manifest) = self.read_pi_manifest(package_root) {
            for resource_type in RESOURCE_TYPES {
                self.add_manifest_entries(
                    manifest.entries(resource_type),
                    package_root,
                    resource_type,
                    acc,
                    metadata,
                );
            }
            return true;
        }

        let mut has_any_dir = false;
        for resource_type in RESOURCE_TYPES {
            if self.collect_convention_dir(package_root, resource_type, acc, metadata) {
                has_any_dir = true;
            }
        }
        has_any_dir
    }

    /// Collect every file under `<package_root>/<resourceType>` (all enabled),
    /// returning whether that convention directory existed. Shared by
    /// `collectPackageResources`' and `collectDefaultResources`' fallbacks.
    fn collect_convention_dir(
        &self,
        package_root: &str,
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
    ) -> bool {
        let dir = join_path(package_root, &[resource_type.key()]);
        if !Path::new(&dir).exists() {
            return false;
        }
        let files = collect_resource_files(&dir, resource_type);
        let map = acc.target_map(resource_type);
        for f in files {
            ResourceAccumulator::add_resource(map, &f, metadata, true);
        }
        true
    }

    fn collect_default_resources(
        &self,
        package_root: &str,
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
    ) {
        if let Some(manifest) = self.read_pi_manifest(package_root) {
            if let Some(entries) = manifest.entries(resource_type) {
                self.add_manifest_entries(
                    Some(entries),
                    package_root,
                    resource_type,
                    acc,
                    metadata,
                );
                return;
            }
        }
        self.collect_convention_dir(package_root, resource_type, acc, metadata);
    }

    fn apply_package_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
    ) {
        let all_files = self.collect_manifest_files(package_root, resource_type);
        let map = acc.target_map(resource_type);
        if user_patterns.is_empty() {
            for f in &all_files {
                ResourceAccumulator::add_resource(map, f, metadata, false);
            }
            return;
        }
        let enabled = apply_patterns(&all_files, user_patterns, package_root);
        for f in &all_files {
            ResourceAccumulator::add_resource(map, f, metadata, enabled.contains(f));
        }
    }

    fn apply_package_delta_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
    ) {
        if user_patterns.is_empty() {
            return;
        }
        let all_files = self.collect_manifest_files(package_root, resource_type);
        let deltas = apply_autoload_disabled_patterns(&all_files, user_patterns, package_root);
        let map = acc.target_map(resource_type);
        for (file_path, enabled) in deltas {
            ResourceAccumulator::add_resource(map, &file_path, metadata, enabled);
        }
    }

    /// Port of pi's `collectManifestFiles` (returns only the manifest-enabled
    /// files, ready for user-pattern layering).
    fn collect_manifest_files(
        &self,
        package_root: &str,
        resource_type: ResourceType,
    ) -> Vec<String> {
        if let Some(manifest) = self.read_pi_manifest(package_root) {
            if let Some(entries) = manifest.entries(resource_type) {
                if !entries.is_empty() {
                    let all_files = self.collect_files_from_manifest_entries(
                        entries,
                        package_root,
                        resource_type,
                    );
                    let manifest_patterns: Vec<String> = entries
                        .iter()
                        .filter(|e| is_override_pattern(e))
                        .cloned()
                        .collect();
                    if manifest_patterns.is_empty() {
                        return all_files;
                    }
                    let enabled = apply_patterns(&all_files, &manifest_patterns, package_root);
                    return all_files
                        .into_iter()
                        .filter(|f| enabled.contains(f))
                        .collect();
                }
            }
        }

        let convention_dir = join_path(package_root, &[resource_type.key()]);
        if !Path::new(&convention_dir).exists() {
            return Vec::new();
        }
        collect_resource_files(&convention_dir, resource_type)
    }

    fn read_pi_manifest(&self, package_root: &str) -> Option<PiManifest> {
        let package_json = join_path(package_root, &["package.json"]);
        if !Path::new(&package_json).exists() {
            return None;
        }
        read_pi_manifest_file(&package_json)
    }

    fn add_manifest_entries(
        &self,
        entries: Option<&Vec<String>>,
        root: &str,
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
    ) {
        let Some(entries) = entries else {
            return;
        };
        let all_files = self.collect_files_from_manifest_entries(entries, root, resource_type);
        let patterns: Vec<String> = entries
            .iter()
            .filter(|e| is_override_pattern(e))
            .cloned()
            .collect();
        let enabled = apply_patterns(&all_files, &patterns, root);
        let map = acc.target_map(resource_type);
        for f in &all_files {
            if enabled.contains(f) {
                ResourceAccumulator::add_resource(map, f, metadata, true);
            }
        }
    }

    fn collect_files_from_manifest_entries(
        &self,
        entries: &[String],
        root: &str,
        resource_type: ResourceType,
    ) -> Vec<String> {
        let mut resolved = Vec::new();
        for entry in entries.iter().filter(|e| !is_override_pattern(e)) {
            if !has_glob_pattern(entry) {
                resolved.push(path_resolve(root, entry));
            } else {
                resolved.extend(glob_sync(root, entry));
            }
        }
        self.collect_files_from_paths(&resolved, resource_type)
    }

    fn resolve_local_entries(
        &self,
        entries: &[String],
        resource_type: ResourceType,
        acc: &mut ResourceAccumulator,
        metadata: &PathMetadata,
        base_dir: &str,
    ) {
        if entries.is_empty() {
            return;
        }
        let (plain, patterns) = split_patterns(entries);
        let resolved_plain: Vec<String> = plain
            .iter()
            .map(|p| self.resolve_path_from_base(p, base_dir))
            .collect();
        let all_files = self.collect_files_from_paths(&resolved_plain, resource_type);
        let enabled = apply_patterns(&all_files, &patterns, base_dir);
        let map = acc.target_map(resource_type);
        for f in &all_files {
            ResourceAccumulator::add_resource(map, f, metadata, enabled.contains(f));
        }
    }

    fn collect_files_from_paths(
        &self,
        paths: &[String],
        resource_type: ResourceType,
    ) -> Vec<String> {
        let mut files = Vec::new();
        for p in paths {
            let Ok(meta_fs) = std::fs::metadata(p) else {
                continue;
            };
            if meta_fs.is_file() {
                files.push(p.clone());
            } else if meta_fs.is_dir() {
                files.extend(collect_resource_files(p, resource_type));
            }
        }
        files
    }

    #[allow(clippy::too_many_arguments)]
    fn add_resources(
        &self,
        acc: &mut ResourceAccumulator,
        resource_type: ResourceType,
        paths: &[String],
        metadata: &PathMetadata,
        overrides: &[String],
        base_dir: &str,
    ) {
        let map = acc.target_map(resource_type);
        for path in paths {
            let enabled = super::patterns::is_enabled_by_overrides(path, overrides, base_dir);
            ResourceAccumulator::add_resource(map, path, metadata, enabled);
        }
    }

    fn add_auto_discovered_resources(
        &self,
        acc: &mut ResourceAccumulator,
        settings: &ResolveSettings,
        global_base: &str,
        project_base: &str,
    ) {
        let user_meta = meta(
            "auto",
            SourceScope::User,
            SourceOrigin::TopLevel,
            Some(global_base.to_string()),
        );
        let project_meta = meta(
            "auto",
            SourceScope::Project,
            SourceOrigin::TopLevel,
            Some(project_base.to_string()),
        );

        let user_agents_skills_dir = join_path(&self.home_dir, &[".agents", "skills"]);
        let project_trusted = settings.project_trusted;
        let project_agents_skill_dirs: Vec<String> = if project_trusted {
            collect_ancestor_agents_skill_dirs(&self.cwd)
                .into_iter()
                .filter(|dir| {
                    path_resolve(&self.cwd, dir) != path_resolve(&self.cwd, &user_agents_skills_dir)
                })
                .collect()
        } else {
            Vec::new()
        };

        if project_trusted {
            self.add_resources(
                acc,
                ResourceType::Extensions,
                &collect_auto_extension_entries(&join_path(project_base, &["extensions"])),
                &project_meta,
                &settings.project.extensions,
                project_base,
            );
            self.add_resources(
                acc,
                ResourceType::Skills,
                &collect_skill_entries(
                    &join_path(project_base, &["skills"]),
                    SkillDiscoveryMode::Pi,
                ),
                &project_meta,
                &settings.project.skills,
                project_base,
            );
        }

        for agents_skills_dir in &project_agents_skill_dirs {
            let agents_base = dirname(agents_skills_dir);
            let agents_meta = meta(
                "auto",
                SourceScope::Project,
                SourceOrigin::TopLevel,
                Some(agents_base.clone()),
            );
            self.add_resources(
                acc,
                ResourceType::Skills,
                &collect_skill_entries(agents_skills_dir, SkillDiscoveryMode::Agents),
                &agents_meta,
                &settings.project.skills,
                &agents_base,
            );
        }

        if project_trusted {
            self.add_resources(
                acc,
                ResourceType::Prompts,
                &collect_auto_prompt_entries(&join_path(project_base, &["prompts"])),
                &project_meta,
                &settings.project.prompts,
                project_base,
            );
            self.add_resources(
                acc,
                ResourceType::Themes,
                &collect_auto_theme_entries(&join_path(project_base, &["themes"])),
                &project_meta,
                &settings.project.themes,
                project_base,
            );
        }

        self.add_resources(
            acc,
            ResourceType::Extensions,
            &collect_auto_extension_entries(&join_path(global_base, &["extensions"])),
            &user_meta,
            &settings.global.extensions,
            global_base,
        );
        self.add_resources(
            acc,
            ResourceType::Skills,
            &collect_skill_entries(&join_path(global_base, &["skills"]), SkillDiscoveryMode::Pi),
            &user_meta,
            &settings.global.skills,
            global_base,
        );

        let user_agents_base = dirname(&user_agents_skills_dir);
        let user_agents_meta = meta(
            "auto",
            SourceScope::User,
            SourceOrigin::TopLevel,
            Some(user_agents_base.clone()),
        );
        self.add_resources(
            acc,
            ResourceType::Skills,
            &collect_skill_entries(&user_agents_skills_dir, SkillDiscoveryMode::Agents),
            &user_agents_meta,
            &settings.global.skills,
            &user_agents_base,
        );
        self.add_resources(
            acc,
            ResourceType::Prompts,
            &collect_auto_prompt_entries(&join_path(global_base, &["prompts"])),
            &user_meta,
            &settings.global.prompts,
            global_base,
        );
        self.add_resources(
            acc,
            ResourceType::Themes,
            &collect_auto_theme_entries(&join_path(global_base, &["themes"])),
            &user_meta,
            &settings.global.themes,
            global_base,
        );
    }

    // -- dedupe / identity / parsing ---------------------------------------

    fn dedupe_packages(
        &self,
        packages: Vec<(PackageSpec, SourceScope)>,
    ) -> Vec<(PackageSpec, SourceScope)> {
        let mut result: Vec<(PackageSpec, SourceScope)> = Vec::new();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (spec, scope) in packages {
            let identity = self.package_identity(&spec.source, Some(scope));
            match seen.get(&identity) {
                None => {
                    seen.insert(identity, result.len());
                    result.push((spec, scope));
                }
                Some(&index) => {
                    let existing_scope = result[index].1;
                    let existing_autoload_false = result[index]
                        .0
                        .filter
                        .as_ref()
                        .is_some_and(|f| f.autoload == Some(false));
                    if existing_scope == SourceScope::Project && scope == SourceScope::User {
                        if existing_autoload_false {
                            result.push((spec, scope));
                        }
                    } else if scope == SourceScope::Project {
                        result[index] = (spec, scope);
                    }
                }
            }
        }
        result
    }

    /// Compute a package's dedupe identity. Port of pi's `getPackageIdentity`:
    /// npm sources key on name, git on `host/path` (normalizing SSH vs HTTPS),
    /// and local paths on the base-resolved path (or cwd-resolved when no scope).
    pub fn package_identity(&self, source: &str, scope: Option<SourceScope>) -> String {
        match self.parse_source(source) {
            ParsedSource::Npm { name } => format!("npm:{name}"),
            ParsedSource::Git { host, path, .. } => format!("git:{host}/{path}"),
            ParsedSource::Local { path } => match scope {
                Some(scope) => {
                    let base = self.base_dir_for_scope(scope);
                    format!("local:{}", self.resolve_path_from_base(&path, &base))
                }
                None => format!("local:{}", self.resolve_path(&path)),
            },
        }
    }

    fn parse_source(&self, source: &str) -> ParsedSource {
        if let Some(spec) = source.strip_prefix("npm:") {
            let spec = spec.trim();
            let name = parse_npm_name(spec);
            return ParsedSource::Npm { name };
        }
        if is_local_path(source) {
            return ParsedSource::Local {
                path: source.to_string(),
            };
        }
        if let Some(git) = parse_git_url(source) {
            return ParsedSource::Git {
                host: git.host,
                path: git.path,
                pinned: git.pinned,
            };
        }
        ParsedSource::Local {
            path: source.to_string(),
        }
    }

    fn installed_npm_matches(&self, source: &str, installed_path: &str) -> bool {
        let installed_version = installed_npm_version(installed_path);
        let Some(installed_version) = installed_version else {
            return false;
        };
        let range = source
            .strip_prefix("npm:")
            .and_then(|spec| npm_version_from_spec(spec.trim()));
        match range {
            Some(range) => version_satisfies(&installed_version, &range),
            None => true,
        }
    }

    // -- path calculations --------------------------------------------------

    fn base_dir_for_scope(&self, scope: SourceScope) -> String {
        match scope {
            SourceScope::Project => join_path(&self.cwd, &[CONFIG_DIR_NAME]),
            SourceScope::User => self.agent_dir.clone(),
            SourceScope::Temporary => self.cwd.clone(),
        }
    }

    fn resolve_path(&self, input: &str) -> String {
        let options = PathInputOptions {
            trim: true,
            home_dir: Some(self.home_dir.clone()),
            ..PathInputOptions::default()
        };
        resolve_path(input, &self.cwd, &options).unwrap_or_else(|_| input.to_string())
    }

    fn resolve_path_from_base(&self, input: &str, base_dir: &str) -> String {
        let options = PathInputOptions {
            trim: true,
            home_dir: Some(self.home_dir.clone()),
            ..PathInputOptions::default()
        };
        resolve_path(input, base_dir, &options).unwrap_or_else(|_| input.to_string())
    }

    fn npm_install_path(&self, name: &str, scope: SourceScope) -> String {
        // Managed path; pi's user-scope legacy fallback needs `npm root -g`
        // (the out-of-scope command concern), so we always use the managed root.
        match scope {
            SourceScope::Temporary => {
                join_path(&self.temporary_dir("npm", None), &["node_modules", name])
            }
            SourceScope::Project => {
                join_path(&self.cwd, &[CONFIG_DIR_NAME, "npm", "node_modules", name])
            }
            SourceScope::User => join_path(&self.agent_dir, &["npm", "node_modules", name]),
        }
    }

    fn git_install_path(&self, host: &str, path: &str, scope: SourceScope) -> String {
        if scope == SourceScope::Temporary {
            return self.temporary_dir(&format!("git-{host}"), Some(path));
        }
        let install_root = match scope {
            SourceScope::Project => join_path(&self.cwd, &[CONFIG_DIR_NAME, "git"]),
            _ => join_path(&self.agent_dir, &["git"]),
        };
        join_path(&install_root, &[host, path])
    }

    fn temporary_dir(&self, prefix: &str, suffix: Option<&str>) -> String {
        let temp_folder = extension_temp_folder(&self.agent_dir);
        let root = join_path(&temp_folder, &[prefix]);
        let hash = short_sha256(&format!("{prefix}-{}", suffix.unwrap_or("")));
        join_path(&root, &[&hash, suffix.unwrap_or("")])
    }
}

// -- free helpers -----------------------------------------------------------

/// Node-`path.resolve(base, part)` semantics with NO tilde expansion (manifest
/// entries are package-relative, so a leading `~` stays literal). Exposed to
/// `discovery` for `resolveExtensionEntries`.
pub(crate) fn path_resolve(base: &str, part: &str) -> String {
    let options = PathInputOptions {
        trim: false,
        expand_tilde: false,
        ..PathInputOptions::default()
    };
    resolve_path(part, base, &options).unwrap_or_else(|_| join_path(base, &[part]))
}

fn normalize_resolve(input: &str) -> String {
    let options = PathInputOptions {
        trim: true,
        ..PathInputOptions::default()
    };
    resolve_path(input, &current_dir(), &options).unwrap_or_else(|_| input.to_string())
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn meta(
    source: &str,
    scope: SourceScope,
    origin: SourceOrigin,
    base_dir: Option<String>,
) -> PathMetadata {
    PathMetadata {
        source: source.to_string(),
        scope,
        origin,
        base_dir,
    }
}

fn is_offline_env() -> bool {
    match std::env::var("PI_OFFLINE") {
        Ok(value) if !value.is_empty() => {
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        }
        _ => false,
    }
}

/// pi's `parseNpmSpec` name extraction (`@scope/name@version` -> name).
fn parse_npm_name(spec: &str) -> String {
    // Mirror the regex `^(@?[^@]+(?:/[^@]+)?)(?:@(.+))?$`: the name is
    // everything up to a version `@` that is not the scope's leading `@`.
    let (scope_at, rest) = if let Some(stripped) = spec.strip_prefix('@') {
        (true, stripped)
    } else {
        (false, spec)
    };
    let name_body = match rest.find('@') {
        Some(idx) => &rest[..idx],
        None => rest,
    };
    if scope_at {
        format!("@{name_body}")
    } else {
        name_body.to_string()
    }
}

fn npm_version_from_spec(spec: &str) -> Option<String> {
    let rest = spec.strip_prefix('@').unwrap_or(spec);
    rest.find('@').map(|idx| rest[idx + 1..].to_string())
}

fn installed_npm_version(installed_path: &str) -> Option<String> {
    let package_json = join_path(installed_path, &["package.json"]);
    let content = std::fs::read_to_string(package_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn version_satisfies(version: &str, range: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::VersionReq::parse(range),
    ) {
        (Ok(v), Ok(req)) => req.matches(&v),
        _ => true,
    }
}

fn extension_temp_folder(agent_dir: &str) -> String {
    let temp_folder = join_path(agent_dir, &["tmp", "extensions"]);
    let _ = std::fs::create_dir_all(&temp_folder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_folder, std::fs::Permissions::from_mode(0o700));
    }
    temp_folder
}

fn short_sha256(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}
