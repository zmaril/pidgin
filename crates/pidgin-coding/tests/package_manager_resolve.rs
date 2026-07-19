// straitjacket-allow-file:duplication — faithful port of pi coding-agent's
// package-manager resolve/discovery test suites; the per-`it` fixture setup
// (mkdir/write real trees, resolve, assert paths) parallels pi by design.

//! Ported from `vendor/pi/packages/coding-agent/test/package-manager.test.ts`,
//! the pure-filesystem resolution describe-blocks (`resolve`,
//! `auto-discovered skill metadata`, `.agents/skills auto-discovery`,
//! `ignore files`, `resolveExtensionSources`, `pattern filtering ...`,
//! `force-include/-exclude patterns`, `package deduplication`, and
//! `multi-file extension discovery`).
//!
//! pi mutates `process.env.HOME` and drives `SettingsManager`; here the resolver
//! takes an explicit home dir and a plain [`ResolveSettings`] snapshot, so the
//! tests avoid mutating the process environment (which is racy under parallel
//! `cargo test`). The command-mock cohort of `package-manager.test.ts` (npm/git
//! install argv) is the separate command concern ported in #72 and is not here.

use pidgin_coding::core::package_manager::{ResolvedResource, ScopeResources};
use pidgin_coding::core::source_info::SourceScope;
use serde_json::json;

mod common;
use common::*;

// -- describe("resolve") ----------------------------------------------------

#[test]
fn returns_no_package_paths_when_no_sources_configured() {
    let fx = Fixture::new();
    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    assert!(result.extensions.is_empty());
    assert!(result.prompts.is_empty());
    assert!(result.themes.is_empty());
    assert!(result.skills.iter().all(|r| r.metadata.source == "auto"
        && r.metadata.origin == pidgin_coding::core::source_info::SourceOrigin::TopLevel));
}

