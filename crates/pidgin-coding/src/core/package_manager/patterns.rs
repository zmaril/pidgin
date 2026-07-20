//! Pattern classification and glob filtering for resolved resources.
//!
//! Ports pi's pure pattern helpers from `core/package-manager.ts`: `isPattern`,
//! `isOverridePattern`, `hasGlobPattern`, `splitPatterns`, `matchesAnyPattern`,
//! `normalizeExactPattern`, `matchesAnyExactPattern`, `getOverridePatterns`,
//! `isEnabledByOverrides`, `applyPatterns`, and `applyAutoloadDisabledPatterns`,
//! plus a filesystem `globSync` used to expand manifest glob entries.
//!
//! pi matches with `minimatch`; here `globset` with `literal_separator(true)`
//! reproduces the same `*`-does-not-cross-`/` / `**`-crosses-`/` semantics the
//! resolver relies on (the same configuration the `find` / `grep` tools use).

use globset::GlobBuilder;
use std::collections::BTreeSet;
use std::path::Path;

/// Convert an OS path to POSIX (`/`) separators. Port of pi's `toPosixPath`.
pub fn to_posix_path(path: &str) -> String {
    path.replace(std::path::MAIN_SEPARATOR, "/")
}

/// The final path segment. Mirrors Node's `basename`.
pub fn basename(path: &str) -> String {
    let posix = to_posix_path(path);
    let trimmed = posix.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// The parent directory. Mirrors Node's `dirname` for POSIX paths.
pub fn dirname(path: &str) -> String {
    let posix = to_posix_path(path);
    let trimmed = posix.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// POSIX path of `path` relative to `base`. Falls back to the POSIX form of
/// `path` when it does not sit under `base` (the resolver only relativizes
/// paths that do).
pub fn relative(base: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(Path::new(base)) {
        Ok(rel) => to_posix_path(&rel.to_string_lossy()),
        Err(_) => to_posix_path(path),
    }
}

/// Whether an entry is a pattern (override prefix or glob char). Port of
/// pi's `isPattern`.
pub fn is_pattern(s: &str) -> bool {
    s.starts_with('!')
        || s.starts_with('+')
        || s.starts_with('-')
        || s.contains('*')
        || s.contains('?')
}

/// Whether an entry is an override pattern (`!` / `+` / `-`). Port of
/// pi's `isOverridePattern`.
pub fn is_override_pattern(s: &str) -> bool {
    s.starts_with('!') || s.starts_with('+') || s.starts_with('-')
}

/// Whether an entry contains a glob wildcard. Port of pi's `hasGlobPattern`.
pub fn has_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

/// Split entries into plain paths and patterns. Port of pi's `splitPatterns`.
pub fn split_patterns(entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut plain = Vec::new();
    let mut patterns = Vec::new();
    for entry in entries {
        if is_pattern(entry) {
            patterns.push(entry.clone());
        } else {
            plain.push(entry.clone());
        }
    }
    (plain, patterns)
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    match GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(glob) => glob.compile_matcher().is_match(candidate),
        Err(_) => false,
    }
}

/// Candidate strings a file path is matched against (rel/name/posix, plus the
/// skill-parent variants when the file is a `SKILL.md`).
fn match_candidates(file_path: &str, base_dir: &str) -> Vec<String> {
    let rel = relative(base_dir, file_path);
    let name = basename(file_path);
    let file_path_posix = to_posix_path(file_path);
    let mut candidates = vec![rel, name.clone(), file_path_posix];
    if name == "SKILL.md" {
        let parent_dir = dirname(file_path);
        candidates.push(relative(base_dir, &parent_dir));
        candidates.push(basename(&parent_dir));
        candidates.push(to_posix_path(&parent_dir));
    }
    candidates
}

/// Whether `file_path` matches any of `patterns` (glob semantics). Port of
/// pi's `matchesAnyPattern`, including the SKILL.md parent-directory matches.
pub fn matches_any_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let name = basename(file_path);
    let is_skill_file = name == "SKILL.md";
    let rel = relative(base_dir, file_path);
    let file_path_posix = to_posix_path(file_path);

    let (parent_rel, parent_name, parent_dir_posix) = if is_skill_file {
        let parent_dir = dirname(file_path);
        (
            Some(relative(base_dir, &parent_dir)),
            Some(basename(&parent_dir)),
            Some(to_posix_path(&parent_dir)),
        )
    } else {
        (None, None, None)
    };

    patterns.iter().any(|pattern| {
        let normalized = to_posix_path(pattern);
        if glob_matches(&normalized, &rel)
            || glob_matches(&normalized, &name)
            || glob_matches(&normalized, &file_path_posix)
        {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        glob_matches(&normalized, parent_rel.as_deref().unwrap_or(""))
            || glob_matches(&normalized, parent_name.as_deref().unwrap_or(""))
            || glob_matches(&normalized, parent_dir_posix.as_deref().unwrap_or(""))
    })
}

/// Normalize an exact (non-glob) pattern: strip a leading `./` and posix-ify.
/// Port of pi's `normalizeExactPattern`.
pub fn normalize_exact_pattern(pattern: &str) -> String {
    let stripped = pattern
        .strip_prefix("./")
        .or_else(|| pattern.strip_prefix(".\\"))
        .unwrap_or(pattern);
    to_posix_path(stripped)
}

/// Whether `file_path` exactly matches any of `patterns`. Port of pi's
/// `matchesAnyExactPattern` (with SKILL.md parent handling).
pub fn matches_any_exact_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let candidates = match_candidates(file_path, base_dir);
    let rel = &candidates[0];
    let file_path_posix = &candidates[2];
    let name = basename(file_path);
    let is_skill_file = name == "SKILL.md";
    let (parent_rel, parent_dir_posix) = if is_skill_file {
        (Some(&candidates[3]), Some(&candidates[5]))
    } else {
        (None, None)
    };

    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_pattern(pattern);
        if &normalized == rel || &normalized == file_path_posix {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        Some(&normalized) == parent_rel || Some(&normalized) == parent_dir_posix
    })
}

