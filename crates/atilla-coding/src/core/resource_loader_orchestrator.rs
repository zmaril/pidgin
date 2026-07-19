//! Stateful `DefaultResourceLoader` orchestrator, ported from pi's
//! `core/resource-loader.ts` (`class DefaultResourceLoader`, ~line 159).
//!
//! pi's orchestrator wires the settings manager, package manager, extension
//! runtime and the skill/prompt/theme loaders, then caches the resolved results
//! and decorates each with provenance. The **pure discovery/precedence engine**
//! it delegates to already landed as [`crate::core::resource_loader`] (PR #100);
//! this module adds the stateful shell around it.
//!
//! # What is ported (unblocked)
//! * the [`DefaultResourceLoader`] struct shell — every pi field: roots,
//!   `SettingsManager`, `EventBus`, the config/override options, the seven
//!   override closures, and the ~20 cached-result fields;
//! * the [`ExtensionLoader`] trait seam (held as `Box<dyn ExtensionLoader>`,
//!   defaulting to [`StubExtensionLoader`]);
//! * the getters and [`DefaultResourceLoader::extend_resources`] for the
//!   **skills and prompts** paths, reusing the landed `load_skills` /
//!   `load_prompt_templates` loaders and the discovery engine's
//!   `merge_paths` / `normalize_extension_paths` / `find_source_info_for_path`.
//!
//! # What is deferred (blocked — see follow-ups)
//! * **`reload()`** — pi's `reload()` always calls
//!   `packageManager.resolve()` / `resolveExtensionSources()` to discover
//!   resources from settings + auto-discovery dirs. That pure-FS `resolve()` is
//!   **NOT yet ported** (blocker **B**, branch
//!   `port/coding-package-manager-resolve`; `package_manager/mod.rs` ported only
//!   the command concern, PR #72). There is no faithful pi code path that
//!   discovers skills/prompts/themes without `resolve()`, so `reload()` is left
//!   to the follow-up rather than stubbed in a way that fakes discovery.
//! * **theme slice** of `extend_resources` — pi's `updateThemesFromPaths` calls
//!   `loadThemeFromPath` -> `createTheme`, building a runtime `Theme`
//!   (`colorMode`, fg/bg split, `{name, sourcePath}`). atilla has only the
//!   name-based `load_theme_json` returning `ThemeJson`; no
//!   `loadThemeFromPath` / `createTheme` / runtime `Theme` struct exists
//!   (theme-loader gap, ~150-250 LOC). [`ResourceExtensionPaths::theme_paths`]
//!   is accepted but not yet applied.
//! * **extension wiring** — the `Box<dyn ExtensionLoader>` is a
//!   [`StubExtensionLoader`] returning an empty `LoadExtensionsResult`. pi's
//!   real `loadExtensionsCached` / `createExtensionRuntime` is a `jiti`
//!   dynamic-TS host owned by the extension-plane session (blocker **A**).
//!
//! See the `wi3-orchestrator-blockers-ownership` team memory for the full
//! ownership map and incremental order.

// straitjacket-allow-file:duplication

use crate::core::diagnostics::{ResourceDiagnostic, ResourceType};
use crate::core::event_bus::EventBus;
use crate::core::extensions::loader::{
    create_extension_runtime, ExtensionLoader, LoadExtensionsResult, StubExtensionLoader,
};
use crate::core::prompt_templates::{
    self, load_prompt_templates, LoadPromptTemplatesOptions, PromptTemplate,
};
use crate::core::resource_loader::{dedupe_named, ContextFile, ExtensionPathEntry, ResourceLoader};
use crate::core::settings_manager::SettingsManager;
use crate::core::skills::{self, load_skills, LoadSkillsOptions, LoadSkillsResult, Skill};
use crate::core::source_info::{self, PathMetadata, SourceInfo, SourceOrigin, SourceScope};

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