#[test]
fn resolves_local_extension_paths_from_settings() {
    let fx = Fixture::new();
    let ext_path = join(&fx.agent_dir, &["extensions", "my-extension.ts"]);
    write(&ext_path, "export default function() {}");
    let global = ScopeResources {
        extensions: vec!["extensions/my-extension.ts".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(some_enabled(&result.extensions, &ext_path));
}

#[test]
fn resolves_skill_paths_from_settings() {
    let fx = Fixture::new();
    let skill_file = join(&fx.agent_dir, &["skills", "my-skill", "SKILL.md"]);
    write(&skill_file, &skill_md("test-skill"));
    let global = ScopeResources {
        skills: vec!["skills".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(some_enabled(&result.skills, &skill_file));
}

#[test]
fn auto_discovers_root_markdown_skills_from_pi_skill_dirs() {
    let fx = Fixture::new();
    let skill_file = join(&fx.agent_dir, &["skills", "single-file.md"]);
    write(&skill_file, &skill_md("single-file"));
    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    assert!(some_enabled(&result.skills, &skill_file));
}

#[test]
fn resolves_project_paths_relative_to_pi() {
    let fx = Fixture::new();
    let ext_path = join(&fx.root, &[".pi", "extensions", "project-ext.ts"]);
    write(&ext_path, "export default function() {}");
    let project = ScopeResources {
        extensions: vec!["extensions/project-ext.ts".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(empty(), project), None);
    assert!(some_enabled(&result.extensions, &ext_path));
}

#[test]
fn auto_discovers_user_prompts_with_overrides() {
    let fx = Fixture::new();
    let prompt_path = join(&fx.agent_dir, &["prompts", "auto.md"]);
    write(&prompt_path, "Auto prompt");
    let global = ScopeResources {
        prompts: vec!["!prompts/auto.md".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(some_disabled(&result.prompts, &prompt_path));
}

#[test]
fn resolves_symlinked_user_and_project_resources_once() {
    let fx = Fixture::new();
    let shared = join(&fx.root, &["shared-resources"]);
    for kind in ["extensions", "skills", "prompts", "themes"] {
        mkdir(&join(&shared, &[kind]));
    }
    write(
        &join(&shared, &["extensions", "shared.ts"]),
        "export default function() {}",
    );
    write(
        &join(&shared, &["skills", "shared-skill", "SKILL.md"]),
        &skill_md("shared-skill"),
    );
    write(&join(&shared, &["prompts", "shared.md"]), "Shared prompt");
    write(
        &join(&shared, &["themes", "shared.json"]),
        "{\"name\":\"shared-theme\"}",
    );

    mkdir(&fx.agent_dir);
    mkdir(&join(&fx.root, &[".pi"]));
    for kind in ["extensions", "skills", "prompts", "themes"] {
        symlink_dir(&join(&shared, &[kind]), &join(&fx.agent_dir, &[kind]));
        symlink_dir(&join(&shared, &[kind]), &join(&fx.root, &[".pi", kind]));
    }

    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.prompts.len(), 1);
    assert_eq!(result.themes.len(), 1);
    assert_eq!(result.extensions[0].metadata.scope, SourceScope::Project);
    assert_eq!(result.skills[0].metadata.scope, SourceScope::Project);
    assert_eq!(result.prompts[0].metadata.scope, SourceScope::Project);
    assert_eq!(result.themes[0].metadata.scope, SourceScope::Project);
}

#[test]
fn auto_discovers_project_prompts_with_overrides() {
    let fx = Fixture::new();
    let prompt_path = join(&fx.root, &[".pi", "prompts", "is.md"]);
    write(&prompt_path, "Is prompt");
    let project = ScopeResources {
        prompts: vec!["!prompts/is.md".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(empty(), project), None);
    assert!(some_disabled(&result.prompts, &prompt_path));
}

#[test]
fn resolves_directory_with_package_json_pi_extensions_in_extensions_setting() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["my-extensions-pkg"]);
    mkdir(&join(&pkg_dir, &["extensions"]));
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "my-extensions-pkg",
            "pi": { "extensions": ["./extensions/clip.ts", "./extensions/cost.ts"] }
        })
        .to_string(),
    );
    write(
        &join(&pkg_dir, &["extensions", "clip.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "cost.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "helper.ts"]),
        "export const x = 1;",
    );
    let global = ScopeResources {
        extensions: vec![pkg_dir.clone()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(some_enabled(
        &result.extensions,
        &join(&pkg_dir, &["extensions", "clip.ts"])
    ));
    assert!(some_enabled(
        &result.extensions,
        &join(&pkg_dir, &["extensions", "cost.ts"])
    ));
    assert!(!ends_any(&result.extensions, "helper.ts"));
}

// -- describe("auto-discovered skill metadata") -----------------------------

#[test]
fn basedir_is_agent_dir_for_user_pi_skills() {
    let fx = Fixture::new();
    let skill_path = join(&fx.agent_dir, &["skills", "user-pi", "SKILL.md"]);
    write(&skill_path, &skill_md("user-pi"));
    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    let skill = result.skills.iter().find(|r| r.path == skill_path).unwrap();
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(skill.metadata.scope, SourceScope::User);
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(fx.agent_dir.as_str())
    );
}

#[test]
fn basedir_is_project_pi_dir_for_project_pi_skills() {
    let fx = Fixture::new();
    let project_base = join(&fx.root, &[".pi"]);
    let skill_path = join(&project_base, &["skills", "project-pi", "SKILL.md"]);
    write(&skill_path, &skill_md("project-pi"));
    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    let skill = result.skills.iter().find(|r| r.path == skill_path).unwrap();
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(skill.metadata.scope, SourceScope::Project);
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(project_base.as_str())
    );
}

#[test]
fn basedir_is_home_agents_for_user_agents_skills() {
    let fx = Fixture::new();
    let agents_base = join(&fx.root, &[".agents"]);
    let skill_path = join(&agents_base, &["skills", "user-agents", "SKILL.md"]);
    write(&skill_path, &skill_md("user-agents"));
    let result = fx
        .resolver_home_root(&fx.root, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    let skill = result.skills.iter().find(|r| r.path == skill_path).unwrap();
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(skill.metadata.scope, SourceScope::User);
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(agents_base.as_str())
    );
}

#[test]
fn basedir_is_each_project_agents_dir_for_project_agents_skills() {
    let fx = Fixture::new();
    let repo_root = join(&fx.root, &["repo"]);
    let nested_cwd = join(&repo_root, &["packages", "feature"]);
    mkdir(&nested_cwd);
    mkdir(&join(&repo_root, &[".git"]));

    let repo_agents_base = join(&repo_root, &[".agents"]);
    let repo_skill = join(&repo_agents_base, &["skills", "repo", "SKILL.md"]);
    write(&repo_skill, &skill_md("repo"));

    let package_agents_base = join(&repo_root, &["packages", ".agents"]);
    let package_skill = join(&package_agents_base, &["skills", "package", "SKILL.md"]);
    write(&package_skill, &skill_md("package"));

    let result = fx
        .resolver_at(&nested_cwd, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    let repo = result.skills.iter().find(|r| r.path == repo_skill).unwrap();
    let pkg = result
        .skills
        .iter()
        .find(|r| r.path == package_skill)
        .unwrap();
    assert_eq!(repo.metadata.scope, SourceScope::Project);
    assert_eq!(
        repo.metadata.base_dir.as_deref(),
        Some(repo_agents_base.as_str())
    );
    assert_eq!(pkg.metadata.scope, SourceScope::Project);
    assert_eq!(
        pkg.metadata.base_dir.as_deref(),
        Some(package_agents_base.as_str())
    );
}

// -- describe(".agents/skills auto-discovery") ------------------------------

#[test]
fn scans_agents_skills_from_cwd_up_to_git_repo_root() {
    let fx = Fixture::new();
    let repo_root = join(&fx.root, &["repo"]);
    let nested_cwd = join(&repo_root, &["packages", "feature"]);
    mkdir(&nested_cwd);
    mkdir(&join(&repo_root, &[".git"]));

    let above_repo_skill = join(&fx.root, &[".agents", "skills", "above-repo", "SKILL.md"]);
    write(&above_repo_skill, &skill_md("above-repo"));
    let repo_root_skill = join(&repo_root, &[".agents", "skills", "repo-root", "SKILL.md"]);
    write(&repo_root_skill, &skill_md("repo-root"));
    let nested_skill = join(
        &repo_root,
        &["packages", ".agents", "skills", "nested", "SKILL.md"],
    );
    write(&nested_skill, &skill_md("nested"));

    let result = fx
        .resolver_at(&nested_cwd, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    assert!(some_enabled(&result.skills, &repo_root_skill));
    assert!(some_enabled(&result.skills, &nested_skill));
    assert!(!result.skills.iter().any(|r| r.path == above_repo_skill));
}

#[test]
fn scans_agents_skills_up_to_fs_root_when_not_in_git_repo() {
    let fx = Fixture::new();
    let non_repo_root = join(&fx.root, &["non-repo"]);
    let nested_cwd = join(&non_repo_root, &["a", "b"]);
    mkdir(&nested_cwd);

    let root_skill = join(&non_repo_root, &[".agents", "skills", "root", "SKILL.md"]);
    write(&root_skill, &skill_md("root"));
    let middle_skill = join(
        &non_repo_root,
        &["a", ".agents", "skills", "middle", "SKILL.md"],
    );
    write(&middle_skill, &skill_md("middle"));

    let result = fx
        .resolver_at(&nested_cwd, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    assert!(some_enabled(&result.skills, &root_skill));
    assert!(some_enabled(&result.skills, &middle_skill));
}

#[test]
fn ignores_root_markdown_files_in_agents_skills() {
    let fx = Fixture::new();
    let agents_skills_dir = join(&fx.root, &[".agents", "skills"]);
    let root_skill = join(&agents_skills_dir, &["root-file.md"]);
    let nested_skill = join(&agents_skills_dir, &["nested-skill", "SKILL.md"]);
    write(&root_skill, &skill_md("root-file"));
    write(&nested_skill, &skill_md("nested-skill"));

    let work = join(&fx.root, &["work"]);
    mkdir(&work);
    let result = fx
        .resolver_at(&work, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    assert!(!result.skills.iter().any(|r| r.path == root_skill));
    assert!(some_enabled(&result.skills, &nested_skill));
}

#[test]
fn keeps_home_agents_skills_user_scoped_under_home_non_git() {
    let fx = Fixture::new();
    let cwd = join(&fx.root, &["scratch", "nested"]);
    let local_agent_dir = join(&fx.root, &[".pi", "agent"]);
    mkdir(&cwd);
    mkdir(&local_agent_dir);

    let home_skill = join(&fx.root, &[".agents", "skills", "home-skill", "SKILL.md"]);
    write(&home_skill, &skill_md("home-skill"));

    let result = fx
        .resolver_home_root(&cwd, &local_agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    let matching: Vec<&ResolvedResource> = result
        .skills
        .iter()
        .filter(|r| r.path == home_skill)
        .collect();
    assert_eq!(matching.len(), 1);
    assert!(matching[0].enabled);
    assert_eq!(matching[0].metadata.scope, SourceScope::User);
    assert_eq!(matching[0].metadata.source, "auto");
}

#[test]
fn dedupes_user_skill_entries_when_pi_agent_skills_symlinks_agents_skills() {
    let fx = Fixture::new();
    let agent_skills_dir = join(&fx.agent_dir, &["skills"]);
    let agents_skills_dir = join(&fx.root, &[".agents", "skills"]);
    mkdir(&agents_skills_dir);
    symlink_dir(&agents_skills_dir, &agent_skills_dir);

    let skill_path = join(&agents_skills_dir, &["foo", "SKILL.md"]);
    write(&skill_path, &skill_md("foo"));

    let result = fx
        .resolver_home_root(&fx.root, &fx.agent_dir)
        .resolve(&trusted(empty(), empty()), None);
    let foo: Vec<&ResolvedResource> = result
        .skills
        .iter()
        .filter(|r| norm(&r.path).ends_with("foo/SKILL.md"))
        .collect();
    assert_eq!(foo.len(), 1);
}

// -- describe("ignore files") -----------------------------------------------

#[test]
fn respects_gitignore_in_skill_directories() {
    let fx = Fixture::new();
    let skills_dir = join(&fx.agent_dir, &["skills"]);
    mkdir(&skills_dir);
    write(&join(&skills_dir, &[".gitignore"]), "venv\n__pycache__\n");
    write(
        &join(&skills_dir, &["good-skill", "SKILL.md"]),
        &skill_md("good-skill"),
    );
    write(
        &join(&skills_dir, &["venv", "bad-skill", "SKILL.md"]),
        &skill_md("bad-skill"),
    );
    let global = ScopeResources {
        skills: vec!["skills".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(incl_enabled(&result.skills, "good-skill"));
    assert!(!incl_enabled(&result.skills, "venv"));
}

#[test]
fn does_not_apply_parent_gitignore_to_pi_auto_discovery() {
    let fx = Fixture::new();
    write(&join(&fx.root, &[".gitignore"]), ".pi\n");
    let skill_path = join(&fx.root, &[".pi", "skills", "auto-skill", "SKILL.md"]);
    write(&skill_path, &skill_md("auto-skill"));
    let result = fx.resolver().resolve(&trusted(empty(), empty()), None);
    assert!(some_enabled(&result.skills, &skill_path));
}

// -- describe("resolveExtensionSources") ------------------------------------

#[test]
fn ext_sources_resolves_local_paths() {
    let fx = Fixture::new();
    let ext_path = join(&fx.root, &["ext.ts"]);
    write(&ext_path, "export default function() {}");
    let result =
        fx.resolver()
            .resolve_extension_sources(std::slice::from_ref(&ext_path), false, false);
    assert!(some_enabled(&result.extensions, &ext_path));
}

#[test]
fn ext_sources_handles_directories_with_pi_manifest() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["my-package"]);
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "my-package",
            "pi": { "extensions": ["./src/index.ts"], "skills": ["./skills"] }
        })
        .to_string(),
    );
    write(
        &join(&pkg_dir, &["src", "index.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["skills", "my-skill", "SKILL.md"]),
        &skill_md("my-skill"),
    );
    let result =
        fx.resolver()
            .resolve_extension_sources(std::slice::from_ref(&pkg_dir), false, false);
    assert!(some_enabled(
        &result.extensions,
        &join(&pkg_dir, &["src", "index.ts"])
    ));
    assert!(some_enabled(
        &result.skills,
        &join(&pkg_dir, &["skills", "my-skill", "SKILL.md"])
    ));
}

#[test]
fn ext_sources_keeps_pi_manifest_entries_with_leading_tilde_package_relative() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["tilde-manifest-package"]);
    let direct_ext = join(&pkg_dir, &["~extensions", "main.ts"]);
    let slash_ext = join(&pkg_dir, &["~", "extensions", "alt.ts"]);
    let direct_skill = join(&pkg_dir, &["~skills", "direct-skill", "SKILL.md"]);
    let slash_skill = join(&pkg_dir, &["~", "skills", "slash-skill", "SKILL.md"]);
    write(&direct_ext, "export default function() {}");
    write(&slash_ext, "export default function() {}");
    write(&direct_skill, &skill_md("direct-skill"));
    write(&slash_skill, &skill_md("slash-skill"));
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "tilde-manifest-package",
            "pi": {
                "extensions": ["~extensions/main.ts", "~/extensions/alt.ts"],
                "skills": ["~skills", "~/skills"]
            }
        })
        .to_string(),
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(some_enabled(&result.extensions, &direct_ext));
    assert!(some_enabled(&result.extensions, &slash_ext));
    assert!(some_enabled(&result.skills, &direct_skill));
    assert!(some_enabled(&result.skills, &slash_skill));
}

#[test]
fn ext_sources_handles_directories_with_auto_discovery_layout() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["auto-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "main.ts"]),
        "export default function() {}",
    );
    write(&join(&pkg_dir, &["themes", "dark.json"]), "{}");
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "main.ts"));
    assert!(ends_enabled(&result.themes, "dark.json"));
}

