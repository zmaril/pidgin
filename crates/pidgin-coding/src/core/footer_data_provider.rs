//! Footer data assembly and width/layout computation.
//!
//! Ported from two pi sources:
//! - `core/footer-data-provider.ts` — the [`FooterDataProvider`], which supplies
//!   git-branch and extension-status data that is not otherwise reachable from
//!   extensions.
//! - `modes/interactive/components/footer.ts` — [`render_footer`] and the
//!   [`format_tokens`] / [`format_cwd_for_footer`] helpers, which turn session
//!   state into the fixed-width footer lines the interactive TUI draws.
//!
//! # Seams
//!
//! The footer renderer reads from pi's live `AgentSession`, `model`, and
//! `settings` state. Rather than pulling in the unported session-manager and
//! model-runtime, this port takes a plain [`FooterInput`] value carrying exactly
//! the fields pi reads. The caller (a later interactive-mode port) is
//! responsible for populating it. Each such seam is marked `// NOTE:`.
//!
//! # Deferred: theme / ANSI styling
//!
//! NOTE: pi wraps footer fragments in `theme.fg(...)` / `theme.bold(...)` ANSI
//! sequences (dim body text, error/warning colouring past the 90% / 70% context
//! thresholds, the bold `xp` experimental marker). ANSI escapes have zero
//! display width, so they do not affect any layout or truncation math — the
//! width tests assert on `visible_width` and on `strip_ansi`-ed content. This
//! port therefore emits the un-styled text and leaves colourisation to the
//! interactive theme layer. The 90%/70% severity branch is a pure styling
//! choice and collapses to identical text here.
//!
//! # Deferred: filesystem watchers
//!
//! NOTE: pi's `FooterDataProvider` also runs `fs.watch`/`watchFile` debounced
//! watchers over `.git/HEAD` and the reftable directory, an async
//! branch-refresh pipeline, WSL-mount polling heuristics, and a watcher-retry
//! timer. That machinery is interactive/event-loop coupling, not footer data
//! assembly, so it is not ported here; the synchronous branch-resolution core
//! (`find_git_paths` + `resolve_git_branch_sync`) is. The five watcher/debounce/
//! retry tests in `footer-data-provider.test.ts` are consequently deferred; the
//! four synchronous branch-detection tests are ported below.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pidgin_tui::{truncate_to_width, visible_width};

// ---------------------------------------------------------------------------
// footer.ts: pure formatting helpers
// ---------------------------------------------------------------------------

/// `formatTokens` (footer.ts:23). Compact human-readable token count.
pub fn format_tokens(count: i64) -> String {
    let n = count as f64;
    if count < 1_000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", n / 1_000.0)
    } else if count < 1_000_000 {
        format!("{}k", (n / 1_000.0).round() as i64)
    } else if count < 10_000_000 {
        format!("{:.1}M", n / 1_000_000.0)
    } else {
        format!("{}M", (n / 1_000_000.0).round() as i64)
    }
}

/// `formatCwdForFooter` (footer.ts:31). Abbreviate `cwd` to `~`-relative form
/// when it lives inside `home`; otherwise return it unchanged.
///
/// NOTE: pi uses `node:path`, whose separator is platform-dependent. CI and the
/// ported tests are POSIX, so this uses `/` and POSIX-style lexical resolution.
pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return cwd.to_string();
    };

    let resolved_cwd = normalize_lexical(Path::new(cwd));
    let resolved_home = normalize_lexical(Path::new(home));
    let relative_to_home = path_relative(&resolved_home, &resolved_cwd);

    let is_inside_home = relative_to_home.is_empty()
        || (relative_to_home != ".."
            && !relative_to_home.starts_with("../")
            && !Path::new(&relative_to_home).is_absolute());

    if !is_inside_home {
        return cwd.to_string();
    }
    if relative_to_home.is_empty() {
        "~".to_string()
    } else {
        format!("~/{relative_to_home}")
    }
}