/// The override patterns (`!` / `+` / `-`) from `entries`. Port of pi's
/// `getOverridePatterns`.
pub fn get_override_patterns(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|p| is_override_pattern(p))
        .cloned()
        .collect()
}

/// Whether a path is enabled after applying only override patterns. Port of
/// pi's `isEnabledByOverrides`.
pub fn is_enabled_by_overrides(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let overrides = get_override_patterns(patterns);
    let excludes: Vec<String> = overrides
        .iter()
        .filter(|p| p.starts_with('!'))
        .map(|p| p[1..].to_string())
        .collect();
    let force_includes: Vec<String> = overrides
        .iter()
        .filter(|p| p.starts_with('+'))
        .map(|p| p[1..].to_string())
        .collect();
    let force_excludes: Vec<String> = overrides
        .iter()
        .filter(|p| p.starts_with('-'))
        .map(|p| p[1..].to_string())
        .collect();

    let mut enabled = true;
    if !excludes.is_empty() && matches_any_pattern(file_path, &excludes, base_dir) {
        enabled = false;
    }
    if !force_includes.is_empty() && matches_any_exact_pattern(file_path, &force_includes, base_dir)
    {
        enabled = true;
    }
    if !force_excludes.is_empty() && matches_any_exact_pattern(file_path, &force_excludes, base_dir)
    {
        enabled = false;
    }
    enabled
}

/// Apply include/exclude/force patterns to paths, returning the enabled set.
/// Port of pi's `applyPatterns`.
pub fn apply_patterns(
    all_paths: &[String],
    patterns: &[String],
    base_dir: &str,
) -> BTreeSet<String> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();

    for p in patterns {
        if let Some(rest) = p.strip_prefix('+') {
            force_includes.push(rest.to_string());
        } else if let Some(rest) = p.strip_prefix('-') {
            force_excludes.push(rest.to_string());
        } else if let Some(rest) = p.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(p.clone());
        }
    }

    // Step 1: includes (or all).
    let mut result: Vec<String> = if includes.is_empty() {
        all_paths.to_vec()
    } else {
        all_paths
            .iter()
            .filter(|f| matches_any_pattern(f, &includes, base_dir))
            .cloned()
            .collect()
    };

    // Step 2: excludes.
    if !excludes.is_empty() {
        result.retain(|f| !matches_any_pattern(f, &excludes, base_dir));
    }

    // Step 3: force-include (add back, overriding exclusions).
    if !force_includes.is_empty() {
        for file_path in all_paths {
            if !result.contains(file_path)
                && matches_any_exact_pattern(file_path, &force_includes, base_dir)
            {
                result.push(file_path.clone());
            }
        }
    }

    // Step 4: force-exclude (remove even if included).
    if !force_excludes.is_empty() {
        result.retain(|f| !matches_any_exact_pattern(f, &force_excludes, base_dir));
    }

    result.into_iter().collect()
}

/// Map paths to enabled/disabled deltas for autoload-disabled package filters.
/// Port of pi's `applyAutoloadDisabledPatterns` (insertion order preserved via
/// a returned vec of `(path, enabled)` pairs, last write winning per path).
pub fn apply_autoload_disabled_patterns(
    all_paths: &[String],
    patterns: &[String],
    base_dir: &str,
) -> Vec<(String, bool)> {
    let mut order: Vec<String> = Vec::new();
    let mut states: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    for pattern in patterns {
        let has_prefix =
            pattern.starts_with('+') || pattern.starts_with('-') || pattern.starts_with('!');
        let target = if has_prefix {
            pattern[1..].to_string()
        } else {
            pattern.clone()
        };
        let enabled = !pattern.starts_with('-') && !pattern.starts_with('!');
        let exact = pattern.starts_with('+') || pattern.starts_with('-');
        let targets = [target];
        for file_path in all_paths {
            let matched = if exact {
                matches_any_exact_pattern(file_path, &targets, base_dir)
            } else {
                matches_any_pattern(file_path, &targets, base_dir)
            };
            if matched {
                if !states.contains_key(file_path) {
                    order.push(file_path.clone());
                }
                states.insert(file_path.clone(), enabled);
            }
        }
    }

    order
        .into_iter()
        .map(|path| {
            let enabled = states[&path];
            (path, enabled)
        })
        .collect()
}

/// Expand a glob `pattern` against `root` on disk, returning absolute matches
/// (files and directories). Mirrors pi's `globSync(pattern, { cwd: root,
/// absolute: true, dot: false, nodir: false })`.
pub fn glob_sync(root: &str, pattern: &str) -> Vec<String> {
    let normalized = pattern.strip_prefix("./").unwrap_or(pattern);
    let matcher = match GlobBuilder::new(&to_posix_path(normalized))
        .literal_separator(true)
        .build()
    {
        Ok(glob) => glob.compile_matcher(),
        Err(_) => return Vec::new(),
    };

    let root_path = Path::new(root);
    let mut matches: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let full = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_dir = if file_type.is_symlink() {
                std::fs::metadata(&full)
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            let rel = relative(root, &full.to_string_lossy());
            if matcher.is_match(&rel) {
                matches.insert(full.to_string_lossy().into_owned());
            }
            if is_dir {
                stack.push(full);
            }
        }
    }
    matches.into_iter().collect()
}
