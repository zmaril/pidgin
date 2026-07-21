//! Hand-written core implementation behind the generated `PidginCore` trait.
//!
//! The generated binding (`src/generated.rs`) routes every JS-visible op through
//! this trait impl, so the engine wiring lives here — hand-written and stable —
//! while the napi surface is regenerated from the fluessig api schema
//! (`schema/api.json`). See `regen.sh` for the regeneration pipeline.

/// The engine-backed implementation of the generated `Pidgin` contract.
///
/// Stateless: every op delegates straight into the leaf engine crates, reaching
/// the SAME underlying logic the hand-written `#[napi]` exports called before the
/// fluessig swap, so the JS-visible behavior is byte-for-byte unchanged.
///
/// - `version` reports this addon crate's own `CARGO_PKG_VERSION`.
/// - the `path-utils` ops (`expandPath`, `resolveToCwd`, and the three private
///   macOS filename transforms) route into `pidgin_coding::core::tools::path_utils`.
///   The two fallible ops map `PathError` through `anyhow::Error`; because
///   `PathError`'s `Display` is its message and the generated wrapper throws
///   `napi::Error::from_reason(e.to_string())`, the thrown message is identical to
///   the pre-swap hand-written `map_err(|e| Error::from_reason(e.to_string()))`.
/// - the `keys` ops (`parseKey`, `matchesKey`, the two decoders, and
///   `setKittyProtocolActive`) route into `pidgin_tui::keys`. The kitty-protocol
///   flag lives in a Rust static, so the setter and readers share one addon
///   instance and stay consistent — identical to the pre-swap hand-written pair.
/// - the tui width ops (`visibleWidth`, `normalizeTerminalOutput`,
///   `truncateToWidth`, `wrapTextWithAnsi`, `sliceWithWidth`, `extractSegments`)
///   route into the `pidgin_tui` width layer, backing the native `utils.ts`
///   shim. Numeric params/returns cross as `int32` (JS `number`) and are widened
///   to the engine's `i64`/`usize` at the seam, matching the pre-swap `as i64`
///   casts — the JS-visible width values are identical.
pub struct PidginImpl;

impl crate::generated::PidginCore for PidginImpl {
    fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn expand_path(file_path: String) -> anyhow::Result<String> {
        pidgin_coding::core::tools::path_utils::expand_path(&file_path).map_err(anyhow::Error::from)
    }

    fn resolve_to_cwd(file_path: String, cwd: String) -> anyhow::Result<String> {
        pidgin_coding::core::tools::path_utils::resolve_to_cwd(&file_path, &cwd)
            .map_err(anyhow::Error::from)
    }

    fn path_try_macos_screenshot_path(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_macos_screenshot_path(&file_path)
    }

    fn path_try_nfd_variant(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_nfd_variant(&file_path)
    }

    fn path_try_curly_quote_variant(file_path: String) -> String {
        pidgin_coding::core::tools::path_utils::try_curly_quote_variant(&file_path)
    }

    fn parse_key(data: String) -> Option<String> {
        pidgin_tui::parse_key(&data)
    }

    fn matches_key(data: String, key_id: String) -> bool {
        pidgin_tui::matches_key(&data, &key_id)
    }

    fn decode_kitty_printable(data: String) -> Option<String> {
        pidgin_tui::decode_kitty_printable(&data)
    }

    fn decode_printable_key(data: String) -> Option<String> {
        pidgin_tui::decode_printable_key(&data)
    }

    fn set_kitty_protocol_active(active: bool) {
        pidgin_tui::set_kitty_protocol_active(active);
    }

    fn visible_width(s: String) -> i32 {
        pidgin_tui::visible_width(&s) as i32
    }

    fn normalize_terminal_output(s: String) -> String {
        pidgin_tui::normalize_terminal_output(&s)
    }

    fn truncate_to_width(text: String, max_width: i32, ellipsis: String, pad: bool) -> String {
        pidgin_tui::truncate_to_width(&text, max_width as i64, &ellipsis, pad)
    }

    fn wrap_text_with_ansi(text: String, width: i32) -> Vec<String> {
        pidgin_tui::wrap_text_with_ansi(&text, width.max(0) as usize)
    }

    fn slice_with_width(
        line: String,
        start_col: i32,
        length: i32,
        strict: bool,
    ) -> crate::generated::SliceWithWidth {
        let (text, width) =
            pidgin_tui::slice_with_width(&line, start_col as i64, length as i64, strict);
        crate::generated::SliceWithWidth {
            text,
            width: width as i32,
        }
    }