/// `sanitizeStatusText` (footer.ts:12). Flatten a status string to a single line.
fn sanitize_status_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        let ch = if matches!(ch, '\r' | '\n' | '\t') {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// footer.ts: render input seam
// ---------------------------------------------------------------------------

/// Resolved model fields the footer reads from `session.state.model`.
///
/// NOTE: seam for pi's `model` runtime object.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Model identifier shown on the right of the stats line.
    pub id: String,
    /// Provider name (e.g. `"anthropic"`, `"kimi-coding"`).
    pub provider: String,
    /// Model context window in tokens; the fallback when no live usage exists.
    pub context_window: i64,
    /// Whether the model supports a reasoning/thinking level indicator.
    pub reasoning: bool,
}

/// One assistant message's token usage, mirroring the fields the footer sums
/// out of `sessionManager.getEntries()`.
///
/// NOTE: seam for pi's session-manager message entries.
#[derive(Debug, Clone)]
pub struct AssistantUsage {
    /// Input (prompt) tokens.
    pub input: i64,
    /// Output (completion) tokens.
    pub output: i64,
    /// Cache-read tokens.
    pub cache_read: i64,
    /// Cache-write tokens.
    pub cache_write: i64,
    /// Total cost for this message, in dollars.
    pub cost_total: f64,
}

/// Live context-window usage, mirroring `session.getContextUsage()`.
///
/// NOTE: seam for pi's context-usage computation. `percent == None` models pi's
/// JavaScript `null` (unknown after compaction), which renders as `"?"`.
#[derive(Debug, Clone)]
pub struct ContextUsage {
    /// Effective context window in tokens.
    pub context_window: i64,
    /// Percent of the context window in use, or `None` when unknown.
    pub percent: Option<f64>,
}

/// Everything [`render_footer`] needs, decoupled from pi's session internals.
#[derive(Debug, Clone)]
pub struct FooterInput {
    /// Current model, or `None` (renders as `no-model`).
    pub model: Option<ModelInfo>,
    /// Thinking level; empty string is treated as `"off"`.
    pub thinking_level: String,
    /// Assistant usages, oldest first. Totals are summed; the cache-hit rate is
    /// taken from the last entry.
    pub usages: Vec<AssistantUsage>,
    /// Live context usage, if known.
    pub context_usage: Option<ContextUsage>,
    /// Working directory to display.
    pub cwd: String,
    /// Home directory used to abbreviate `cwd` to `~`. NOTE: pi reads
    /// `process.env.HOME || process.env.USERPROFILE`; supplied by the caller.
    pub home: Option<String>,
    /// Git branch, or `None` when not in a repository.
    pub git_branch: Option<String>,
    /// Session name; empty string omits the ` • name` suffix.
    pub session_name: String,
    /// Number of providers with available models (gates the provider prefix).
    pub available_provider_count: usize,
    /// Whether auto-compaction is on (renders the ` (auto)` indicator).
    pub auto_compact_enabled: bool,
    /// Whether the model is subscription-backed via OAuth. NOTE: seam for pi's
    /// `modelRuntime.isUsingOAuth(provider)`; `"kimi-coding"` also counts.
    pub is_using_oauth: bool,
    /// Whether experimental features are enabled (renders the `xp` marker).
    /// NOTE: seam for pi's `areExperimentalFeaturesEnabled()`.
    pub experimental_features_enabled: bool,
}

impl FooterInput {
    /// A minimal input: no model, no usage, no branch, everything else default.
    pub fn new(cwd: impl Into<String>) -> Self {
        FooterInput {
            model: None,
            thinking_level: String::new(),
            usages: Vec::new(),
            context_usage: None,
            cwd: cwd.into(),
            home: None,
            git_branch: None,
            session_name: String::new(),
            available_provider_count: 1,
            auto_compact_enabled: true,
            is_using_oauth: false,
            experimental_features_enabled: false,
        }
    }
}

// ---------------------------------------------------------------------------
// footer.ts: render
// ---------------------------------------------------------------------------

const MIN_PADDING: i64 = 2;

