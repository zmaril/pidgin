//! Faithful Rust port of pi's `utils/syntax-highlight.ts`.
//!
//! pi's module has two halves. The **pure** half — [`render_highlighted_html`]
//! and its private helpers — is a linear scan over the HTML-string output that
//! highlight.js produces (`<span class="hljs-keyword">…</span>` runs, `&…;`
//! entities). It maintains a scope stack, selects an active theme formatter per
//! text run, and decodes HTML entities via [`super::html`]. That half is ported
//! here byte-for-byte (pi lines ~14-132).
//!
//! The **engine** half — [`highlight`] and [`supports_language`] — is where pi
//! calls into highlight.js (`hljs.highlight` / `hljs.highlightAuto` /
//! `hljs.getLanguage`). highlight.js is a JavaScript library; running the real
//! grammar engine belongs to the deno/V8 plane and is gated behind the `deno`
//! feature. The default (non-deno) build ships a pi-faithful fallback: the
//! engine is unavailable, so [`highlight`] returns the code unhighlighted and
//! [`supports_language`] returns `false`. This mirrors exactly what pi's own
//! caller does — `highlightCode` in
//! `modes/interactive/theme/theme.ts` gates on `supportsLanguage` and wraps
//! `highlight(code, opts)` in `try { … } catch { return code.split("\n"); }`,
//! falling back to raw, unhighlighted lines (theme.ts:1138-1157).

// straitjacket-allow-file:duplication

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use super::html::decode_html_entity_at;

/// A theme formatter: wraps a run of source text (typically in terminal
/// styling escapes) and returns the decorated string. Mirrors pi's
/// `HighlightFormatter = (text: string) => string`.
pub type HighlightFormatter = Box<dyn Fn(&str) -> String>;

/// A theme: a map from highlight.js scope name (the class suffix after
/// `hljs-`) to a [`HighlightFormatter`]. The special key `"default"` is the
/// fallback formatter applied to text with no matching scope. Mirrors pi's
/// `HighlightTheme = Partial<Record<string, HighlightFormatter>>`.
pub type HighlightTheme = HashMap<String, HighlightFormatter>;

/// Options for [`highlight`]. Mirrors pi's `HighlightOptions`.
#[derive(Default)]
pub struct HighlightOptions {
    /// Explicit language grammar. When `None`, pi uses auto-detection.
    pub language: Option<String>,
    /// Passed through to highlight.js; tolerate illegal syntax rather than
    /// bailing out of the grammar.
    pub ignore_illegals: Option<bool>,
    /// Restrict auto-detection to this subset of languages.
    pub language_subset: Option<Vec<String>>,
    /// The theme used to decorate the rendered spans.
    pub theme: Option<HighlightTheme>,
}

const SPAN_CLOSE: &str = "</span>";
const HIGHLIGHT_CLASS_PREFIX: &str = "hljs-";

/// Extract the highlight.js scope (the class suffix after `hljs-`) from a
/// `<span …>` open tag, or `None` if the tag carries no `hljs-` class.
fn get_scope_from_span_tag(tag: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid class-attr regex")
    });

    let captures = re.captures(tag)?;
    let class_value = captures
        .get(1)
        .or_else(|| captures.get(2))
        .map(|m| m.as_str())?;
    if class_value.is_empty() {
        return None;
    }

    for class_name in class_value.split_whitespace() {
        if let Some(scope) = class_name.strip_prefix(HIGHLIGHT_CLASS_PREFIX) {
            return Some(scope.to_string());
        }
    }

    None
}

/// Resolve a scope to a formatter: exact match first, then a `.`-prefix
/// fallback, then a `-`-prefix fallback (e.g. `selector-tag` falls back to the
/// `selector` formatter). Mirrors pi's `getScopeFormatter`.
fn get_scope_formatter<'a>(
    scope: &str,
    theme: &'a HighlightTheme,
) -> Option<&'a HighlightFormatter> {
    if let Some(exact) = theme.get(scope) {
        return Some(exact);
    }

    if let Some(dot_index) = scope.find('.') {
        if let Some(prefix_formatter) = theme.get(&scope[..dot_index]) {
            return Some(prefix_formatter);
        }
    }

    if let Some(dash_index) = scope.find('-') {
        if let Some(prefix_formatter) = theme.get(&scope[..dash_index]) {
            return Some(prefix_formatter);
        }
    }

    None
}