#[test]
fn ext_sources_stops_recursing_when_package_skill_dir_contains_skill_md() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["skill-root-pkg"]);
    let root_skill = join(&pkg_dir, &["skills", "root-skill", "SKILL.md"]);
    let nested_skill = join(
        &pkg_dir,
        &["skills", "root-skill", "nested-skill", "SKILL.md"],
    );
    write(&root_skill, &skill_md("root-skill"));
    write(&nested_skill, &skill_md("nested-skill"));
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(some_enabled(&result.skills, &root_skill));
    assert!(!result.skills.iter().any(|r| r.path == nested_skill));
}

// -- describe("pattern filtering in top-level arrays") ----------------------

#[test]
fn excludes_extensions_with_bang_pattern() {
    let fx = Fixture::new();
    let ext_dir = join(&fx.agent_dir, &["extensions"]);
    write(
        &join(&ext_dir, &["keep.ts"]),
        "export default function() {}",
    );
    write(
        &join(&ext_dir, &["remove.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        extensions: vec!["extensions".into(), "!**/remove.ts".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "keep.ts"));
    assert!(ends_disabled(&result.extensions, "remove.ts"));
}

#[test]
fn filters_themes_with_glob_patterns() {
    let fx = Fixture::new();
    let themes_dir = join(&fx.agent_dir, &["themes"]);
    for t in ["dark", "light", "funky"] {
        write(&join(&themes_dir, &[&format!("{t}.json")]), "{}");
    }
    let global = ScopeResources {
        themes: vec!["themes".into(), "!funky.json".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.themes, "dark.json"));
    assert!(ends_enabled(&result.themes, "light.json"));
    assert!(ends_disabled(&result.themes, "funky.json"));
}

#[test]
fn filters_prompts_with_exclusion_pattern() {
    let fx = Fixture::new();
    let prompts_dir = join(&fx.agent_dir, &["prompts"]);
    write(&join(&prompts_dir, &["review.md"]), "Review code");
    write(&join(&prompts_dir, &["explain.md"]), "Explain code");
    let global = ScopeResources {
        prompts: vec!["prompts".into(), "!explain.md".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.prompts, "review.md"));
    assert!(ends_disabled(&result.prompts, "explain.md"));
}

#[test]
fn filters_skills_with_exclusion_pattern() {
    let fx = Fixture::new();
    let skills_dir = join(&fx.agent_dir, &["skills"]);
    write(
        &join(&skills_dir, &["good-skill", "SKILL.md"]),
        &skill_md("good-skill"),
    );
    write(
        &join(&skills_dir, &["bad-skill", "SKILL.md"]),
        &skill_md("bad-skill"),
    );
    let global = ScopeResources {
        skills: vec!["skills".into(), "!**/bad-skill".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(incl_enabled(&result.skills, "good-skill"));
    assert!(incl_disabled(&result.skills, "bad-skill"));
}

#[test]
fn works_without_patterns_backward_compatible() {
    let fx = Fixture::new();
    let ext_path = join(&fx.agent_dir, &["extensions", "my-ext.ts"]);
    write(&ext_path, "export default function() {}");
    let global = ScopeResources {
        extensions: vec!["extensions/my-ext.ts".into()],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(some_enabled(&result.extensions, &ext_path));
}

// -- describe("pattern filtering in pi manifest") ---------------------------

#[test]
fn supports_glob_patterns_in_manifest_extensions() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["manifest-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "local.ts"]),
        "export default function() {}",
    );
    write(
        &join(
            &pkg_dir,
            &["node_modules", "dep", "extensions", "remote.ts"],
        ),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["node_modules", "dep", "extensions", "skip.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "manifest-pkg",
            "pi": { "extensions": ["extensions", "node_modules/dep/extensions", "!**/skip.ts"] }
        })
        .to_string(),
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "local.ts"));
    assert!(ends_enabled(&result.extensions, "remote.ts"));
    assert!(!ends_any(&result.extensions, "skip.ts"));
}

#[test]
fn supports_glob_patterns_in_manifest_skills() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["skill-manifest-pkg"]);
    write(
        &join(&pkg_dir, &["skills", "good-skill", "SKILL.md"]),
        &skill_md("good-skill"),
    );
    write(
        &join(&pkg_dir, &["skills", "bad-skill", "SKILL.md"]),
        &skill_md("bad-skill"),
    );
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "skill-manifest-pkg",
            "pi": { "skills": ["skills", "!**/bad-skill"] }
        })
        .to_string(),
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(incl_enabled(&result.skills, "good-skill"));
    assert!(!result
        .skills
        .iter()
        .any(|r| norm(&r.path).contains("bad-skill")));
}