/// `FooterComponent.render` (footer.ts:83). Produce the footer lines, each with
/// a display width no greater than `width`.
///
/// Line 0 is the pwd/branch/session line, line 1 is the stats line (usage,
/// cost, context, model), and an optional line 2 holds extension statuses.
pub fn render_footer(input: &FooterInput, width: i64) -> Vec<String> {
    // Cumulative usage across all session entries.
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    let mut total_cache_read = 0i64;
    let mut total_cache_write = 0i64;
    let mut total_cost = 0.0f64;
    let mut latest_cache_hit_rate: Option<f64> = None;

    for usage in &input.usages {
        total_input += usage.input;
        total_output += usage.output;
        total_cache_read += usage.cache_read;
        total_cache_write += usage.cache_write;
        total_cost += usage.cost_total;

        let latest_prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
        latest_cache_hit_rate = if latest_prompt_tokens > 0 {
            Some((usage.cache_read as f64 / latest_prompt_tokens as f64) * 100.0)
        } else {
            None
        };
    }

    // Context usage (handles the post-compaction unknown case).
    let context_window = input
        .context_usage
        .as_ref()
        .map(|c| c.context_window)
        .or_else(|| input.model.as_ref().map(|m| m.context_window))
        .unwrap_or(0);
    // pi: `contextUsage?.percent ?? 0`; a present-but-null percent stays 0.
    let context_percent_value = input
        .context_usage
        .as_ref()
        .and_then(|c| c.percent)
        .unwrap_or(0.0);
    // pi: `contextUsage?.percent !== null ? value.toFixed(1) : "?"`. Only a
    // present usage whose percent is explicitly null renders as "?".
    let context_percent_is_unknown = matches!(
        input.context_usage.as_ref(),
        Some(ContextUsage { percent: None, .. })
    );

    // pwd + branch + session name.
    let mut pwd = format_cwd_for_footer(&input.cwd, input.home.as_deref());
    if let Some(branch) = input.git_branch.as_ref().filter(|b| !b.is_empty()) {
        pwd = format!("{pwd} ({branch})");
    }
    if !input.session_name.is_empty() {
        pwd = format!("{pwd} \u{2022} {}", input.session_name);
    }

    // Stats line, left side.
    let mut stats_parts: Vec<String> = Vec::new();
    if total_input != 0 {
        stats_parts.push(format!("\u{2191}{}", format_tokens(total_input)));
    }
    if total_output != 0 {
        stats_parts.push(format!("\u{2193}{}", format_tokens(total_output)));
    }
    if total_cache_read != 0 {
        stats_parts.push(format!("R{}", format_tokens(total_cache_read)));
    }
    if total_cache_write != 0 {
        stats_parts.push(format!("W{}", format_tokens(total_cache_write)));
    }
    if let Some(rate) = latest_cache_hit_rate {
        if total_cache_read > 0 || total_cache_write > 0 {
            stats_parts.push(format!("CH{rate:.1}%"));
        }
    }
    // Kimi Coding is subscription-backed despite API-key auth.
    let using_subscription = match input.model.as_ref() {
        Some(model) => model.provider == "kimi-coding" || input.is_using_oauth,
        None => false,
    };
    if total_cost != 0.0 || using_subscription {
        let sub = if using_subscription { " (sub)" } else { "" };
        stats_parts.push(format!("${:.3}{sub}", total_cost));
    }

    // Context percentage display.
    let auto_indicator = if input.auto_compact_enabled {
        " (auto)"
    } else {
        ""
    };
    let context_percent_display = if context_percent_is_unknown {
        format!("?/{}{auto_indicator}", format_tokens(context_window))
    } else {
        format!(
            "{:.1}%/{}{auto_indicator}",
            context_percent_value,
            format_tokens(context_window)
        )
    };
    // NOTE: pi colourises this past 90% / 70%; styling is deferred (see module
    // docs), so the text is identical across severities.
    stats_parts.push(context_percent_display);
    if input.experimental_features_enabled {
        stats_parts.push("\u{2022} xp".to_string());
    }

    let mut stats_left = stats_parts.join(" ");
    let mut stats_left_width = visible_width(&stats_left) as i64;
    if stats_left_width > width {
        stats_left = truncate_to_width(&stats_left, width, "...", false);
        stats_left_width = visible_width(&stats_left) as i64;
    }

    // Right side: model name (+ thinking level, + provider prefix).
    let model_name = input
        .model
        .as_ref()
        .map(|m| m.id.clone())
        .unwrap_or_else(|| "no-model".to_string());

    let mut right_side_without_provider = model_name.clone();
    if input.model.as_ref().is_some_and(|m| m.reasoning) {
        let thinking_level = if input.thinking_level.is_empty() {
            "off"
        } else {
            &input.thinking_level
        };
        right_side_without_provider = if thinking_level == "off" {
            format!("{model_name} \u{2022} thinking off")
        } else {
            format!("{model_name} \u{2022} {thinking_level}")
        };
    }

    let mut right_side = right_side_without_provider.clone();
    if input.available_provider_count > 1 {
        if let Some(model) = input.model.as_ref() {
            right_side = format!("({}) {right_side_without_provider}", model.provider);
            if stats_left_width + MIN_PADDING + visible_width(&right_side) as i64 > width {
                right_side = right_side_without_provider.clone();
            }
        }
    }

    let right_side_width = visible_width(&right_side) as i64;
    let total_needed = stats_left_width + MIN_PADDING + right_side_width;

    let stats_line = if total_needed <= width {
        let padding = " ".repeat((width - stats_left_width - right_side_width) as usize);
        format!("{stats_left}{padding}{right_side}")
    } else {
        let available_for_right = width - stats_left_width - MIN_PADDING;
        if available_for_right > 0 {
            let truncated_right = truncate_to_width(&right_side, available_for_right, "", false);
            let truncated_right_width = visible_width(&truncated_right) as i64;
            let pad = (width - stats_left_width - truncated_right_width).max(0) as usize;
            format!("{stats_left}{}{truncated_right}", " ".repeat(pad))
        } else {
            stats_left.clone()
        }
    };

    let pwd_line = truncate_to_width(&pwd, width, "...", false);
    // NOTE: pi appends a third line of extension statuses here, pulled from the
    // FooterDataProvider rather than the render input. Because those statuses
    // live on the provider (not FooterInput), that line is assembled separately
    // by `append_extension_statuses`, mirroring pi's provider/input split.
    vec![pwd_line, stats_line]
}

