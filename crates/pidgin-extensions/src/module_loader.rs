//! Bare-specifier module resolution for the deno extension plane.
//!
//! # Why this exists
//!
//! A real upstream pi extension that registers an LLM-callable tool builds the
//! tool's parameter schema with TypeBox:
//!
//! ```ignore
//! import { Type } from "typebox";
//! // ...
//! pi.registerTool({ name: "…", parameters: Type.Object({}), … });
//! ```
//!
//! `import type` is erased at transpile, but this is a **value** import: after
//! `deno_ast` type-strips the source ([`crate::loader::transpile_ts`]) the
//! `import { Type } from "typebox"` survives, so evaluating the module triggers
//! `module_loader.resolve("typebox", <entry>, …)` then `.load(…)`. Before this
//! loader existed the plane wired no [`deno_core::ModuleLoader`] (the default
//! `NoopModuleLoader`), so every such extension failed to load — the deferred
//! follow-up called out at `loader.rs`'s "TypeScript transpile" note (the
//! `ModuleLoader` reproducing jiti's resolution, "recon blocker #6").
//!
//! # What pi does (the behavior this mirrors)
//!
//! pi loads extensions with jiti and makes TypeBox available through jiti's
//! `virtualModules` alias map. See pi's
//! `packages/coding-agent/src/core/extensions/loader.ts` `VIRTUAL_MODULES`
//! (loader.ts:47-57), which aliases `typebox` and `@sinclair/typebox` (plus the
//! `/compile` and `/value` subpaths and the `@earendil-works/*` packages) to
//! bundled modules. This loader mirrors the **`typebox` / `@sinclair/typebox`
//! root** slice of that map with a single vendored, pinned TypeBox 1.1.38 ESM.
//!
//! # Specifier classes
//!
//! [`PidginModuleLoader::resolve`] partitions every import specifier into three
//! classes:
//!
//! * **Vendored bare specifier** — exactly `typebox` or `@sinclair/typebox`:
//!   resolved to the synthetic URL [`TYPEBOX_URL`], whose source
//!   [`PidginModuleLoader::load`] serves from the embedded bundle (behind a small
//!   `TextEncoder` shim the bundle needs — see [`TEXTENCODER_SHIM`]). This is the
//!   only bare specifier that resolves.
//! * **Relative / URL specifier** — starts with `.` or `/`, or already has a URL
//!   scheme (`file:`, `http:`, …): delegated to [`deno_core::resolve_import`],
//!   which handles the extension entry module itself (loaded under
//!   `file:///pidgin-extension/…`) and any sibling relative import.
//! * **Any other bare specifier** — e.g. `typebox/compile`, `typebox/value`,
//!   `@earendil-works/pi-ai`, `node:fs`: rejected with a clear
//!   [`ModuleLoaderError`] naming the unresolvable specifier and stating that
//!   only `typebox` is vendored. These are the deliberate scope boundary: pi's
//!   full alias map also serves the `/compile` + `/value` subpaths and the
//!   pi-ai / pi-tui packages; wiring those (and a `node:` shim) is a larger
//!   pi-runtime-shim follow-up.

use deno_core::error::ModuleLoaderError;
use deno_core::{
    resolve_import, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
};

// `ModuleLoaderError` is a type alias for `deno_error::JsErrorBox`. deno_error is
// only a transitive dependency (via deno_core), so it cannot be named directly;
// the associated constructors (`generic`, `from_err`) are called through this
// alias, which resolves to the same struct.

/// The vendored, pinned TypeBox 1.1.38 bundle (produced by esbuild; see
/// `vendor/NOTICE`). A single self-contained ESM: no internal relative imports
/// remain, so the loader serves it as one asset.
const TYPEBOX_SRC: &str = include_str!("vendor/typebox-1.1.38.mjs");

