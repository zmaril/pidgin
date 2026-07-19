//! Stateful `DefaultResourceLoader` orchestrator, ported from pi's
//! `core/resource-loader.ts` (`class DefaultResourceLoader`, ~line 159).
//!
//! pi's orchestrator wires the settings manager, package manager, extension
//! runtime and the skill/prompt/theme loaders, then caches the resolved results
//! and decorates each with provenance. The **pure discovery/precedence engine**
//! it delegates to already landed as [`crate::core::resource_loader`] (PR #100),
//! the pure-filesystem package resolver as [`crate::core::package_manager`]
//! `PackageResolver` (#129), and the runtime theme loader as
//! [`crate::modes::interactive::theme::runtime`] (#136); this module adds the
//! stateful shell (`reload()` + `extendResources`) around all three.
//!
//! # What is ported
//! * the [`DefaultResourceLoader`] struct shell — every pi field: roots,
//!   `SettingsManager`, `EventBus`, `PackageResolver`, the config/override
//!   options, the seven override closures, and the ~20 cached-result fields;
//! * the [`ExtensionLoader`] trait seam (held as `Box<dyn ExtensionLoader>`,
//!   defaulting to [`StubExtensionLoader`]);
//! * [`DefaultResourceLoader::reload`] — the full pi `reload()`:
//!   `settingsManager.reload()`, `packageManager.resolve()` /
//!   `resolveExtensionSources()`, skills / prompts / themes discovery, context
//!   files + system/append-system-prompt discovery, the seven override
//!   closures, and the trust two-pass (`loadFinalExtensionSet`) that threads a
//!   single `Option<Box<dyn ExtensionRuntime>>` handle across the pre- and
//!   post-trust passes, deduped by `resolved_path`;
//! * [`DefaultResourceLoader::extend_resources`] — the full pi
//!   `extendResources` for skills, prompts **and** themes.
//!
//! # What the seam still stubs
//! The `Box<dyn ExtensionLoader>` defaults to [`StubExtensionLoader`], which
//! returns an empty `LoadExtensionsResult`. pi's real `loadExtensionsCached` /
//! `createExtensionRuntime` is a `jiti` dynamic-TS host owned by the
//! extension-plane session (blocker **A**). `reload()` wires the trust two-pass,
//! runtime threading, dedup and conflict detection **faithfully** so the real
//! loader drops in behind the seam; inline extension factories
//! (`loadExtensionFactories`) likewise need the real host and are a no-op here.
//!
//! See the `wi3-orchestrator-blockers-ownership` team memory for the full
//! ownership map.

// straitjacket-allow-file:duplication

use std::path::Path;

use crate::core::diagnostics::{DiagnosticType, ResourceDiagnostic, ResourceType};
use crate::core::event_bus::EventBus;
use crate::core::extensions::loader::{
    create_extension_runtime, Extension, ExtensionLoadError, ExtensionLoader, LoadExtensionsResult,
    StubExtensionLoader,
};
use crate::core::package_manager::{
    PackageResolver, ResolveSettings, ResolvedResource, ScopeResources,
};
use crate::core::prompt_templates::{
    self, load_prompt_templates, LoadPromptTemplatesOptions, PromptTemplate,
};
use crate::core::resource_loader::{
    dedupe_named, detect_extension_conflicts, load_project_context_files, resolve_prompt_input,
    ContextFile, ExtensionConflictInput, ExtensionPathEntry, ResourceLoader,
};
use crate::core::settings_manager::{Settings, SettingsManager};
use crate::core::skills::{self, load_skills, LoadSkillsOptions, LoadSkillsResult, Skill};
use crate::core::source_info::{self, PathMetadata, SourceInfo, SourceOrigin, SourceScope};
use crate::modes::interactive::theme::{load_theme_from_path, Theme};
use crate::utils::paths::is_local_path;

/// Convert the discovery engine's [`SourceInfo`] into the `skills` module's
/// parallel `SourceInfo` type (the loaders predate the shared `source_info`
/// module and each carry a structurally-identical copy). Faithful-mirror
/// duplication, hence the file-level straitjacket allowance.
fn to_skill_source_info(si: SourceInfo) -> skills::SourceInfo {
    skills::SourceInfo {
        path: si.path,
        source: si.source,
        scope: match si.scope {
            SourceScope::User => skills::SourceScope::User,
            SourceScope::Project => skills::SourceScope::Project,
            SourceScope::Temporary => skills::SourceScope::Temporary,
        },
        origin: match si.origin {
            SourceOrigin::Package => skills::SourceOrigin::Package,
            SourceOrigin::TopLevel => skills::SourceOrigin::TopLevel,
        },
        base_dir: si.base_dir,
    }
}

/// Convert the discovery engine's [`SourceInfo`] into the `prompt_templates`
/// module's parallel `SourceInfo` type. See [`to_skill_source_info`].
fn to_prompt_source_info(si: SourceInfo) -> prompt_templates::SourceInfo {
    prompt_templates::SourceInfo {
        path: si.path,
        source: si.source,
        scope: match si.scope {
            SourceScope::User => prompt_templates::SourceScope::User,
            SourceScope::Project => prompt_templates::SourceScope::Project,
            SourceScope::Temporary => prompt_templates::SourceScope::Temporary,
        },
        origin: match si.origin {
            SourceOrigin::Package => prompt_templates::SourceOrigin::Package,
            SourceOrigin::TopLevel => prompt_templates::SourceOrigin::TopLevel,
        },
        base_dir: si.base_dir,
    }
}