// ---------------------------------------------------------------------------
// footer.ts: extension statuses live on FooterDataProvider; render pulls them
// via the provider. Kept as a separate entry point mirroring pi's split.
// ---------------------------------------------------------------------------

/// Append the extension-status line (footer.ts:235) to already-rendered footer
/// `lines`, given the provider's statuses. Separated from [`render_footer`]
/// because the statuses live on [`FooterDataProvider`], matching pi's split
/// between the render input and the provider.
///
/// NOTE: pi sorts keys with `String.localeCompare`; the [`BTreeMap`] here orders
/// by byte value. No test pins locale-specific ordering.
pub fn append_extension_statuses(
    lines: &mut Vec<String>,
    statuses: &BTreeMap<String, String>,
    width: i64,
) {
    if statuses.is_empty() {
        return;
    }
    let joined = statuses
        .values()
        .map(|text| sanitize_status_text(text))
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(truncate_to_width(&joined, width, "...", false));
}

// ---------------------------------------------------------------------------
// footer-data-provider.ts: git branch resolution
// ---------------------------------------------------------------------------

/// Resolves the current branch by asking git, used when `HEAD` is `.invalid`
/// (reftable repos). NOTE: seam over pi's `spawnSync("git", ["symbolic-ref"…])`,
/// so tests can resolve without spawning a subprocess.
pub trait GitBranchResolver {
    /// Return the current branch for `repo_dir`, or `None` on detached HEAD /
    /// when git is unavailable.
    fn resolve(&self, repo_dir: &Path) -> Option<String>;
}

/// Default resolver that shells out to git, mirroring
/// `resolveBranchWithGitSync` (footer-data-provider.ts:51).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGitResolver;

impl GitBranchResolver for SystemGitResolver {
    fn resolve(&self, repo_dir: &Path) -> Option<String> {
        let output = Command::new("git")
            .args([
                "--no-optional-locks",
                "symbolic-ref",
                "--quiet",
                "--short",
                "HEAD",
            ])
            .current_dir(repo_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            None
        } else {
            Some(branch)
        }
    }
}

