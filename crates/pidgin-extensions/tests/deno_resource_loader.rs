//! Acceptance tests for the real [`RealExtensionLoader`] wired into pi's
//! `DefaultResourceLoader.reload()`.
//!
//! These are the five cases that need the REAL extension runtime — they write
//! real `.ts` extension modules to disk and assert on what those modules
//! register when executed on the embedded `deno_core` plane:
//!
//!   1. `load_symlinked_user_and_project_extensions_once` — a shared extension
//!      symlinked into both roots collapses to one load.
//!   2. `load_user_extensions_before_trust_and_reuse_after_trust_resolves` — the
//!      trust two-pass loads the user extension pre-trust and reuses it after.
//!   3. `keep_both_extensions_loaded_when_command_names_collide` — command-name
//!      collisions are NOT conflict errors.
//!   4. `detect_tool_conflicts_between_extensions` — duplicate tool names across
//!      extensions ARE conflict errors.
//!   5. `prefer_explicit_cli_extensions_over_discovered_on_conflict` — CLI
//!      extensions win precedence over discovered ones.
//!
//! # Why they live here (and not in pidgin-coding)
//!
//! These MOVED out of `crates/pidgin-coding/tests/resource_loader_orchestrator.rs`
//! (where they were `#[ignore]`'d behind `StubExtensionLoader`). The real
//! `impl ExtensionLoader` lives in pidgin-extensions — which depends on
//! pidgin-coding and needs V8 — so an pidgin-coding test cannot reference it
//! (dependency cycle) and cannot build it in-sandbox (the V8 blob 403s). They
//! run only under `--features deno` in the dedicated `deno runtime (V8)` CI job.
//! The interface-level `ExtensionLoader` coverage against `StubExtensionLoader`
//! (e.g. `custom_extension_loader_is_held`) stays in pidgin-coding's default
//! V8-free suite.
//!
//! The whole file is gated on the `deno` feature — without it, it is empty.
#![cfg(feature = "deno")]

use pidgin_coding::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions,
};
use pidgin_extensions::RealExtensionLoader;

mod common;
use common::{canonical, join, mkdir, symlink_dir, write};

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

/// Base options with the REAL extension loader injected via the
/// `DefaultResourceLoaderOptions.extension_loader` seam.
fn base_opts(e: &Env) -> DefaultResourceLoaderOptions {
    DefaultResourceLoaderOptions {
        cwd: e.cwd.clone(),
        agent_dir: e.agent.clone(),
        home_dir: Some(e.home.clone()),
        extension_loader: Some(Box::new(RealExtensionLoader::spawn())),
        ..Default::default()
    }
}

#[test]
fn load_symlinked_user_and_project_extensions_once() {
    let e = env();
    let shared = join(&e.root, &["shared-extensions"]);
    write(
        &join(&shared, &["shared.ts"]),
        "export default function(pi) { pi.registerCommand(\"shared\", { description: \"shared\", handler: async () => {} }); }",
    );
    mkdir(&join(&e.cwd, &[".pi"]));
    symlink_dir(&shared, &join(&e.agent, &["extensions"]));
    symlink_dir(&shared, &join(&e.cwd, &[".pi", "extensions"]));

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let result = loader.get_extensions();
    assert_eq!(result.extensions.len(), 1);
    assert!(result.errors.is_empty());
    // mergePaths processes project before user, so the project alias survives.
    assert_eq!(
        result.extensions[0].path,
        join(&e.cwd, &[".pi", "extensions", "shared.ts"])
    );
}