/// Build a [`ScopeResources`] from one settings scope's raw JSON bag, reading
/// the `packages` / `extensions` / `skills` / `prompts` / `themes` keys the way
/// pi's `DefaultPackageManager.resolve` reads them off the settings.
fn scope_from_settings(settings: &Settings) -> ScopeResources {
    let map = settings.as_map();
    let str_array = |key: &str| -> Vec<String> {
        map.get(key)
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    ScopeResources {
        packages: map
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
        extensions: str_array("extensions"),
        skills: str_array("skills"),
        prompts: str_array("prompts"),
        themes: str_array("themes"),
    }
}

/// The enabled paths of a resolved-resource slice (pi's `getEnabledPaths`).
fn enabled_paths(resources: &[ResolvedResource]) -> Vec<String> {
    resources
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.path.clone())
        .collect()
}

/// Insert `(key, metadata)` only when `key` is absent, mirroring pi's
/// `if (!metadataByPath.has(r.path)) metadataByPath.set(...)` (first wins,
/// insertion order preserved for deterministic prefix-match precedence).
fn md_set_if_absent(list: &mut Vec<(String, PathMetadata)>, key: String, md: PathMetadata) {
    if !list.iter().any(|(k, _)| *k == key) {
        list.push((key, md));
    }
}

/// Result view returned by [`DefaultResourceLoader::get_prompts`], mirroring
/// pi's `{ prompts, diagnostics }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptsResult {
    /// The loaded prompt templates.
    pub prompts: Vec<PromptTemplate>,
    /// Load / collision diagnostics.
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// Result view returned by [`DefaultResourceLoader::get_themes`], mirroring
/// pi's `{ themes, diagnostics }`. Runtime [`Theme`] is not `PartialEq`, so this
/// carries only `Debug`/`Clone`/`Default`.
#[derive(Debug, Clone, Default)]
pub struct ThemesResult {
    /// The loaded runtime themes.
    pub themes: Vec<Theme>,
    /// Load / collision diagnostics.
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// The `{ skillPaths?, promptPaths?, themePaths? }` argument pi's
/// `extendResources` accepts.
#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    /// Extra skill `{ path, metadata }` entries to merge in.
    pub skill_paths: Vec<ExtensionPathEntry>,
    /// Extra prompt `{ path, metadata }` entries to merge in.
    pub prompt_paths: Vec<ExtensionPathEntry>,
    /// Extra theme `{ path, metadata }` entries to merge in.
    pub theme_paths: Vec<ExtensionPathEntry>,
}

type SkillsOverride = Box<dyn Fn(LoadSkillsResult) -> LoadSkillsResult>;
type PromptsOverride = Box<dyn Fn(PromptsResult) -> PromptsResult>;
type ThemesOverride = Box<dyn Fn(ThemesResult) -> ThemesResult>;
type ExtensionsOverride = Box<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult>;
type AgentsFilesOverride = Box<dyn Fn(Vec<ContextFile>) -> Vec<ContextFile>>;
type SystemPromptOverride = Box<dyn Fn(Option<String>) -> Option<String>>;
type AppendSystemPromptOverride = Box<dyn Fn(Vec<String>) -> Vec<String>>;
type ResolveProjectTrust = Box<dyn Fn(&LoadExtensionsResult) -> bool>;

/// Options for [`DefaultResourceLoader::reload`], mirroring pi's
/// `ResourceLoaderReloadOptions`.
#[derive(Default)]
pub struct ReloadOptions {
    /// The trust-resolution callback. When present, `reload()` runs the pre-trust
    /// pass first (loading only user/global + temporary extensions), invokes this
    /// with the pre-trust result to decide project trust, then continues with the
    /// resolved trust state. Mirrors pi's async `resolveProjectTrust`.
    pub resolve_project_trust: Option<ResolveProjectTrust>,
}

/// Construction options, mirroring pi's `DefaultResourceLoaderOptions`.
#[derive(Default)]
pub struct DefaultResourceLoaderOptions {
    /// The working-directory root.
    pub cwd: String,
    /// The agent-config directory root.
    pub agent_dir: String,
    /// Injected settings manager; defaults to `SettingsManager::create`.
    pub settings_manager: Option<SettingsManager>,
    /// Injected event bus; defaults to a fresh bus.
    pub event_bus: Option<EventBus>,
    /// Injected extension loader; defaults to [`StubExtensionLoader`].
    pub extension_loader: Option<Box<dyn ExtensionLoader>>,
    /// Home directory used by the package resolver for `~/.agents` discovery.
    /// A **test seam** (pi reads `$HOME`); injecting it keeps `reload()` tests
    /// off the ambient home dir. `None` falls back to `$HOME`.
    pub home_dir: Option<String>,
    /// Extra CLI extension paths.
    pub additional_extension_paths: Vec<String>,
    /// Extra CLI skill paths.
    pub additional_skill_paths: Vec<String>,
    /// Extra CLI prompt-template paths.
    pub additional_prompt_template_paths: Vec<String>,
    /// Extra CLI theme paths.
    pub additional_theme_paths: Vec<String>,
    /// Disable auto-discovered extensions (CLI paths still load).
    pub no_extensions: bool,
    /// Disable auto-discovered skills (additional paths still load).
    pub no_skills: bool,
    /// Disable auto-discovered prompt templates.
    pub no_prompt_templates: bool,
    /// Disable auto-discovered themes.
    pub no_themes: bool,
    /// Disable AGENTS.md / CLAUDE.md context-file discovery.
    pub no_context_files: bool,
    /// Explicit system-prompt source (path or literal).
    pub system_prompt: Option<String>,
    /// Explicit append-system-prompt sources.
    pub append_system_prompt: Option<Vec<String>>,
    /// Override for the resolved extension set.
    pub extensions_override: Option<ExtensionsOverride>,
    /// Override for the resolved skills.
    pub skills_override: Option<SkillsOverride>,
    /// Override for the resolved prompts.
    pub prompts_override: Option<PromptsOverride>,
    /// Override for the resolved themes.
    pub themes_override: Option<ThemesOverride>,
    /// Override for the resolved context files.
    pub agents_files_override: Option<AgentsFilesOverride>,
    /// Override for the resolved system prompt.
    pub system_prompt_override: Option<SystemPromptOverride>,
    /// Override for the resolved append-system prompts.
    pub append_system_prompt_override: Option<AppendSystemPromptOverride>,
}