/// Git metadata paths, mirroring pi's `GitPaths`.
#[derive(Debug, Clone)]
struct GitPaths {
    /// Directory whose `.git` was found (the branch-resolution cwd).
    repo_dir: PathBuf,
    /// The common git dir (`.git`, or the worktree's resolved gitdir).
    #[allow(dead_code)] // NOTE: only the deferred fs-watchers read this.
    common_git_dir: PathBuf,
    /// Path to the `HEAD` file that holds the current ref.
    head_path: PathBuf,
}

/// `findGitPaths` (footer-data-provider.ts:16). Walk up from `cwd` locating git
/// metadata, handling both plain repos (`.git` dir) and worktrees (`.git` file).
fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = normalize_lexical(cwd);
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            let meta = std::fs::metadata(&git_path).ok()?;
            if meta.is_file() {
                let content = std::fs::read_to_string(&git_path).ok()?;
                let content = content.trim();
                if let Some(rest) = content.strip_prefix("gitdir: ") {
                    let git_dir = resolve_path(&dir, rest.trim());
                    let head_path = git_dir.join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    let common_dir_path = git_dir.join("commondir");
                    let common_git_dir = if common_dir_path.exists() {
                        let rel = std::fs::read_to_string(&common_dir_path).ok()?;
                        resolve_path(&git_dir, rel.trim())
                    } else {
                        git_dir.clone()
                    };
                    return Some(GitPaths {
                        repo_dir: dir,
                        common_git_dir,
                        head_path,
                    });
                }
                // A `.git` file that is not a gitdir pointer: keep walking up.
            } else if meta.is_dir() {
                let head_path = git_path.join("HEAD");
                if !head_path.exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git_path,
                    head_path,
                });
            }
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Provides git branch and extension statuses — data not otherwise accessible
/// to extensions. Mirrors pi's `FooterDataProvider`, minus the deferred
/// filesystem-watcher machinery (see module docs).
pub struct FooterDataProvider<R: GitBranchResolver = SystemGitResolver> {
    git_paths: Option<GitPaths>,
    cached_branch: Option<String>,
    extension_statuses: BTreeMap<String, String>,
    available_provider_count: usize,
    resolver: R,
}

impl FooterDataProvider<SystemGitResolver> {
    /// Construct a provider rooted at `cwd`, resolving `.invalid` HEADs via the
    /// system `git` binary.
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self::with_resolver(cwd, SystemGitResolver)
    }
}

impl<R: GitBranchResolver> FooterDataProvider<R> {
    /// Construct a provider with a custom branch resolver (used in tests).
    pub fn with_resolver(cwd: impl AsRef<Path>, resolver: R) -> Self {
        let git_paths = find_git_paths(cwd.as_ref());
        let cached_branch = Self::resolve_git_branch_sync(&git_paths, &resolver);
        FooterDataProvider {
            git_paths,
            cached_branch,
            extension_statuses: BTreeMap::new(),
            available_provider_count: 0,
            resolver,
        }
    }

    /// Current git branch, `None` if not in a repo, `"detached"` if detached.
    pub fn git_branch(&self) -> Option<&str> {
        self.cached_branch.as_deref()
    }

    /// Point the provider at a new `cwd`, recomputing the cached branch.
    pub fn set_cwd(&mut self, cwd: impl AsRef<Path>) {
        self.git_paths = find_git_paths(cwd.as_ref());
        self.cached_branch = Self::resolve_git_branch_sync(&self.git_paths, &self.resolver);
    }

    /// Extension status texts, keyed as set via `set_extension_status`.
    pub fn extension_statuses(&self) -> &BTreeMap<String, String> {
        &self.extension_statuses
    }

    /// Set (`Some`) or clear (`None`) an extension status by key.
    pub fn set_extension_status(&mut self, key: impl Into<String>, text: Option<String>) {
        match text {
            Some(text) => {
                self.extension_statuses.insert(key.into(), text);
            }
            None => {
                self.extension_statuses.remove(&key.into());
            }
        }
    }

    /// Clear all extension statuses.
    pub fn clear_extension_statuses(&mut self) {
        self.extension_statuses.clear();
    }

