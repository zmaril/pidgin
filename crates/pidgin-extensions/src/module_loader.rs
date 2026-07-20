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
//! pi loads extensions with jiti and makes its bundled modules available through
//! jiti's `virtualModules` alias map. See pi's
//! `packages/coding-agent/src/core/extensions/loader.ts` `VIRTUAL_MODULES`
//! (loader.ts:47-57), which aliases `typebox` and `@sinclair/typebox` (plus the
//! `/compile` and `/value` subpaths and the `@earendil-works/*` packages) to
//! bundled modules. This loader mirrors the **`typebox` / `@sinclair/typebox`
//! root** slice of that map (a single vendored, pinned TypeBox 1.1.38 ESM) plus
//! two small hand-written faithful shims for the extension-facing VALUE surface
//! of the `@earendil-works/pi-ai` and `@earendil-works/pi-coding-agent` packages,
//! so real upstream `defineTool` tool extensions load, register, and invoke.
//!
//! # Specifier classes
//!
//! [`PidginModuleLoader::resolve`] partitions every import specifier into three
//! classes, driven by the [`MODULE_TABLE`] of embedded assets:
//!
//! * **Vendored / shimmed bare specifier** — matched exactly against a
//!   [`ModuleTableEntry::specifiers`] list, then resolved to that entry's
//!   synthetic `file:///pidgin-vendor/…` URL, whose source
//!   [`PidginModuleLoader::load`] serves as `prelude + source`:
//!     * `typebox` / `@sinclair/typebox` → the vendored TypeBox 1.1.38 bundle
//!       ([`TYPEBOX_SRC`]), served behind a small `TextEncoder` shim the bundle
//!       needs (see [`TEXTENCODER_SHIM`], carried as the entry's `prelude`).
//!     * `@earendil-works/pi-ai` → a shim re-exporting `Type` (from `typebox`,
//!       which nest-resolves through this same loader to the SAME bundle, so
//!       `Type`'s identity is shared) plus `StringEnum`.
//!     * `@earendil-works/pi-coding-agent` → a shim exporting the identity
//!       `defineTool`.
//! * **Relative / URL specifier** — starts with `.` or `/`, or already has a URL
//!   scheme (`file:`, `http:`, …): delegated to [`deno_core::resolve_import`],
//!   which handles the extension entry module itself (loaded under
//!   `file:///pidgin-extension/…`) and any sibling relative import.
//! * **Any other bare specifier** — e.g. `typebox/compile`, `typebox/value`,
//!   `@earendil-works/pi-tui`, `node:fs`: rejected with a clear
//!   [`ModuleLoaderError`] naming the unresolvable specifier. These are the
//!   deliberate scope boundary: pi's full alias map also serves the `/compile` +
//!   `/value` subpaths, the pi-tui package (`Text` etc.), and the host-backed
//!   tool factories (`createBashTool`'s family); wiring those (and a `node:`
//!   shim) is a larger pi-runtime / pi-tui shim follow-up.

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

/// Faithful shim of the extension-facing VALUE surface of `@earendil-works/pi-ai`
/// (`Type` re-export + `StringEnum`). Its nested `import { Type } from "typebox"`
/// nest-resolves through this loader to [`TYPEBOX_URL`], sharing `Type`'s identity
/// with direct typebox importers. See `vendor/NOTICE`.
const PI_AI_SHIM_SRC: &str = include_str!("vendor/pi-ai-shim.mjs");

/// The synthetic URL the pi-ai shim loads under.
const PI_AI_SHIM_URL: &str = "file:///pidgin-vendor/pi-ai-shim.mjs";

/// Faithful shim of `@earendil-works/pi-coding-agent`'s identity `defineTool`
/// (pi's `types.ts:497`), the one value export a `defineTool` tool extension
/// imports from that package. See `vendor/NOTICE`.
const PI_CODING_AGENT_SHIM_SRC: &str = include_str!("vendor/pi-coding-agent-shim.mjs");

/// The synthetic URL the pi-coding-agent shim loads under.
const PI_CODING_AGENT_SHIM_URL: &str = "file:///pidgin-vendor/pi-coding-agent-shim.mjs";

