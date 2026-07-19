//! Shared fixtures and assertion helpers for the package-manager resolve /
//! discovery integration tests. Extracted so the (large) ported test suites can
//! split across files without duplicating setup.

// straitjacket-allow-file:duplication -- the path/write/symlink helpers here are
// faithfully mirrored by `crates/atilla-extensions/tests/common/mod.rs` (a
// crate-boundary test-helper file can't be shared across crates); the small
// overlap is intentional mirror duplication, not an accident to hoist away.

// Each integration-test binary that includes this module uses a different
// subset of these helpers, so per-binary `dead_code` is expected and allowed.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use atilla_coding::core::package_manager::{
    PackageResolver, ResolveSettings, ResolvedResource, ScopeResources,
};

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

pub fn canonical(path: &str) -> String {
    fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

pub fn join(base: &str, parts: &[&str]) -> String {
    let mut p = std::path::PathBuf::from(base);
    for part in parts {
        p.push(part);
    }
    p.to_string_lossy().into_owned()
}

pub fn write(path: &str, content: &str) {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub fn mkdir(path: &str) {
    fs::create_dir_all(path).unwrap();
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

#[cfg(unix)]
pub fn symlink_dir(src: &str, dst: &str) {
    std::os::unix::fs::symlink(src, dst).unwrap();
}

#[cfg(not(unix))]
pub fn symlink_dir(src: &str, dst: &str) {
    let _ = (src, dst);
    panic!("symlink tests require unix");
}