#[test]
fn expands_positive_glob_manifest_entries_before_collecting_skills() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["skill-manifest-glob-pkg"]);
    write(
        &join(
            &pkg_dir,
            &[
                "plugins",
                "pdf-to-markdown",
                "skills",
                "pdf-to-markdown",
                "SKILL.md",
            ],
        ),
        &skill_md("pdf-to-markdown"),
    );
    write(
        &join(
            &pkg_dir,
            &[
                "plugins",
                "nutrient-dws",
                "skills",
                "document-processor-api",
                "SKILL.md",
            ],
        ),
        &skill_md("document-processor-api"),
    );
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "skill-manifest-glob-pkg",
            "pi": { "skills": ["./plugins/*/skills"] }
        })
        .to_string(),
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(incl_enabled(&result.skills, "pdf-to-markdown"));
    assert!(incl_enabled(&result.skills, "document-processor-api"));
}

// -- describe("pattern filtering in package filters") -----------------------

#[test]
fn applies_user_filters_on_top_of_manifest_filters() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["layered-pkg"]);
    for f in ["foo", "bar", "baz"] {
        write(
            &join(&pkg_dir, &["extensions", &format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({ "name": "layered-pkg", "pi": { "extensions": ["extensions", "!**/baz.ts"] } })
            .to_string(),
    );
    let global = ScopeResources {
        packages: vec![pkg_filter(&pkg_dir, "extensions", &["!**/bar.ts"])],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "foo.ts"));
    assert!(ends_disabled(&result.extensions, "bar.ts"));
    assert!(!ends_any(&result.extensions, "baz.ts"));
}

#[test]
fn excludes_extensions_from_package_with_bang_pattern() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["pattern-pkg"]);
    for f in ["foo", "bar", "baz"] {
        write(
            &join(&pkg_dir, &["extensions", &format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    let global = ScopeResources {
        packages: vec![pkg_filter(&pkg_dir, "extensions", &["!**/baz.ts"])],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "foo.ts"));
    assert!(ends_enabled(&result.extensions, "bar.ts"));
    assert!(ends_disabled(&result.extensions, "baz.ts"));
}

#[test]
fn filters_themes_from_package() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["theme-pkg"]);
    write(&join(&pkg_dir, &["themes", "nice.json"]), "{}");
    write(&join(&pkg_dir, &["themes", "ugly.json"]), "{}");
    let global = ScopeResources {
        packages: vec![pkg_filter(&pkg_dir, "themes", &["!ugly.json"])],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.themes, "nice.json"));
    assert!(ends_disabled(&result.themes, "ugly.json"));
}

