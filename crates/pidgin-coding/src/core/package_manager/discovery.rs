// straitjacket-allow-file:duplication — the ignore-stack / entry-classification
// helpers here faithfully mirror pi's `package-manager.ts` traversal, which runs
// the same shape as the (independently ported) `skills.rs` discovery walk.

//! Filesystem discovery for package resources.
//!
//! Ports pi's pure-FS collectors from `core/package-manager.ts`: `collectFiles`,
//! `collectSkillEntries` / `collectAutoSkillEntries`, `collectAutoPromptEntries`,
//! `collectAutoThemeEntries`, `resolveExtensionEntries`,
//! `collectAutoExtensionEntries`, `collectResourceFiles`, `readPiManifestFile`,
//! `findGitRepoRoot`, and `collectAncestorAgentsSkillDirs`, plus the shared
//! ignore-matcher accumulation (`addIgnoreRules` / `prefixIgnorePattern`).

use super::patterns::to_posix_path;
use super::resource::{PiManifest, ResourceType};
use ignore::gitignore::GitignoreBuilder;
use std::fs;
use std::path::{Path, PathBuf};

const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Skill-directory discovery mode, mirroring pi's `SkillDiscoveryMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiscoveryMode {
    /// `.pi` layout: root `.md` files are skills.
    Pi,
    /// `.agents` layout: only nested `SKILL.md` files are skills.
    Agents,
}

/// Accumulates prefixed ignore rules while a tree is walked, matching paths
/// relative to the scan root. Port of pi's shared `ignore()` matcher and
/// `addIgnoreRules`.
struct IgnoreStack {
    root: PathBuf,
    builder: GitignoreBuilder,
}

impl IgnoreStack {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            builder: GitignoreBuilder::new(root),
        }
    }

    fn dir_prefix(&self, dir: &Path) -> String {
        let rel = rel_posix(&self.root, dir);
        if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        }
    }

    fn add_rules_from_dir(&mut self, dir: &Path) {
        let prefix = self.dir_prefix(dir);
        for filename in IGNORE_FILE_NAMES {
            let Ok(content) = fs::read_to_string(dir.join(filename)) else {
                continue;
            };
            for raw_line in content.split('\n') {
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                    let _ = self.builder.add_line(None, &pattern);
                }
            }
        }
    }

    fn ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        match self.builder.build() {
            Ok(gitignore) => gitignore.matched(rel_path, is_dir).is_ignore(),
            Err(_) => false,
        }
    }
}

/// Rewrite one ignore-file line so it applies relative to the scan root.
/// Port of pi's `prefixIgnorePattern`.
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line.to_string();
    let mut negated = false;

    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest.to_string();
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest.to_string();
    }

    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest.to_string();
    }

    let prefixed = if prefix.is_empty() {
        pattern
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

fn rel_posix(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => to_posix_path(&rel.to_string_lossy()),
        Err(_) => to_posix_path(&path.to_string_lossy()),
    }
}

fn entry_kind(path: &Path, file_type: &fs::FileType) -> Option<(bool, bool)> {
    if file_type.is_symlink() {
        match fs::metadata(path) {
            Ok(meta) => Some((meta.is_file(), meta.is_dir())),
            Err(_) => None,
        }
    } else {
        Some((file_type.is_file(), file_type.is_dir()))
    }
}

fn read_entries(dir: &Path) -> Vec<fs::DirEntry> {
    match fs::read_dir(dir) {
        Ok(read_dir) => read_dir.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Recursively collect files matching `resource_type` under `dir`, honoring
/// ignore files and skipping hidden entries / `node_modules`. Port of pi's
/// `collectFiles`.
pub fn collect_files(dir: &str, resource_type: ResourceType) -> Vec<String> {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        return Vec::new();
    }
    let mut ig = IgnoreStack::new(&dir_path);
    let mut files = Vec::new();
    collect_files_inner(&dir_path, resource_type, &dir_path, &mut ig, &mut files);
    files
}

fn collect_files_inner(
    dir: &Path,
    resource_type: ResourceType,
    root: &Path,
    ig: &mut IgnoreStack,
    files: &mut Vec<String>,
) {
    if !dir.exists() {
        return;
    }
    ig.add_rules_from_dir(dir);

    for entry in read_entries(dir) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Some((full, is_file, is_dir)) = classify(&entry) else {
            continue;
        };
        let rel = rel_posix(root, &full);
        if ig.ignores(&rel, is_dir) {
            continue;
        }
        if is_dir {
            collect_files_inner(&full, resource_type, root, ig, files);
        } else if is_file && resource_type.matches_file(&name) {
            files.push(full.to_string_lossy().into_owned());
        }
    }
}

