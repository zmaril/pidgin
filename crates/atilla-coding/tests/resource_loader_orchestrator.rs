// straitjacket-allow-file:duplication — faithful port of pi coding-agent's
// `resource-loader.test.ts`; the per-`it` fixture setup (mkdir/write real trees,
// reload, assert) parallels pi by design.

//! Ported from `vendor/pi/packages/coding-agent/test/resource-loader.test.ts`.
//!
//! Covers the integration cases that drive `DefaultResourceLoader.reload()` /
//! `extendResources` and were previously blocked on the package-manager
//! `resolve()` (now landed, #129) and the runtime theme loader (#136): skills /
//! prompts / themes / settings / context-file / system-prompt discovery,
//! project-vs-user precedence, override closures, `AGENTS.md` / `SYSTEM.md` /
//! `APPEND_SYSTEM.md` discovery, `noSkills`, and trust-gated project resources.
//!
//! The 5 cases that need the REAL extension runtime (loading `.ts` extensions,
//! trust reuse load-count, tool/command conflicts, CLI-vs-discovered precedence)
//! have MOVED to `crates/atilla-extensions/tests/deno_resource_loader.rs`
//! (deno-gated), where the real `RealExtensionLoader` is injected via the
//! `DefaultResourceLoaderOptions.extension_loader` seam. They can't live here:
//! the real loader is in atilla-extensions (which depends on atilla-coding and
//! needs V8), so referencing it from an atilla-coding test would be a dependency
//! cycle, and V8 can't build in-sandbox. The `ExtensionLoader` interface itself
//! stays covered in this default V8-free suite against `StubExtensionLoader`
//! (see `custom_extension_loader_is_held`).
//!
//! pi mutates `process.env.HOME`; here the loader takes an explicit `home_dir`
//! so tests avoid mutating the process environment (racy under parallel
//! `cargo test`).

use atilla_coding::core::resource_loader::ExtensionPathEntry;
use atilla_coding::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions, ResourceExtensionPaths,
};
use atilla_coding::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
use atilla_coding::core::source_info::{PathMetadata, SourceOrigin, SourceScope};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod common;
use common::{canonical, join, mkdir, write};

/// The crate's own `dark.json` — the base pi's theme cases clone.
const DARK_THEME_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/modes/interactive/theme/dark.json"
));

/// A temp fixture: an isolated `project/` (cwd), `agent/`, and empty `home/`.
struct Env {
    _tmp: tempfile::TempDir,
    root: String,
    cwd: String,
    agent: String,
    home: String,
}

fn env() -> Env {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = canonical(&tmp.path().to_string_lossy());
    let cwd = join(&root, &["project"]);
    let agent = join(&root, &["agent"]);
    let home = join(&root, &["home"]);
    mkdir(&cwd);
    mkdir(&agent);
    mkdir(&home);
    Env {
        _tmp: tmp,
        root,
        cwd,
        agent,
        home,
    }
}

fn base_opts(e: &Env) -> DefaultResourceLoaderOptions {
    DefaultResourceLoaderOptions {
        cwd: e.cwd.clone(),
        agent_dir: e.agent.clone(),
        home_dir: Some(e.home.clone()),
        ..Default::default()
    }
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\nContent")
}

/// Clone `dark.json` with a new theme name (pi's theme-collision base).
fn theme_json(name: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(DARK_THEME_JSON).unwrap();
    value["name"] = serde_json::Value::String(name.to_string());
    serde_json::to_string_pretty(&value).unwrap()
}

fn ext_meta(source: &str, base_dir: &str) -> PathMetadata {
    PathMetadata {
        source: source.to_string(),
        scope: SourceScope::Temporary,
        origin: SourceOrigin::TopLevel,
        base_dir: Some(base_dir.to_string()),
    }
}

// == describe("reload") =====================================================

