// straitjacket-allow-file:color — theme color values appear in doc-tests and assertions here.
//! HTML transcript export.
//!
//! Ported from pi's `core/export-html` module. The server side of the export is
//! pure: it computes theme colors, substitutes placeholders in a bundled
//! template, and embeds the session data as base64-encoded JSON. Markdown parsing
//! and syntax highlighting run entirely client-side in the reader's browser, via
//! the vendored `marked` and `highlight.js` libraries embedded verbatim. The only
//! HTML this module generates server-side is pi's own deterministic
//! [`ansi_to_html`] conversion and the base64 JSON embed.
//!
//! pi's `exportSessionToHtml(SessionManager, ...)` and `exportFromFile(path, ...)`
//! wrappers resolve a session from a `SessionManager` and derive a default output
//! filename from `APP_NAME`. `SessionManager` is owned by a sibling crate and is
//! not yet on main, so those wrappers are deferred. This module provides the
//! `SessionData`-level entry points ([`generate_html`], [`assemble_session_data`],
//! and [`export_session_data_to_html`]); the `SessionManager`-backed wrappers
//! migrate here once session-manager lands.

pub mod ansi_to_html;
pub mod theme;
pub mod tool_renderer;
pub mod types;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use std::path::PathBuf;

pub use theme::{ThemeExportColors, ThemeInputs};
pub use tool_renderer::{
    pre_render_custom_tools, RenderedToolResult, ToolHtmlRenderer, TEMPLATE_RENDERED_TOOLS,
};
pub use types::{
    AgentMessage, ContentBlock, EntryBase, EntryHookMeta, MessageContent, MessageEntry,
    RenderedToolHtml, SessionData, SessionEntry, SessionHeader, ToolInfo, ToolResultMessage,
};

/// Template and third-party assets, embedded verbatim. The `marked` and
/// `highlight.js` bundles keep their upstream license banners (MIT and
/// BSD-3-Clause) intact.
const TEMPLATE_HTML: &str = include_str!("assets/template.html");
const TEMPLATE_CSS: &str = include_str!("assets/template.css");
const TEMPLATE_JS: &str = include_str!("assets/template.js");
const MARKED_JS: &str = include_str!("assets/vendor/marked.min.js");
const HIGHLIGHT_JS: &str = include_str!("assets/vendor/highlight.min.js");

/// Inputs for building a [`SessionData`] payload. Mirrors the fields pi collects
/// in `exportSessionToHtml` before pre-rendering custom tools.
pub struct SessionDataInputs {
    pub header: Option<SessionHeader>,
    pub entries: Vec<SessionEntry>,
    pub leaf_id: Option<String>,
    pub system_prompt: Option<String>,
    pub tools: Option<Vec<ToolInfo>>,
}

/// Options for writing an export to disk.
///
/// pi's `ExportOptions` also carries a `themeName` and an optional `toolRenderer`.
/// Theme resolution is decoupled here (the resolved colors are passed via
/// [`ThemeInputs`]), and the tool renderer is applied during
/// [`assemble_session_data`], so this struct only needs the destination and the
/// theme inputs.
pub struct ExportOptions {
    pub output_path: PathBuf,
    pub theme_inputs: ThemeInputs,
}

/// Assemble a [`SessionData`] payload, optionally pre-rendering custom tools with
/// an injected renderer.
///
/// Mirrors pi's `SessionData` construction: when a tool renderer is provided, its
/// output is collected via [`pre_render_custom_tools`] and attached only if it
/// rendered at least one tool (otherwise `renderedTools` is omitted).
pub fn assemble_session_data(
    inputs: SessionDataInputs,
    tool_renderer: Option<&dyn ToolHtmlRenderer>,
) -> SessionData {
    let rendered_tools = tool_renderer.and_then(|renderer| {
        let map = pre_render_custom_tools(&inputs.entries, renderer);
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    });

    SessionData {
        header: inputs.header,
        entries: inputs.entries,
        leaf_id: inputs.leaf_id,
        system_prompt: inputs.system_prompt,
        tools: inputs.tools,
        rendered_tools,
    }
}

