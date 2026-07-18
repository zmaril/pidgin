// straitjacket-allow-file:duplication — `render_inline_tokens` mirrors pi's
// `renderInlineTokens` switch verbatim (strong/em/codespan/del arms each render
// children then re-append the style prefix); a faithful parallel structure.
//! Byte-exact Rust port of pi's Markdown terminal renderer
//! (`vendor/pi/packages/tui/src/components/markdown.ts`).
//!
//! `renderToken` / `renderInlineTokens` / `renderList` / `renderTable` are
//! ported verbatim (mechanical 1:1) against the marked-shaped token tree
//! produced by [`lexer`]. Styling is delegated to caller-supplied theme
//! closures exactly as in pi (the "chalk contract"); the renderer only chooses
//! which closure to call and where to re-inject style prefixes after ANSI
//! resets. Correctness == byte-identical output vs pi, validated against the 70
//! vectors extracted from `markdown.test.ts`.

mod inline;
mod lexer;
mod tables;

use crate::renderer::is_image_line;
use crate::terminal_image::hyperlink;
use crate::text_util::apply_background_to_line;
use crate::width::{visible_width, wrap_text_with_ansi};
use lexer::{Kind, Lexer, Token};

/// A caller-supplied styling function (chalk-equivalent): `text -> styled`.
pub type StyleFn = Box<dyn Fn(&str) -> String>;

/// Theme functions for markdown elements (mirrors pi's `MarkdownTheme`).
pub struct MarkdownTheme {
    pub heading: StyleFn,
    pub link: StyleFn,
    pub link_url: StyleFn,
    pub code: StyleFn,
    pub code_block: StyleFn,
    pub code_block_border: StyleFn,
    pub quote: StyleFn,
    pub quote_border: StyleFn,
    pub hr: StyleFn,
    pub list_bullet: StyleFn,
    pub bold: StyleFn,
    pub italic: StyleFn,
    pub strikethrough: StyleFn,
    pub underline: StyleFn,
    #[allow(clippy::type_complexity)]
    pub highlight_code: Option<Box<dyn Fn(&str, Option<&str>) -> Vec<String>>>,
    pub code_block_indent: Option<String>,
}

/// Default text styling applied to all content unless overridden.
#[derive(Default)]
pub struct DefaultTextStyle {
    pub color: Option<StyleFn>,
    pub bg_color: Option<StyleFn>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

/// Renderer options (mirrors pi's `MarkdownOptions`).
#[derive(Default, Clone)]
pub struct MarkdownOptions {
    pub preserve_ordered_list_markers: bool,
    pub preserve_backslash_escapes: bool,
}

/// The style context threaded through inline rendering. Mirrors pi's
/// `InlineStyleContext` variants (default / heading / blockquote).
#[derive(Clone, Copy)]
enum StyleCtx {
    Default,
    Heading(usize),
    Quote,
}

const SENTINEL: &str = "\u{0}";

/// pi's `Markdown` component.
pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    theme: MarkdownTheme,
    default_text_style: Option<DefaultTextStyle>,
    options: MarkdownOptions,
    hyperlinks: bool,
}