/// One embedded module the loader can resolve and serve for a set of bare
/// specifiers. Refactoring the resolve/load pair around this table lets N
/// specifiers scale cleanly rather than growing hardcoded `if` arms.
struct ModuleTableEntry {
    /// The exact bare specifiers that resolve to this entry (aliases share one
    /// entry, mirroring pi's jiti alias map — e.g. `typebox` / `@sinclair/typebox`).
    specifiers: &'static [&'static str],
    /// The synthetic URL the entry loads under; [`PidginModuleLoader::resolve`]
    /// maps every `specifiers` value to it, and [`PidginModuleLoader::load`]
    /// matches the incoming `module_specifier` against it.
    url: &'static str,
    /// The embedded module source served for `url`.
    source: &'static str,
    /// Text prepended to `source` at load (e.g. the [`TEXTENCODER_SHIM`] the
    /// TypeBox bundle needs). `None` for shims that need no host prelude.
    prelude: Option<&'static str>,
}

/// The embedded modules pidgin's loader resolves, mirroring the `typebox` root +
/// `@earendil-works/*` value slice of pi's jiti `virtualModules` alias map.
const MODULE_TABLE: &[ModuleTableEntry] = &[
    ModuleTableEntry {
        specifiers: &["typebox", "@sinclair/typebox"],
        url: TYPEBOX_URL,
        source: TYPEBOX_SRC,
        prelude: Some(TEXTENCODER_SHIM),
    },
    ModuleTableEntry {
        specifiers: &["@earendil-works/pi-ai"],
        url: PI_AI_SHIM_URL,
        source: PI_AI_SHIM_SRC,
        prelude: None,
    },
    ModuleTableEntry {
        specifiers: &["@earendil-works/pi-coding-agent"],
        url: PI_CODING_AGENT_SHIM_URL,
        source: PI_CODING_AGENT_SHIM_SRC,
        prelude: None,
    },
];

/// pidgin's [`deno_core::ModuleLoader`]: resolves the vendored/shimmed bare
/// specifiers in [`MODULE_TABLE`] (the `typebox` root plus the
/// `@earendil-works/pi-ai` / `@earendil-works/pi-coding-agent` value shims),
/// delegates relative/URL specifiers to deno_core's default resolution, and
/// rejects every other bare specifier with a clear error. See the module docs for
/// the full rationale and specifier-class table.
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
        // 1. A vendored/shimmed bare specifier from the module table: TypeBox's
        //    root export and the `@earendil-works/*` value shims, aliased exactly
        //    as pi's jiti virtualModules map does (loader.ts:47-57).
        for entry in MODULE_TABLE {
            if entry.specifiers.contains(&specifier) {
                return ModuleSpecifier::parse(entry.url).map_err(ModuleLoaderError::from_err);
            }
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
             vendors `typebox` (and its `@sinclair/typebox` alias) and the value shims for \
             `@earendil-works/pi-ai` (Type, StringEnum) and `@earendil-works/pi-coding-agent` \
             (defineTool). Subpaths like `typebox/compile` / `typebox/value`, `@earendil-works/pi-tui`, \
             `node:` builtins, and host-backed tool factories are not yet available on the plane."
        )))
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        for entry in MODULE_TABLE {
            if module_specifier.as_str() == entry.url {
                // Serve `prelude + source`. For the TypeBox entry the prelude is
                // the TextEncoder shim, so the bundle's top-level
                // `new TextEncoder()` succeeds on the deno_web-less plane (see
                // TEXTENCODER_SHIM); the shim entries have no prelude. One String
                // allocation per load; loads are rare.
                let source = format!("{}{}", entry.prelude.unwrap_or(""), entry.source);
                return ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                    ModuleType::JavaScript,
                    ModuleSourceCode::String(source.into()),
                    module_specifier,
                    None,
                )));
            }
        }

        // resolve() only ever hands us a MODULE_TABLE url or a specifier it
        // delegated to deno_core (whose source the extension supplies inline via
        // load_side_es_module_from_code, so load() is never called for it). Any
        // other specifier reaching here has no source to serve.
        ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(format!(
            "module not found: {module_specifier}"
        ))))
    }
}
