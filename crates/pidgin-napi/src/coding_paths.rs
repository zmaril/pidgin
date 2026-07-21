//! Coding-agent path-helpers surface (`utils/paths.ts`): drives pi's pure path
//! string transforms natively.
//!
//! Scope of the native flip is the pure lexical helpers from
//! `packages/coding-agent/src/utils/paths.ts`, ported to
//! [`pidgin_coding::utils::paths`]: `canonicalizePath`, `isLocalPath`,
//! `normalizePath`, `resolvePath`, and `getCwdRelativePath`. These are pure
//! path-string transforms (canonicalize is a `realpath` wrapper with a raw
//! fallback); the Rust code runs in the Node process, so `std::env::current_dir`
//! and `$HOME` observe the same cwd/home Node does.
//!
//! Two boundary details preserve pi's contract exactly:
//! * pi's `normalizePath`/`resolvePath` throw on a malformed `file://` URL
//!   (`fileURLToPath` throws); the Rust port returns `Err`, which the addon maps
//!   to a thrown JS error.
//! * pi's `getCwdRelativePath` returns `string | undefined` (NOT null); the Rust
//!   `Option<String>` marshals `None` to JS `undefined`, matching the test's
//!   `.toBeUndefined()` assertion.
//!
//! `resolvePath`'s JS default `baseDir = process.cwd()` is re-added on the JS
//! side by the shim (it passes an explicit resolved cwd into the native call).
//! `markPathIgnoredByCloudSync` is a side-effecting (`xattr`/`setfattr`) shell-out
//! and is intentionally NOT ported — the shim keeps it delegated to pi's original.

use napi_derive::napi;
use pidgin_coding::utils::paths::PathInputOptions;

/// JS `PathInputOptions` mirror. Every field is optional so pi's default
/// semantics (`expandTilde ?? true`, all others falsy/undefined) survive the
/// crossing. napi renders the snake_case fields as their camelCase JS names
/// (`expandTilde`, `homeDir`, `stripAtPrefix`, `normalizeUnicodeSpaces`).
#[napi(object)]
pub struct CodingPathInputOptions {
    pub trim: Option<bool>,
    pub expand_tilde: Option<bool>,
    pub home_dir: Option<String>,
    pub strip_at_prefix: Option<bool>,
    pub normalize_unicode_spaces: Option<bool>,
}

/// Fold JS options into the Rust `PathInputOptions`, applying pi's defaults for
/// any field the caller omitted (`PathInputOptions::default()` already encodes
/// `expand_tilde = true` and everything else off).
fn to_opts(options: Option<CodingPathInputOptions>) -> PathInputOptions {
    let mut opts = PathInputOptions::default();
    if let Some(o) = options {
        if let Some(v) = o.trim {
            opts.trim = v;
        }
        if let Some(v) = o.expand_tilde {
            opts.expand_tilde = v;
        }
        if o.home_dir.is_some() {
            opts.home_dir = o.home_dir;
        }
        if let Some(v) = o.strip_at_prefix {
            opts.strip_at_prefix = v;
        }
        if let Some(v) = o.normalize_unicode_spaces {
            opts.normalize_unicode_spaces = v;
        }
    }
    opts
}

/// `canonicalizePath` (utils/paths.ts): resolve to the real (symlink-followed)
/// path, falling back to the raw path when resolution fails.
#[napi(js_name = "canonicalizePath")]
pub fn canonicalize_path(path: String) -> String {
    pidgin_coding::utils::paths::canonicalize_path(&path)
}

/// `isLocalPath` (utils/paths.ts): true unless the value carries a non-local
/// source/URL prefix (`npm:`/`git:`/`github:`/`http:`/`https:`/`ssh:`).
#[napi(js_name = "isLocalPath")]
pub fn is_local_path(value: String) -> bool {
    pidgin_coding::utils::paths::is_local_path(&value)
}

/// `normalizePath` (utils/paths.ts): trim/unicode-space fold/`@`-strip/`~`-expand
/// and `file://` → path conversion. A malformed `file://` URL throws (pi's
/// `fileURLToPath` contract).
#[napi(js_name = "normalizePath")]
pub fn normalize_path(
    input: String,
    options: Option<CodingPathInputOptions>,
) -> napi::Result<String> {
    pidgin_coding::utils::paths::normalize_path(&input, &to_opts(options))
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `resolvePath` (utils/paths.ts): normalize `input`, then resolve it against
/// `base_dir` (absolute inputs ignore the base). The JS `baseDir = process.cwd()`
/// default is supplied by the shim, which passes an explicit cwd here. Throws on
/// a malformed `file://` URL in either argument.
#[napi(js_name = "resolvePath")]
pub fn resolve_path(
    input: String,
    base_dir: String,
    options: Option<CodingPathInputOptions>,
) -> napi::Result<String> {
    pidgin_coding::utils::paths::resolve_path(&input, &base_dir, &to_opts(options))
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `getCwdRelativePath` (utils/paths.ts): `file_path` relative to `cwd` when it
/// lies inside `cwd`, else `undefined` (pi returns `string | undefined`; `None`
/// crosses as JS `undefined`, NOT null).
#[napi(js_name = "getCwdRelativePath")]
pub fn get_cwd_relative_path(file_path: String, cwd: String) -> napi::Result<Option<String>> {
    pidgin_coding::utils::paths::get_cwd_relative_path(&file_path, &cwd)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