#[test]
fn combines_include_and_exclude_patterns() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["combo-pkg"]);
    for f in ["alpha", "beta", "gamma"] {
        write(
            &join(&pkg_dir, &["extensions", &format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    let global = ScopeResources {
        packages: vec![pkg_filter(
            &pkg_dir,
            "extensions",
            &["**/alpha.ts", "**/beta.ts", "!**/beta.ts"],
        )],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "alpha.ts"));
    assert!(ends_disabled(&result.extensions, "beta.ts"));
    assert!(ends_disabled(&result.extensions, "gamma.ts"));
}

#[test]
fn works_with_direct_paths_no_patterns() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["direct-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "one.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "two.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        packages: vec![pkg_filter(&pkg_dir, "extensions", &["extensions/one.ts"])],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "one.ts"));
    assert!(ends_disabled(&result.extensions, "two.ts"));
}

#[test]
fn resolves_autoload_disabled_project_entries_as_deltas_over_global() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.agent_dir, &["npm", "node_modules", "pi-tools"]);
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({ "name": "pi-tools", "version": "1.0.0" }).to_string(),
    );
    write(
        &join(&pkg_dir, &["extensions", "foo.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "bar.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        packages: vec![json!("npm:pi-tools")],
        ..empty()
    };
    let project = ScopeResources {
        packages: vec![json!({
            "source": "npm:pi-tools", "autoload": false, "extensions": ["-extensions/foo.ts"]
        })],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, project), None);
    let foo = join(&pkg_dir, &["extensions", "foo.ts"]);
    let bar = join(&pkg_dir, &["extensions", "bar.ts"]);
    let foo_res = result.extensions.iter().find(|r| r.path == foo).unwrap();
    let bar_res = result.extensions.iter().find(|r| r.path == bar).unwrap();
    assert!(!foo_res.enabled);
    assert_eq!(foo_res.metadata.scope, SourceScope::Project);
    assert!(bar_res.enabled);
    assert_eq!(bar_res.metadata.scope, SourceScope::User);
}