/// Walk the scope stack innermost-first and return the first matching
/// formatter, else the theme's `"default"` formatter. Mirrors pi's
/// `getActiveFormatter`.
fn get_active_formatter<'a>(
    scopes: &[Option<String>],
    theme: &'a HighlightTheme,
) -> Option<&'a HighlightFormatter> {
    for scope in scopes.iter().rev() {
        let Some(scope) = scope else {
            continue;
        };
        if let Some(formatter) = get_scope_formatter(scope, theme) {
            return Some(formatter);
        }
    }
    theme.get("default")
}

/// Whether a `<span` open tag begins at byte `index`: the literal `<span`
/// followed by `>`, space, or ASCII whitespace. Mirrors pi's
/// `isSpanOpenTagStart`.
fn is_span_open_tag_start(html: &str, index: usize) -> bool {
    let bytes = html.as_bytes();
    if !bytes[index..].starts_with(b"<span") {
        return false;
    }
    matches!(
        bytes.get(index + "<span".len()),
        Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
    )
}

/// Flush the buffered text through the active formatter (or verbatim when
/// there is none) and clear the buffer. Mirrors pi's inner `flushText`.
fn flush_text(
    output: &mut String,
    text_buffer: &mut String,
    scopes: &[Option<String>],
    theme: &HighlightTheme,
) {
    if text_buffer.is_empty() {
        return;
    }
    match get_active_formatter(scopes, theme) {
        Some(formatter) => output.push_str(&formatter(text_buffer)),
        None => output.push_str(text_buffer),
    }
    text_buffer.clear();
}

/// Render highlight.js HTML-span output into a themed terminal string.
///
/// Linear scan: `<span …>` open tags push a scope, `</span>` pops one, `&…;`
/// entities are decoded, and every other byte is buffered until the next tag
/// boundary flushes it through the active formatter. Byte-faithful to pi's
/// `renderHighlightedHtml` (syntax-highlight.ts:80-132). `theme` is `None` for
/// pi's default empty theme (verbatim passthrough).
pub fn render_highlighted_html(html: &str, theme: Option<&HighlightTheme>) -> String {
    let empty = HighlightTheme::new();
    let theme = theme.unwrap_or(&empty);

    let mut output = String::new();
    let mut text_buffer = String::new();
    let mut scopes: Vec<Option<String>> = Vec::new();

    let bytes = html.as_bytes();
    let mut index = 0;
    while index < html.len() {
        if is_span_open_tag_start(html, index) {
            if let Some(tag_end_index) = html[index + 5..].find('>').map(|i| index + 5 + i) {
                flush_text(&mut output, &mut text_buffer, &scopes, theme);
                let tag = &html[index..tag_end_index + 1];
                let scope = get_scope_from_span_tag(tag);
                scopes.push(scope);
                index = tag_end_index + 1;
                continue;
            }
        }

        if bytes[index..].starts_with(SPAN_CLOSE.as_bytes()) {
            flush_text(&mut output, &mut text_buffer, &scopes, theme);
            if !scopes.is_empty() {
                scopes.pop();
            }
            index += SPAN_CLOSE.len();
            continue;
        }

        if bytes[index] == b'&' {
            if let Some(decoded) = decode_html_entity_at(html, index) {
                text_buffer.push_str(&decoded.text);
                index += decoded.length;
                continue;
            }
        }

        // Copy one whole UTF-8 character verbatim. hljs output is valid UTF-8
        // and all structural markers above are ASCII, so text runs never split
        // a multibyte character.
        let ch = html[index..].chars().next().expect("valid UTF-8 boundary");
        text_buffer.push(ch);
        index += ch.len_utf8();
    }

    flush_text(&mut output, &mut text_buffer, &scopes, theme);
    output
}