    /// Number of unique providers with available models (for footer display).
    pub fn available_provider_count(&self) -> usize {
        self.available_provider_count
    }

    /// Set the available-provider count.
    pub fn set_available_provider_count(&mut self, count: usize) {
        self.available_provider_count = count;
    }

    /// `resolveGitBranchSync` (footer-data-provider.ts:239).
    fn resolve_git_branch_sync(git_paths: &Option<GitPaths>, resolver: &R) -> Option<String> {
        let paths = git_paths.as_ref()?;
        let content = std::fs::read_to_string(&paths.head_path).ok()?;
        let content = content.trim();
        let Some(rest) = content.strip_prefix("ref: refs/heads/") else {
            return Some("detached".to_string());
        };
        if rest == ".invalid" {
            Some(
                resolver
                    .resolve(&paths.repo_dir)
                    .unwrap_or_else(|| "detached".to_string()),
            )
        } else {
            Some(rest.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Path helpers (POSIX lexical; mirror node:path resolve/relative on Linux CI)
// ---------------------------------------------------------------------------

/// Lexically normalise an (assumed absolute) path, collapsing `.`/`..` without
/// touching the filesystem — the relevant slice of `node:path.resolve`.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s),
        }
    }
    let mut out = PathBuf::from("/");
    for p in parts {
        out.push(p);
    }
    out
}

/// Resolve `rel` against `base`, then normalise — the slice of
/// `node:path.resolve(base, rel)` used by `find_git_paths`.
fn resolve_path(base: &Path, rel: &str) -> PathBuf {
    let r = Path::new(rel);
    let joined = if r.is_absolute() {
        r.to_path_buf()
    } else {
        base.join(r)
    };
    normalize_lexical(&joined)
}

/// POSIX `path.relative(from, to)` over two absolute, already-normalised paths.
/// Returns the empty string when they are equal.
fn path_relative(from: &Path, to: &Path) -> String {
    let from_parts = normal_parts(from);
    let to_parts = normal_parts(to);
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_parts.len() - common;
    let mut segs: Vec<String> = vec!["..".to_string(); ups];
    segs.extend(to_parts[common..].iter().cloned());
    segs.join("/")
}

/// Collect the `Normal` path components as strings (after lexical normalise).
fn normal_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // -- module-local temp fixture (no core/test_support on this base) --------

    struct TempRepo {
        root: PathBuf,
    }