#[test]
fn resolves_autoload_disabled_entries_as_positive_only_without_global() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["positive-only-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "foo.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "bar.ts"]),
        "export default function() {}",
    );
    write(&join(&pkg_dir, &["skills", "foo", "SKILL.md"]), "# Foo\n");
    // relative(join(root, ".pi"), pkgDir) == "../positive-only-pkg"
    let project = ScopeResources {
        packages: vec![json!({
            "source": "../positive-only-pkg",
            "autoload": false,
            "extensions": ["+extensions/foo.ts"]
        })],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(empty(), project), None);
    let paths: Vec<String> = result.extensions.iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, vec![join(&pkg_dir, &["extensions", "foo.ts"])]);
    assert!(result.skills.is_empty());
}

// -- describe("force-include patterns") -------------------------------------

#[test]
fn force_includes_extensions_with_plus_after_exclusion() {
    let fx = Fixture::new();
    let ext_dir = join(&fx.agent_dir, &["extensions"]);
    for f in ["keep", "excluded", "force-back"] {
        write(
            &join(&ext_dir, &[&format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    let global = ScopeResources {
        extensions: vec![
            "extensions".into(),
            "!extensions/*.ts".into(),
            "+extensions/force-back.ts".into(),
        ],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.extensions, "keep.ts"));
    assert!(ends_disabled(&result.extensions, "excluded.ts"));
    assert!(ends_enabled(&result.extensions, "force-back.ts"));
}

#[test]
fn force_include_overrides_exclude_in_package_filters() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["force-pkg"]);
    for f in ["alpha", "beta", "gamma"] {
        write(
            &join(&pkg_dir, &["extensions", &format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    let global = ScopeResources {
        packages: vec![pkg_filter(
            &pkg_dir,
            "extensions",
            &["!**/*.ts", "+extensions/beta.ts"],
        )],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.extensions, "alpha.ts"));
    assert!(ends_enabled(&result.extensions, "beta.ts"));
    assert!(ends_disabled(&result.extensions, "gamma.ts"));
}

#[test]
fn force_includes_multiple_resources() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["multi-force-pkg"]);
    for s in ["skill-a", "skill-b", "skill-c"] {
        write(&join(&pkg_dir, &["skills", s, "SKILL.md"]), &skill_md(s));
    }
    let global = ScopeResources {
        packages: vec![pkg_filter(
            &pkg_dir,
            "skills",
            &["!**/*", "+skills/skill-a", "+skills/skill-c"],
        )],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(incl_enabled(&result.skills, "skill-a"));
    assert!(incl_disabled(&result.skills, "skill-b"));
    assert!(incl_enabled(&result.skills, "skill-c"));
}

#[test]
fn force_includes_after_specific_exclusion() {
    let fx = Fixture::new();
    let ext_dir = join(&fx.agent_dir, &["extensions"]);
    write(&join(&ext_dir, &["a.ts"]), "export default function() {}");
    write(&join(&ext_dir, &["b.ts"]), "export default function() {}");
    let global = ScopeResources {
        extensions: vec![
            "extensions".into(),
            "!extensions/b.ts".into(),
            "+extensions/b.ts".into(),
        ],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_enabled(&result.extensions, "a.ts"));
    assert!(ends_enabled(&result.extensions, "b.ts"));
}

#[test]
fn handles_force_include_in_manifest_patterns() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["manifest-force-pkg"]);
    for f in ["one", "two", "three"] {
        write(
            &join(&pkg_dir, &["extensions", &format!("{f}.ts")]),
            "export default function() {}",
        );
    }
    write(
        &join(&pkg_dir, &["package.json"]),
        &json!({
            "name": "manifest-force-pkg",
            "pi": { "extensions": ["extensions", "!**/two.ts", "+extensions/two.ts"] }
        })
        .to_string(),
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "one.ts"));
    assert!(ends_enabled(&result.extensions, "two.ts"));
    assert!(ends_enabled(&result.extensions, "three.ts"));
}