/// A minimal `TextEncoder` host global the vendored TypeBox bundle needs at load.
///
/// TypeBox's FNV-hash module constructs `new TextEncoder()` at module top level
/// (`typebox-1.1.38.mjs`), so the class must exist the moment the module
/// evaluates — even though `Type.Object(...)` never invokes hashing. `TextEncoder`
/// is a Web Platform global that Node/Bun (where pi runs) provide ambiently, but
/// the bare `deno_core` plane wires no `deno_web`, so it is absent and the module
/// throws `ReferenceError: TextEncoder is not defined`. [`PidginModuleLoader::load`]
/// prepends this shim to the served TypeBox source so the global is defined
/// (non-destructively via `??=`) before the bundle runs. It is a correct minimal
/// UTF-8 encoder — BMP plus surrogate pairs — verified byte-for-byte against the
/// platform `TextEncoder`.
const TEXTENCODER_SHIM: &str = r#"
globalThis.TextEncoder ??= class TextEncoder {
  get encoding() { return "utf-8"; }
  encode(input = "") {
    const s = String(input);
    const out = [];
    for (let i = 0; i < s.length; i++) {
      let c = s.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
        const n = s.charCodeAt(i + 1);
        if (n >= 0xdc00 && n <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (n - 0xdc00); i++; }
      }
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    }
    return new Uint8Array(out);
  }
  encodeInto(source, dest) {
    const enc = this.encode(source);
    const n = Math.min(enc.length, dest.length);
    dest.set(enc.subarray(0, n));
    return { read: source.length, written: n };
  }
};
"#;

/// The synthetic URL the vendored TypeBox bundle loads under. Extensions never
/// see this URL: [`PidginModuleLoader::resolve`] maps the bare `typebox` /
/// `@sinclair/typebox` specifiers to it, and [`PidginModuleLoader::load`]
/// answers it with [`TYPEBOX_SRC`].
const TYPEBOX_URL: &str = "file:///pidgin-vendor/typebox-1.1.38.mjs";

/// pidgin's [`deno_core::ModuleLoader`]: resolves the bare `typebox` /
/// `@sinclair/typebox` specifiers to a vendored TypeBox 1.1.38 bundle, delegates
/// relative/URL specifiers to deno_core's default resolution, and rejects every
/// other bare specifier with a clear error. See the module docs for the full
/// rationale and specifier-class table.
pub struct PidginModuleLoader;

impl PidginModuleLoader {
    /// Construct the loader. It is stateless (the vendored source is embedded at
    /// compile time), so this is cheap; it is built on the runtime's owning
    /// thread and wrapped in an `Rc` for `RuntimeOptions::module_loader`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PidginModuleLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleLoader for PidginModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // 1. The one vendored bare specifier: TypeBox's root export, aliased
        //    exactly as pi's jiti virtualModules map does (loader.ts:49,52).
        if specifier == "typebox" || specifier == "@sinclair/typebox" {
            return ModuleSpecifier::parse(TYPEBOX_URL).map_err(ModuleLoaderError::from_err);
        }

        // 2. Relative or already-qualified loadable-URL specifiers: the
        //    extension entry module and any sibling relative import. deno_core's
        //    default import resolution joins these against the referrer. Only the
        //    schemes the plane can actually serve (`file:`/`http(s):`) count as
        //    URLs here; builtin-style schemes such as `node:` fall through to the
        //    clear out-of-scope error below rather than resolving to a URL whose
        //    load has no source.
        let is_loadable_url = ModuleSpecifier::parse(specifier)
            .is_ok_and(|url| matches!(url.scheme(), "file" | "http" | "https"));
        if specifier.starts_with('.') || specifier.starts_with('/') || is_loadable_url {
            return resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err);
        }

        // 3. Every other bare specifier is out of scope. Name it, and say what
        //    IS vendored, so the failure is actionable rather than a generic
        //    "module loading is not supported".
        Err(ModuleLoaderError::generic(format!(
            "cannot resolve bare specifier {specifier:?}: pidgin's extension module loader only \
             vendors `typebox` (and its `@sinclair/typebox` alias). Subpaths like \
             `typebox/compile` / `typebox/value` and packages like `@earendil-works/pi-ai` are \
             not yet available on the plane."
        )))
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        if module_specifier.as_str() == TYPEBOX_URL {
            // Prepend the TextEncoder shim so the bundle's top-level
            // `new TextEncoder()` succeeds on the deno_web-less plane (see
            // TEXTENCODER_SHIM). One String allocation per load; loads are rare.
            let source = format!("{TEXTENCODER_SHIM}{TYPEBOX_SRC}");
            return ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(source.into()),
                module_specifier,
                None,
            )));
        }

        // resolve() only ever hands us TYPEBOX_URL or a specifier it delegated to
        // deno_core (whose source the extension supplies inline via
        // load_side_es_module_from_code, so load() is never called for it). Any
        // other specifier reaching here has no source to serve.
        ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(format!(
            "module not found: {module_specifier}"
        ))))
    }
}