#[test]
fn discover_skills_from_agent_dir() {
    let e = env();
    write(
        &join(&e.agent, &["skills", "test-skill.md"]),
        &skill_md("test-skill", "A test skill"),
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let skills = loader.get_skills().skills;
    assert!(skills.iter().any(|s| s.name == "test-skill"));
}

#[test]
fn ignore_extra_markdown_files_in_auto_discovered_skill_dirs() {
    let e = env();
    let skill_dir = join(&e.agent, &["skills", "pi-skills", "browser-tools"]);
    write(
        &join(&skill_dir, &["SKILL.md"]),
        &skill_md("browser-tools", "Browser tools"),
    );
    write(&join(&skill_dir, &["EFFICIENCY.md"]), "No frontmatter here");

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let result = loader.get_skills();
    assert!(result.skills.iter().any(|s| s.name == "browser-tools"));
    assert!(!result.diagnostics.iter().any(|d| d
        .path
        .as_deref()
        .is_some_and(|p| p.ends_with("EFFICIENCY.md"))));
}

#[test]
fn discover_prompts_from_agent_dir() {
    let e = env();
    write(
        &join(&e.agent, &["prompts", "test-prompt.md"]),
        "---\ndescription: A test prompt\n---\nPrompt content.",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let prompts = loader.get_prompts().prompts;
    assert!(prompts.iter().any(|p| p.name == "test-prompt"));
}

#[test]
fn prefer_project_resources_over_user_on_name_collisions() {
    let e = env();
    let user_prompt = join(&e.agent, &["prompts", "commit.md"]);
    let project_prompt = join(&e.cwd, &[".pi", "prompts", "commit.md"]);
    write(&user_prompt, "User prompt");
    write(&project_prompt, "Project prompt");

    let user_skill = join(&e.agent, &["skills", "collision-skill", "SKILL.md"]);
    let project_skill = join(&e.cwd, &[".pi", "skills", "collision-skill", "SKILL.md"]);
    write(&user_skill, &skill_md("collision-skill", "user"));
    write(&project_skill, &skill_md("collision-skill", "project"));

    let user_theme = join(&e.agent, &["themes", "collision.json"]);
    let project_theme = join(&e.cwd, &[".pi", "themes", "collision.json"]);
    write(&user_theme, &theme_json("collision-theme"));
    write(&project_theme, &theme_json("collision-theme"));

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|p| p.name == "commit");
    assert_eq!(prompt.map(|p| p.file_path), Some(project_prompt.clone()));

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|s| s.name == "collision-skill");
    assert_eq!(skill.map(|s| s.file_path), Some(project_skill.clone()));

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|t| t.name.as_deref() == Some("collision-theme"));
    assert_eq!(
        theme.and_then(|t| t.source_path),
        Some(project_theme.clone())
    );
}

#[test]
fn honor_overrides_for_auto_discovered_resources() {
    let e = env();
    // Global settings carry `-` override patterns that disable each resource.
    write(
        &join(&e.agent, &["settings.json"]),
        &serde_json::to_string(&serde_json::json!({
            "extensions": ["-extensions/disabled.ts"],
            "skills": ["-skills/skip-skill"],
            "prompts": ["-prompts/skip.md"],
            "themes": ["-themes/skip.json"],
        }))
        .unwrap(),
    );

    write(
        &join(&e.agent, &["extensions", "disabled.ts"]),
        "export default function() {}",
    );
    write(
        &join(&e.agent, &["skills", "skip-skill", "SKILL.md"]),
        &skill_md("skip-skill", "Skip me"),
    );
    write(&join(&e.agent, &["prompts", "skip.md"]), "Skip prompt");
    write(&join(&e.agent, &["themes", "skip.json"]), "{}");

    let mut options = base_opts(&e);
    options.settings_manager = Some(SettingsManager::create(&e.cwd, &e.agent));
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert!(!loader
        .get_extensions()
        .extensions
        .iter()
        .any(|x| x.path.ends_with("disabled.ts")));
    assert!(!loader
        .get_skills()
        .skills
        .iter()
        .any(|s| s.name == "skip-skill"));
    assert!(!loader
        .get_prompts()
        .prompts
        .iter()
        .any(|p| p.name == "skip"));
    assert!(!loader.get_themes().themes.iter().any(|t| t
        .source_path
        .as_deref()
        .is_some_and(|p| p.ends_with("skip.json"))));
}