impl Markdown {
    pub fn new(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: Option<MarkdownOptions>,
    ) -> Self {
        Markdown {
            text: text.into(),
            padding_x,
            padding_y,
            theme,
            default_text_style,
            options: options.unwrap_or_default(),
            hyperlinks: false,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    /// Replaces pi's global `getCapabilities().hyperlinks` seam. Off by default,
    /// matching an unknown-terminal capability probe.
    pub fn set_hyperlinks(&mut self, enabled: bool) {
        self.hyperlinks = enabled;
    }

    /// Port of pi's `render(width)`.
    pub fn render(&self, width: usize) -> Vec<String> {
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);

        if self.text.is_empty() || self.text.trim().is_empty() {
            return Vec::new();
        }

        let normalized = self.text.replace('\t', "   ");
        let mut lexer = Lexer::new();
        let mut tokens = lexer.lex(&normalized);
        trim_partial_closing_fences(&mut tokens);

        let mut rendered_lines: Vec<String> = Vec::new();
        for i in 0..tokens.len() {
            let next_type = tokens.get(i + 1).map(|t| t.kind);
            let lines = self.render_token(&tokens[i], content_width, next_type, StyleCtx::Default);
            rendered_lines.extend(lines);
        }

        // Wrap (no padding, no background yet).
        let mut wrapped: Vec<String> = Vec::new();
        for line in &rendered_lines {
            if is_image_line(line) {
                wrapped.push(line.clone());
            } else {
                wrapped.extend(wrap_text_with_ansi(line, content_width));
            }
        }

        // Margins + background/padding.
        let left = " ".repeat(self.padding_x);
        let right = " ".repeat(self.padding_x);
        let bg = self
            .default_text_style
            .as_ref()
            .and_then(|s| s.bg_color.as_ref());
        let mut content_lines: Vec<String> = Vec::new();
        for line in &wrapped {
            if is_image_line(line) {
                content_lines.push(line.clone());
                continue;
            }
            let with_margins = format!("{left}{line}{right}");
            if let Some(bgf) = bg {
                content_lines.push(apply_background_to_line(&with_margins, width, bgf));
            } else {
                let visible_len = visible_width(&with_margins);
                let pad = width.saturating_sub(visible_len);
                content_lines.push(format!("{with_margins}{}", " ".repeat(pad)));
            }
        }

        let empty_line = " ".repeat(width);
        let mut empties: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            if let Some(bgf) = bg {
                empties.push(apply_background_to_line(&empty_line, width, bgf));
            } else {
                empties.push(empty_line.clone());
            }
        }

        let mut result = Vec::new();
        result.extend(empties.iter().cloned());
        result.extend(content_lines);
        result.extend(empties);

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    // ---- styling helpers ----

    fn apply_default_style(&self, text: &str) -> String {
        let style = match &self.default_text_style {
            Some(s) => s,
            None => return text.to_string(),
        };
        let mut styled = text.to_string();
        if let Some(color) = &style.color {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        styled
    }

    fn heading_style(&self, level: usize, text: &str) -> String {
        if level == 1 {
            (self.theme.heading)(&(self.theme.bold)(&(self.theme.underline)(text)))
        } else {
            (self.theme.heading)(&(self.theme.bold)(text))
        }
    }

    fn quote_style(&self, text: &str) -> String {
        (self.theme.quote)(&(self.theme.italic)(text))
    }

    /// pi's `getStylePrefix`: apply `style_fn` to a sentinel and return the ANSI
    /// prefix emitted before it.
    fn style_prefix_of(&self, styled_sentinel: &str) -> String {
        match styled_sentinel.find(SENTINEL) {
            Some(idx) => styled_sentinel[..idx].to_string(),
            None => String::new(),
        }
    }

    fn apply_text(&self, ctx: StyleCtx, text: &str) -> String {
        match ctx {
            StyleCtx::Default => self.apply_default_style(text),
            StyleCtx::Heading(level) => self.heading_style(level, text),
            StyleCtx::Quote => text.to_string(),
        }
    }

    fn style_prefix(&self, ctx: StyleCtx) -> String {
        let styled = match ctx {
            StyleCtx::Default => self.apply_default_style(SENTINEL),
            StyleCtx::Heading(level) => self.heading_style(level, SENTINEL),
            StyleCtx::Quote => self.quote_style(SENTINEL),
        };
        self.style_prefix_of(&styled)
    }

    // ---- token rendering ----

    fn render_token(
        &self,
        token: &Token,
        width: usize,
        next_type: Option<Kind>,
        ctx: StyleCtx,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        match token.kind {
            Kind::Heading => {
                let level = token.depth;
                let prefix = format!("{} ", "#".repeat(level));
                let hctx = StyleCtx::Heading(level);
                let heading_text = self.render_inline_tokens(&token.tokens, hctx);
                let styled = if level >= 3 {
                    format!("{}{}", self.heading_style(level, &prefix), heading_text)
                } else {
                    heading_text
                };
                lines.push(styled);
                if let Some(nt) = next_type {
                    if nt != Kind::Space {
                        lines.push(String::new());
                    }
                }
            }
            Kind::Paragraph => {
                let text = self.render_inline_tokens(&token.tokens, ctx);
                lines.push(text);
                if let Some(nt) = next_type {
                    if nt != Kind::List && nt != Kind::Space {
                        lines.push(String::new());
                    }
                }
            }
            Kind::Text => {
                lines.push(self.render_inline_tokens(std::slice::from_ref(token), ctx));
            }
            Kind::Code => {
                let indent = self
                    .theme
                    .code_block_indent
                    .clone()
                    .unwrap_or_else(|| "  ".to_string());
                let lang = token.lang.clone().unwrap_or_default();
                lines.push((self.theme.code_block_border)(&format!("```{lang}")));
                if let Some(hl) = &self.theme.highlight_code {
                    let lang_opt = token.lang.as_deref().filter(|s| !s.is_empty());
                    for hl_line in hl(&token.text, lang_opt) {
                        lines.push(format!("{indent}{hl_line}"));
                    }
                } else {
                    for code_line in token.text.split('\n') {
                        lines.push(format!("{indent}{}", (self.theme.code_block)(code_line)));
                    }
                }
                lines.push((self.theme.code_block_border)("```"));
                if let Some(nt) = next_type {
                    if nt != Kind::Space {
                        lines.push(String::new());
                    }
                }
            }
            Kind::List => {
                lines.extend(self.render_list(token, 0, width, ctx));
            }
            Kind::Table => {
                lines.extend(self.render_table(token, width, next_type, ctx));
            }
            Kind::Blockquote => {
                lines.extend(self.render_blockquote(token, width, next_type));
            }
            Kind::Hr => {
                lines.push((self.theme.hr)(&"─".repeat(width.min(80))));
                if let Some(nt) = next_type {
                    if nt != Kind::Space {
                        lines.push(String::new());
                    }
                }
            }
            Kind::Html => {
                lines.push(self.apply_default_style(token.raw.trim()));
            }
            Kind::Space => {
                lines.push(String::new());
            }
            Kind::Checkbox => {
                // marked's checkbox token has no `text` field, so renderToken's
                // default branch emits nothing for it.
            }
            _ => {
                if !token.text.is_empty() {
                    lines.push(token.text.clone());
                }
            }
        }
        lines
    }

    fn render_blockquote(
        &self,
        token: &Token,
        width: usize,
        next_type: Option<Kind>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let quote_prefix = self.style_prefix(StyleCtx::Quote);
        let apply_quote_style = |line: &str| -> String {
            if quote_prefix.is_empty() {
                self.quote_style(line)
            } else {
                let reapplied = line.replace("\x1b[0m", &format!("\x1b[0m{quote_prefix}"));
                self.quote_style(&reapplied)
            }
        };

        let quote_content_width = width.saturating_sub(2).max(1);
        let mut rendered: Vec<String> = Vec::new();
        for i in 0..token.tokens.len() {
            let next = token.tokens.get(i + 1).map(|t| t.kind);
            rendered.extend(self.render_token(
                &token.tokens[i],
                quote_content_width,
                next,
                StyleCtx::Quote,
            ));
        }
        while rendered.last().map(|s| s.is_empty()).unwrap_or(false) {
            rendered.pop();
        }

        for quote_line in &rendered {
            let styled = apply_quote_style(quote_line);
            for wrapped in wrap_text_with_ansi(&styled, quote_content_width) {
                lines.push(format!("{}{}", (self.theme.quote_border)("│ "), wrapped));
            }
        }
        if let Some(nt) = next_type {
            if nt != Kind::Space {
                lines.push(String::new());
            }
        }
        lines
    }

    fn render_inline_tokens(&self, tokens: &[Token], ctx: StyleCtx) -> String {
        let mut result = String::new();
        let style_prefix = self.style_prefix(ctx);
        let apply_with_newlines = |text: &str| -> String {
            text.split('\n')
                .map(|seg| self.apply_text(ctx, seg))
                .collect::<Vec<_>>()
                .join("\n")
        };

        for token in tokens {
            match token.kind {
                Kind::Escape => {
                    let src = if self.options.preserve_backslash_escapes {
                        &token.raw
                    } else {
                        &token.text
                    };
                    result.push_str(&apply_with_newlines(src));
                }
                Kind::Text => {
                    if !token.tokens.is_empty() {
                        result.push_str(&self.render_inline_tokens(&token.tokens, ctx));
                    } else {
                        result.push_str(&apply_with_newlines(&token.text));
                    }
                }
                Kind::Paragraph => {
                    result.push_str(&self.render_inline_tokens(&token.tokens, ctx));
                }
                Kind::Strong => {
                    let content = self.render_inline_tokens(&token.tokens, ctx);
                    result.push_str(&(self.theme.bold)(&content));
                    result.push_str(&style_prefix);
                }
                Kind::Em => {
                    let content = self.render_inline_tokens(&token.tokens, ctx);
                    result.push_str(&(self.theme.italic)(&content));
                    result.push_str(&style_prefix);
                }
                Kind::Codespan => {
                    result.push_str(&(self.theme.code)(&token.text));
                    result.push_str(&style_prefix);
                }
                Kind::Link => {
                    let link_text = self.render_inline_tokens(&token.tokens, ctx);
                    let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                    if self.hyperlinks {
                        result.push_str(&hyperlink(&styled_link, &token.href));
                        result.push_str(&style_prefix);
                    } else {
                        let href_cmp = if let Some(rest) = token.href.strip_prefix("mailto:") {
                            rest.to_string()
                        } else {
                            token.href.clone()
                        };
                        if token.text == token.href || token.text == href_cmp {
                            result.push_str(&styled_link);
                            result.push_str(&style_prefix);
                        } else {
                            result.push_str(&styled_link);
                            result.push_str(&(self.theme.link_url)(&format!(" ({})", token.href)));
                            result.push_str(&style_prefix);
                        }
                    }
                }
                Kind::Br => {
                    result.push('\n');
                }
                Kind::Del => {
                    let content = self.render_inline_tokens(&token.tokens, ctx);
                    result.push_str(&(self.theme.strikethrough)(&content));
                    result.push_str(&style_prefix);
                }
                Kind::Html => {
                    result.push_str(&apply_with_newlines(&token.raw));
                }
                _ => {
                    if !token.text.is_empty() {
                        result.push_str(&apply_with_newlines(&token.text));
                    }
                }
            }
        }

        while !style_prefix.is_empty() && result.ends_with(&style_prefix) {
            result.truncate(result.len() - style_prefix.len());
        }
        result
    }

    fn ordered_list_marker(&self, item: &Token) -> Option<String> {
        let re =
            fancy_regex::Regex::new(r"^(?: {0,3})(\d{1,9}[.)])[ \t]+").expect("ordered marker re");
        match re.captures(&item.raw).ok().flatten() {
            Some(c) => c.get(1).map(|m| format!("{} ", m.as_str())),
            None => None,
        }
    }

    fn unordered_list_marker(&self, item: &Token) -> Option<String> {
        let re = fancy_regex::Regex::new(r"^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|$))")
            .expect("unordered marker re");
        match re.captures(&item.raw).ok().flatten() {
            Some(c) => c.get(1).map(|m| format!("{} ", m.as_str())),
            None => None,
        }
    }

    fn render_list(&self, token: &Token, depth: usize, width: usize, ctx: StyleCtx) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth);
        let start_number = token.start.unwrap_or(1);

        for (i, item) in token.items.iter().enumerate() {
            let is_last = i == token.items.len() - 1;
            let bullet = if token.ordered {
                if self.options.preserve_ordered_list_markers {
                    self.ordered_list_marker(item)
                        .unwrap_or_else(|| format!("{}. ", start_number + i as i64))
                } else {
                    format!("{}. ", start_number + i as i64)
                }
            } else if self.options.preserve_ordered_list_markers {
                self.unordered_list_marker(item)
                    .unwrap_or_else(|| "- ".to_string())
            } else {
                "- ".to_string()
            };
            let task_marker = if item.task {
                format!("[{}] ", if item.checked { "x" } else { " " })
            } else {
                String::new()
            };
            let marker = format!("{bullet}{task_marker}");
            let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker));
            let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker)));
            let item_width = width.saturating_sub(visible_width(&first_prefix)).max(1);
            let mut rendered_any = false;

            for item_token in &item.tokens {
                if item_token.kind == Kind::List {
                    lines.extend(self.render_list(item_token, depth + 1, width, ctx));
                    rendered_any = true;
                    continue;
                }
                let item_lines = self.render_token(item_token, item_width, None, ctx);
                for line in item_lines {
                    for wrapped in wrap_text_with_ansi(&line, item_width) {
                        let prefix = if rendered_any {
                            &continuation_prefix
                        } else {
                            &first_prefix
                        };
                        lines.push(format!("{prefix}{wrapped}"));
                        rendered_any = true;
                    }
                }
            }

            if !rendered_any {
                lines.push(first_prefix.clone());
            }

            if token.loose && !is_last {
                lines.push(String::new());
            }
        }
        lines
    }

    // table rendering lives in `tables.rs` (impl Markdown)
}