#[test]
fn force_includes_themes() {
    let fx = Fixture::new();
    let themes_dir = join(&fx.agent_dir, &["themes"]);
    for t in ["dark", "light", "special"] {
        write(&join(&themes_dir, &[&format!("{t}.json")]), "{}");
    }
    let global = ScopeResources {
        themes: vec![
            "themes".into(),
            "!themes/*.json".into(),
            "+themes/special.json".into(),
        ],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.themes, "dark.json"));
    assert!(ends_disabled(&result.themes, "light.json"));
    assert!(ends_enabled(&result.themes, "special.json"));
}

#[test]
fn force_includes_prompts() {
    let fx = Fixture::new();
    let prompts_dir = join(&fx.agent_dir, &["prompts"]);
    for p in ["review", "explain", "debug"] {
        write(&join(&prompts_dir, &[&format!("{p}.md")]), p);
    }
    let global = ScopeResources {
        prompts: vec![
            "prompts".into(),
            "!prompts/*.md".into(),
            "+prompts/debug.md".into(),
        ],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.prompts, "review.md"));
    assert!(ends_disabled(&result.prompts, "explain.md"));
    assert!(ends_enabled(&result.prompts, "debug.md"));
}

// -- describe("force-exclude patterns") -------------------------------------

#[test]
fn force_excludes_top_level_resources() {
    let fx = Fixture::new();
    let ext_dir = join(&fx.agent_dir, &["extensions"]);
    write(
        &join(&ext_dir, &["alpha.ts"]),
        "export default function() {}",
    );
    write(
        &join(&ext_dir, &["beta.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        extensions: vec![
            "extensions".into(),
            "+extensions/alpha.ts".into(),
            "-extensions/alpha.ts".into(),
        ],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.extensions, "alpha.ts"));
    assert!(ends_enabled(&result.extensions, "beta.ts"));
}

#[test]
fn force_excludes_in_package_filters() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["force-exclude-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "alpha.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "beta.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        packages: vec![pkg_filter(
            &pkg_dir,
            "extensions",
            &[
                "extensions/*.ts",
                "+extensions/alpha.ts",
                "-extensions/alpha.ts",
            ],
        )],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, empty()), None);
    assert!(ends_disabled(&result.extensions, "alpha.ts"));
    assert!(ends_enabled(&result.extensions, "beta.ts"));
}

// -- describe("package deduplication") --------------------------------------

#[test]
fn dedupes_same_local_package_in_global_and_project_project_wins() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["shared-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "shared.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        packages: vec![json!(pkg_dir)],
        ..empty()
    };
    let project = ScopeResources {
        packages: vec![json!(pkg_dir)],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, project), None);
    let shared: Vec<&ResolvedResource> = result
        .extensions
        .iter()
        .filter(|r| norm(&r.path).contains("shared-pkg"))
        .collect();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].metadata.scope, SourceScope::Project);
}

#[test]
fn keeps_both_if_different_packages() {
    let fx = Fixture::new();
    let pkg1 = join(&fx.root, &["pkg1"]);
    let pkg2 = join(&fx.root, &["pkg2"]);
    write(
        &join(&pkg1, &["extensions", "from-pkg1.ts"]),
        "export default function() {}",
    );
    write(
        &join(&pkg2, &["extensions", "from-pkg2.ts"]),
        "export default function() {}",
    );
    let global = ScopeResources {
        packages: vec![json!(pkg1)],
        ..empty()
    };
    let project = ScopeResources {
        packages: vec![json!(pkg2)],
        ..empty()
    };
    let result = fx.resolver().resolve(&trusted(global, project), None);
    assert!(result
        .extensions
        .iter()
        .any(|r| norm(&r.path).contains("pkg1")));
    assert!(result
        .extensions
        .iter()
        .any(|r| norm(&r.path).contains("pkg2")));
}