#[test]
fn discover_agents_md_context_files() {
    let e = env();
    write(
        &join(&e.cwd, &["AGENTS.md"]),
        "# Project Guidelines\n\nBe helpful.",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    assert!(loader
        .get_agents_files()
        .iter()
        .any(|f| f.path.contains("AGENTS.md")));
}

#[test]
fn skip_context_files_when_no_context_files_is_true() {
    let e = env();
    write(&join(&e.cwd, &["AGENTS.md"]), "# Project Guidelines");
    write(&join(&e.cwd, &["CLAUDE.md"]), "# Claude Guidelines");

    let mut options = base_opts(&e);
    options.no_context_files = true;
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert!(loader.get_agents_files().is_empty());
}

#[test]
fn discover_system_md_from_cwd_pi() {
    let e = env();
    write(
        &join(&e.cwd, &[".pi", "SYSTEM.md"]),
        "You are a helpful assistant.",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("You are a helpful assistant.")
    );
}

#[test]
fn skip_project_resources_that_require_trust_when_not_trusted() {
    let e = env();
    let pi_dir = join(&e.cwd, &[".pi"]);
    write(
        &join(&pi_dir, &["extensions", "project.ts"]),
        "throw new Error(\"should not load\");",
    );
    write(
        &join(&pi_dir, &["skills", "project-skill", "SKILL.md"]),
        &skill_md("project-skill", "Project skill"),
    );
    write(&join(&pi_dir, &["prompts", "project.md"]), "Project prompt");
    write(
        &join(&pi_dir, &["themes", "project.json"]),
        &theme_json("project-theme"),
    );
    write(&join(&pi_dir, &["SYSTEM.md"]), "Project system prompt.");
    write(&join(&e.agent, &["SYSTEM.md"]), "Global system prompt.");
    write(&join(&e.agent, &["AGENTS.md"]), "Global instructions");
    write(&join(&e.cwd, &["AGENTS.md"]), "Project instructions");

    let mut options = base_opts(&e);
    options.settings_manager = Some(SettingsManager::create_with_options(
        &e.cwd,
        &e.agent,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    ));
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Global system prompt.")
    );
    let agents = loader.get_agents_files();
    assert!(agents
        .iter()
        .any(|f| f.path == join(&e.agent, &["AGENTS.md"])));
    assert!(agents
        .iter()
        .any(|f| f.path == join(&e.cwd, &["AGENTS.md"])));
    assert!(loader.get_extensions().extensions.is_empty());
    assert!(loader.get_extensions().errors.is_empty());
    assert!(!loader
        .get_skills()
        .skills
        .iter()
        .any(|s| s.name == "project-skill"));
    assert!(!loader
        .get_prompts()
        .prompts
        .iter()
        .any(|p| p.name == "project"));
    assert!(!loader
        .get_themes()
        .themes
        .iter()
        .any(|t| t.name.as_deref() == Some("project-theme")));
}

#[test]
fn discover_append_system_md() {
    let e = env();
    write(
        &join(&e.cwd, &[".pi", "APPEND_SYSTEM.md"]),
        "Additional instructions.",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    assert!(loader
        .get_append_system_prompt()
        .iter()
        .any(|s| s.contains("Additional instructions.")));
}

/// Reachable slice of pi's "load user extensions before trust and reuse them
/// after trust resolves": the two-pass control flow (pre-trust pass, callback,
/// trust applied) runs through the stub. With the real loader this also asserts
/// the load-count and extension ordering (see the `#[ignore]` case below).
#[test]
fn trust_callback_resolving_true_enables_project_skills() {
    let e = env();
    write(
        &join(&e.cwd, &[".pi", "skills", "project-skill", "SKILL.md"]),
        &skill_md("project-skill", "Project skill"),
    );

    let mut options = base_opts(&e);
    options.settings_manager = Some(SettingsManager::create_with_options(
        &e.cwd,
        &e.agent,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    ));
    let mut loader = DefaultResourceLoader::new(options);

    let fired = Arc::new(AtomicBool::new(false));
    let fired_cb = Arc::clone(&fired);
    loader.reload(ReloadOptions {
        resolve_project_trust: Some(Box::new(move |pre| {
            // The pre-trust pass loads only user/global + temporary extensions
            // (empty behind the stub); the callback resolves the project trusted.
            assert!(pre.extensions.is_empty());
            fired_cb.store(true, Ordering::SeqCst);
            true
        })),
    });

    assert!(fired.load(Ordering::SeqCst));
    // Trust resolved true, so the project skill is now discovered.
    assert!(loader
        .get_skills()
        .skills
        .iter()
        .any(|s| s.name == "project-skill"));
}

// == describe("extendResources") ============================================

#[test]
fn extend_resources_loads_skills_and_prompts_with_metadata() {
    let e = env();
    let extra_skill_dir = join(&e.root, &["extra-skills", "extra-skill"]);
    let skill_path = join(&extra_skill_dir, &["SKILL.md"]);
    write(&skill_path, &skill_md("extra-skill", "Extra skill"));

    let extra_prompt_dir = join(&e.root, &["extra-prompts"]);
    let prompt_path = join(&extra_prompt_dir, &["extra.md"]);
    write(
        &prompt_path,
        "---\ndescription: Extra prompt\n---\nExtra prompt content",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    loader.extend_resources(&ResourceExtensionPaths {
        skill_paths: vec![ExtensionPathEntry {
            path: extra_skill_dir.clone(),
            metadata: ext_meta("extension:extra", &extra_skill_dir),
        }],
        prompt_paths: vec![ExtensionPathEntry {
            path: prompt_path.clone(),
            metadata: ext_meta("extension:extra", &extra_prompt_dir),
        }],
        theme_paths: vec![],
    });

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|s| s.name == "extra-skill")
        .expect("extra-skill loaded");
    assert_eq!(skill.source_info.source, "extension:extra");
    assert_eq!(skill.source_info.path, skill_path);

    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|p| p.name == "extra")
        .expect("extra prompt loaded");
    assert_eq!(prompt.source_info.source, "extension:extra");
    assert_eq!(prompt.source_info.path, prompt_path);
}