/// The stateful resource-loader orchestrator. Port of pi's
/// `DefaultResourceLoader`. Fields mirror pi 1:1.
pub struct DefaultResourceLoader {
    // -- roots + collaborators ------------------------------------------------
    /// The pure discovery/precedence engine (landed), rebuilt on trust change.
    discovery: ResourceLoader,
    cwd: String,
    agent_dir: String,
    settings_manager: SettingsManager,
    event_bus: EventBus,
    package_resolver: PackageResolver,
    extension_loader: Box<dyn ExtensionLoader>,

    // -- config / override options --------------------------------------------
    additional_extension_paths: Vec<String>,
    additional_skill_paths: Vec<String>,
    additional_prompt_template_paths: Vec<String>,
    additional_theme_paths: Vec<String>,
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    no_themes: bool,
    no_context_files: bool,
    system_prompt_source: Option<String>,
    append_system_prompt_source: Option<Vec<String>>,
    extensions_override: Option<ExtensionsOverride>,
    skills_override: Option<SkillsOverride>,
    prompts_override: Option<PromptsOverride>,
    themes_override: Option<ThemesOverride>,
    agents_files_override: Option<AgentsFilesOverride>,
    system_prompt_override: Option<SystemPromptOverride>,
    append_system_prompt_override: Option<AppendSystemPromptOverride>,

    // -- cached results -------------------------------------------------------
    extensions_result: LoadExtensionsResult,
    skills: Vec<Skill>,
    // `skills` carries its own parallel `ResourceDiagnostic` (predates the
    // shared `diagnostics` module), so the skill diagnostics use that type.
    skill_diagnostics: Vec<skills::ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Theme>,
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
    last_skill_paths: Vec<String>,
    last_prompt_paths: Vec<String>,
    last_theme_paths: Vec<String>,
    extension_skill_source_infos: Vec<(String, SourceInfo)>,
    extension_prompt_source_infos: Vec<(String, SourceInfo)>,
    extension_theme_source_infos: Vec<(String, SourceInfo)>,
    #[allow(dead_code)] // pi gates jiti-cache clearing on this; the stub seam has no cache.
    loaded: bool,
}

/// Upsert `(path, info)` into an ordered assoc-list, mirroring JS `Map.set`
/// semantics: overwrite an existing key's value in place, otherwise append
/// (preserving insertion order so prefix-match precedence is deterministic).
fn upsert(list: &mut Vec<(String, SourceInfo)>, key: String, info: SourceInfo) {
    if let Some(slot) = list.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = info;
    } else {
        list.push((key, info));
    }
}