/// Result view returned by [`DefaultResourceLoader::get_prompts`], mirroring
/// pi's `{ prompts, diagnostics }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptsResult {
    /// The loaded prompt templates.
    pub prompts: Vec<PromptTemplate>,
    /// Load / collision diagnostics.
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// The `{ skillPaths?, promptPaths?, themePaths? }` argument pi's
/// `extendResources` accepts. `theme_paths` is accepted for shape parity but
/// not yet applied (theme-loader gap; see the module docs).
#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    /// Extra skill `{ path, metadata }` entries to merge in.
    pub skill_paths: Vec<ExtensionPathEntry>,
    /// Extra prompt `{ path, metadata }` entries to merge in.
    pub prompt_paths: Vec<ExtensionPathEntry>,
    /// Extra theme entries — **accepted but deferred** (blocked on the
    /// theme-loader gap: `loadThemeFromPath` / `createTheme` / runtime `Theme`).
    pub theme_paths: Vec<ExtensionPathEntry>,
}

type SkillsOverride = Box<dyn Fn(LoadSkillsResult) -> LoadSkillsResult>;
type PromptsOverride = Box<dyn Fn(PromptsResult) -> PromptsResult>;
type ExtensionsOverride = Box<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult>;
type AgentsFilesOverride = Box<dyn Fn(Vec<ContextFile>) -> Vec<ContextFile>>;
type SystemPromptOverride = Box<dyn Fn(Option<String>) -> Option<String>>;
type AppendSystemPromptOverride = Box<dyn Fn(Vec<String>) -> Vec<String>>;

/// Construction options, mirroring pi's `DefaultResourceLoaderOptions`.
///
/// `cwd` / `agent_dir` are required; everything else defaults to
/// empty/`false`/`None`. The seven `*_override` closures and the two collaborator
/// handles (`settings_manager`, `event_bus`, `extension_loader`) are supplied
/// only when a caller needs to inject them.
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
    /// Override for the resolved context files.
    pub agents_files_override: Option<AgentsFilesOverride>,
    /// Override for the resolved system prompt.
    pub system_prompt_override: Option<SystemPromptOverride>,
    /// Override for the resolved append-system prompts.
    pub append_system_prompt_override: Option<AppendSystemPromptOverride>,
}

/// The stateful resource-loader orchestrator. Port of pi's
/// `DefaultResourceLoader`. Fields mirror pi 1:1; see the module docs for the
/// unblocked-vs-deferred split.
pub struct DefaultResourceLoader {
    // -- roots + collaborators ------------------------------------------------
    /// The pure discovery/precedence engine (landed), rooted at cwd/agent_dir.
    discovery: ResourceLoader,
    cwd: String,
    agent_dir: String,
    #[allow(dead_code)] // wired by the deferred reload(); held on the shell now.
    settings_manager: SettingsManager,
    event_bus: EventBus,
    #[allow(dead_code)] // exercised by the deferred reload().
    extension_loader: Box<dyn ExtensionLoader>,

    // -- config / override options --------------------------------------------
    #[allow(dead_code)]
    additional_extension_paths: Vec<String>,
    #[allow(dead_code)]
    additional_skill_paths: Vec<String>,
    #[allow(dead_code)]
    additional_prompt_template_paths: Vec<String>,
    #[allow(dead_code)]
    additional_theme_paths: Vec<String>,
    #[allow(dead_code)]
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    #[allow(dead_code)]
    no_themes: bool,
    #[allow(dead_code)]
    no_context_files: bool,
    #[allow(dead_code)]
    system_prompt_source: Option<String>,
    #[allow(dead_code)]
    append_system_prompt_source: Option<Vec<String>>,
    #[allow(dead_code)]
    extensions_override: Option<ExtensionsOverride>,
    skills_override: Option<SkillsOverride>,
    prompts_override: Option<PromptsOverride>,
    #[allow(dead_code)]
    agents_files_override: Option<AgentsFilesOverride>,
    #[allow(dead_code)]
    system_prompt_override: Option<SystemPromptOverride>,
    #[allow(dead_code)]
    append_system_prompt_override: Option<AppendSystemPromptOverride>,

    // -- cached results -------------------------------------------------------
    extensions_result: LoadExtensionsResult,
    skills: Vec<Skill>,
    // `skills` carries its own parallel `ResourceDiagnostic` (predates the
    // shared `diagnostics` module), so the skill diagnostics use that type.
    skill_diagnostics: Vec<skills::ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    // themes: deferred (theme-loader gap — no runtime `Theme` type yet).
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
    last_skill_paths: Vec<String>,
    last_prompt_paths: Vec<String>,
    #[allow(dead_code)]
    last_theme_paths: Vec<String>,
    extension_skill_source_infos: Vec<(String, SourceInfo)>,
    extension_prompt_source_infos: Vec<(String, SourceInfo)>,
    #[allow(dead_code)]
    extension_theme_source_infos: Vec<(String, SourceInfo)>,
    #[allow(dead_code)] // flips true once the deferred reload() runs.
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
    /// `extensions_result` with a fresh runtime.
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

