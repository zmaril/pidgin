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