impl DefaultResourceLoader {
    /// Construct a loader from `options`, mirroring pi's constructor: resolve the
    /// roots, default the collaborators, seed every cached field empty, and seed
    /// `extensions_result` with a fresh runtime handle.
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| SettingsManager::create(&options.cwd, &options.agent_dir));
        let discovery = ResourceLoader::new(
            &options.cwd,
            &options.agent_dir,
            settings_manager.is_project_trusted(),
        );
        let cwd = discovery.cwd().to_string();
        let agent_dir = discovery.agent_dir().to_string();
        let package_resolver = {
            let resolver = PackageResolver::new(&cwd, &agent_dir);
            match &options.home_dir {
                Some(home_dir) => resolver.with_home_dir(home_dir),
                None => resolver,
            }
        };

        Self {
            discovery,
            cwd,
            agent_dir,
            settings_manager,
            event_bus: options.event_bus.unwrap_or_default(),
            package_resolver,
            extension_loader: options
                .extension_loader
                .unwrap_or_else(|| Box::new(StubExtensionLoader)),

            additional_extension_paths: options.additional_extension_paths,
            additional_skill_paths: options.additional_skill_paths,
            additional_prompt_template_paths: options.additional_prompt_template_paths,
            additional_theme_paths: options.additional_theme_paths,
            no_extensions: options.no_extensions,
            no_skills: options.no_skills,
            no_prompt_templates: options.no_prompt_templates,
            no_themes: options.no_themes,
            no_context_files: options.no_context_files,
            system_prompt_source: options.system_prompt,
            append_system_prompt_source: options.append_system_prompt,
            extensions_override: options.extensions_override,
            skills_override: options.skills_override,
            prompts_override: options.prompts_override,
            themes_override: options.themes_override,
            agents_files_override: options.agents_files_override,
            system_prompt_override: options.system_prompt_override,
            append_system_prompt_override: options.append_system_prompt_override,

            extensions_result: LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                runtime: Some(create_extension_runtime()),
            },
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompts: Vec::new(),
            prompt_diagnostics: Vec::new(),
            themes: Vec::new(),
            theme_diagnostics: Vec::new(),
            agents_files: Vec::new(),
            system_prompt: None,
            append_system_prompt: Vec::new(),
            last_skill_paths: Vec::new(),
            last_prompt_paths: Vec::new(),
            last_theme_paths: Vec::new(),
            extension_skill_source_infos: Vec::new(),
            extension_prompt_source_infos: Vec::new(),
            extension_theme_source_infos: Vec::new(),
            loaded: false,
        }
    }

    // -- getters (port of pi's get* accessors) --------------------------------

    /// The cached extension-load result. Port of `getExtensions()`.
    ///
    /// Returns a reference (pi returns the object by JS reference); the `runtime`
    /// handle is move-only so the result cannot be cloned out.
    pub fn get_extensions(&self) -> &LoadExtensionsResult {
        &self.extensions_result
    }

    /// The cached skills + diagnostics. Port of `getSkills()`.
    pub fn get_skills(&self) -> LoadSkillsResult {
        LoadSkillsResult {
            skills: self.skills.clone(),
            diagnostics: self.skill_diagnostics.clone(),
        }
    }

    /// The cached prompts + diagnostics. Port of `getPrompts()`.
    pub fn get_prompts(&self) -> PromptsResult {
        PromptsResult {
            prompts: self.prompts.clone(),
            diagnostics: self.prompt_diagnostics.clone(),
        }
    }

    /// The cached themes + diagnostics. Port of `getThemes()`.
    pub fn get_themes(&self) -> ThemesResult {
        ThemesResult {
            themes: self.themes.clone(),
            diagnostics: self.theme_diagnostics.clone(),
        }
    }

    /// The cached project context files. Port of `getAgentsFiles()`.
    pub fn get_agents_files(&self) -> Vec<ContextFile> {
        self.agents_files.clone()
    }

    /// The cached system prompt. Port of `getSystemPrompt()`.
    pub fn get_system_prompt(&self) -> Option<String> {
        self.system_prompt.clone()
    }

    /// The cached append-system prompts. Port of `getAppendSystemPrompt()`.
    pub fn get_append_system_prompt(&self) -> Vec<String> {
        self.append_system_prompt.clone()
    }

    /// Accessor for the shared event bus.
    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    // -- reload ---------------------------------------------------------------

    /// Reload every resource from settings + auto-discovery. Faithful port of
    /// pi's `reload()`. Synchronous here (pi's `async` covers the extension host
    /// dynamic import + the trust callback; the trust callback is a plain
    /// closure behind the seam).
    pub fn reload(&mut self, options: ReloadOptions) {
        // pi resets extension timings + clears the jiti extension cache here; the
        // stub seam has no timings/cache, so both collapse to no-ops.

        let mut pre_trust: Option<LoadExtensionsResult> = None;
        if let Some(cb) = &options.resolve_project_trust {
            let pre = self.load_project_trust_extensions();
            let trusted = cb(&pre);
            self.settings_manager.set_project_trusted(trusted);
            pre_trust = Some(pre);
        }

        // reload() preserves SettingsManager.projectTrusted and reloads settings
        // for that trust state, then rebuilds the trust-gated discovery engine.
        self.settings_manager.reload();
        self.rebuild_discovery();

        let settings = self.build_resolve_settings();
        let resolved_paths = self.package_resolver.resolve(&settings, None);
        let cli_extension_paths = self.package_resolver.resolve_extension_sources(
            &self.additional_extension_paths,
            false,
            true,
        );

        let mut metadata_by_path: Vec<(String, PathMetadata)> = Vec::new();
        self.extension_skill_source_infos.clear();
        self.extension_prompt_source_infos.clear();
        self.extension_theme_source_infos.clear();

        // Record metadata for every resolved resource (first wins), then keep the
        // enabled paths. Mirrors pi's `getEnabledResources` / `getEnabledPaths`.
        for r in &resolved_paths.extensions {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), r.metadata.clone());
        }
        let enabled_extensions = enabled_paths(&resolved_paths.extensions);
        for r in &resolved_paths.skills {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), r.metadata.clone());
        }
        let enabled_skill_resources: Vec<ResolvedResource> = resolved_paths
            .skills
            .iter()
            .filter(|r| r.enabled)
            .cloned()
            .collect();
        for r in &resolved_paths.prompts {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), r.metadata.clone());
        }
        let enabled_prompts = enabled_paths(&resolved_paths.prompts);
        for r in &resolved_paths.themes {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), r.metadata.clone());
        }
        let enabled_themes = enabled_paths(&resolved_paths.themes);

        // Map auto/package skill directories to their `SKILL.md`, recording the
        // extra provenance entry pi's `mapSkillPath` surfaces.
        let mut enabled_skills: Vec<String> = Vec::new();
        for resource in &enabled_skill_resources {
            let mapping = self
                .discovery
                .map_skill_path(&resource.path, &resource.metadata);
            if let Some((path, md)) = mapping.extra_metadata {
                md_set_if_absent(&mut metadata_by_path, path, md);
            }
            enabled_skills.push(mapping.path);
        }

        // CLI extension/skill paths carry a synthetic `cli` provenance.
        let cli_meta = PathMetadata {
            source: "cli".to_string(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        };
        for r in &cli_extension_paths.extensions {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), cli_meta.clone());
        }
        for r in &cli_extension_paths.skills {
            md_set_if_absent(&mut metadata_by_path, r.path.clone(), cli_meta.clone());
        }
        let cli_enabled_extensions = enabled_paths(&cli_extension_paths.extensions);
        let cli_enabled_skills = enabled_paths(&cli_extension_paths.skills);
        let cli_enabled_prompts = enabled_paths(&cli_extension_paths.prompts);
        let cli_enabled_themes = enabled_paths(&cli_extension_paths.themes);

        // -- extensions -------------------------------------------------------
        let extension_paths = if self.no_extensions {
            cli_enabled_extensions.clone()
        } else {
            self.discovery
                .merge_paths(&cli_enabled_extensions, &enabled_extensions)
        };
        let mut extensions_result = self.load_final_extension_set(&extension_paths, pre_trust);
        for p in self.additional_extension_paths.clone() {
            if is_local_path(&p) {
                let resolved = self.discovery.resolve_resource_path(&p);
                if !Path::new(&resolved).exists() {
                    extensions_result.errors.push(ExtensionLoadError {
                        error: format!("Extension path does not exist: {resolved}"),
                        path: resolved,
                    });
                }
            }
        }
        let extensions_result = match &self.extensions_override {
            Some(f) => f(extensions_result),
            None => extensions_result,
        };
        self.extensions_result = extensions_result;
        self.apply_extension_source_info(&metadata_by_path);

        // -- skills -----------------------------------------------------------
        let skill_paths = if self.no_skills {
            self.discovery
                .merge_paths(&cli_enabled_skills, &self.additional_skill_paths)
        } else {
            let mut combined = cli_enabled_skills.clone();
            combined.extend(enabled_skills.clone());
            self.discovery
                .merge_paths(&combined, &self.additional_skill_paths)
        };
        self.last_skill_paths = skill_paths.clone();
        self.update_skills_from_paths(&skill_paths, &metadata_by_path);
        for p in self.additional_skill_paths.clone() {
            if is_local_path(&p) {
                let resolved = self.discovery.resolve_resource_path(&p);
                if !Path::new(&resolved).exists()
                    && !self
                        .skill_diagnostics
                        .iter()
                        .any(|d| d.path.as_deref() == Some(resolved.as_str()))
                {
                    self.skill_diagnostics.push(skills::ResourceDiagnostic {
                        kind: skills::DiagnosticKind::Error,
                        message: "Skill path does not exist".to_string(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
        }

        // -- prompts ----------------------------------------------------------
        let prompt_paths = if self.no_prompt_templates {
            self.discovery
                .merge_paths(&cli_enabled_prompts, &self.additional_prompt_template_paths)
        } else {
            let mut combined = cli_enabled_prompts.clone();
            combined.extend(enabled_prompts.clone());
            self.discovery
                .merge_paths(&combined, &self.additional_prompt_template_paths)
        };
        self.last_prompt_paths = prompt_paths.clone();
        self.update_prompts_from_paths(&prompt_paths, &metadata_by_path);
        for p in self.additional_prompt_template_paths.clone() {
            if is_local_path(&p) {
                let resolved = self.discovery.resolve_resource_path(&p);
                if !Path::new(&resolved).exists()
                    && !self
                        .prompt_diagnostics
                        .iter()
                        .any(|d| d.path.as_deref() == Some(resolved.as_str()))
                {
                    self.prompt_diagnostics.push(ResourceDiagnostic {
                        diagnostic_type: DiagnosticType::Error,
                        message: "Prompt template path does not exist".to_string(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
        }

        // -- themes -----------------------------------------------------------
        let theme_paths = if self.no_themes {
            self.discovery
                .merge_paths(&cli_enabled_themes, &self.additional_theme_paths)
        } else {
            let mut combined = cli_enabled_themes.clone();
            combined.extend(enabled_themes.clone());
            self.discovery
                .merge_paths(&combined, &self.additional_theme_paths)
        };
        self.last_theme_paths = theme_paths.clone();
        self.update_themes_from_paths(&theme_paths, &metadata_by_path);
        for p in self.additional_theme_paths.clone() {
            let resolved = self.discovery.resolve_resource_path(&p);
            if !Path::new(&resolved).exists()
                && !self
                    .theme_diagnostics
                    .iter()
                    .any(|d| d.path.as_deref() == Some(resolved.as_str()))
            {
                self.theme_diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: DiagnosticType::Error,
                    message: "Theme path does not exist".to_string(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }

        // -- context files ----------------------------------------------------
        let agents_files = if self.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(&self.cwd, &self.agent_dir)
        };
        self.agents_files = match &self.agents_files_override {
            Some(f) => f(agents_files),
            None => agents_files,
        };

        // -- system prompt ----------------------------------------------------
        let discovered_system = self.discovery.discover_system_prompt_file();
        let system_source = self.system_prompt_source.clone().or(discovered_system);
        let base_system = resolve_prompt_input(system_source.as_deref(), "system prompt");
        self.system_prompt = match &self.system_prompt_override {
            Some(f) => f(base_system),
            None => base_system,
        };

        // -- append system prompt --------------------------------------------
        let append_sources: Vec<String> = match &self.append_system_prompt_source {
            Some(sources) => sources.clone(),
            None => self
                .discovery
                .discover_append_system_prompt_file()
                .map(|f| vec![f])
                .unwrap_or_default(),
        };
        let base_append: Vec<String> = append_sources
            .iter()
            .filter_map(|s| resolve_prompt_input(Some(s), "append system prompt"))
            .collect();
        self.append_system_prompt = match &self.append_system_prompt_override {
            Some(f) => f(base_append),
            None => base_append,
        };

        self.loaded = true;
    }

    /// Port of pi's `loadProjectTrustExtensions`: force the project untrusted for
    /// the bootstrap pass (keeping project-local resources out while still
    /// loading user/global + temporary CLI extensions), then load the current
    /// extension set (including inline factories).
    fn load_project_trust_extensions(&mut self) -> LoadExtensionsResult {
        self.settings_manager.set_project_trusted(false);
        self.settings_manager.reload();
        self.rebuild_discovery();
        self.load_current_extension_set(true)
    }

    /// Port of pi's `loadCurrentExtensionSet`: resolve the enabled extension set
    /// and load it through the seam. Inline factories (`includeInlineFactories`)
    /// need the real extension host and are a no-op behind the stub.
    fn load_current_extension_set(&self, _include_inline_factories: bool) -> LoadExtensionsResult {
        let settings = self.build_resolve_settings();
        let resolved_paths = self.package_resolver.resolve(&settings, None);
        let cli_extension_paths = self.package_resolver.resolve_extension_sources(
            &self.additional_extension_paths,
            false,
            true,
        );
        let enabled_extensions = enabled_paths(&resolved_paths.extensions);
        let cli_enabled_extensions = enabled_paths(&cli_extension_paths.extensions);
        let extension_paths = if self.no_extensions {
            cli_enabled_extensions
        } else {
            self.discovery
                .merge_paths(&cli_enabled_extensions, &enabled_extensions)
        };
        self.extension_loader.load_extensions_cached(
            &extension_paths,
            &self.cwd,
            &self.event_bus,
            None,
        )
    }

    /// Port of pi's `loadFinalExtensionSet`: the trust two-pass. With no pre-trust
    /// result, a single load pass (+ conflict diagnostics). With a pre-trust
    /// result, the SAME runtime handle is threaded into the second pass, the
    /// remaining (not-yet-loaded, not-failed) paths are loaded, and the final set
    /// is reassembled in `extension_paths` order deduped by `resolved_path` with
    /// inline extensions appended.
    fn load_final_extension_set(
        &self,
        extension_paths: &[String],
        pre_trust: Option<LoadExtensionsResult>,
    ) -> LoadExtensionsResult {
        let Some(pre) = pre_trust else {
            let mut result = self.extension_loader.load_extensions_cached(
                extension_paths,
                &self.cwd,
                &self.event_bus,
                None,
            );
            // Inline factories are deferred behind the stub seam.
            Self::add_extension_conflict_diagnostics(&mut result);
            return result;
        };

        let LoadExtensionsResult {
            extensions: pre_exts,
            errors: pre_errors,
            runtime: pre_runtime,
        } = pre;

        let preloaded: Vec<(String, Extension)> = pre_exts
            .iter()
            .filter(|e| !e.path.starts_with("<inline:"))
            .map(|e| (e.resolved_path.clone(), e.clone()))
            .collect();
        let failed: Vec<String> = pre_errors
            .iter()
            .map(|e| self.resolve_extension_load_path(&e.path))
            .collect();
        let remaining_paths: Vec<String> = extension_paths
            .iter()
            .filter(|path| {
                let resolved = self.resolve_extension_load_path(path);
                !preloaded.iter().any(|(k, _)| *k == resolved) && !failed.contains(&resolved)
            })
            .cloned()
            .collect();

        // The pre-trust runtime handle is moved into the second pass and threaded
        // back out unchanged (identity preserved), matching pi's reuse.
        let remaining = self.extension_loader.load_extensions_cached(
            &remaining_paths,
            &self.cwd,
            &self.event_bus,
            pre_runtime,
        );

        let mut loaded_by_path = preloaded;
        for e in &remaining.extensions {
            if let Some(slot) = loaded_by_path
                .iter_mut()
                .find(|(k, _)| *k == e.resolved_path)
            {
                slot.1 = e.clone();
            } else {
                loaded_by_path.push((e.resolved_path.clone(), e.clone()));
            }
        }

        let inline: Vec<Extension> = pre_exts
            .into_iter()
            .filter(|e| e.path.starts_with("<inline:"))
            .collect();
        let mut ordered: Vec<Extension> = extension_paths
            .iter()
            .filter_map(|path| {
                let resolved = self.resolve_extension_load_path(path);
                loaded_by_path
                    .iter()
                    .find(|(k, _)| *k == resolved)
                    .map(|(_, e)| e.clone())
            })
            .collect();
        ordered.extend(inline);

        let mut errors = pre_errors;
        errors.extend(remaining.errors);

        let mut result = LoadExtensionsResult {
            extensions: ordered,
            errors,
            runtime: remaining.runtime,
        };
        Self::add_extension_conflict_diagnostics(&mut result);
        result
    }

    /// Port of pi's `addExtensionConflictDiagnostics`: report tool/flag name
    /// clashes between different extensions as errors (commands are NOT
    /// conflict-checked). Reads names through the seam's accessors only.
    fn add_extension_conflict_diagnostics(result: &mut LoadExtensionsResult) {
        let inputs: Vec<ExtensionConflictInput> = result
            .extensions
            .iter()
            .map(|e| ExtensionConflictInput {
                path: e.path.clone(),
                tools: e.tool_names().map(str::to_string).collect(),
                flags: e.flag_names().map(str::to_string).collect(),
            })
            .collect();
        for conflict in detect_extension_conflicts(&inputs) {
            result.errors.push(ExtensionLoadError {
                path: conflict.path,
                error: conflict.message,
            });
        }
    }

    /// Port of pi's `applyExtensionSourceInfo`: stamp each extension's
    /// `sourceInfo` from `metadata_by_path` (prefix-matched) or the default. pi
    /// also propagates the extension's info onto its commands/tools; the seam's
    /// name-string extensions carry no per-command info, so that is a no-op here.
    fn apply_extension_source_info(&mut self, metadata_by_path: &[(String, PathMetadata)]) {
        let updates: Vec<SourceInfo> = self
            .extensions_result
            .extensions
            .iter()
            .map(|ext| {
                self.discovery
                    .find_source_info_for_path(&ext.path, &[], metadata_by_path)
                    .unwrap_or_else(|| self.discovery.default_source_info_for_path(&ext.path))
            })
            .collect();
        for (ext, si) in self.extensions_result.extensions.iter_mut().zip(updates) {
            ext.source_info = Some(si);
        }
    }

    /// Rebuild the trust-gated discovery engine from the current trust state.
    fn rebuild_discovery(&mut self) {
        self.discovery = ResourceLoader::new(
            &self.cwd,
            &self.agent_dir,
            self.settings_manager.is_project_trusted(),
        );
    }

    /// Build the settings snapshot the package resolver consumes.
    fn build_resolve_settings(&self) -> ResolveSettings {
        ResolveSettings {
            global: scope_from_settings(&self.settings_manager.get_global_settings()),
            project: scope_from_settings(&self.settings_manager.get_project_settings()),
            project_trusted: self.settings_manager.is_project_trusted(),
        }
    }

    /// Port of pi's `resolveExtensionLoadPath`: resolve `path` against `cwd`
    /// normalizing unicode spaces (the key the two-pass dedup matches on).
    fn resolve_extension_load_path(&self, path: &str) -> String {
        use crate::utils::paths::{resolve_path, PathInputOptions};
        let options = PathInputOptions {
            normalize_unicode_spaces: true,
            ..PathInputOptions::default()
        };
        resolve_path(path, &self.cwd, &options).unwrap_or_else(|_| path.to_string())
    }

    // -- extendResources ------------------------------------------------------

    /// Merge extra resource paths into the loaded set. Full port of pi's
    /// `extendResources` for skills, prompts and themes: normalize each entry,
    /// record its extension-supplied provenance, then extend the per-kind
    /// `last*Paths` list (merge + canonical de-dup) and re-run the loader so the
    /// getters reflect the new resources with correct `sourceInfo`.
    pub fn extend_resources(&mut self, paths: &ResourceExtensionPaths) {
        let skill_paths = self.discovery.normalize_extension_paths(&paths.skill_paths);
        let prompt_paths = self
            .discovery
            .normalize_extension_paths(&paths.prompt_paths);
        let theme_paths = self.discovery.normalize_extension_paths(&paths.theme_paths);

        for entry in &skill_paths {
            upsert(
                &mut self.extension_skill_source_infos,
                entry.path.clone(),
                source_info::create(entry.path.clone(), entry.metadata.clone()),
            );
        }
        for entry in &prompt_paths {
            upsert(
                &mut self.extension_prompt_source_infos,
                entry.path.clone(),
                source_info::create(entry.path.clone(), entry.metadata.clone()),
            );
        }
        for entry in &theme_paths {
            upsert(
                &mut self.extension_theme_source_infos,
                entry.path.clone(),
                source_info::create(entry.path.clone(), entry.metadata.clone()),
            );
        }

        if !skill_paths.is_empty() {
            let added: Vec<String> = skill_paths.iter().map(|e| e.path.clone()).collect();
            self.last_skill_paths = self.discovery.merge_paths(&self.last_skill_paths, &added);
            let paths = self.last_skill_paths.clone();
            self.update_skills_from_paths(&paths, &[]);
        }

        if !prompt_paths.is_empty() {
            let added: Vec<String> = prompt_paths.iter().map(|e| e.path.clone()).collect();
            self.last_prompt_paths = self.discovery.merge_paths(&self.last_prompt_paths, &added);
            let paths = self.last_prompt_paths.clone();
            self.update_prompts_from_paths(&paths, &[]);
        }

        if !theme_paths.is_empty() {
            let added: Vec<String> = theme_paths.iter().map(|e| e.path.clone()).collect();
            self.last_theme_paths = self.discovery.merge_paths(&self.last_theme_paths, &added);
            let paths = self.last_theme_paths.clone();
            self.update_themes_from_paths(&paths, &[]);
        }
    }

    /// Port of pi's `updateSkillsFromPaths`: load skills from `skill_paths`
    /// (defaults excluded), apply the skills override, then stamp each skill's
    /// `sourceInfo` from the extension-supplied infos / `metadata_by_path`,
    /// falling back to the skill's own info.
    fn update_skills_from_paths(
        &mut self,
        skill_paths: &[String],
        metadata_by_path: &[(String, PathMetadata)],
    ) {
        let skills_result = if self.no_skills && skill_paths.is_empty() {
            LoadSkillsResult::default()
        } else {
            load_skills(LoadSkillsOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                skill_paths: skill_paths.to_vec(),
                include_defaults: false,
            })
        };
        let resolved = match &self.skills_override {
            Some(f) => f(skills_result),
            None => skills_result,
        };
        self.skills = resolved
            .skills
            .into_iter()
            .map(|mut skill| {
                let found = self.discovery.find_source_info_for_path(
                    &skill.file_path,
                    &self.extension_skill_source_infos,
                    metadata_by_path,
                );
                if let Some(found) = found {
                    skill.source_info = to_skill_source_info(found);
                }
                skill
            })
            .collect();
        self.skill_diagnostics = resolved.diagnostics;
    }

    /// Port of pi's `updatePromptsFromPaths`: load prompt templates from
    /// `prompt_paths` (defaults excluded), de-dup by name, apply the prompts
    /// override, then stamp each prompt's `sourceInfo`.
    fn update_prompts_from_paths(
        &mut self,
        prompt_paths: &[String],
        metadata_by_path: &[(String, PathMetadata)],
    ) {
        let prompts_result = if self.no_prompt_templates && prompt_paths.is_empty() {
            PromptsResult::default()
        } else {
            let all = load_prompt_templates(&LoadPromptTemplatesOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                prompt_paths: prompt_paths.to_vec(),
                include_defaults: false,
            });
            let (prompts, diagnostics) = dedupe_named(
                all,
                ResourceType::Prompt,
                |p| p.name.clone(),
                |p| Some(p.file_path.clone()),
                |name| format!("name \"/{name}\" collision"),
                "",
            );
            PromptsResult {
                prompts,
                diagnostics,
            }
        };
        let resolved = match &self.prompts_override {
            Some(f) => f(prompts_result),
            None => prompts_result,
        };
        self.prompts = resolved
            .prompts
            .into_iter()
            .map(|mut prompt| {
                let found = self.discovery.find_source_info_for_path(
                    &prompt.file_path,
                    &self.extension_prompt_source_infos,
                    metadata_by_path,
                );
                if let Some(found) = found {
                    prompt.source_info = to_prompt_source_info(found);
                }
                prompt
            })
            .collect();
        self.prompt_diagnostics = resolved.diagnostics;
    }

    /// Port of pi's `updateThemesFromPaths`: load runtime themes from
    /// `theme_paths` (defaults excluded), de-dup by name, apply the themes
    /// override, then stamp each theme's `sourceInfo` from its `sourcePath`.
    fn update_themes_from_paths(
        &mut self,
        theme_paths: &[String],
        metadata_by_path: &[(String, PathMetadata)],
    ) {
        let themes_result = if self.no_themes && theme_paths.is_empty() {
            ThemesResult::default()
        } else {
            let (loaded, mut diagnostics) = self.load_themes_impl(theme_paths);
            let (deduped, dedup_diags) = dedupe_named(
                loaded,
                ResourceType::Theme,
                |t: &Theme| t.name.clone().unwrap_or_else(|| "unnamed".to_string()),
                |t: &Theme| t.source_path.clone(),
                |name| format!("name \"{name}\" collision"),
                "<builtin>",
            );
            diagnostics.extend(dedup_diags);
            ThemesResult {
                themes: deduped,
                diagnostics,
            }
        };
        let resolved = match &self.themes_override {
            Some(f) => f(themes_result),
            None => themes_result,
        };
        self.themes = resolved
            .themes
            .into_iter()
            .map(|mut theme| {
                if let Some(source_path) = theme.source_path.clone() {
                    let si = self
                        .discovery
                        .find_source_info_for_path(
                            &source_path,
                            &self.extension_theme_source_infos,
                            metadata_by_path,
                        )
                        .or_else(|| theme.source_info.clone())
                        .unwrap_or_else(|| {
                            self.discovery.default_source_info_for_path(&source_path)
                        });
                    theme.source_info = Some(si);
                }
                theme
            })
            .collect();
        self.theme_diagnostics = resolved.diagnostics;
    }

    /// Port of the discovery + load halves of pi's `loadThemes(paths, false)`:
    /// discover the candidate `.json` files under each path (warnings for
    /// missing / non-JSON paths), then load each via `loadThemeFromPath`
    /// (per-file load failures become warnings).
    fn load_themes_impl(&self, theme_paths: &[String]) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        let discovery = self.discovery.discover_theme_files(theme_paths);
        let mut themes: Vec<Theme> = Vec::new();
        let mut diagnostics = discovery.diagnostics;
        for file in &discovery.files {
            match load_theme_from_path(Path::new(file), None) {
                Ok(theme) => themes.push(theme),
                Err(error) => diagnostics.push(ResourceDiagnostic {
                    diagnostic_type: DiagnosticType::Warning,
                    message: error.to_string(),
                    path: Some(file.clone()),
                    collision: None,
                }),
            }
        }
        (themes, diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source_info::{SourceOrigin, SourceScope};
    use crate::core::test_support::{s, scratch_dir, write};
    use std::fs;

    /// Scratch root with `project/` (cwd), `agent/` and an isolated empty
    /// `home/` (so the package resolver's `~/.agents` discovery finds nothing).
    fn roots(tag: &str) -> (std::path::PathBuf, String, String, String) {
        let base = scratch_dir(tag);
        let cwd = base.join("project");
        let agent = base.join("agent");
        let home = base.join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&home).unwrap();
        (base.clone(), s(&cwd), s(&agent), s(&home))
    }

    fn opts(cwd: &str, agent: &str, home: &str) -> DefaultResourceLoaderOptions {
        DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent.to_string(),
            home_dir: Some(home.to_string()),
            ..Default::default()
        }
    }

    fn ext_meta(source: &str, base_dir: &str) -> PathMetadata {
        PathMetadata {
            source: source.to_string(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(base_dir.to_string()),
        }
    }

    /// Port of `resource-loader.test.ts` "should initialize with empty results
    /// before reload".
    #[test]
    fn initialize_empty_before_reload() {
        let (_base, cwd, agent, home) = roots("rl-empty");
        let loader = DefaultResourceLoader::new(opts(&cwd, &agent, &home));

        assert!(loader.get_extensions().extensions.is_empty());
        assert!(loader.get_skills().skills.is_empty());
        assert!(loader.get_prompts().prompts.is_empty());
        assert!(loader.get_themes().themes.is_empty());
        assert!(loader.get_agents_files().is_empty());
        assert!(loader.get_system_prompt().is_none());
        assert!(loader.get_append_system_prompt().is_empty());
    }

    /// Constructor seeds a runtime handle into `extensions_result`.
    #[test]
    fn constructor_seeds_extension_runtime() {
        let (_base, cwd, agent, home) = roots("rl-ctor");
        let loader = DefaultResourceLoader::new(opts(&cwd, &agent, &home));
        assert!(loader.get_extensions().runtime.is_some());
    }

    /// A custom `ExtensionLoader` can be injected via options and drives the seam.
    #[test]
    fn custom_extension_loader_is_held() {
        let (_base, cwd, agent, home) = roots("rl-loader");
        let mut options = opts(&cwd, &agent, &home);
        options.extension_loader = Some(Box::new(StubExtensionLoader));
        let loader = DefaultResourceLoader::new(options);
        let result =
            loader
                .extension_loader
                .load_extensions_cached(&[], &cwd, loader.event_bus(), None);
        assert!(result.extensions.is_empty());
        assert!(result.runtime.is_some());
    }

    /// `extend_resources` with only skills leaves prompts untouched, and merged
    /// paths de-dup (calling twice with the same path keeps a single skill).
    #[test]
    fn extend_resources_skills_only_and_dedup() {
        let (base, cwd, agent, home) = roots("rl-extend-dedup");

        let extra_skill_dir = base.join("s").join("only-skill");
        fs::create_dir_all(&extra_skill_dir).unwrap();
        write(
            &s(&extra_skill_dir.join("SKILL.md")),
            "---\nname: only-skill\ndescription: Only skill\n---\nBody",
        );

        let mut loader = DefaultResourceLoader::new(opts(&cwd, &agent, &home));
        let entry = ExtensionPathEntry {
            path: s(&extra_skill_dir),
            metadata: ext_meta("extension:only", &s(&extra_skill_dir)),
        };
        loader.extend_resources(&ResourceExtensionPaths {
            skill_paths: vec![entry.clone()],
            ..Default::default()
        });
        loader.extend_resources(&ResourceExtensionPaths {
            skill_paths: vec![entry],
            ..Default::default()
        });

        let skills = loader.get_skills().skills;
        assert_eq!(
            skills.iter().filter(|sk| sk.name == "only-skill").count(),
            1
        );
        assert!(loader.get_prompts().prompts.is_empty());
    }
}