        Self {
            discovery,
            cwd,
            agent_dir,
            settings_manager,
            event_bus: options.event_bus.unwrap_or_default(),
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
            agents_files_override: options.agents_files_override,
            system_prompt_override: options.system_prompt_override,
            append_system_prompt_override: options.append_system_prompt_override,

            extensions_result: LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                runtime: create_extension_runtime(),
            },
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompts: Vec::new(),
            prompt_diagnostics: Vec::new(),
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
    pub fn get_extensions(&self) -> LoadExtensionsResult {
        self.extensions_result.clone()
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

    /// The cached theme diagnostics. The theme *results* themselves are deferred
    /// (theme-loader gap); this returns only the diagnostics accumulated so far,
    /// always empty until the theme slice lands. Partial port of `getThemes()`.
    pub fn get_theme_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.theme_diagnostics.clone()
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

    // -- extendResources (skills + prompts) -----------------------------------

    /// Merge extra resource paths into the loaded set. Port of pi's
    /// `extendResources`, for the **skills and prompts** slices (the theme slice
    /// is deferred behind the theme-loader gap). For each supplied entry the
    /// path is normalized, its extension-supplied provenance recorded, then the
    /// per-kind `last*Paths` list is extended (merge + canonical de-dup) and the
    /// corresponding loader re-run so `get_skills` / `get_prompts` reflect the
    /// new resources with correct `sourceInfo`.
    pub fn extend_resources(&mut self, paths: &ResourceExtensionPaths) {
        let skill_paths = self.discovery.normalize_extension_paths(&paths.skill_paths);
        let prompt_paths = self
            .discovery
            .normalize_extension_paths(&paths.prompt_paths);

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

    /// Accessor for the shared event bus (held for the deferred `reload()`).
    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source_info::{SourceOrigin, SourceScope};
    use crate::core::test_support::{s, scratch_dir, write};
    use std::fs;

    /// Scratch root with `project/` (cwd) and `agent/` subdirs.
    fn roots(tag: &str) -> (std::path::PathBuf, String, String) {
        let base = scratch_dir(tag);
        let cwd = base.join("project");
        let agent = base.join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        (base.clone(), s(&cwd), s(&agent))
    }

    fn opts(cwd: &str, agent: &str) -> DefaultResourceLoaderOptions {
        DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent.to_string(),
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
    /// before reload" (adapted: themes are deferred, so the theme assertion is
    /// replaced by the theme-diagnostics accessor being empty).
    #[test]
    fn initialize_empty_before_reload() {
        let (_base, cwd, agent) = roots("rl-empty");
        let loader = DefaultResourceLoader::new(opts(&cwd, &agent));

        assert!(loader.get_extensions().extensions.is_empty());
        assert!(loader.get_skills().skills.is_empty());
        assert!(loader.get_prompts().prompts.is_empty());
        assert!(loader.get_theme_diagnostics().is_empty());
        assert!(loader.get_agents_files().is_empty());
        assert!(loader.get_system_prompt().is_none());
        assert!(loader.get_append_system_prompt().is_empty());
    }

    /// Constructor seeds a runtime into `extensions_result`.
    #[test]
    fn constructor_seeds_extension_runtime() {
        let (_base, cwd, agent) = roots("rl-ctor");
        let loader = DefaultResourceLoader::new(opts(&cwd, &agent));
        assert_eq!(loader.get_extensions().runtime, create_extension_runtime());
    }

    /// Port of `resource-loader.test.ts` "should load skills and prompts with
    /// extension metadata" (adapted: pi calls `reload()` first over the empty
    /// cwd/agentDir, which is a no-op for these assertions since neither dir
    /// holds resources; `reload()` is deferred on blocker B, so we construct and
    /// call `extend_resources` directly).
    #[test]
    fn extend_resources_loads_skills_and_prompts_with_metadata() {
        let (base, cwd, agent) = roots("rl-extend");

        let extra_skill_dir = base.join("extra-skills").join("extra-skill");
        fs::create_dir_all(&extra_skill_dir).unwrap();
        let skill_path = extra_skill_dir.join("SKILL.md");
        write(
            &s(&skill_path),
            "---\nname: extra-skill\ndescription: Extra skill\n---\nExtra content",
        );

        let extra_prompt_dir = base.join("extra-prompts");
        fs::create_dir_all(&extra_prompt_dir).unwrap();
        let prompt_path = extra_prompt_dir.join("extra.md");
        write(
            &s(&prompt_path),
            "---\ndescription: Extra prompt\n---\nExtra prompt content",
        );

        let mut loader = DefaultResourceLoader::new(opts(&cwd, &agent));
        loader.extend_resources(&ResourceExtensionPaths {
            skill_paths: vec![ExtensionPathEntry {
                path: s(&extra_skill_dir),
                metadata: ext_meta("extension:extra", &s(&extra_skill_dir)),
            }],
            prompt_paths: vec![ExtensionPathEntry {
                path: s(&prompt_path),
                metadata: ext_meta("extension:extra", &s(&extra_prompt_dir)),
            }],
            theme_paths: vec![],
        });

        let skills = loader.get_skills().skills;
        let loaded_skill = skills
            .iter()
            .find(|skill| skill.name == "extra-skill")
            .expect("extra-skill should be loaded");
        assert_eq!(loaded_skill.source_info.source, "extension:extra");
        assert_eq!(loaded_skill.source_info.path, s(&skill_path));

        let prompts = loader.get_prompts().prompts;
        let loaded_prompt = prompts
            .iter()
            .find(|prompt| prompt.name == "extra")
            .expect("extra prompt should be loaded");
        assert_eq!(loaded_prompt.source_info.source, "extension:extra");
        assert_eq!(loaded_prompt.source_info.path, s(&prompt_path));
    }

    /// `extend_resources` with only skills leaves prompts untouched, and merged
    /// paths de-dup (calling twice with the same path keeps a single skill).
    #[test]
    fn extend_resources_skills_only_and_dedup() {
        let (base, cwd, agent) = roots("rl-extend-dedup");

        let extra_skill_dir = base.join("s").join("only-skill");
        fs::create_dir_all(&extra_skill_dir).unwrap();
        write(
            &s(&extra_skill_dir.join("SKILL.md")),
            "---\nname: only-skill\ndescription: Only skill\n---\nBody",
        );

        let mut loader = DefaultResourceLoader::new(opts(&cwd, &agent));
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

    /// The skills override closure is applied by `extend_resources`.
    #[test]
    fn skills_override_is_applied() {
        let (base, cwd, agent) = roots("rl-override");
        let extra_skill_dir = base.join("s").join("drop-me");
        fs::create_dir_all(&extra_skill_dir).unwrap();
        write(
            &s(&extra_skill_dir.join("SKILL.md")),
            "---\nname: drop-me\ndescription: Drop\n---\nBody",
        );

        let mut options = opts(&cwd, &agent);
        options.skills_override = Some(Box::new(|_base| LoadSkillsResult::default()));
        let mut loader = DefaultResourceLoader::new(options);
        loader.extend_resources(&ResourceExtensionPaths {
            skill_paths: vec![ExtensionPathEntry {
                path: s(&extra_skill_dir),
                metadata: ext_meta("extension:drop", &s(&extra_skill_dir)),
            }],
            ..Default::default()
        });
        assert!(loader.get_skills().skills.is_empty());
    }

    /// A custom `ExtensionLoader` can be injected via options.
    #[test]
    fn custom_extension_loader_is_held() {
        let (_base, cwd, agent) = roots("rl-loader");
        let mut options = opts(&cwd, &agent);
        options.extension_loader = Some(Box::new(StubExtensionLoader));
        let loader = DefaultResourceLoader::new(options);
        // The seam is exercised directly here; reload() wiring is deferred.
        let result =
            loader
                .extension_loader
                .load_extensions_cached(&[], &cwd, loader.event_bus(), None);
        assert!(result.extensions.is_empty());
    }
}