/// pi's `trimPartialClosingFences`: trim streamed partial closing fences so code
/// blocks do not shrink/flicker when the final fence char arrives.
fn trim_partial_closing_fences(tokens: &mut [Token]) {
    let last = match tokens.last_mut() {
        Some(t) => t,
        None => return,
    };
    match last.kind {
        Kind::List => {
            if let Some(item) = last.items.last_mut() {
                trim_partial_closing_fences(&mut item.tokens);
            }
            return;
        }
        Kind::Blockquote => {
            trim_partial_closing_fences(&mut last.tokens);
            return;
        }
        Kind::Code => {}
        _ => return,
    }

    let marker_re = fancy_regex::Regex::new(r"^(`{3,}|~{3,})").expect("fence marker re");
    let marker = match marker_re.captures(&last.raw).ok().flatten() {
        Some(c) => c.get(1).map(|m| m.as_str().to_string()),
        None => None,
    };
    let marker = match marker {
        Some(m) => m,
        None => return,
    };
    let last_line = match last.raw.split('\n').next_back() {
        Some(l) => l.to_string(),
        None => return,
    };
    let marker_first = marker.chars().next().unwrap_or('`');
    let repeated: String = std::iter::repeat_n(marker_first, last_line.chars().count()).collect();
    if last_line.is_empty()
        || last_line.chars().count() >= marker.chars().count()
        || last_line != repeated
    {
        return;
    }
    // token.text = token.text.slice(0, -lastLine.length).replace(/\n$/, "")
    let text_chars: Vec<char> = last.text.chars().collect();
    let cut = text_chars.len().saturating_sub(last_line.chars().count());
    let mut new_text: String = text_chars[..cut].iter().collect();
    if new_text.ends_with('\n') {
        new_text.pop();
    }
    last.text = new_text;
}