    impl TempRepo {
        fn new(tag: &str) -> Self {
            let stamp = std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut root = std::env::temp_dir();
            root.push(format!(
                "pidgin-footer-{tag}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            TempRepo { root }
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let dir = self.root.join(rel);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let file = self.root.join(rel);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, content).unwrap();
            file
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A resolver that records how many times it was asked and returns a fixed
    /// answer, standing in for pi's mocked `spawnSync`.
    struct StubResolver {
        answer: Option<String>,
        calls: std::cell::Cell<usize>,
    }

    impl StubResolver {
        fn new(answer: Option<&str>) -> Self {
            StubResolver {
                answer: answer.map(str::to_string),
                calls: std::cell::Cell::new(0),
            }
        }
    }

    impl GitBranchResolver for StubResolver {
        fn resolve(&self, _repo_dir: &Path) -> Option<String> {
            self.calls.set(self.calls.get() + 1);
            self.answer.clone()
        }
    }

    // -- render input builders (base_*() + functional-record-update) ---------

    fn base_model() -> ModelInfo {
        ModelInfo {
            id: "test-model".to_string(),
            provider: "test".to_string(),
            context_window: 200_000,
            reasoning: false,
        }
    }

    fn base_usage() -> AssistantUsage {
        AssistantUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost_total: 0.0,
        }
    }

    /// Mirrors footer-width.test.ts `createSession`: default model, context
    /// window 200k, live context percent 12.3.
    fn base_input() -> FooterInput {
        FooterInput {
            model: Some(base_model()),
            thinking_level: "off".to_string(),
            usages: Vec::new(),
            context_usage: Some(ContextUsage {
                context_window: 200_000,
                percent: Some(12.3),
            }),
            cwd: "/tmp/project".to_string(),
            home: None,
            git_branch: Some("main".to_string()),
            session_name: String::new(),
            available_provider_count: 1,
            auto_compact_enabled: true,
            is_using_oauth: false,
            experimental_features_enabled: false,
        }
    }

    // -- footer-data-provider.test.ts (synchronous branch detection) ----------

    #[test]
    fn uses_head_directly_in_regular_repo_from_nested_dir() {
        let repo = TempRepo::new("regular");
        repo.mkdir("repo/.git");
        repo.write("repo/.git/HEAD", "ref: refs/heads/main\n");
        let nested = repo.mkdir("repo/src/nested");

        let resolver = StubResolver::new(Some("should-not-be-used"));
        let provider = FooterDataProvider::with_resolver(&nested, resolver);

        assert_eq!(provider.git_branch(), Some("main"));
        // spawnSync-equivalent must not be consulted for a valid HEAD.
        assert_eq!(provider.resolver.calls.get(), 0);
    }

    #[test]
    fn resolves_branch_via_git_when_head_is_invalid_in_reftable_repo() {
        let repo = TempRepo::new("reftable");
        repo.mkdir("repo/.git/reftable");
        let repo_dir = repo.write("repo/.git/HEAD", "ref: refs/heads/.invalid\n");
        let repo_dir = repo_dir.parent().unwrap().parent().unwrap().to_path_buf();

        let resolver = StubResolver::new(Some("main"));
        let provider = FooterDataProvider::with_resolver(&repo_dir, resolver);

        assert_eq!(provider.git_branch(), Some("main"));
        assert_eq!(provider.resolver.calls.get(), 1);
    }

    #[test]
    fn resolves_branch_via_git_in_reftable_backed_worktree() {
        let repo = TempRepo::new("worktree");
        let git_dir = repo.mkdir("repo/.git/worktrees/src");
        repo.mkdir("repo/.git/reftable");
        let worktree_dir = repo.mkdir("worktree");
        repo.write("worktree/.git", &format!("gitdir: {}\n", git_dir.display()));
        repo.write("repo/.git/worktrees/src/HEAD", "ref: refs/heads/.invalid\n");
        repo.write("repo/.git/worktrees/src/commondir", "../..\n");
        repo.write("repo/.git/reftable/tables.list", "0\n");

        let resolver = StubResolver::new(Some("main"));
        let provider = FooterDataProvider::with_resolver(&worktree_dir, resolver);

        assert_eq!(provider.git_branch(), Some("main"));
    }

    #[test]
    fn treats_unresolved_invalid_reftable_head_as_detached() {
        let repo = TempRepo::new("detached");
        repo.mkdir("repo/.git/reftable");
        let head = repo.write("repo/.git/HEAD", "ref: refs/heads/.invalid\n");
        let repo_dir = head.parent().unwrap().parent().unwrap().to_path_buf();

        let resolver = StubResolver::new(None);
        let provider = FooterDataProvider::with_resolver(&repo_dir, resolver);

        assert_eq!(provider.git_branch(), Some("detached"));
    }

    #[test]
    fn extension_statuses_and_provider_count_round_trip() {
        let repo = TempRepo::new("statuses");
        repo.mkdir("repo/.git");
        repo.write("repo/.git/HEAD", "ref: refs/heads/main\n");
        let repo_dir = repo.root.join("repo");

        let mut provider = FooterDataProvider::with_resolver(&repo_dir, StubResolver::new(None));
        assert!(provider.extension_statuses().is_empty());

        provider.set_extension_status("lint", Some("ok".to_string()));
        provider.set_extension_status("build", Some("running".to_string()));
        assert_eq!(
            provider.extension_statuses().get("lint"),
            Some(&"ok".to_string())
        );

        provider.set_extension_status("lint", None);
        assert!(provider.extension_statuses().get("lint").is_none());

        provider.set_available_provider_count(3);
        assert_eq!(provider.available_provider_count(), 3);

        provider.clear_extension_statuses();
        assert!(provider.extension_statuses().is_empty());
    }

    // -- footer-width.test.ts: formatCwdForFooter -----------------------------

    #[test]
    fn format_cwd_cases() {
        // (cwd, home, expected)
        let cases = [
            // Sibling paths sharing the home prefix are not abbreviated.
            ("/home/user2", "/home/user", "/home/user2"),
            // The home directory and its descendants are abbreviated.
            ("/home/user", "/home/user", "~"),
            ("/home/user/project", "/home/user", "~/project"),
        ];
        for (cwd, home, expected) in cases {
            assert_eq!(
                format_cwd_for_footer(cwd, Some(home)),
                expected,
                "cwd={cwd} home={home}"
            );
        }
    }

    // -- footer-width.test.ts: FooterComponent width handling -----------------

    #[test]
    fn keeps_all_lines_within_width_for_wide_session_names() {
        let width = 93;
        // "\u{d55c}\u{ae00}" == "한글", repeated 30 times (wide CJK).
        let input = FooterInput {
            session_name: "\u{d55c}\u{ae00}".repeat(30),
            ..base_input()
        };

        for line in render_footer(&input, width) {
            assert!(
                visible_width(&line) as i64 <= width,
                "line {line:?} exceeds width {width}"
            );
        }
    }

    #[test]
    fn keeps_stats_line_within_width_for_wide_model_and_provider_names() {
        let width = 60;
        let input = FooterInput {
            session_name: String::new(),
            model: Some(ModelInfo {
                // "\u{6a21}" == "模", repeated 30 times.
                id: "\u{6a21}".repeat(30),
                // "\u{acf5}\u{ae09}\u{c790}" == "공급자".
                provider: "\u{acf5}\u{ae09}\u{c790}".to_string(),
                reasoning: true,
                ..base_model()
            }),
            thinking_level: "high".to_string(),
            usages: vec![AssistantUsage {
                input: 12_345,
                output: 6_789,
                cost_total: 1.234,
                ..base_usage()
            }],
            available_provider_count: 2,
            ..base_input()
        };

        for line in render_footer(&input, width) {
            assert!(
                visible_width(&line) as i64 <= width,
                "line {line:?} exceeds width {width}"
            );
        }
    }

    #[test]
    fn shows_latest_cache_hit_rate_when_cache_usage_present() {
        let input = FooterInput {
            session_name: String::new(),
            usages: vec![AssistantUsage {
                input: 100,
                output: 10,
                cache_read: 50,
                cache_write: 50,
                cost_total: 0.001,
            }],
            ..base_input()
        };

        let stats_line = crate::utils::ansi::strip_ansi(&render_footer(&input, 120)[1]);
        assert!(stats_line.contains("CH25.0%"), "stats line: {stats_line:?}");
    }

    #[test]
    fn marks_kimi_coding_costs_as_subscription_estimates() {
        let input = FooterInput {
            session_name: String::new(),
            model: Some(ModelInfo {
                provider: "kimi-coding".to_string(),
                ..base_model()
            }),
            usages: vec![AssistantUsage {
                input: 100,
                output: 10,
                cost_total: 1.234,
                ..base_usage()
            }],
            ..base_input()
        };

        let stats_line = crate::utils::ansi::strip_ansi(&render_footer(&input, 120)[1]);
        assert!(
            stats_line.contains("$1.234 (sub)"),
            "stats line: {stats_line:?}"
        );
    }

    // -- extension-status line assembly ---------------------------------------

    #[test]
    fn extension_status_line_is_sorted_and_sanitized() {
        let mut statuses = BTreeMap::new();
        statuses.insert("b".to_string(), "one\ttwo".to_string());
        statuses.insert("a".to_string(), "  hi   there  ".to_string());
        let mut lines = vec!["pwd".to_string(), "stats".to_string()];
        append_extension_statuses(&mut lines, &statuses, 120);
        assert_eq!(lines.len(), 3);
        // "a" sorts first; whitespace collapsed and trimmed.
        assert_eq!(lines[2], "hi there one two");
    }

    #[test]
    fn format_tokens_boundaries() {
        let cases = [
            (0i64, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (6_789, "6.8k"),
            (12_345, "12k"),
            (200_000, "200k"),
            (1_500_000, "1.5M"),
            (12_000_000, "12M"),
        ];
        for (count, expected) in cases {
            assert_eq!(format_tokens(count), expected, "count={count}");
        }
    }
}
