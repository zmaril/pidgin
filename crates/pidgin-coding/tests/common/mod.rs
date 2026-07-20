//! Shared fixtures and assertion helpers for the package-manager resolve /
//! discovery integration tests. Extracted so the (large) ported test suites can
//! split across files without duplicating setup.
//!
//! The generic path/write/symlink helpers (`canonical`, `join`, `write`,
//! `mkdir`, `symlink_dir`) live in the `pidgin-testkit` dev crate and are
//! re-exported below, so they are shared with `pidgin-extensions` rather than
//! copied across the crate boundary. The crate-specific fixtures and assertion
//! helpers below stay local because they reference `pidgin-coding`'s own
//! package-manager types.

// Each integration-test binary that includes this module uses a different
// subset of these helpers, so per-binary `dead_code` is expected and allowed.
#![allow(dead_code)]

use std::fs;

use pidgin_coding::core::package_manager::{
    PackageResolver, ResolveSettings, ResolvedResource, ScopeResources,
};

pub use pidgin_testkit::{canonical, join, mkdir, symlink_dir, write};

/// A temp-dir fixture: a project root, an agent dir, and a dedicated empty home.
pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub root: String,
    pub agent_dir: String,
    pub home: String,
}

impl Fixture {
    pub fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = canonical(tmp.path().to_string_lossy().as_ref());
        let agent_dir = join(&root, &["agent"]);
        fs::create_dir_all(&agent_dir).unwrap();
        // A dedicated empty home dir so `~/.agents` discovery stays empty unless
        // a test explicitly targets it (pi relies on the real HOME being outside
        // the temp tree; we make that explicit instead of mutating the env).
        let home = join(&root, &["home"]);
        fs::create_dir_all(&home).unwrap();
        Self {
            _tmp: tmp,
            root,
            agent_dir,
            home,
        }
    }

    pub fn resolver(&self) -> PackageResolver {
        PackageResolver::new(&self.root, &self.agent_dir).with_home_dir(&self.home)
    }

    pub fn resolver_at(&self, cwd: &str, agent_dir: &str) -> PackageResolver {
        PackageResolver::new(cwd, agent_dir).with_home_dir(&self.home)
    }

    /// A resolver whose home is the temp root (for the `~/.agents` cases that
    /// pi drives by setting `process.env.HOME = tempDir`).
    pub fn resolver_home_root(&self, cwd: &str, agent_dir: &str) -> PackageResolver {
        PackageResolver::new(cwd, agent_dir).with_home_dir(&self.root)
    }
}

pub fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: {name} desc\n---\nContent")
}

pub fn trusted(global: ScopeResources, project: ScopeResources) -> ResolveSettings {
    ResolveSettings {
        global,
        project,
        project_trusted: true,
    }
}

pub fn empty() -> ScopeResources {
    ScopeResources::default()
}

/// Build an object-form `PackageSource` with one resource-type pattern array set.
pub fn pkg_filter(source: &str, key: &str, patterns: &[&str]) -> serde_json::Value {
    let pats: Vec<&str> = patterns.to_vec();
    let mut obj = serde_json::json!({
        "source": source, "extensions": [], "skills": [], "prompts": [], "themes": []
    });
    obj[key] = serde_json::json!(pats);
    obj
}

pub fn norm(p: &str) -> String {
    p.replace('\\', "/")
}

pub fn some_enabled(list: &[ResolvedResource], path: &str) -> bool {
    list.iter().any(|r| r.path == path && r.enabled)
}

pub fn some_disabled(list: &[ResolvedResource], path: &str) -> bool {
    list.iter().any(|r| r.path == path && !r.enabled)
}

pub fn ends_enabled(list: &[ResolvedResource], suffix: &str) -> bool {
    list.iter()
        .any(|r| norm(&r.path).ends_with(&norm(suffix)) && r.enabled)
}

pub fn ends_disabled(list: &[ResolvedResource], suffix: &str) -> bool {
    list.iter()
        .any(|r| norm(&r.path).ends_with(&norm(suffix)) && !r.enabled)
}

pub fn incl_enabled(list: &[ResolvedResource], needle: &str) -> bool {
    list.iter()
        .any(|r| norm(&r.path).contains(&norm(needle)) && r.enabled)
}

pub fn incl_disabled(list: &[ResolvedResource], needle: &str) -> bool {
    list.iter()
        .any(|r| norm(&r.path).contains(&norm(needle)) && !r.enabled)
}

pub fn ends_any(list: &[ResolvedResource], suffix: &str) -> bool {
    list.iter().any(|r| norm(&r.path).ends_with(&norm(suffix)))
}