fn classify(entry: &fs::DirEntry) -> Option<(PathBuf, bool, bool)> {
    let full = entry.path();
    let file_type = entry.file_type().ok()?;
    let (is_file, is_dir) = entry_kind(&full, &file_type)?;
    Some((full, is_file, is_dir))
}

/// Collect skill entry paths under `dir` for the given mode. Port of pi's
/// `collectSkillEntries`: a directory holding `SKILL.md` yields just that file
/// and stops recursing; `pi` mode also treats root-level `.md` files as skills.
pub fn collect_skill_entries(dir: &str, mode: SkillDiscoveryMode) -> Vec<String> {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        return Vec::new();
    }
    let mut ig = IgnoreStack::new(&dir_path);
    let mut entries = Vec::new();
    collect_skill_entries_inner(&dir_path, mode, &dir_path, &mut ig, &mut entries);
    entries
}

fn collect_skill_entries_inner(
    dir: &Path,
    mode: SkillDiscoveryMode,
    root: &Path,
    ig: &mut IgnoreStack,
    out: &mut Vec<String>,
) {
    if !dir.exists() {
        return;
    }
    ig.add_rules_from_dir(dir);
    let entries = read_entries(dir);

    // First pass: a `SKILL.md` makes this a skill root; emit it and stop.
    for entry in &entries {
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let Some((full, is_file, _)) = classify(entry) else {
            continue;
        };
        let rel = rel_posix(root, &full);
        if is_file && !ig.ignores(&rel, false) {
            out.push(full.to_string_lossy().into_owned());
            return;
        }
    }

    // Second pass: recurse into subdirectories (and, in `pi` mode at the root,
    // pick up loose `.md` files).
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Some((full, is_file, is_dir)) = classify(entry) else {
            continue;
        };
        let rel = rel_posix(root, &full);
        if mode == SkillDiscoveryMode::Pi
            && dir == root
            && is_file
            && name.ends_with(".md")
            && !ig.ignores(&rel, false)
        {
            out.push(full.to_string_lossy().into_owned());
            continue;
        }
        if !is_dir {
            continue;
        }
        if ig.ignores(&rel, true) {
            continue;
        }
        collect_skill_entries_inner(&full, mode, root, ig, out);
    }
}

/// Non-recursive helper for `collectAutoPromptEntries` / `collectAutoThemeEntries`:
/// collect top-level files with `suffix`, honoring the directory's ignore files.
fn collect_top_level_files(dir: &str, suffix: &str) -> Vec<String> {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        return Vec::new();
    }
    let mut ig = IgnoreStack::new(&dir_path);
    ig.add_rules_from_dir(&dir_path);
    let mut out = Vec::new();
    for entry in read_entries(&dir_path) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Some((full, is_file, _)) = classify(&entry) else {
            continue;
        };
        let rel = rel_posix(&dir_path, &full);
        if ig.ignores(&rel, false) {
            continue;
        }
        if is_file && name.ends_with(suffix) {
            out.push(full.to_string_lossy().into_owned());
        }
    }
    out
}

/// Collect top-level `.md` prompt files. Port of pi's `collectAutoPromptEntries`.
pub fn collect_auto_prompt_entries(dir: &str) -> Vec<String> {
    collect_top_level_files(dir, ".md")
}