#[test]
fn dedupes_ssh_and_https_urls_for_same_repo() {
    let fx = Fixture::new();
    let r = fx.resolver();
    assert_eq!(
        r.package_identity("https://github.com/user/repo", None),
        "git:github.com/user/repo"
    );
    assert_eq!(
        r.package_identity("git:git@github.com:user/repo", None),
        "git:github.com/user/repo"
    );
}

#[test]
fn dedupes_ssh_and_https_with_refs() {
    let fx = Fixture::new();
    let r = fx.resolver();
    assert_eq!(
        r.package_identity("https://github.com/user/repo@v1.0.0", None),
        "git:github.com/user/repo"
    );
    assert_eq!(
        r.package_identity("git:git@github.com:user/repo@v1.0.0", None),
        "git:github.com/user/repo"
    );
}

#[test]
fn dedupes_ssh_protocol_and_git_at_format() {
    let fx = Fixture::new();
    let r = fx.resolver();
    assert_eq!(
        r.package_identity("ssh://git@github.com/user/repo", None),
        "git:github.com/user/repo"
    );
    assert_eq!(
        r.package_identity("git:git@github.com:user/repo", None),
        "git:github.com/user/repo"
    );
}

#[test]
fn dedupes_all_supported_url_formats_for_same_repo() {
    let fx = Fixture::new();
    let r = fx.resolver();
    let urls = [
        "https://github.com/user/repo",
        "https://github.com/user/repo.git",
        "ssh://git@github.com/user/repo",
        "git:https://github.com/user/repo",
        "git:github.com/user/repo",
        "git:git@github.com:user/repo",
        "git:git@github.com:user/repo.git",
    ];
    let identities: std::collections::HashSet<String> =
        urls.iter().map(|u| r.package_identity(u, None)).collect();
    assert_eq!(identities.len(), 1);
    assert!(identities.contains("git:github.com/user/repo"));
}

#[test]
fn keeps_different_repos_separate() {
    let fx = Fixture::new();
    let r = fx.resolver();
    assert_eq!(
        r.package_identity("https://github.com/user/repo1", None),
        "git:github.com/user/repo1"
    );
    assert_eq!(
        r.package_identity("git:git@github.com:user/repo2", None),
        "git:github.com/user/repo2"
    );
}

// -- describe("multi-file extension discovery (issue #1102)") ---------------

#[test]
fn only_loads_index_ts_from_subdirectories_not_helper_modules() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["multifile-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "subagent", "index.ts"]),
        "import { helper } from \"./agents.ts\";\nexport default function(api) {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "subagent", "agents.ts"]),
        "export function helper() { return \"helper\"; }",
    );
    write(
        &join(&pkg_dir, &["extensions", "standalone.ts"]),
        "export default function(api) {}",
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "subagent/index.ts"));
    assert!(ends_enabled(&result.extensions, "standalone.ts"));
    assert!(!ends_any(&result.extensions, "agents.ts"));
}

#[test]
fn respects_package_json_pi_extensions_manifest_in_subdirectories() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["manifest-subdir-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "custom", "package.json"]),
        &json!({ "pi": { "extensions": ["./main.ts"] } }).to_string(),
    );
    write(
        &join(&pkg_dir, &["extensions", "custom", "main.ts"]),
        "export default function(api) {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "custom", "utils.ts"]),
        "export const util = 1;",
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "custom/main.ts"));
    assert!(!ends_any(&result.extensions, "utils.ts"));
}

#[test]
fn handles_mixed_top_level_files_and_subdirectories() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["mixed-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "simple.ts"]),
        "export default function(api) {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "complex", "index.ts"]),
        "import { a } from './a.ts'; export default function(api) {}",
    );
    write(
        &join(&pkg_dir, &["extensions", "complex", "a.ts"]),
        "export const a = 1;",
    );
    write(
        &join(&pkg_dir, &["extensions", "complex", "b.ts"]),
        "export const b = 2;",
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "simple.ts"));
    assert!(ends_enabled(&result.extensions, "complex/index.ts"));
    assert!(!ends_any(&result.extensions, "complex/a.ts"));
    assert!(!ends_any(&result.extensions, "complex/b.ts"));
    assert_eq!(result.extensions.iter().filter(|r| r.enabled).count(), 2);
}

#[test]
fn skips_subdirectories_without_index_ts_or_manifest() {
    let fx = Fixture::new();
    let pkg_dir = join(&fx.root, &["no-entry-pkg"]);
    write(
        &join(&pkg_dir, &["extensions", "broken", "helper.ts"]),
        "export const x = 1;",
    );
    write(
        &join(&pkg_dir, &["extensions", "broken", "another.ts"]),
        "export const y = 2;",
    );
    write(
        &join(&pkg_dir, &["extensions", "valid.ts"]),
        "export default function(api) {}",
    );
    let result = fx
        .resolver()
        .resolve_extension_sources(&[pkg_dir], false, false);
    assert!(ends_enabled(&result.extensions, "valid.ts"));
    assert_eq!(result.extensions.iter().filter(|r| r.enabled).count(), 1);
}