/// Core HTML generation: substitute theme colors and assets into the template and
/// embed the session data as base64 JSON.
///
/// The placeholder substitutions replace only the first occurrence of each token,
/// in the same order as pi, so the output is byte-for-byte identical.
pub fn generate_html(session_data: &SessionData, theme_inputs: &ThemeInputs) -> String {
    let theme_vars = theme::generate_theme_vars(theme_inputs);
    let backgrounds = theme::resolve_export_backgrounds(theme_inputs);

    // Base64-encode session data to avoid escaping issues. Standard alphabet with
    // padding, matching Node's `Buffer.toString("base64")`.
    let session_json = serde_json::to_string(session_data).expect("SessionData serializes to JSON");
    let session_data_base64 = BASE64_STANDARD.encode(session_json.as_bytes());

    // Build the CSS with theme variables injected.
    let css = TEMPLATE_CSS
        .replacen("{{THEME_VARS}}", &theme_vars, 1)
        .replacen("{{BODY_BG}}", &backgrounds.page_bg, 1)
        .replacen("{{CONTAINER_BG}}", &backgrounds.card_bg, 1)
        .replacen("{{INFO_BG}}", &backgrounds.info_bg, 1);

    TEMPLATE_HTML
        .replacen("{{CSS}}", &css, 1)
        .replacen("{{JS}}", TEMPLATE_JS, 1)
        .replacen("{{SESSION_DATA}}", &session_data_base64, 1)
        .replacen("{{MARKED_JS}}", MARKED_JS, 1)
        .replacen("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS, 1)
}

/// Generate the export HTML for a session and write it to `options.output_path`.
///
/// This is the `SessionData`-level entry point. The `SessionManager`-backed
/// wrappers (default filename derivation, in-memory-session guards) are deferred
/// until session-manager lands.
pub fn export_session_data_to_html(
    session_data: &SessionData,
    options: &ExportOptions,
) -> std::io::Result<PathBuf> {
    let html = generate_html(session_data, &options.theme_inputs);
    std::fs::write(&options.output_path, html)?;
    Ok(options.output_path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn matches(pattern: &str, haystack: &str) -> bool {
        Regex::new(pattern).expect("valid regex").is_match(haystack)
    }

    // Ported from export-html-xss.test.ts. These assert against the embedded
    // template.js source text; since template.js ships verbatim, they mirror pi's
    // markdown link/image sanitization and attribute-escaping guarantees.
    #[test]
    fn xss_link_renderer_uses_scheme_allow_list() {
        assert!(matches(r"link\s*\(\s*token\s*\)", TEMPLATE_JS));
        assert!(matches(r"sanitizeMarkdownUrl\(token\.href\)", TEMPLATE_JS));
        assert!(matches(r"\^\(https\?\|mailto\|tel\|ftp\)", TEMPLATE_JS));
    }

    #[test]
    fn xss_image_renderer_uses_scheme_allow_list() {
        assert!(matches(r"image\s*\(\s*token\s*\)", TEMPLATE_JS));
        assert!(matches(r"sanitizeMarkdownUrl\(token\.href\)", TEMPLATE_JS));
    }

    #[test]
    fn xss_strips_c0_controls() {
        assert!(TEMPLATE_JS.contains(r"replace(/[\x00-\x1f\x7f]/g, '')"));
        assert!(!matches(
            r"(?i)\^\\s\*\(javascript\|vbscript\|data\):",
            TEMPLATE_JS
        ));
    }

    #[test]
    fn xss_escapes_href_in_link_renderer() {
        assert!(matches(r"escapeHtml\(href\)", TEMPLATE_JS));
    }

    #[test]
    fn xss_escapes_image_mimetype() {
        assert!(!matches(r"\$\{img\.mimeType\}", TEMPLATE_JS));
        assert!(matches(r"escapeHtml\(img\.mimeType", TEMPLATE_JS));
    }

    #[test]
    fn xss_escapes_image_data() {
        assert!(!matches(r#";base64,\$\{img\.data\}""#, TEMPLATE_JS));
        assert!(matches(
            r#";base64,\$\{escapeHtml\(img\.data \|\| (?:''|"")\)\}""#,
            TEMPLATE_JS
        ));
    }

    #[test]
    fn xss_escapes_entry_ids() {
        assert!(!matches(r#"id="\$\{entryId\}""#, TEMPLATE_JS));
        assert!(!matches(r#"data-entry-id="\$\{entryId\}""#, TEMPLATE_JS));
        assert!(matches(r"entry-\$\{escapeHtml\(entry\.id\)\}", TEMPLATE_JS));
        assert!(matches(
            r#"data-entry-id="\$\{escapeHtml\(entryId\)\}""#,
            TEMPLATE_JS
        ));
    }

    #[test]
    fn xss_escapes_tree_metadata() {
        assert!(!matches(
            r"\[\$\{msg\.toolName \|\| 'tool'\}\]",
            TEMPLATE_JS
        ));
        assert!(!matches(r"\[\$\{msg\.role\}\]", TEMPLATE_JS));
        assert!(!matches(r"\[model: \$\{entry\.modelId\}\]", TEMPLATE_JS));
        assert!(!matches(
            r"\[thinking: \$\{entry\.thinkingLevel\}\]",
            TEMPLATE_JS
        ));
        assert!(!matches(r"\[\$\{entry\.type\}\]", TEMPLATE_JS));
        assert!(matches(
            r"\$\{escapeHtml\(msg\.toolName \|\| 'tool'\)\}",
            TEMPLATE_JS
        ));
        assert!(matches(r"\$\{escapeHtml\(msg\.role\)\}", TEMPLATE_JS));
        assert!(matches(r"\$\{escapeHtml\(entry\.modelId\)\}", TEMPLATE_JS));
        assert!(matches(
            r"\$\{escapeHtml\(entry\.thinkingLevel\)\}",
            TEMPLATE_JS
        ));
        assert!(matches(r"\$\{escapeHtml\(entry\.type\)\}", TEMPLATE_JS));
    }

    #[test]
    fn xss_escapes_model_names_in_header() {
        assert!(!matches(
            r"\$\{globalStats\.models\.join\(', '\) \|\| 'unknown'\}",
            TEMPLATE_JS
        ));
        assert!(matches(
            r"\$\{escapeHtml\(globalStats\.models\.join\(', '\) \|\| 'unknown'\)\}",
            TEMPLATE_JS
        ));
    }

    // Ported from export-html-skill-block.test.ts.
    #[test]
    fn skill_block_strips_wrapper_and_renders_prompt() {
        assert!(matches(r"parseSkillBlock", TEMPLATE_JS));
        assert!(matches(r"skillBlock\.userMessage", TEMPLATE_JS));
    }

    #[test]
    fn skill_block_renders_invocation_and_message_as_siblings() {
        assert!(matches(r"skill-invocation", TEMPLATE_JS));
        assert!(matches(r"hasUserContent", TEMPLATE_JS));
    }

    #[test]
    fn skill_block_renders_content_as_markdown() {
        assert!(matches(
            r"safeMarkedParse\(skillBlock\.content\)",
            TEMPLATE_JS
        ));
    }

    #[test]
    fn skill_block_shows_name_in_tree() {
        assert!(matches(r"tree-role-skill", TEMPLATE_JS));
    }

    // Ported from export-html-whitespace.test.ts CSS source assertions.
    #[test]
    fn css_preserves_plain_text_whitespace_without_template_whitespace() {
        assert!(matches(
            r"\.output-preview > div:not\(\.expand-hint\),\s*\.output-full > div:not\(\.expand-hint\) \{[\s\S]*?white-space:\s*pre-wrap;",
            TEMPLATE_CSS
        ));
        assert!(matches(
            r"\.ansi-line\s*\{[\s\S]*?white-space:\s*pre;",
            TEMPLATE_CSS
        ));
        assert!(!matches(
            r"\.output-preview,\s*\.output-full\s*\{[\s\S]*?white-space:\s*pre-wrap;",
            TEMPLATE_CSS
        ));
    }

    #[test]
    fn generate_html_produces_complete_document() {
        let session_data = assemble_session_data(
            SessionDataInputs {
                header: Some(SessionHeader::new("s1", "2026-01-01T00:00:00Z", "/tmp")),
                entries: vec![types::user_message_entry("e1", "hello")],
                leaf_id: Some("e1".to_string()),
                system_prompt: None,
                tools: None,
            },
            None,
        );

        let theme_inputs = ThemeInputs {
            resolved_colors: vec![("userMessageBg".to_string(), "#343541".to_string())],
            export_colors: ThemeExportColors::default(),
        };

        let html = generate_html(&session_data, &theme_inputs);

        // The base64 session-data script tag is present and decodes to the session.
        assert!(html.contains(r#"<script id="session-data" type="application/json">"#));
        let start = html.find("application/json\">").unwrap() + "application/json\">".len();
        let end = html[start..].find("</script>").unwrap() + start;
        let encoded = &html[start..end];
        let decoded = BASE64_STANDARD.decode(encoded).expect("valid base64");
        let decoded = String::from_utf8(decoded).expect("valid utf-8");
        assert!(decoded.contains("\"header\""));
        assert!(decoded.contains("\"hello\""));

        // The vendored and template JS are embedded.
        assert!(html.contains("marked v18.0.5"));
        assert!(html.contains("Highlight.js v11.9.0"));
        assert!(html.contains("sanitizeMarkdownUrl"));

        // The theme CSS was substituted: variables and derived backgrounds present,
        // no placeholders left behind.
        assert!(html.contains("--userMessageBg: #343541;"));
        assert!(html.contains("--exportPageBg: rgb(36, 37, 46);"));
        assert!(html.contains("--body-bg: rgb(36, 37, 46);"));
        for placeholder in [
            "{{CSS}}",
            "{{JS}}",
            "{{SESSION_DATA}}",
            "{{MARKED_JS}}",
            "{{HIGHLIGHT_JS}}",
            "{{THEME_VARS}}",
            "{{BODY_BG}}",
            "{{CONTAINER_BG}}",
            "{{INFO_BG}}",
        ] {
            assert!(
                !html.contains(placeholder),
                "leftover placeholder: {placeholder}"
            );
        }
    }
}
