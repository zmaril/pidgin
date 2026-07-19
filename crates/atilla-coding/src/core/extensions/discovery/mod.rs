// straitjacket-allow-file:duplication
//! Extension discovery: the filesystem-convention scan that locates extensions
//! and resolves each one's declared entrypoint into an inventory.
//!
//! Faithful port of the *discovery + entrypoint-resolution* half of pi's
//! `packages/coding-agent/src/core/extensions/loader.ts` — specifically
//! `discoverAndLoadExtensions`, `discoverExtensionsInDir`,
//! `resolveExtensionEntries`, `readPiManifest`, and `isExtensionFile`. It stops
//! exactly where pi hands the resolved paths to jiti: this module answers "which
//! files are extensions and where is each entrypoint", and produces a
//! [`DiscoveryResult`] of [`DiscoveredExtension`]s. It does **not** execute any
//! JavaScript — loading each entrypoint on the embedded `deno_core` runtime and
//! running its `(pi) => {}` factory (which populates the
//! [`Registry`](super::registry::Registry) with tools/hooks/commands) is the
//! separate JS-execution plane (a later PR). See "Handoff to the loader" below.
//!
//! # Discovery rules (mirroring pi exactly)
//!
//! pi has no manifest file; the filesystem convention *is* the manifest.
//! [`discover_extensions`] scans three roots, in this precedence, de-duplicating
//! by resolved path (first occurrence wins):
//!
//! 1. **Project-local** — `cwd/.pi/extensions/`.
//! 2. **Global** — `agent_dir/extensions/`.
//! 3. **Explicitly configured paths** — each may be a direct entrypoint file, or
//!    a directory resolved the same way a discovered subdirectory is.
//!
//! Within a scanned directory ([`discover_extensions_in_dir`]) the rules are:
//!
//! 1. Direct files: `*.ts` or `*.js` → an entrypoint.
//! 2. Subdirectory with `index.ts` / `index.js` (`.ts` preferred) → an entrypoint.
//! 3. Subdirectory with a `package.json` carrying a `"pi"` field whose
//!    `extensions` array declares one or more paths → those declared entrypoints
//!    (this takes precedence over `index.ts`).
//!
//! Discovery does **not** recurse beyond one level: a subdirectory that has
//! neither an index file nor a `package.json` `pi` manifest is ignored, even if
//! it contains a nested extension.
//!
//! # atilla's language declaration
//!
//! Per `notes/design.md` §Extensions, atilla mirrors pi's TS/JS convention and
//! *extends* it with a per-extension language declaration so a host-language
//! extension (PHP/Python/Node) can be discovered the same way. Today discovery
//! infers [`ExtensionLanguage`] from the entrypoint suffix (pi only recognizes
//! `.ts`/`.js`); the enum is the seam a manifest-declared `language` field will
//! populate when host-language discovery lands.
//!
//! # Parity notes (Rust path resolution vs Node/jiti)
//!
//! - **Entrypoint resolution is a literal suffix check, not jiti resolution.**
//!   pi's *discovery* only recognizes files ending in `.ts` or `.js` and the two
//!   fixed `index.{ts,js}` names. jiti's richer module resolution (extension-less
//!   specifiers, `.mjs`/`.cjs`, nested `index`, `node_modules`) happens *inside*
//!   an extension when it is imported — that is the JS-execution plane's job, not
//!   discovery's. We mirror pi's literal check and deliberately do **not** invent
//!   `.mjs`/extension-less discovery here.
//! - **`package.json` `pi.extensions` paths use `path.resolve(dir, entry)`
//!   semantics with no `~` expansion.** A declared `"~entry.ts"` resolves to
//!   `dir/~entry.ts` and `"~/entry.ts"` to `dir/~/entry.ts`, package-relative and
//!   literal — matching Node. We call [`resolve_path`] with `expand_tilde:false`
//!   to reproduce this. (Explicitly *configured* top-level paths, by contrast,
//!   keep pi's `~`-expanding + unicode-space-normalizing `resolvePath`.)
//! - **Directory entries are sorted by name for determinism.** pi relies on
//!   `fs.readdirSync` order (filesystem-dependent); Rust's `read_dir` is
//!   likewise unordered. No pi test asserts intra-directory order (they sort or
//!   assert counts), so sorting is a safe, behavior-preserving determinism fix.
//!
//! # Handoff to the loader
//!
//! Each [`DiscoveredExtension`] is what the JS-execution plane consumes: it reads
//! `entrypoint_path`, transpiles/imports it on the `deno_core` runtime, and runs
//! the default-exported factory, whose `registerTool` / `on` / `registerCommand`
//! calls land in the core [`Registry`](super::registry::Registry). Load-time
//! failures (a malformed module, a factory that throws, a missing default export)
//! surface as [`DiscoveryResult::errors`] entries *there*; the pure filesystem
//! scan in this module produces no errors of its own (it silently skips
//! unreadable directories and non-existent declared paths, exactly like pi).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/loader.ts`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::paths::{resolve_path, PathInputOptions};