    fn extract_segments(
        line: String,
        before_end: i32,
        after_start: i32,
        after_len: i32,
        strict_after: bool,
    ) -> crate::generated::ExtractSegmentsResult {
        let r = pidgin_tui::extract_segments(
            &line,
            before_end as i64,
            after_start as i64,
            after_len as i64,
            strict_after,
        );
        crate::generated::ExtractSegmentsResult {
            before: r.before,
            before_width: r.before_width as i32,
            after: r.after,
            after_width: r.after_width as i32,
        }
    }
}

// --- tui keybindings layer (packages/tui/src/keybindings.ts) ----------------
//
// The engine-backed implementation behind the generated `KeybindingsManagerCore`
// handle class (its ctor + `matches`/`getKeys`/`getConflictsJson`/
// `getResolvedBindingsJson` methods). Wraps `pidgin_tui::KeybindingsManager`,
// reaching the SAME resolution logic the hand-written `#[napi]` class called
// before the fluessig swap. The core is immutable per construction (all `&self`);
// the shim's `setUserBindings` builds a fresh core. Definitions and user bindings
// cross as ordered JSON arrays (not objects) so JS insertion order is preserved
// without relying on serde_json's `preserve_order` feature.

/// JSON shape of a keybinding definition crossing into the ctor
/// (`[{ id, defaultKeys, description? }]`).
#[derive(serde::Deserialize)]
struct KeybindingDefinitionIn {
    id: String,
    #[serde(rename = "defaultKeys")]
    default_keys: Vec<String>,
    description: Option<String>,
}

/// JSON shape of a user binding crossing into the ctor (`[{ id, keys }]`).
#[derive(serde::Deserialize)]
struct UserBindingIn {
    id: String,
    // `null` = pi's explicit `undefined` (falls back to the default keys).
    keys: Option<Vec<String>>,
}

/// The engine-backed implementation of the generated `KeybindingsManagerCore`
/// contract. Holds one immutable `pidgin_tui::KeybindingsManager`; the generated
/// handle class owns it as `Arc<KeybindingsManagerCoreImpl>` and delegates each
/// method straight through, so the JS-visible behavior is byte-for-byte unchanged
/// from the pre-swap hand-written class. The ctor reproduces the hand-written
/// parse-error messages (`invalid definitions: …` / `invalid userBindings: …`)
/// via `anyhow`, which the generated wrapper throws through
/// `napi::Error::from_reason(e.to_string())`.
pub struct KeybindingsManagerCoreImpl {
    inner: pidgin_tui::KeybindingsManager,
}

impl crate::generated::KeybindingsManagerCoreCore for KeybindingsManagerCoreImpl {
    fn new(definitions_json: String, user_bindings_json: String) -> anyhow::Result<Self> {
        let defs_in: Vec<KeybindingDefinitionIn> = serde_json::from_str(&definitions_json)
            .map_err(|e| anyhow::anyhow!("invalid definitions: {e}"))?;
        let user_in: Vec<UserBindingIn> = serde_json::from_str(&user_bindings_json)
            .map_err(|e| anyhow::anyhow!("invalid userBindings: {e}"))?;

        let defs_owned: Vec<(String, pidgin_tui::KeybindingDefinition)> = defs_in
            .into_iter()
            .map(|d| {
                (
                    d.id,
                    pidgin_tui::KeybindingDefinition {
                        default_keys: d.default_keys,
                        description: d.description,
                    },
                )
            })
            .collect();
        let definitions: Vec<(&str, pidgin_tui::KeybindingDefinition)> = defs_owned
            .iter()
            .map(|(id, def)| (id.as_str(), def.clone()))
            .collect();
        let user_bindings: Vec<(&str, Option<Vec<String>>)> = user_in
            .iter()
            .map(|u| (u.id.as_str(), u.keys.clone()))
            .collect();

        Ok(Self {
            inner: pidgin_tui::KeybindingsManager::new(definitions, user_bindings),
        })
    }

    fn matches(&self, data: String, keybinding: String) -> bool {
        self.inner.matches(&data, &keybinding)
    }

    fn get_keys(&self, keybinding: String) -> Vec<String> {
        self.inner.get_keys(&keybinding)
    }

    fn get_conflicts_json(&self) -> anyhow::Result<String> {
        let conflicts: Vec<serde_json::Value> = self
            .inner
            .get_conflicts()
            .into_iter()
            .map(|c| serde_json::json!({ "key": c.key, "keybindings": c.keybindings }))
            .collect();
        serde_json::to_string(&conflicts).map_err(anyhow::Error::from)
    }

    fn get_resolved_bindings_json(&self) -> anyhow::Result<String> {
        let resolved: Vec<(String, Vec<String>)> = self.inner.get_resolved_bindings();
        serde_json::to_string(&resolved).map_err(anyhow::Error::from)
    }
}