/// Highlight `code` and render it through the options' theme.
///
/// The real highlight.js grammar engine runs in the deno/V8 plane, gated behind
/// the `deno` feature (see the seam below). On the default build the engine is
/// unavailable and this returns `code` unhighlighted — the exact behavior pi's
/// caller falls back to: `highlightCode` (theme.ts:1152-1156) wraps this call in
/// `try { … } catch { return code.split("\n"); }`, yielding raw lines when the
/// engine is not usable.
#[cfg(not(feature = "deno"))]
pub fn highlight(code: &str, _options: &HighlightOptions) -> String {
    code.to_string()
}

/// Highlight `code` and render it through the options' theme.
#[cfg(feature = "deno")]
pub fn highlight(code: &str, options: &HighlightOptions) -> String {
    // SEAM: real highlight.js runs in the deno/V8 plane; loading the pinned
    // hljs single-file dist as a plane asset + the deno-gated render vector is
    // a scheduled separate slice.
    let _ = (code, options);
    unimplemented!(
        "highlight() grammar engine is deno-plane-only; the pinned hljs dist \
         asset + deno-gated render vector is a scheduled separate slice"
    )
}

/// Whether the highlight.js grammar engine recognizes `name` as a language.
///
/// On the default (non-deno) build the engine is unavailable, so this returns
/// `false` — which is pi-faithful: pi's `highlightCode` (theme.ts:1140) gates on
/// `supportsLanguage(lang)` and, when it is falsy, skips highlighting and emits
/// raw lines.
#[cfg(not(feature = "deno"))]
pub fn supports_language(_name: &str) -> bool {
    false
}