#[cfg(test)]
mod tests;

/// Config directory name (`CONFIG_DIR_NAME` in pi's `config.ts`).
const CONFIG_DIR_NAME: &str = ".pi";

/// The implementation language of a discovered extension.
///
/// pi's discovery convention only recognizes TypeScript and JavaScript
/// entrypoints; this enum is inferred from the entrypoint suffix today and is
/// the seam a manifest-declared `language` field will drive once host-language
/// discovery lands (`notes/design.md` §Extensions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionLanguage {
    /// A `.ts` entrypoint.
    TypeScript,
    /// A `.js` (or any non-`.ts`) entrypoint.
    JavaScript,
}

/// Which discovery root an extension was found through, in precedence order.
///
/// Analogous to pi's `SourceScope` (project vs user vs ad-hoc); recorded as
/// inventory metadata so a binding can report where an extension came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryOrigin {
    /// Found under `cwd/.pi/extensions/`.
    ProjectLocal,
    /// Found under `agent_dir/extensions/`.
    Global,
    /// Supplied as an explicitly configured path.
    Configured,
}

/// A single extension located by discovery, with its entrypoint resolved to a
/// concrete file path.
///
/// This is the inventory record the JS-execution plane consumes; see the module
/// docs' "Handoff to the loader".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredExtension {
    /// Stable identifier: the entrypoint's file stem, or — for an `index.{ts,js}`
    /// entrypoint — the containing directory's name (so a subdirectory extension
    /// is identified by its directory rather than the generic `index`).
    pub id: String,
    /// The directory the entrypoint lives in (pi's `baseDir`, i.e.
    /// `dirname(resolvedPath)`).
    pub root: PathBuf,
    /// Inferred/declared implementation language.
    pub language: ExtensionLanguage,
    /// Absolute, resolved path to the entrypoint file. Equivalent to pi's
    /// `extension.path`.
    pub entrypoint_path: PathBuf,
    /// Which root this extension was discovered through.
    pub origin: DiscoveryOrigin,
}

/// A load-time error for one extension path.
///
/// The pure filesystem discovery in this module never produces these (it mirrors
/// pi's silent skipping); the field exists so the JS-execution plane can report
/// module/factory failures through the same [`DiscoveryResult`] shape pi returns
/// from `discoverAndLoadExtensions` (`LoadExtensionsResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryError {
    /// The path that failed.
    pub path: String,
    /// A human-readable error message.
    pub error: String,
}

/// The result of a discovery pass: the resolved inventory plus any load errors.
///
/// Mirrors pi's `LoadExtensionsResult` (minus the runtime handle, which is a
/// JS-execution-plane concern).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryResult {
    /// Discovered extensions, de-duplicated by resolved path, in discovery order.
    pub extensions: Vec<DiscoveredExtension>,
    /// Per-path load errors (always empty from the filesystem scan alone).
    pub errors: Vec<DiscoveryError>,
}

/// A `package.json` `pi` manifest (only the extension-discovery-relevant field).
///
/// Mirrors pi's `PiManifest`; `themes`/`skills`/`prompts` are out of scope for
/// extension discovery.
struct PiManifest {
    extensions: Option<Vec<String>>,
}

/// Current working directory as a string, for resolving relative roots.
fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Join `segment` onto `base` with a single separator (mirrors `path.join`
/// for the simple absolute-base case discovery uses).
fn join2(base: &str, segment: &str) -> String {
    format!("{}/{segment}", base.trim_end_matches('/'))
}

/// Whether a filename is an extension entrypoint file (pi's `isExtensionFile`).
fn is_extension_file(name: &str) -> bool {
    name.ends_with(".ts") || name.ends_with(".js")
}

/// Read a `package.json`'s `pi` manifest, or `None` if the file is unreadable,
/// not valid JSON, or has no object-valued `pi` field (pi's `readPiManifest`).
fn read_pi_manifest(package_json_path: &str) -> Option<PiManifest> {
    let content = fs::read_to_string(package_json_path).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&content).ok()?;
    let pi = pkg.get("pi")?;
    if !pi.is_object() {
        return None;
    }
    let extensions = pi.get("extensions").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });
    Some(PiManifest { extensions })
}