/// Collect top-level `.json` theme files. Port of pi's `collectAutoThemeEntries`.
pub fn collect_auto_theme_entries(dir: &str) -> Vec<String> {
    collect_top_level_files(dir, ".json")
}

/// Read a `package.json`'s `pi` field. Port of pi's `readPiManifestFile`.
pub fn read_pi_manifest_file(package_json_path: &str) -> Option<PiManifest> {
    let content = fs::read_to_string(package_json_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let pi = value.get("pi")?;
    serde_json::from_value(pi.clone()).ok()
}

/// Resolve the extension entry points for a directory: explicit
/// `pi.extensions` from `package.json`, else `index.ts` / `index.js`. Port of
/// pi's `resolveExtensionEntries`.
pub fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let dir_path = PathBuf::from(dir);
    let package_json = dir_path.join("package.json");
    if package_json.exists() {
        if let Some(manifest) = read_pi_manifest_file(&package_json.to_string_lossy()) {
            if let Some(exts) = manifest.extensions.as_ref() {
                if !exts.is_empty() {
                    let mut entries = Vec::new();
                    for ext_path in exts {
                        let resolved = super::path_resolve(dir, ext_path);
                        if Path::new(&resolved).exists() {
                            entries.push(resolved);
                        }
                    }
                    if !entries.is_empty() {
                        return Some(entries);
                    }
                }
            }
        }
    }

    let index_ts = dir_path.join("index.ts");
    let index_js = dir_path.join("index.js");
    if index_ts.exists() {
        return Some(vec![index_ts.to_string_lossy().into_owned()]);
    }
    if index_js.exists() {
        return Some(vec![index_js.to_string_lossy().into_owned()]);
    }
    None
}

/// Auto-discover extension entry points under `dir`. Port of pi's
/// `collectAutoExtensionEntries`.
pub fn collect_auto_extension_entries(dir: &str) -> Vec<String> {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        return Vec::new();
    }

    if let Some(root_entries) = resolve_extension_entries(dir) {
        return root_entries;
    }

    let mut ig = IgnoreStack::new(&dir_path);
    ig.add_rules_from_dir(&dir_path);
    let mut entries = Vec::new();
    for entry in read_entries(&dir_path) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Some((full, is_file, is_dir)) = classify(&entry) else {
            continue;
        };
        let rel = rel_posix(&dir_path, &full);
        if ig.ignores(&rel, is_dir) {
            continue;
        }
        if is_file && (name.ends_with(".ts") || name.ends_with(".js")) {
            entries.push(full.to_string_lossy().into_owned());
        } else if is_dir {
            if let Some(resolved) = resolve_extension_entries(&full.to_string_lossy()) {
                entries.extend(resolved);
            }
        }
    }
    entries
}

/// Collect resource files from a directory by type. Extensions use smart
/// discovery, skills use SKILL.md discovery, others recurse. Port of pi's
/// `collectResourceFiles`.
pub fn collect_resource_files(dir: &str, resource_type: ResourceType) -> Vec<String> {
    match resource_type {
        ResourceType::Skills => collect_skill_entries(dir, SkillDiscoveryMode::Pi),
        ResourceType::Extensions => collect_auto_extension_entries(dir),
        _ => collect_files(dir, resource_type),
    }
}

/// Walk up from `start_dir` to find the nearest ancestor containing `.git`.
/// Port of pi's `findGitRepoRoot`.
pub fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = PathBuf::from(start_dir);
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_string_lossy().into_owned());
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Collect `.agents/skills` directories from `start_dir` up to the git repo
/// root (or filesystem root). Port of pi's `collectAncestorAgentsSkillDirs`.
pub fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs = Vec::new();
    let git_root = find_git_repo_root(start_dir);

    let mut dir = PathBuf::from(start_dir);
    loop {
        skill_dirs.push(
            dir.join(".agents")
                .join("skills")
                .to_string_lossy()
                .into_owned(),
        );
        if let Some(root) = &git_root {
            if dir.to_string_lossy() == *root {
                break;
            }
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }

    skill_dirs
}
