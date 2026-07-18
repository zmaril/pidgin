//! Replays the markdown render vectors extracted from pi's OWN `Markdown`
//! component (`crates/atilla-tui/vectors/gen/generate_markdown.mjs`, driven
//! through pi's renderer) and asserts the Rust port emits a byte-identical
//! line stream for every one of the ~70 `markdown.test.ts` cases. pi is the
//! source of truth; any disagreement is a bug in the port.
//!
//! The per-case theme is a chalk-equivalent (`chalk` level 3) reproduced below
//! so the ANSI bytes match pi's `defaultMarkdownTheme` exactly.

use std::path::PathBuf;

use serde::Deserialize;

use atilla_tui::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme, StyleFn};

#[derive(Deserialize)]
struct Case {
    name: String,
    input: String,
    width: usize,
    #[serde(rename = "paddingX")]
    padding_x: usize,
    #[serde(rename = "paddingY")]
    padding_y: usize,
    style: Option<String>,
    opts: Option<Opts>,
    hyperlinks: bool,
    raw: Vec<String>,
    stripped: Vec<String>,
}

#[derive(Deserialize)]
struct Opts {
    #[serde(default, rename = "preserveOrderedListMarkers")]
    preserve_ordered_list_markers: bool,
    #[serde(default, rename = "preserveBackslashEscapes")]
    preserve_backslash_escapes: bool,
}

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

// ---- chalk (level 3) equivalent ----

/// Apply a chain of (open, close) SGR code pairs to `s`, byte-identically to
/// chalk's `applyStyle` (nesting fix + newline encasing).
fn chalk(codes: &[(u16, u16)], s: &str) -> String {
    // chalk returns the input unchanged for an empty string.
    if s.is_empty() {
        return String::new();
    }
    let open_all: String = codes.iter().map(|(o, _)| format!("\x1b[{o}m")).collect();
    let close_all: String = codes
        .iter()
        .rev()
        .map(|(_, c)| format!("\x1b[{c}m"))
        .collect();

    let mut string = s.to_string();
    if string.contains('\u{1b}') {
        // chalk's `stringReplaceAll` KEEPS the close code and appends the open
        // after it (reopening the style the inner reset terminated).
        for (o, c) in codes.iter().rev() {
            let close = format!("\x1b[{c}m");
            let replacement = format!("\x1b[{c}m\x1b[{o}m");
            string = string.replace(&close, &replacement);
        }
    }
    if string.contains('\n') {
        string = encase_newlines(&string, &close_all, &open_all);
    }
    format!("{open_all}{string}{close_all}")
}

/// chalk's `stringEncaseCRLFWithFirstIndex` for `\n`-only strings.
fn encase_newlines(s: &str, close: &str, open: &str) -> String {
    let replacement = format!("{close}\n{open}");
    s.replace('\n', &replacement)
}

fn style_fn(codes: &'static [(u16, u16)]) -> StyleFn {
    Box::new(move |t: &str| chalk(codes, t))
}

const CYAN: &[(u16, u16)] = &[(36, 39)];
const BLUE: &[(u16, u16)] = &[(34, 39)];
const DIM: &[(u16, u16)] = &[(2, 22)];
const YELLOW: &[(u16, u16)] = &[(33, 39)];
const GREEN: &[(u16, u16)] = &[(32, 39)];
const BOLD: &[(u16, u16)] = &[(1, 22)];
const ITALIC: &[(u16, u16)] = &[(3, 23)];
const STRIKE: &[(u16, u16)] = &[(9, 29)];
const UNDERLINE: &[(u16, u16)] = &[(4, 24)];
const BOLD_CYAN: &[(u16, u16)] = &[(1, 22), (36, 39)];

/// pi's `defaultMarkdownTheme` (`test-themes.ts`).
fn default_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: style_fn(BOLD_CYAN),
        link: style_fn(BLUE),
        link_url: style_fn(DIM),
        code: style_fn(YELLOW),
        code_block: style_fn(GREEN),
        code_block_border: style_fn(DIM),
        quote: style_fn(ITALIC),
        quote_border: style_fn(DIM),
        hr: style_fn(DIM),
        list_bullet: style_fn(CYAN),
        bold: style_fn(BOLD),
        italic: style_fn(ITALIC),
        strikethrough: style_fn(STRIKE),
        underline: style_fn(UNDERLINE),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn gray_fn() -> StyleFn {
    Box::new(|t: &str| chalk(&[(90, 39)], t))
}
fn magenta_fn() -> StyleFn {
    Box::new(|t: &str| chalk(&[(35, 39)], t))
}
fn cyan_fn() -> StyleFn {
    Box::new(|t: &str| chalk(&[(36, 39)], t))
}
fn yellow_fn() -> StyleFn {
    Box::new(|t: &str| chalk(&[(33, 39)], t))
}

fn style_for(name: &str) -> DefaultTextStyle {
    match name {
        "gray-italic" => DefaultTextStyle {
            color: Some(gray_fn()),
            italic: true,
            ..Default::default()
        },
        "magenta" => DefaultTextStyle {
            color: Some(magenta_fn()),
            ..Default::default()
        },
        "cyan" => DefaultTextStyle {
            color: Some(cyan_fn()),
            ..Default::default()
        },
        "yellow-italic" => DefaultTextStyle {
            color: Some(yellow_fn()),
            italic: true,
            ..Default::default()
        },
        other => panic!("unknown style {other}"),
    }
}

/// Minimal ANSI SGR stripper matching the test's `/\x1b\[[0-9;]*m/g`.
fn strip_ansi(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && i + 1 < chars.len() && chars[i + 1] == '[' {
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ';') {
                j += 1;
            }
            if j < chars.len() && chars[j] == 'm' {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn trim_end(s: &str) -> String {
    s.trim_end().to_string()
}

fn load() -> Vectors {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join("markdown_render.json");
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[test]
fn markdown_render_vectors_are_byte_exact() {
    let vectors = load();
    assert!(vectors.cases.len() >= 70, "expected >=70 cases");
    let mut failures: Vec<String> = Vec::new();

    for case in &vectors.cases {
        let theme = default_theme();
        let default_style = case.style.as_deref().map(style_for);
        let options = case.opts.as_ref().map(|o| MarkdownOptions {
            preserve_ordered_list_markers: o.preserve_ordered_list_markers,
            preserve_backslash_escapes: o.preserve_backslash_escapes,
        });
        let mut md = Markdown::new(
            case.input.clone(),
            case.padding_x,
            case.padding_y,
            theme,
            default_style,
            options,
        );
        md.set_hyperlinks(case.hyperlinks);

        let got = md.render(case.width);

        if got != case.raw {
            failures.push(format!(
                "[{}] RAW mismatch:\n  expected ({} lines): {:?}\n  got      ({} lines): {:?}",
                case.name,
                case.raw.len(),
                case.raw,
                got.len(),
                got,
            ));
            continue;
        }
        let got_stripped: Vec<String> = got.iter().map(|l| trim_end(&strip_ansi(l))).collect();
        if got_stripped != case.stripped {
            failures.push(format!(
                "[{}] STRIPPED mismatch:\n  expected: {:?}\n  got:      {:?}",
                case.name, case.stripped, got_stripped
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} / {} markdown vectors failed:\n\n{}",
        failures.len(),
        vectors.cases.len(),
        failures.join("\n\n")
    );
}