#[test]
fn extend_resources_loads_extension_resources_as_file_urls() {
    let e = env();
    let extra_skill_dir = join(&e.root, &["extra skills", "file-url-skill"]);
    let skill_path = join(&extra_skill_dir, &["SKILL.md"]);
    write(&skill_path, &skill_md("file-url-skill", "File URL skill"));

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let file_url = format!("file://{extra_skill_dir}");
    loader.extend_resources(&ResourceExtensionPaths {
        skill_paths: vec![ExtensionPathEntry {
            path: file_url,
            metadata: ext_meta("extension:file-url", &extra_skill_dir),
        }],
        ..Default::default()
    });

    let result = loader.get_skills();
    assert!(result.diagnostics.is_empty());
    let skill = result
        .skills
        .into_iter()
        .find(|s| s.name == "file-url-skill")
        .expect("file-url-skill loaded");
    assert_eq!(skill.file_path, skill_path);
    assert_eq!(skill.source_info.source, "extension:file-url");
}

// == describe("noSkills option") ============================================

#[test]
fn skip_skill_discovery_when_no_skills_is_true() {
    let e = env();
    write(
        &join(&e.agent, &["skills", "test-skill.md"]),
        &skill_md("test-skill", "A test skill"),
    );

    let mut options = base_opts(&e);
    options.no_skills = true;
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert!(loader.get_skills().skills.is_empty());
}

#[test]
fn still_load_additional_skill_paths_when_no_skills_is_true() {
    let e = env();
    let custom_dir = join(&e.root, &["custom-skills"]);
    write(
        &join(&custom_dir, &["custom.md"]),
        &skill_md("custom", "Custom skill"),
    );

    let mut options = base_opts(&e);
    options.no_skills = true;
    options.additional_skill_paths = vec![custom_dir];
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert!(loader
        .get_skills()
        .skills
        .iter()
        .any(|s| s.name == "custom"));
}

// == describe("override functions") =========================================

#[test]
fn apply_skills_override() {
    let e = env();
    let injected = atilla_coding::core::skills::Skill {
        name: "injected".to_string(),
        description: "Injected skill".to_string(),
        file_path: "/fake/path".to_string(),
        base_dir: "/fake".to_string(),
        source_info: atilla_coding::core::skills::SourceInfo {
            path: "/fake/path".to_string(),
            source: "custom".to_string(),
            scope: atilla_coding::core::skills::SourceScope::Temporary,
            origin: atilla_coding::core::skills::SourceOrigin::TopLevel,
            base_dir: None,
        },
        disable_model_invocation: false,
    };

    let mut options = base_opts(&e);
    options.skills_override = Some(Box::new(move |_base| {
        atilla_coding::core::skills::LoadSkillsResult {
            skills: vec![injected.clone()],
            diagnostics: vec![],
        }
    }));
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    let skills = loader.get_skills().skills;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "injected");
}

#[test]
fn apply_system_prompt_override() {
    let e = env();
    let mut options = base_opts(&e);
    options.system_prompt_override =
        Some(Box::new(|_base| Some("Custom system prompt".to_string())));
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Custom system prompt")
    );
}

// == cases that need the REAL extension runtime ============================
// The 5 cases that load real `.ts` extension modules (symlinked-extensions-once,
// trust-reuse-load-count, command-name-collision, tool-conflict-detection,
// CLI-vs-discovered precedence) MOVED to
// `crates/atilla-extensions/tests/deno_resource_loader.rs` (deno-gated), where
// the real `RealExtensionLoader` is injected via the seam. They can't live here:
// the real loader is in atilla-extensions (depends on atilla-coding + needs V8),
// so referencing it from an atilla-coding test would be a dependency cycle and
// V8 cannot build in-sandbox. The `ExtensionLoader` interface stays covered here
// against `StubExtensionLoader` (see `custom_extension_loader_is_held`).