/// Whether the highlight.js grammar engine recognizes `name` as a language.
#[cfg(feature = "deno")]
pub fn supports_language(name: &str) -> bool {
    // SEAM: real highlight.js runs in the deno/V8 plane; loading the pinned
    // hljs single-file dist as a plane asset + the deno-gated render vector is
    // a scheduled separate slice.
    let _ = name;
    unimplemented!(
        "supports_language() grammar engine is deno-plane-only; the pinned hljs \
         dist asset + deno-gated render vector is a scheduled separate slice"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures below are the verbatim HTML-string output of highlight.js
    // v10.7.3 (`hljs.highlight(code, { language }).value`), captured by running
    // the library under Node against representative snippets. Each `HTML:` line
    // is the exact `.value` string; they exercise nested scopes, unmapped
    // scopes, entity decoding, and the `.`/`-`-prefix formatter fallbacks.

    /// Build a theme whose formatters wrap text in `[scope:…]` sentinels so
    /// placement is asserted exactly. Deliberately has no `"default"` key, so
    /// unscoped text passes through verbatim (see [`default_formatter_applies_to_unscoped_text`]
    /// for the default-formatter path).
    fn sentinel_theme() -> HighlightTheme {
        let mut theme: HighlightTheme = HashMap::new();
        for scope in ["keyword", "string", "comment", "selector"] {
            let name = scope.to_string();
            theme.insert(
                name.clone(),
                Box::new(move |text: &str| format!("[{name}:{text}]")),
            );
        }
        theme
    }

    #[test]
    fn passthrough_without_theme() {
        // `const x = "a & b < c";` (language=javascript).
        let html = "<span class=\"hljs-keyword\">const</span> x = \
                    <span class=\"hljs-string\">&quot;a &amp; b &lt; c&quot;</span>;";
        // No theme: spans are stripped, entities decoded, text emitted verbatim.
        assert_eq!(
            render_highlighted_html(html, None),
            "const x = \"a & b < c\";"
        );
    }

    #[test]
    fn applies_theme_with_entity_decoding() {
        // Same JS fixture as above, now with the sentinel theme.
        let html = "<span class=\"hljs-keyword\">const</span> x = \
                    <span class=\"hljs-string\">&quot;a &amp; b &lt; c&quot;</span>;";
        let theme = sentinel_theme();
        assert_eq!(
            render_highlighted_html(html, Some(&theme)),
            "[keyword:const] x = [string:\"a & b < c\"];"
        );
    }

    #[test]
    fn nested_scopes_use_innermost_formatter() {
        // `function f(a) { return a; }` (language=javascript): the outer
        // hljs-function span wraps nested hljs-keyword/hljs-title/hljs-params.
        let html = "<span class=\"hljs-function\"><span class=\"hljs-keyword\">function</span> \
                    <span class=\"hljs-title\">f</span>(<span class=\"hljs-params\">a</span>) </span>\
                    { <span class=\"hljs-keyword\">return</span> a; }";
        let theme = sentinel_theme();
        // `function` -> keyword (innermost); `f`, `(`, `a`, `)` have no mapped
        // scope (function/title/params unmapped) and no default here, so verbatim.
        assert_eq!(
            render_highlighted_html(html, Some(&theme)),
            "[keyword:function] f(a) { [keyword:return] a; }"
        );
    }

    #[test]
    fn unmapped_scope_falls_through_to_raw() {
        // `# hi\nprint("x")` (language=python): hljs-comment is mapped,
        // hljs-built_in and hljs-string are not (no `built_in`/`string` here).
        let html = "<span class=\"hljs-comment\"># hi</span>\n\
                    <span class=\"hljs-built_in\">print</span>\
                    (<span class=\"hljs-string\">&quot;x&quot;</span>)";
        let mut theme: HighlightTheme = HashMap::new();
        theme.insert(
            "comment".to_string(),
            Box::new(|text: &str| format!("[comment:{text}]")),
        );
        // Only the comment is decorated; built_in/string text passes through raw.
        assert_eq!(
            render_highlighted_html(html, Some(&theme)),
            "[comment:# hi]\nprint(\"x\")"
        );
    }

    #[test]
    fn dash_prefix_formatter_fallback() {
        // `a.cls { color: red; }` (language=css): hljs-selector-tag and
        // hljs-selector-class fall back to the `selector` formatter via the
        // `-`-prefix rule; hljs-attribute is unmapped.
        let html = "<span class=\"hljs-selector-tag\">a</span>\
                    <span class=\"hljs-selector-class\">.cls</span> { \
                    <span class=\"hljs-attribute\">color</span>: red; }";
        let theme = sentinel_theme();
        assert_eq!(
            render_highlighted_html(html, Some(&theme)),
            "[selector:a][selector:.cls] { color: red; }"
        );
    }

    #[test]
    fn dot_prefix_formatter_fallback() {
        // Synthetic hljs-style span whose scope is `meta.keyword`; there is no
        // exact `meta.keyword` formatter, so the `.`-prefix rule resolves it to
        // the `meta` formatter.
        let html = "<span class=\"hljs-meta.keyword\">use</span> x";
        let mut theme: HighlightTheme = HashMap::new();
        theme.insert(
            "meta".to_string(),
            Box::new(|text: &str| format!("[meta:{text}]")),
        );
        assert_eq!(render_highlighted_html(html, Some(&theme)), "[meta:use] x");
    }

    #[test]
    fn default_formatter_applies_to_unscoped_text() {
        let html = "plain <span class=\"hljs-keyword\">kw</span> text";
        let mut theme = sentinel_theme();
        theme.insert(
            "default".to_string(),
            Box::new(|text: &str| format!("[default:{text}]")),
        );
        // `plain ` and ` text` fall to the `default` formatter; `kw` -> keyword.
        assert_eq!(
            render_highlighted_html(html, Some(&theme)),
            "[default:plain ][keyword:kw][default: text]"
        );
    }

    #[test]
    #[cfg(not(feature = "deno"))]
    fn engine_fallback_is_pi_faithful() {
        // Default (non-deno) build: engine unavailable -> caller-faithful raw
        // fallback (theme.ts highlightCode try/catch).
        let opts = HighlightOptions {
            language: Some("javascript".to_string()),
            ..HighlightOptions::default()
        };
        assert_eq!(highlight("const x = 1;", &opts), "const x = 1;");
        assert!(!supports_language("javascript"));
    }
}