#[test]
fn load_user_extensions_before_trust_and_reuse_after_trust_resolves() {
    let e = env();
    let user_ext = join(&e.agent, &["extensions", "user.ts"]);
    let project_ext = join(&e.cwd, &[".pi", "extensions", "project.ts"]);
    write(
        &user_ext,
        "export default function(pi) { pi.on(\"project_trust\", () => ({ trusted: \"yes\" })); pi.registerCommand(\"user-trust\", { description: \"user\", handler: async () => {} }); }",
    );
    write(
        &project_ext,
        "export default function(pi) { pi.registerCommand(\"project-trusted\", { description: \"project\", handler: async () => {} }); }",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    let user_ext_cb = user_ext.clone();
    loader.reload(ReloadOptions {
        resolve_project_trust: Some(Box::new(move |pre| {
            // Pre-trust pass loads ONLY the user extension.
            let paths: Vec<&str> = pre.extensions.iter().map(|x| x.path.as_str()).collect();
            assert_eq!(paths, vec![user_ext_cb.as_str()]);
            true
        })),
    });

    let paths: Vec<String> = loader
        .get_extensions()
        .extensions
        .iter()
        .map(|x| x.path.clone())
        .collect();
    assert_eq!(paths, vec![project_ext, user_ext]);
    // The `user.ts` module executes exactly once: its pre-trust `Extension` is
    // kept in `preloaded` (keyed by resolved_path) and reused post-trust, so it
    // never re-enters `remaining_paths`.
}

#[test]
fn keep_both_extensions_loaded_when_command_names_collide() {
    let e = env();
    write(
        &join(&e.cwd, &[".pi", "extensions", "project.ts"]),
        "export default function(pi) { pi.registerCommand(\"deploy\", { description: \"project deploy\", handler: async () => {} }); pi.registerCommand(\"project-only\", { description: \"project only\", handler: async () => {} }); }",
    );
    write(
        &join(&e.agent, &["extensions", "user.ts"]),
        "export default function(pi) { pi.registerCommand(\"deploy\", { description: \"user deploy\", handler: async () => {} }); pi.registerCommand(\"user-only\", { description: \"user only\", handler: async () => {} }); }",
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let result = loader.get_extensions();
    // Both extensions stay loaded; command collisions are NOT conflict errors
    // (the runner renames them `:1`/`:2`).
    assert_eq!(result.extensions.len(), 2);
    assert!(!result
        .errors
        .iter()
        .any(|x| x.error.contains("/deploy") && x.error.contains("conflicts")));
}

#[test]
fn detect_tool_conflicts_between_extensions() {
    let e = env();
    // pi's fixture imports typebox for `Type.Object({})`; the current runtime has
    // no module loader for bare specifiers (a deferred, off-critical-path item),
    // so we register the tool with a plain-object parameters schema instead — the
    // tool NAME is all the conflict pass reads, so the assertion is unchanged.
    let tool_ext = |desc: &str| {
        format!("export default function(pi) {{ pi.registerTool({{ name: \"duplicate-tool\", description: \"{desc}\", parameters: {{}}, execute: async () => ({{ result: \"x\" }}) }}); }}")
    };
    write(
        &join(&e.agent, &["extensions", "ext1", "index.ts"]),
        &tool_ext("First"),
    );
    write(
        &join(&e.agent, &["extensions", "ext2", "index.ts"]),
        &tool_ext("Second"),
    );

    let mut loader = DefaultResourceLoader::new(base_opts(&e));
    loader.reload(ReloadOptions::default());

    let errors = &loader.get_extensions().errors;
    assert!(errors
        .iter()
        .any(|x| x.error.contains("duplicate-tool") && x.error.contains("conflicts")));
}

#[test]
fn prefer_explicit_cli_extensions_over_discovered_on_conflict() {
    let e = env();
    let explicit = join(&e.root, &["explicit-extension.ts"]);
    // Plain-object parameters schema instead of typebox (see the note in
    // `detect_tool_conflicts_between_extensions`); the assertion reads only the
    // extension path precedence, so this is faithful.
    let ext_src = |tool: &str, cmd: &str| {
        format!("export default function(pi) {{ pi.registerTool({{ name: \"duplicate-tool\", description: \"{tool}\", parameters: {{}}, execute: async () => ({{ result: \"x\" }}) }}); pi.registerCommand(\"deploy\", {{ description: \"{cmd}\", handler: async () => {{}} }}); }}")
    };
    write(
        &join(&e.agent, &["extensions", "global.ts"]),
        &ext_src("global tool", "global command"),
    );
    write(&explicit, &ext_src("explicit tool", "explicit command"));

    let mut options = base_opts(&e);
    options.additional_extension_paths = vec![explicit.clone()];
    let mut loader = DefaultResourceLoader::new(options);
    loader.reload(ReloadOptions::default());

    // CLI extensions are merged before discovered ones, so the explicit one wins.
    assert_eq!(
        loader
            .get_extensions()
            .extensions
            .first()
            .map(|x| x.path.clone()),
        Some(explicit)
    );
}