/// Options reproducing Node's `path.resolve(dir, entry)`: no `~` expansion, no
/// `@`/unicode rewriting. Used for `package.json`-declared entry paths.
fn package_relative_opts() -> PathInputOptions {
    PathInputOptions {
        expand_tilde: false,
        ..PathInputOptions::default()
    }
}

/// Resolve extension entrypoints declared by a subdirectory, or `None` if the
/// directory declares none (pi's `resolveExtensionEntries`).
///
/// Precedence: a `package.json` `pi.extensions` list (whose existing entries win)
/// over `index.ts` over `index.js`.
fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    // 1. package.json with a "pi.extensions" field takes precedence.
    let package_json_path = join2(dir, "package.json");
    if Path::new(&package_json_path).exists() {
        if let Some(manifest) = read_pi_manifest(&package_json_path) {
            if let Some(declared) = manifest.extensions.filter(|e| !e.is_empty()) {
                let opts = package_relative_opts();
                let mut entries = Vec::new();
                for ext_path in &declared {
                    let resolved =
                        resolve_path(ext_path, dir, &opts).unwrap_or_else(|_| ext_path.clone());
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

    // 2. index.ts, then index.js.
    let index_ts = join2(dir, "index.ts");
    if Path::new(&index_ts).exists() {
        return Some(vec![index_ts]);
    }
    let index_js = join2(dir, "index.js");
    if Path::new(&index_js).exists() {
        return Some(vec![index_js]);
    }

    None
}

/// Discover extension entrypoints directly inside `dir` (pi's
/// `discoverExtensionsInDir`). Returns absolute entrypoint paths; never recurses
/// beyond one level.
fn discover_extensions_in_dir(dir: &str) -> Vec<String> {
    if !Path::new(dir).exists() {
        return Vec::new();
    }

    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return Vec::new(),
    };

    // Collect (name, file_type) then sort by name for deterministic ordering
    // (pi relies on unordered readdir; see the module's parity notes).
    let mut entries: Vec<(String, fs::FileType)> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        entries.push((name, file_type));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut discovered = Vec::new();
    for (name, file_type) in entries {
        let entry_path = join2(dir, &name);
        let is_symlink = file_type.is_symlink();

        // 1. Direct files: *.ts or *.js (symlinks eligible too).
        if (file_type.is_file() || is_symlink) && is_extension_file(&name) {
            discovered.push(entry_path);
            continue;
        }

        // 2 & 3. Subdirectories (symlinks eligible too).
        if file_type.is_dir() || is_symlink {
            if let Some(entries) = resolve_extension_entries(&entry_path) {
                discovered.extend(entries);
            }
        }
    }

    discovered
}

/// Infer the entrypoint language from its suffix. pi's discovery recognizes only
/// `.ts`/`.js`; anything else (reachable only via a non-`.ts` configured path)
/// defaults to [`ExtensionLanguage::JavaScript`], matching how jiti treats an
/// unknown suffix.
fn infer_language(path: &str) -> ExtensionLanguage {
    if path.ends_with(".ts") {
        ExtensionLanguage::TypeScript
    } else {
        ExtensionLanguage::JavaScript
    }
}

/// Derive a [`DiscoveredExtension::id`] from an entrypoint path.
fn derive_id(entrypoint: &Path) -> String {
    let stem = entrypoint
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if stem == "index" {
        if let Some(parent_name) = entrypoint.parent().and_then(Path::file_name) {
            return parent_name.to_string_lossy().into_owned();
        }
    }
    stem
}

/// Build a [`DiscoveredExtension`] from a resolved absolute entrypoint path.
fn make_discovered(path: String, origin: DiscoveryOrigin) -> DiscoveredExtension {
    let entrypoint_path = PathBuf::from(&path);
    let root = entrypoint_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let language = infer_language(&path);
    let id = derive_id(&entrypoint_path);
    DiscoveredExtension {
        id,
        root,
        language,
        entrypoint_path,
        origin,
    }
}

/// Add `paths` to the running inventory, de-duplicating by resolved path so the
/// first occurrence (highest-precedence root) wins (pi's `addPaths`/`seen` set).
fn add_paths(
    all: &mut Vec<(String, DiscoveryOrigin)>,
    seen: &mut HashSet<String>,
    paths: Vec<String>,
    origin: DiscoveryOrigin,
) {
    let opts = PathInputOptions::default();
    let base = current_dir_string();
    for p in paths {
        // `path.resolve(p)`: p is already absolute, so this just normalizes it.
        let key = resolve_path(&p, &base, &opts).unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            all.push((p, origin));
        }
    }
}

/// Discover and resolve extensions from the standard roots plus any explicitly
/// configured paths — the inventory-producing half of pi's
/// `discoverAndLoadExtensions`.
///
/// `cwd` is the project root, `agent_dir` the global agent directory (pi's
/// `getAgentDir()`), and `configured_paths` any additional explicit
/// files/directories. Results are de-duplicated by resolved path with
/// project-local > global > configured precedence.
pub fn discover_extensions(
    configured_paths: &[String],
    cwd: &str,
    agent_dir: &str,
) -> DiscoveryResult {
    let default_opts = PathInputOptions::default();
    let base = current_dir_string();
    let resolved_cwd = resolve_path(cwd, &base, &default_opts).unwrap_or_else(|_| cwd.to_string());
    let resolved_agent_dir =
        resolve_path(agent_dir, &base, &default_opts).unwrap_or_else(|_| agent_dir.to_string());

    let mut all: Vec<(String, DiscoveryOrigin)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 1. Project-local extensions: cwd/.pi/extensions/
    let local_ext_dir = join2(&join2(&resolved_cwd, CONFIG_DIR_NAME), "extensions");
    add_paths(
        &mut all,
        &mut seen,
        discover_extensions_in_dir(&local_ext_dir),
        DiscoveryOrigin::ProjectLocal,
    );

    // 2. Global extensions: agent_dir/extensions/
    let global_ext_dir = join2(&resolved_agent_dir, "extensions");
    add_paths(
        &mut all,
        &mut seen,
        discover_extensions_in_dir(&global_ext_dir),
        DiscoveryOrigin::Global,
    );

    // 3. Explicitly configured paths.
    let configured_opts = PathInputOptions {
        normalize_unicode_spaces: true,
        ..PathInputOptions::default()
    };
    for p in configured_paths {
        let resolved =
            resolve_path(p, &resolved_cwd, &configured_opts).unwrap_or_else(|_| p.clone());
        let resolved_path = Path::new(&resolved);
        if resolved_path.exists() && resolved_path.is_dir() {
            // A directory: resolve its declared/index entries, else scan it.
            if let Some(entries) = resolve_extension_entries(&resolved) {
                add_paths(&mut all, &mut seen, entries, DiscoveryOrigin::Configured);
                continue;
            }
            add_paths(
                &mut all,
                &mut seen,
                discover_extensions_in_dir(&resolved),
                DiscoveryOrigin::Configured,
            );
            continue;
        }
        // A file (or non-existent path): add it directly.
        add_paths(
            &mut all,
            &mut seen,
            vec![resolved],
            DiscoveryOrigin::Configured,
        );
    }

    let extensions = all
        .into_iter()
        .map(|(path, origin)| make_discovered(path, origin))
        .collect();

    DiscoveryResult {
        extensions,
        errors: Vec::new(),
    }
}

/// Resolve an explicit list of extension paths **without** running the
/// filesystem-convention scan — the discovery-layer analog of pi's
/// `loadExtensions`.
///
/// Each path is resolved with pi's `~`-expanding, unicode-normalizing
/// `resolvePath` and turned into a [`DiscoveredExtension`] as-is (no existence
/// check, no `index`/`package.json` resolution, no de-duplication) — exactly the
/// paths pi would hand straight to the loader. An empty list discovers nothing.
pub fn load_extensions(paths: &[String], cwd: &str) -> DiscoveryResult {
    let default_opts = PathInputOptions::default();
    let base = current_dir_string();
    let resolved_cwd = resolve_path(cwd, &base, &default_opts).unwrap_or_else(|_| cwd.to_string());

    let configured_opts = PathInputOptions {
        normalize_unicode_spaces: true,
        ..PathInputOptions::default()
    };

    let extensions = paths
        .iter()
        .map(|p| {
            let resolved =
                resolve_path(p, &resolved_cwd, &configured_opts).unwrap_or_else(|_| p.clone());
            make_discovered(resolved, DiscoveryOrigin::Configured)
        })
        .collect();

    DiscoveryResult {
        extensions,
        errors: Vec::new(),
    }
}
