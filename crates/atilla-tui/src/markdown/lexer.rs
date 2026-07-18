//! Hand-port of the subset of marked 18.0.5's block + inline lexer that pi's
//! `Markdown` renderer consumes (`vendor/pi/packages/tui/src/components/
//! markdown.ts`). The port reproduces marked's exact token tree — the fields
//! `renderToken`/`renderInlineTokens`/`renderList`/`renderTable` read — so the
//! verbatim renderer port is byte-identical to pi for every extracted vector.
//!
//! Source of truth: the compiled regex `.source` strings dumped from
//! `vendor/pi/node_modules/marked` (gfm rule set, `gfm:true` / `breaks:false`)
//! plus the beautified `Tokenizer`/`Lexer` control flow. `del` is overridden by
//! pi's `StrictStrikethroughTokenizer` (single-tilde stays plain text).
//!
//! Only the constructs pi's tests exercise are ported; unreachable branches
//! (reference-link defs, pedantic mode) are intentionally omitted.

use fancy_regex::Regex;

/// Kind discriminant mirroring marked's `token.type` strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Space,
    Code,
    Heading,
    List,
    ListItem,
    Table,
    Blockquote,
    Hr,
    Html,
    Paragraph,
    Text,
    Escape,
    Strong,
    Em,
    Codespan,
    Link,
    Br,
    Del,
    Checkbox,
}

/// A marked token. Fields correspond 1:1 to the marked token fields pi reads.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub raw: String,
    pub text: String,
    pub depth: usize,
    pub ordered: bool,
    pub start: Option<i64>,
    pub loose: bool,
    pub task: bool,
    pub checked: bool,
    pub lang: Option<String>,
    pub href: String,
    pub tokens: Vec<Token>,
    pub items: Vec<Token>,
    pub header: Vec<Token>,
    pub rows: Vec<Vec<Token>>,
}

impl Token {
    fn new(kind: Kind) -> Self {
        Token {
            kind,
            raw: String::new(),
            text: String::new(),
            depth: 0,
            ordered: false,
            start: None,
            loose: false,
            task: false,
            checked: false,
            lang: None,
            href: String::new(),
            tokens: Vec::new(),
            items: Vec::new(),
            header: Vec::new(),
            rows: Vec::new(),
        }
    }
}

// Regex sources copied verbatim from marked's compiled gfm rule set.
struct Rules {
    b_newline: Regex,
    b_code: Regex,
    b_fences: Regex,
    b_hr: Regex,
    b_heading: Regex,
    b_blockquote: Regex,
    b_list: Regex,
    b_html: Regex,
    b_table: Regex,
    b_lheading: Regex,
    b_paragraph: Regex,
    b_text: Regex,
    i_escape: Regex,
    i_code: Regex,
    i_br: Regex,
    i_tag: Regex,
    i_link: Regex,
    i_em_ldelim: Regex,
    i_em_rdelim_ast: Regex,
    i_em_rdelim_und: Regex,
    i_autolink: Regex,
    i_url: Regex,
    i_punctuation: Regex,
    i_any_punctuation: Regex,
    i_block_skip: Regex,
    i_text: Regex,
    i_del_strict: Regex,
    // helper "other" regexes
    o_blockquote_start: Regex,
    o_blockquote_setext1: Regex,
    o_blockquote_setext2: Regex,
    o_blank_line: Regex,
    o_double_blank_line: Regex,
    o_any_line: Regex,
    o_list_is_task: Regex,
    o_list_replace_task: Regex,
    o_list_task_checkbox: Regex,
    o_ending_hash: Regex,
    o_ending_space_char: Regex,
    o_non_space_char: Regex,
    o_table_delimiter: Regex,
    o_table_align_chars: Regex,
    o_table_row_blank_line: Regex,
    o_slash_pipe: Regex,
    o_indent_code_comp: Regex,
    o_beginning_space: Regex,
    o_output_link_replace: Regex,
    o_start_a_tag: Regex,
    o_end_a_tag: Regex,
    o_start_pre_script: Regex,
    o_end_pre_script: Regex,
    o_start_angle: Regex,
}

fn rx(src: &str) -> Regex {
    Regex::new(src).unwrap_or_else(|e| panic!("bad regex {src:?}: {e}"))
}

impl Rules {
    fn new() -> Self {
        Rules {
            b_newline: rx(r"^(?:[ \t]*(?:\n|$))+"),
            b_code: rx(r"^((?: {4}| {0,3}\t)[^\n]+(?:\n(?:[ \t]*(?:\n|$))*)?)+"),
            b_fences: rx(
                r"^ {0,3}(`{3,}(?=[^`\n]*(?:\n|$))|~{3,})([^\n]*)(?:\n|$)(?:|([\s\S]*?)(?:\n|$))(?: {0,3}\1[~`]* *(?=\n|$)|$)",
            ),
            b_hr: rx(r"^ {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)"),
            b_heading: rx(r"^ {0,3}(#{1,6})(?=\s|$)(.*)(?:\n+|$)"),
            b_blockquote: rx(
                r"^( {0,3}> ?(([^\n]+(?:\n(?! {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:\s|$)| {0,3}>| {0,3}(?:`{3,}(?=[^`\n]*\n)|~{3,})[^\n]*\n| {0,3}(?:[*+-]|1[.)])[ \t]+[^ \t\n]|</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)| +\n)[^\n]+)*)|[^\n]*)(?:\n|$))+",
            ),
            b_list: rx(r"^( {0,3}(?:[*+-]|\d{1,9}[.)]))([ \t][^\n]*?)?(?:\n|$)"),
            b_html: rx(
                r#"(?i)^ {0,3}(?:<(script|pre|style|textarea)[\s>][\s\S]*?(?:</\1>[^\n]*\n+|$)|<!--(?:-?>|[\s\S]*?(?:-->|$))[^\n]*(\n+|$)|<\?[\s\S]*?(?:\?>\n*|$)|<![A-Z][\s\S]*?(?:>\n*|$)|<!\[CDATA\[[\s\S]*?(?:\]\]>\n*|$)|</?(address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|/?>)[\s\S]*?(?:(?:\n[ \t]*)+\n|$)|<(?!script|pre|style|textarea)([a-z][\w-]*)(?: +[a-zA-Z:_][\w.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\s"'=<>`]+)?)*? */?>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$)|</(?!script|pre|style|textarea)[a-z][\w-]*\s*>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$))"#,
            ),
            b_table: rx(
                r"^ *([^\n ].*)\n {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)(?:\n((?:(?! *\n| {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:\s|$)| {0,3}>|(?: {4}| {0,3}\t)[^\n]| {0,3}(?:`{3,}(?=[^`\n]*\n)|~{3,})[^\n]*\n| {0,3}(?:[*+-]|1[.)])[ \t]|</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)).*(?:\n|$))*)\n*|$)",
            ),
            b_lheading: rx(
                r"^(?! {0,3}(?:[*+-]|\d{1,9}[.)]) |(?: {4}| {0,3}\t)| {0,3}(?:`{3,}|~{3,})| {0,3}>| {0,3}#{1,6}| {0,3}<[^\n>]+>\n| {0,3}\|?(?:[:\- ]*\|)+[:\- ]*\n)((?:.|\n(?!\s*?\n| {0,3}(?:[*+-]|\d{1,9}[.)]) |(?: {4}| {0,3}\t)| {0,3}(?:`{3,}|~{3,})| {0,3}>| {0,3}#{1,6}| {0,3}<[^\n>]+>\n| {0,3}\|?(?:[:\- ]*\|)+[:\- ]*\n))+?)\n {0,3}(=+|-+) *(?:\n+|$)",
            ),
            b_paragraph: rx(
                r"^([^\n]+(?:\n(?! {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:\s|$)| {0,3}>| {0,3}(?:`{3,}(?=[^`\n]*\n)|~{3,})[^\n]*\n| {0,3}(?:[*+-]|1[.)])[ \t]+[^ \t\n]|</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)| *([^\n ].*)\n {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)(?:\n((?:(?! *\n| {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:\s|$)| {0,3}>|(?: {4}| {0,3}\t)[^\n]| {0,3}(?:`{3,}(?=[^`\n]*\n)|~{3,})[^\n]*\n| {0,3}(?:[*+-]|1[.)])[ \t]|</?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)).*(?:\n|$))*)\n*|$)| +\n)[^\n]+)*)",
            ),
            b_text: rx(r"^[^\n]+"),
            i_escape: rx(r##"^\\([!"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])"##),
            i_code: rx(r"^(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)"),
            i_br: rx(r"^( {2,}|\\)\n(?!\s*$)"),
            i_tag: rx(
                r#"^<!--(?:-?>|[\s\S]*?-->)|^</[a-zA-Z][\w:-]*\s*>|^<[a-zA-Z][\w-]*(?:\s+[a-zA-Z:_][\w.:-]*(?:\s*=\s*"[^"]*"|\s*=\s*'[^']*'|\s*=\s*[^\s"'=<>`]+)?)*?\s*/?>|^<\?[\s\S]*?\?>|^<![a-zA-Z]+\s[\s\S]*?>|^<!\[CDATA\[[\s\S]*?\]\]>"#,
            ),
            i_link: rx(
                r#"^!?\[((?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\])|[^\[\]\\`])*?)\]\(\s*(<(?:\\.|[^\n<>\\])+>|[^ \t\n\x00-\x1f]*)(?:(?:[ \t]+(?:\n[ \t]*)?|\n[ \t]*)("(?:\\"?|[^"\\])*"|'(?:\\'?|[^'\\])*'|\((?:\\\)?|[^)\\])*\)))?\s*\)"#,
            ),
            i_em_ldelim: rx(
                r"^(?:\*+(?:((?!\*)(?!~)[\p{P}\p{S}])|([^\s*]))?)|^_+(?:((?!_)(?!~)[\p{P}\p{S}])|([^\s_]))?",
            ),
            i_em_rdelim_ast: rx(
                r"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)|[^*]+(?=[^*])|(?!\*)(?!~)[\p{P}\p{S}](\*+)(?=[\s]|$)|(?:[^\s\p{P}\p{S}]|~)(\*+)(?!\*)(?=(?!~)[\s\p{P}\p{S}]|$)|(?!\*)(?!~)[\s\p{P}\p{S}](\*+)(?=(?:[^\s\p{P}\p{S}]|~))|[\s](\*+)(?!\*)(?=(?!~)[\p{P}\p{S}])|(?!\*)(?!~)[\p{P}\p{S}](\*+)(?!\*)(?=(?!~)[\p{P}\p{S}])|(?:[^\s\p{P}\p{S}]|~)(\*+)(?=(?:[^\s\p{P}\p{S}]|~))",
            ),
            i_em_rdelim_und: rx(
                r"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)|[^_]+(?=[^_])|(?!_)[\p{P}\p{S}](_+)(?=[\s]|$)|[^\s\p{P}\p{S}](_+)(?!_)(?=[\s\p{P}\p{S}]|$)|(?!_)[\s\p{P}\p{S}](_+)(?=[^\s\p{P}\p{S}])|[\s](_+)(?!_)(?=[\p{P}\p{S}])|(?!_)[\p{P}\p{S}](_+)(?!_)(?=[\p{P}\p{S}])",
            ),
            i_autolink: rx(
                r"^<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\s\x00-\x1f<>]*|[a-zA-Z0-9.!#$%&'*+/=?_`{|}~-]+(@)[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(?![-_]))>",
            ),
            i_url: rx(
                r"^((?:[hH][tT][tT][pP][sS]?|[fF][tT][pP]):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\s<]*|^[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])",
            ),
            i_punctuation: rx(r"^((?![*_])[\s\p{P}\p{S}])"),
            i_any_punctuation: rx(r"\\([\p{P}\p{S}])"),
            i_block_skip: rx(
                r"\[(?:[^\[\]`]|(?<a>`+)[^`]+\k<a>(?!`))*?\]\((?:\\[\s\S]|[^\\\(\)]|\((?:\\[\s\S]|[^\\\(\)])*\))*\)|(?<!`)()(?<b>`+)[^`]+\k<b>(?!`)|<(?! )[^<>]*?>",
            ),
            i_text: rx(
                r"^([`~]+|[^`~])(?:(?= {2,}\n)|(?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)|[\s\S]*?(?:(?=[\\<!\[`*~_]|\b_|[hH][tT][tT][pP][sS]?|[fF][tT][pP]:\/\/|www\.|$)|[^ ](?= {2,}\n)|[^a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-](?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)))",
            ),
            i_del_strict: rx(r"^(~~)(?=[^\s~])((?:\\.|[^\\])*?(?:\\.|[^\s~\\]))\1(?=[^~]|$)"),
            o_blockquote_start: rx(r"^ {0,3}>"),
            o_blockquote_setext1: rx(r"\n {0,3}((?:=+|-+) *)(?=\n|$)"),
            o_blockquote_setext2: rx(r"(?m)^ {0,3}>[ \t]?"),
            o_blank_line: rx(r"^[ \t]*$"),
            o_double_blank_line: rx(r"\n[ \t]*\n[ \t]*$"),
            o_any_line: rx(r"\n.*\n"),
            o_list_is_task: rx(r"^\[[ xX]\] +\S"),
            o_list_replace_task: rx(r"^\[[ xX]\] +"),
            o_list_task_checkbox: rx(r"\[[ xX]\]"),
            o_ending_hash: rx(r"#$"),
            o_ending_space_char: rx(r" $"),
            o_non_space_char: rx(r"[^ ]"),
            o_table_delimiter: rx(r"[:|]"),
            o_table_align_chars: rx(r"^\||\| *$"),
            o_table_row_blank_line: rx(r"\n[ \t]*$"),
            o_slash_pipe: rx(r"\\\|"),
            o_indent_code_comp: rx(r"^(\s+)(?:```)"),
            o_beginning_space: rx(r"^\s+"),
            o_output_link_replace: rx(r"\\([\[\]])"),
            o_start_a_tag: rx(r"(?i)^<a "),
            o_end_a_tag: rx(r"(?i)^</a>"),
            o_start_pre_script: rx(r"(?i)^<(pre|code|kbd|script)(\s|>)"),
            o_end_pre_script: rx(r"(?i)^</(pre|code|kbd|script)(\s|>)"),
            o_start_angle: rx(r"^<"),
        }
    }
}

/// Marked's `rtrim`: strip trailing occurrences of char `c` (or of non-`c`
/// chars when `invert`) from `s`.
fn rtrim(s: &str, c: char, invert: bool) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut cut = 0;
    while cut < n {
        let ch = chars[n - cut - 1];
        if (ch == c) != invert {
            cut += 1;
        } else {
            break;
        }
    }
    chars[..n - cut].iter().collect()
}

/// marked `escape` helper (`ee`): drop trailing blank lines unless <=2 lines.
fn remove_trailing_blank_lines(s: &str, rules: &Rules) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let mut t = lines.len() as i64 - 1;
    while t >= 0 && is_match(&rules.o_blank_line, lines[t as usize]) {
        t -= 1;
    }
    if lines.len() as i64 - t <= 2 {
        s.to_string()
    } else {
        lines[..(t + 1) as usize].join("\n")
    }
}

fn is_match(re: &Regex, s: &str) -> bool {
    re.is_match(s).unwrap_or(false)
}

/// marked `splitCells` (`Y`): split a table row on unescaped pipes.
fn split_cells(row: &str, count: Option<usize>, rules: &Rules) -> Vec<String> {
    // Replace escaped/real pipes: `\|` -> `|`, ` |` markers preserved.
    let mut replaced = String::new();
    let chars: Vec<char> = row.chars().collect();
    for (idx, &ch) in chars.iter().enumerate() {
        if ch == '|' {
            let mut escaped = false;
            let mut a = idx as i64 - 1;
            while a >= 0 && chars[a as usize] == '\\' {
                escaped = !escaped;
                a -= 1;
            }
            if escaped {
                replaced.push('|');
            } else {
                replaced.push_str(" |");
            }
        } else {
            replaced.push(ch);
        }
    }
    let mut cells: Vec<String> = split_keep(&replaced, " |");
    if cells.first().map(|c| c.trim().is_empty()).unwrap_or(false) {
        cells.remove(0);
    }
    if !cells.is_empty() && cells.last().map(|c| c.trim().is_empty()).unwrap_or(false) {
        cells.pop();
    }
    if let Some(e) = count {
        if cells.len() > e {
            cells.truncate(e);
        } else {
            while cells.len() < e {
                cells.push(String::new());
            }
        }
    }
    for cell in cells.iter_mut() {
        *cell = rules
            .o_slash_pipe
            .replace_all(cell.trim(), "|")
            .into_owned();
    }
    cells
}

/// JS `String.split(separator)` for a literal string separator (keeps empties).
fn split_keep(s: &str, sep: &str) -> Vec<String> {
    s.split(sep).map(|x| x.to_string()).collect()
}

/// marked `indentCodeCompensation` (`st`) for fenced code blocks.
fn indent_code_compensation(raw: &str, text: String, rules: &Rules) -> String {
    let caps = match rules.o_indent_code_comp.captures(raw) {
        Ok(Some(c)) => c,
        _ => return text,
    };
    let indent = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let indent_len = indent.chars().count();
    text.split('\n')
        .map(|line| {
            match rules.o_beginning_space.captures(line) {
                Ok(Some(c)) => {
                    let lead = c.get(0).map(|m| m.as_str()).unwrap_or("");
                    if lead.chars().count() >= indent_len {
                        // slice off `indent_len` leading chars
                        line.chars().skip(indent_len).collect::<String>()
                    } else {
                        line.to_string()
                    }
                }
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// marked `outputLink` (`xe`) restricted to plain links (pi never hits images).
fn output_link(lex: &mut Lexer, cap0: &str, cap1: &str, href: String, raw: &str) -> Token {
    let text = lex
        .rules
        .o_output_link_replace
        .replace_all(cap1, "$1")
        .into_owned();
    lex.state_in_link = true;
    let mut tok = Token::new(Kind::Link);
    tok.raw = raw.to_string();
    tok.href = href;
    tok.text = text.clone();
    tok.tokens = lex.inline_tokens(&text);
    let _ = cap0;
    lex.state_in_link = false;
    tok
}

/// The block + inline lexer.
pub struct Lexer {
    rules: Rules,
    state_top: bool,
    state_in_link: bool,
    state_in_raw_block: bool,
}

impl Default for Lexer {
    fn default() -> Self {
        Self::new()
    }
}

impl Lexer {
    pub fn new() -> Self {
        Lexer {
            rules: Rules::new(),
            state_top: true,
            state_in_link: false,
            state_in_raw_block: false,
        }
    }

    /// Entry point: mirror marked's `Lexer.lex` (block tokens then eager inline).
    pub fn lex(&mut self, src: &str) -> Vec<Token> {
        let normalized = src.replace("\r\n", "\n").replace('\r', "\n");
        let mut tokens = Vec::new();
        self.state_top = true;
        self.block_tokens(&normalized, &mut tokens, false);
        tokens
    }

    fn block_tokens(&mut self, src_in: &str, tokens: &mut Vec<Token>, top_continuation: bool) {
        let mut src = src_in.to_string();
        let mut last_len = usize::MAX;
        let mut cont = top_continuation;
        while !src.is_empty() {
            if src.len() < last_len {
                last_len = src.len();
            } else {
                break; // infinite-loop guard (marked throws; we stop)
            }

            // space
            if let Some(tok) = self.space(&src) {
                let raw_len = tok.raw.len();
                src = src[raw_len..].to_string();
                if tok.raw.chars().count() == 1 && !tokens.is_empty() {
                    tokens.last_mut().unwrap().raw.push('\n');
                } else {
                    tokens.push(tok);
                }
                continue;
            }
            // indented code
            if let Some(tok) = self.code(&src) {
                let raw_len = tok.raw.len();
                src = src[raw_len..].to_string();
                let merge = matches!(
                    tokens.last().map(|t| t.kind),
                    Some(Kind::Paragraph) | Some(Kind::Text)
                );
                if merge {
                    let last = tokens.last_mut().unwrap();
                    if !last.raw.ends_with('\n') {
                        last.raw.push('\n');
                    }
                    last.raw.push_str(&tok.raw);
                    last.text.push('\n');
                    last.text.push_str(&tok.text);
                    let text = last.text.clone();
                    last.tokens = self.inline_tokens(&text);
                } else {
                    tokens.push(tok);
                }
                continue;
            }
            // fences
            if let Some(tok) = self.fences(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // heading
            if let Some(tok) = self.heading(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // hr
            if let Some(tok) = self.hr(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // blockquote
            if let Some(tok) = self.blockquote(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // list
            if let Some(tok) = self.list(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // html
            if let Some(tok) = self.html(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // (reference-link def: unreachable for pi's inputs — omitted)
            // table
            if let Some(tok) = self.table(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // lheading
            if let Some(tok) = self.lheading(&src) {
                src = src[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            // paragraph (top only)
            if self.state_top {
                if let Some(tok) = self.paragraph(&src) {
                    let raw_len = tok.raw.len();
                    let merged =
                        cont && matches!(tokens.last().map(|t| t.kind), Some(Kind::Paragraph));
                    if merged {
                        let last = tokens.last_mut().unwrap();
                        if !last.raw.ends_with('\n') {
                            last.raw.push('\n');
                        }
                        last.raw.push_str(&tok.raw);
                        last.text.push('\n');
                        last.text.push_str(&tok.text);
                        let text = last.text.clone();
                        last.tokens = self.inline_tokens(&text);
                    } else {
                        tokens.push(tok);
                    }
                    cont = true; // subsequent single-line srcs continue
                    src = src[raw_len..].to_string();
                    continue;
                }
            }
            // text
            if let Some(tok) = self.text_block(&src) {
                let raw_len = tok.raw.len();
                src = src[raw_len..].to_string();
                let merge = matches!(tokens.last().map(|t| t.kind), Some(Kind::Text));
                if merge {
                    let last = tokens.last_mut().unwrap();
                    if !last.raw.ends_with('\n') {
                        last.raw.push('\n');
                    }
                    last.raw.push_str(&tok.raw);
                    last.text.push('\n');
                    last.text.push_str(&tok.text);
                    let text = last.text.clone();
                    last.tokens = self.inline_tokens(&text);
                } else {
                    tokens.push(tok);
                }
                continue;
            }
            break;
        }
        self.state_top = true;
    }

    // ---- block tokenizers ----

    fn space(&self, src: &str) -> Option<Token> {
        let caps = self.rules.b_newline.captures(src).ok().flatten()?;
        let m0 = caps.get(0)?.as_str();
        if m0.is_empty() {
            return None;
        }
        let mut tok = Token::new(Kind::Space);
        tok.raw = m0.to_string();
        Some(tok)
    }

    fn code(&self, src: &str) -> Option<Token> {
        let caps = self.rules.b_code.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        // codeRemoveIndent: /^(?: {1,4}| {0,3}\t)/gm applied to `raw`
        let trimmed = remove_trailing_blank_lines(&raw, &self.rules);
        let text = code_remove_indent(&trimmed);
        let mut tok = Token::new(Kind::Code);
        tok.raw = trimmed;
        tok.text = text;
        Some(tok)
    }

    fn fences(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_fences.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let cap3 = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let text = indent_code_compensation(&raw, cap3.to_string(), &self.rules);
        let mut tok = Token::new(Kind::Code);
        tok.raw = raw;
        let lang_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let lang = self
            .rules
            .i_any_punctuation
            .replace_all(lang_raw.trim(), "$1")
            .into_owned();
        tok.lang = Some(lang);
        tok.text = text;
        Some(tok)
    }

    fn heading(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_heading.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let hashes = caps.get(1)?.as_str();
        let mut text = caps
            .get(2)
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if is_match(&self.rules.o_ending_hash, &text) {
            let stripped = rtrim(&text, '#', false);
            if stripped.is_empty() || is_match(&self.rules.o_ending_space_char, &stripped) {
                text = stripped.trim().to_string();
            }
        }
        let mut tok = Token::new(Kind::Heading);
        tok.depth = hashes.chars().count();
        tok.raw = rtrim(&raw, '\n', false);
        tok.tokens = self.inline_tokens(&text);
        tok.text = text;
        Some(tok)
    }

    fn hr(&self, src: &str) -> Option<Token> {
        let caps = self.rules.b_hr.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str();
        let mut tok = Token::new(Kind::Hr);
        tok.raw = rtrim(raw, '\n', false);
        Some(tok)
    }

    fn blockquote(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_blockquote.captures(src).ok().flatten()?;
        let m0 = caps.get(0)?.as_str();
        let trimmed = rtrim(m0, '\n', false);
        let mut lines: Vec<String> = trimmed.split('\n').map(|s| s.to_string()).collect();
        let mut raw_acc = String::new();
        let mut text_acc = String::new();
        let mut children: Vec<Token> = Vec::new();

        while !lines.is_empty() {
            let mut in_blockquote = false;
            let mut current: Vec<String> = Vec::new();
            let mut consumed = 0usize;
            for (idx, line) in lines.iter().enumerate() {
                if is_match(&self.rules.o_blockquote_start, line) {
                    current.push(line.clone());
                    in_blockquote = true;
                } else if !in_blockquote {
                    current.push(line.clone());
                } else {
                    consumed = idx;
                    break;
                }
                consumed = idx + 1;
            }
            lines = lines[consumed..].to_vec();

            let cur_joined = current.join("\n");
            let p1 = self
                .rules
                .o_blockquote_setext1
                .replace_all(&cur_joined, "\n    $1")
                .into_owned();
            let p2 = self
                .rules
                .o_blockquote_setext2
                .replace_all(&p1, "")
                .into_owned();

            raw_acc = if raw_acc.is_empty() {
                cur_joined.clone()
            } else {
                format!("{raw_acc}\n{cur_joined}")
            };
            text_acc = if text_acc.is_empty() {
                p2.clone()
            } else {
                format!("{text_acc}\n{p2}")
            };

            let saved_top = self.state_top;
            self.state_top = true;
            self.block_tokens(&p2, &mut children, true);
            self.state_top = saved_top;

            if lines.is_empty() {
                break;
            }
            let last_kind = children.last().map(|t| t.kind);
            if last_kind == Some(Kind::Code) {
                break;
            }
            if last_kind == Some(Kind::Blockquote) {
                let prev = children.pop().unwrap();
                let f = format!("{}\n{}", prev.raw, lines.join("\n"));
                let nested = self.blockquote(&f).unwrap();
                let nlen = raw_acc.len().saturating_sub(prev.raw.len());
                raw_acc = format!("{}{}", &raw_acc[..nlen], nested.raw);
                let tlen = text_acc.len().saturating_sub(prev.text.len());
                text_acc = format!("{}{}", &text_acc[..tlen], nested.text);
                children.push(nested);
                break;
            }
            if last_kind == Some(Kind::List) {
                let prev = children.pop().unwrap();
                let f = format!("{}\n{}", prev.raw, lines.join("\n"));
                let nested = self.list(&f).unwrap();
                let nlen = raw_acc.len().saturating_sub(prev.raw.len());
                raw_acc = format!("{}{}", &raw_acc[..nlen], nested.raw);
                let tlen = text_acc.len().saturating_sub(prev.raw.len());
                text_acc = format!("{}{}", &text_acc[..tlen], nested.raw);
                let nested_raw_len = nested.raw.len();
                children.push(nested);
                lines = f[nested_raw_len..]
                    .split('\n')
                    .map(|s| s.to_string())
                    .collect();
                continue;
            }
        }

        let mut tok = Token::new(Kind::Blockquote);
        tok.raw = raw_acc;
        tok.text = text_acc;
        tok.tokens = children;
        Some(tok)
    }

    fn list(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_list.captures(src).ok().flatten()?;
        let bull_raw = caps.get(1)?.as_str().trim().to_string();
        let ordered = bull_raw.chars().count() > 1;
        let start: Option<i64> = if ordered {
            bull_raw[..bull_raw.len() - 1].parse::<i64>().ok()
        } else {
            None
        };

        // Build the per-item bullet regex source (marked's `listItemRegex`).
        let bull_pat = if ordered {
            let last = &bull_raw[bull_raw.len() - 1..];
            format!(r"\d{{1,9}}\{last}")
        } else {
            format!(r"\{bull_raw}")
        };
        let item_re = rx(&format!(r"^( {{0,3}}{bull_pat})((?:[\t ][^\n]*)?(?:\n|$))"));

        let mut list_tok = Token::new(Kind::List);
        list_tok.ordered = ordered;
        list_tok.start = start;

        let mut rest = src.to_string();
        let mut ended_early = false; // marked's `endEarly` (`o`)
        loop {
            if rest.is_empty() {
                break;
            }
            let cap = match item_re.captures(&rest).ok().flatten() {
                Some(c) => c,
                None => break,
            };
            if is_match(&self.rules.b_hr, &rest) {
                break;
            }
            let mut raw = cap.get(0)?.as_str().to_string();
            let g2 = cap.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
            let g1 = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            drop(cap);
            rest = rest[raw.len()..].to_string();

            let first_line_of_g2 = g2.split('\n').next().unwrap_or("");
            let mut line_ws = tabs_to_spaces(first_line_of_g2, g1.chars().count());
            let mut next_line = rest.split('\n').next().unwrap_or("").to_string();
            let blank = line_ws.trim().is_empty();
            let indent: usize;
            let mut item_text: String;
            if blank {
                indent = g1.chars().count() + 1;
                item_text = String::new();
            } else {
                let search = search_non_space(&line_ws);
                let mut ind = search;
                if ind > 4 {
                    ind = 1;
                }
                indent = ind + g1.chars().count();
                item_text = line_ws.chars().skip(ind).collect();
            }

            let mut end_early = false;
            if blank && is_match(&self.rules.o_blank_line, &next_line) {
                raw.push_str(&next_line);
                raw.push('\n');
                let consume = next_line.len() + 1;
                rest = rest[consume.min(rest.len())..].to_string();
                end_early = true;
            }

            if !end_early {
                // marked memoizes these with the space bound capped at
                // `max(0, min(3, indent - 1))`, not `indent` (see `E(...)`).
                let cap = indent.saturating_sub(1).min(3);
                let next_bullet = rx(&format!(
                    r"^ {{0,{cap}}}(?:[*+-]|\d{{1,9}}[.)])((?:[ \t][^\n]*)?(?:\n|$))"
                ));
                let hr_re = rx(&format!(
                    r"^ {{0,{cap}}}((?:- *){{3,}}|(?:_ *){{3,}}|(?:\* *){{3,}})(?:\n+|$)"
                ));
                let fences_re = rx(&format!(r"^ {{0,{cap}}}(?:```|~~~)"));
                let heading_re = rx(&format!(r"^ {{0,{cap}}}#"));
                let html_re = rx(&format!(r"(?i)^ {{0,{cap}}}<(?:[a-z].*>|!--)"));
                let bq_re = rx(&format!(r"^ {{0,{cap}}}>"));

                // Running blank flag (marked's `R`), updated each iteration.
                let mut r_blank = blank;
                loop {
                    if rest.is_empty() {
                        break;
                    }
                    let g = rest.split('\n').next().unwrap_or("").to_string();
                    next_line = g.clone();
                    let compensated = next_line.replace('\t', "    ");
                    if is_match(&fences_re, &next_line)
                        || is_match(&heading_re, &next_line)
                        || is_match(&html_re, &next_line)
                        || is_match(&bq_re, &next_line)
                        || is_match(&next_bullet, &next_line)
                        || is_match(&hr_re, &next_line)
                    {
                        break;
                    }
                    if search_non_space_full(&compensated) >= indent as i64
                        || next_line.trim().is_empty()
                    {
                        item_text.push('\n');
                        item_text.push_str(&compensated.chars().skip(indent).collect::<String>());
                    } else {
                        if r_blank
                            || search_non_space_full(&line_ws.replace('\t', "    ")) >= 4
                            || is_match(&fences_re, &line_ws)
                            || is_match(&heading_re, &line_ws)
                            || is_match(&hr_re, &line_ws)
                        {
                            break;
                        }
                        item_text.push('\n');
                        item_text.push_str(&next_line);
                    }
                    r_blank = next_line.trim().is_empty();
                    raw.push_str(&g);
                    raw.push('\n');
                    let consume = g.len() + 1;
                    rest = rest[consume.min(rest.len())..].to_string();
                    line_ws = compensated.chars().skip(indent).collect();
                }
            }

            if !list_tok.loose {
                if ended_early {
                    list_tok.loose = true;
                } else if is_match(&self.rules.o_double_blank_line, &raw) {
                    ended_early = true;
                }
            }

            let task = is_match(&self.rules.o_list_is_task, &item_text);
            let mut item = Token::new(Kind::ListItem);
            item.raw = raw.clone();
            item.task = task;
            item.text = item_text;
            list_tok.items.push(item);
            list_tok.raw.push_str(&raw);
        }

        let last = list_tok.items.last_mut()?;
        last.raw = last.raw.trim_end().to_string();
        last.text = last.text.trim_end().to_string();
        list_tok.raw = list_tok.raw.trim_end().to_string();

        // Tokenize each item's inner blocks + apply task/loose rules.
        let loose_flag = self.parse_list_items(&mut list_tok);
        list_tok.loose = loose_flag;
        if list_tok.loose {
            for item in list_tok.items.iter_mut() {
                item.loose = true;
                for child in item.tokens.iter_mut() {
                    if child.kind == Kind::Text {
                        child.kind = Kind::Paragraph;
                    }
                }
            }
        }
        Some(list_tok)
    }

    fn parse_list_items(&mut self, list_tok: &mut Token) -> bool {
        let mut loose = list_tok.loose;
        let items_len = list_tok.items.len();
        for i in 0..items_len {
            let text = list_tok.items[i].text.clone();
            let saved_top = self.state_top;
            self.state_top = false;
            let mut child_tokens = Vec::new();
            self.block_tokens(&text, &mut child_tokens, false);
            self.state_top = saved_top;
            list_tok.items[i].tokens = child_tokens;

            // task-list marker handling
            let is_task = list_tok.items[i].task;
            let first_kind = list_tok.items[i].tokens.first().map(|t| t.kind);
            if is_task && matches!(first_kind, Some(Kind::Text) | Some(Kind::Paragraph)) {
                let new_text = self
                    .rules
                    .o_list_replace_task
                    .replace(&list_tok.items[i].text, "")
                    .into_owned();
                list_tok.items[i].text = new_text.clone();
                {
                    let child = &mut list_tok.items[i].tokens[0];
                    child.raw = self
                        .rules
                        .o_list_replace_task
                        .replace(&child.raw, "")
                        .into_owned();
                    child.text = self
                        .rules
                        .o_list_replace_task
                        .replace(&child.text, "")
                        .into_owned();
                    let ctext = child.text.clone();
                    child.tokens = self.inline_tokens(&ctext);
                }
                if let Ok(Some(cap)) = self
                    .rules
                    .o_list_task_checkbox
                    .captures(&list_tok.items[i].raw)
                {
                    let box_raw = format!("{} ", cap.get(0).unwrap().as_str());
                    let checked = cap.get(0).unwrap().as_str() != "[ ]";
                    list_tok.items[i].checked = checked;
                    let mut checkbox = Token::new(Kind::Checkbox);
                    checkbox.raw = box_raw.clone();
                    checkbox.checked = checked;
                    if loose {
                        let ck0 = list_tok.items[i].tokens[0].kind;
                        if matches!(ck0, Kind::Paragraph | Kind::Text)
                            && !list_tok.items[i].tokens[0].tokens.is_empty()
                        {
                            let child = &mut list_tok.items[i].tokens[0];
                            child.raw = format!("{box_raw}{}", child.raw);
                            child.text = format!("{box_raw}{}", child.text);
                            child.tokens.insert(0, checkbox);
                        } else {
                            let mut para = Token::new(Kind::Paragraph);
                            para.raw = box_raw.clone();
                            para.text = box_raw;
                            para.tokens = vec![checkbox];
                            list_tok.items[i].tokens.insert(0, para);
                        }
                    } else {
                        list_tok.items[i].tokens.insert(0, checkbox);
                    }
                }
            } else if is_task {
                list_tok.items[i].task = false;
            }

            if !loose {
                let spaces: Vec<&Token> = list_tok.items[i]
                    .tokens
                    .iter()
                    .filter(|t| t.kind == Kind::Space)
                    .collect();
                let has_gap = !spaces.is_empty()
                    && spaces
                        .iter()
                        .any(|t| is_match(&self.rules.o_any_line, &t.raw));
                loose = has_gap;
            }
        }
        loose
    }

    fn html(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_html.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut tok = Token::new(Kind::Html);
        tok.raw = raw.clone();
        tok.text = raw;
        Some(tok)
    }

    fn table(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_table.captures(src).ok().flatten()?;
        let delim = caps.get(2)?.as_str();
        if !is_match(&self.rules.o_table_delimiter, delim) {
            return None;
        }
        let header_line = caps.get(1)?.as_str();
        let headers = split_cells(header_line, None, &self.rules);
        let aligns_src = self
            .rules
            .o_table_align_chars
            .replace_all(delim, "")
            .into_owned();
        let align_cells = split_keep(&aligns_src, "|");
        if headers.len() != align_cells.len() {
            return None;
        }
        let rows_src = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let rows_lines: Vec<String> = if rows_src.trim().is_empty() {
            Vec::new()
        } else {
            let cleaned = self
                .rules
                .o_table_row_blank_line
                .replace(rows_src, "")
                .into_owned();
            cleaned.split('\n').map(|s| s.to_string()).collect()
        };

        let raw = rtrim(caps.get(0)?.as_str(), '\n', false);
        let mut tok = Token::new(Kind::Table);
        tok.raw = raw;
        for h in &headers {
            let mut cell = Token::new(Kind::Text);
            cell.text = h.clone();
            cell.tokens = self.inline_tokens(h);
            tok.header.push(cell);
        }
        let ncols = tok.header.len();
        for line in &rows_lines {
            let cells = split_cells(line, Some(ncols), &self.rules);
            let mut row = Vec::new();
            for c in &cells {
                let mut cell = Token::new(Kind::Text);
                cell.text = c.clone();
                cell.tokens = self.inline_tokens(c);
                row.push(cell);
            }
            tok.rows.push(row);
        }
        Some(tok)
    }

    fn lheading(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_lheading.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let text = caps.get(1)?.as_str().trim().to_string();
        let marker = caps.get(2)?.as_str();
        let depth = if marker.starts_with('=') { 1 } else { 2 };
        let mut tok = Token::new(Kind::Heading);
        tok.depth = depth;
        tok.raw = rtrim(&raw, '\n', false);
        tok.tokens = self.inline_tokens(&text);
        tok.text = text;
        Some(tok)
    }

    fn paragraph(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_paragraph.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let cap1 = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let text = cap1.strip_suffix('\n').unwrap_or(cap1).to_string();
        let mut tok = Token::new(Kind::Paragraph);
        tok.raw = raw;
        tok.tokens = self.inline_tokens(&text);
        tok.text = text;
        Some(tok)
    }

    fn text_block(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.b_text.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut tok = Token::new(Kind::Text);
        tok.raw = raw.clone();
        tok.tokens = self.inline_tokens(&raw);
        tok.text = raw;
        Some(tok)
    }

    // ---- inline lexer ----

    pub fn inline_tokens(&mut self, src: &str) -> Vec<Token> {
        let masked = self.build_masked(src);
        let mut tokens: Vec<Token> = Vec::new();
        let mut rest = src.to_string();
        let mut prev_char = String::new();
        let mut keep_prev = false;
        let mut last_len = usize::MAX;
        while !rest.is_empty() {
            if rest.len() < last_len {
                last_len = rest.len();
            } else {
                break;
            }
            if !keep_prev {
                prev_char = String::new();
            }
            keep_prev = false;

            if let Some(tok) = self.i_escape(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_tag(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_link(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_em_strong(&rest, &masked, &prev_char) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_codespan(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_br(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_del(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_autolink(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if !self.state_in_link {
                if let Some(tok) = self.i_url(&rest) {
                    rest = rest[tok.raw.len()..].to_string();
                    tokens.push(tok);
                    continue;
                }
            }
            if let Some(tok) = self.i_inline_text(&rest) {
                let raw_len = tok.raw.len();
                let last_char = tok.raw.chars().last().unwrap_or(' ');
                if last_char != '_' {
                    prev_char = last_char.to_string();
                }
                keep_prev = true;
                rest = rest[raw_len..].to_string();
                if let Some(last) = tokens.last_mut() {
                    if last.kind == Kind::Text {
                        last.raw.push_str(&tok.raw);
                        last.text.push_str(&tok.text);
                        continue;
                    }
                }
                tokens.push(tok);
                continue;
            }
            break;
        }
        tokens
    }

    /// marked's masking pass: escape-punct -> `++`, then block-skip link/code/
    /// html spans -> `[aaa]`, all length-preserving so R-delim indices align.
    fn build_masked(&self, src: &str) -> String {
        // anyPunctuation (global): `\X` -> `++` (length-preserving).
        let mut n = String::new();
        {
            let mut pos = 0usize;
            let mut base = 0usize;
            while let Ok(Some(cap)) = self.rules.i_any_punctuation.captures_from_pos(src, base) {
                let m = cap.get(0).unwrap();
                n.push_str(&src[pos..m.start()]);
                n.push_str("++");
                pos = m.end();
                base = m.end();
            }
            n.push_str(&src[pos..]);
        }
        // blockSkip (global): replace link/codespan/html span with `[aaa]`.
        let mut out = String::new();
        {
            let mut pos = 0usize;
            let mut base = 0usize;
            while let Ok(Some(cap)) = self.rules.i_block_skip.captures_from_pos(&n, base) {
                let m = cap.get(0).unwrap();
                let r = cap.get(2).map(|g| g.as_str().len()).unwrap_or(0);
                out.push_str(&n[pos..m.start() + r]);
                let inner = m.as_str().len().saturating_sub(r).saturating_sub(2);
                out.push('[');
                out.push_str(&"a".repeat(inner));
                out.push(']');
                pos = m.end();
                base = m.end();
                if base > n.len() {
                    break;
                }
            }
            out.push_str(&n[pos..]);
        }
        out
    }

    fn i_escape(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_escape.captures(src).ok().flatten()?;
        let mut tok = Token::new(Kind::Escape);
        tok.raw = caps.get(0)?.as_str().to_string();
        tok.text = caps.get(1)?.as_str().to_string();
        Some(tok)
    }

    fn i_tag(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_tag.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        if !self.state_in_link && is_match(&self.rules.o_start_a_tag, &raw) {
            self.state_in_link = true;
        } else if self.state_in_link && is_match(&self.rules.o_end_a_tag, &raw) {
            self.state_in_link = false;
        }
        if !self.state_in_raw_block && is_match(&self.rules.o_start_pre_script, &raw) {
            self.state_in_raw_block = true;
        } else if self.state_in_raw_block && is_match(&self.rules.o_end_pre_script, &raw) {
            self.state_in_raw_block = false;
        }
        let mut tok = Token::new(Kind::Html);
        tok.raw = raw.clone();
        tok.text = raw;
        Some(tok)
    }

    fn i_link(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_link.captures(src).ok().flatten()?;
        let cap0 = caps.get(0)?.as_str().to_string();
        let cap1 = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut href_field = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();

        let trimmed = href_field.trim().to_string();
        if is_match(&self.rules.o_start_angle, &trimmed) {
            if !trimmed.ends_with('>') {
                return None;
            }
            let inner = &trimmed[..trimmed.len() - 1];
            let bs = rtrim(inner, '\\', true);
            if (trimmed.chars().count() - bs.chars().count()).is_multiple_of(2) {
                return None;
            }
        }
        let mut href = href_field.clone();
        href = href.trim().to_string();
        if is_match(&self.rules.o_start_angle, &href) {
            href = href[1..href.len() - 1].to_string();
        }
        href = self
            .rules
            .i_any_punctuation
            .replace_all(&href, "$1")
            .into_owned();
        let _ = &mut href_field;
        Some(output_link(self, &cap0, &cap1, href, &cap0))
    }

    fn i_em_strong(&mut self, src: &str, masked: &str, prev_char: &str) -> Option<Token> {
        let lcaps = self.rules.i_em_ldelim.captures(src).ok().flatten()?;
        let g1 = lcaps.get(1).map(|m| m.as_str());
        let g2 = lcaps.get(2).map(|m| m.as_str());
        let g3 = lcaps.get(3).map(|m| m.as_str());
        let g4 = lcaps.get(4).map(|m| m.as_str());
        if g1.is_none() && g2.is_none() && g3.is_none() && g4.is_none() {
            return None;
        }
        if g4.is_some() && prev_char_is_alnum(prev_char) {
            return None;
        }
        let l0 = lcaps.get(0)?.as_str();
        let cond = g1.is_none() && g3.is_none();
        if !(cond || prev_char.is_empty() || is_match(&self.rules.i_punctuation, prev_char)) {
            return None;
        }
        let l_length = l0.chars().count() as i64 - 1;
        let first_ch = l0.chars().next()?;
        let end_re = if first_ch == '*' {
            &self.rules.i_em_rdelim_ast
        } else {
            &self.rules.i_em_rdelim_und
        };

        // maskedSrc.slice(-src.length + lLength)
        let masked_chars: Vec<char> = masked.chars().collect();
        let src_len = src.chars().count() as i64;
        let start_idx = (masked_chars.len() as i64 - src_len + l_length).max(0) as usize;
        let scan: String = masked_chars[start_idx.min(masked_chars.len())..]
            .iter()
            .collect();

        let mut delim_total = l_length;
        let mut mid_delim_total = 0i64;
        let mut pos = 0usize;
        while let Some(mcaps) = end_re.captures_from_pos(&scan, pos).ok().flatten() {
            let m0 = mcaps.get(0).unwrap();
            let rdelim = (1..=6)
                .filter_map(|i| mcaps.get(i))
                .map(|m| m.as_str())
                .find(|s| !s.is_empty());
            let next_pos = if m0.end() > pos { m0.end() } else { pos + 1 };
            let rdelim = match rdelim {
                Some(r) => r,
                None => {
                    pos = next_pos;
                    continue;
                }
            };
            let r_length = rdelim.chars().count() as i64;
            let has_g3 = mcaps
                .get(3)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            let has_g4 = mcaps
                .get(4)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            if has_g3 || has_g4 {
                delim_total += r_length;
                pos = next_pos;
                continue;
            }
            let has_g5 = mcaps
                .get(5)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            let has_g6 = mcaps
                .get(6)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            if (has_g5 || has_g6) && l_length % 3 != 0 && (l_length + r_length) % 3 == 0 {
                mid_delim_total += r_length;
                pos = next_pos;
                continue;
            }
            delim_total -= r_length;
            if delim_total > 0 {
                pos = next_pos;
                continue;
            }
            let r_use = r_length.min(r_length + delim_total + mid_delim_total);
            let m0_first_len = i64::from(m0.as_str().chars().next().is_some());
            let m_index = scan[..m0.start()].chars().count() as i64;
            let raw_len = (l_length + m_index + m0_first_len + r_use) as usize;
            let src_chars: Vec<char> = src.chars().collect();
            let raw: String = src_chars[..raw_len.min(src_chars.len())].iter().collect();
            if l_length.min(r_use) % 2 != 0 {
                let text: String = raw.chars().skip(1).take(raw.chars().count() - 2).collect();
                let mut tok = Token::new(Kind::Em);
                tok.raw = raw;
                tok.tokens = self.inline_tokens(&text);
                tok.text = text;
                return Some(tok);
            }
            let text: String = raw.chars().skip(2).take(raw.chars().count() - 4).collect();
            let mut tok = Token::new(Kind::Strong);
            tok.raw = raw;
            tok.tokens = self.inline_tokens(&text);
            tok.text = text;
            return Some(tok);
        }
        None
    }

    fn i_codespan(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_code.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut text = caps.get(2)?.as_str().replace('\n', " ");
        let has_non_space = is_match(&self.rules.o_non_space_char, &text);
        let starts_space = text.starts_with(' ');
        let ends_space = text.ends_with(' ');
        if has_non_space && starts_space && ends_space {
            text = text[1..text.len() - 1].to_string();
        }
        let mut tok = Token::new(Kind::Codespan);
        tok.raw = raw;
        tok.text = text;
        Some(tok)
    }

    fn i_br(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_br.captures(src).ok().flatten()?;
        let mut tok = Token::new(Kind::Br);
        tok.raw = caps.get(0)?.as_str().to_string();
        Some(tok)
    }

    fn i_del(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_del_strict.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let text = caps.get(2)?.as_str().to_string();
        let mut tok = Token::new(Kind::Del);
        tok.raw = raw;
        tok.tokens = self.inline_tokens(&text);
        tok.text = text;
        Some(tok)
    }

    fn i_autolink(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_autolink.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let inner = caps.get(1)?.as_str().to_string();
        let is_email = caps.get(2).is_some();
        let (text, href) = if is_email {
            (inner.clone(), format!("mailto:{inner}"))
        } else {
            (inner.clone(), inner.clone())
        };
        Some(make_autolink(raw, text, href))
    }

    fn i_url(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_url.captures(src).ok().flatten()?;
        let m0 = caps.get(0)?.as_str().to_string();
        let is_email = caps.get(2).is_some();
        if is_email {
            let text = m0.clone();
            let href = format!("mailto:{m0}");
            return Some(make_autolink(m0, text, href));
        }
        // backpedal trailing punctuation for bare URLs
        let mut cur = m0.clone();
        loop {
            let prev = cur.clone();
            cur = backpedal(&prev);
            if prev == cur {
                break;
            }
        }
        let scheme_www = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let href = if scheme_www == "www." {
            format!("http://{cur}")
        } else {
            cur.clone()
        };
        Some(make_autolink(cur.clone(), cur, href))
    }

    fn i_inline_text(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_text.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut tok = Token::new(Kind::Text);
        tok.text = raw.clone();
        tok.raw = raw;
        Some(tok)
    }
}

fn make_autolink(raw: String, text: String, href: String) -> Token {
    let mut tok = Token::new(Kind::Link);
    tok.raw = raw;
    tok.href = href;
    let mut inner = Token::new(Kind::Text);
    inner.raw = text.clone();
    inner.text = text.clone();
    tok.tokens = vec![inner];
    tok.text = text;
    tok
}

/// marked `_backpedal` for gfm bare URLs.
fn backpedal(url: &str) -> String {
    let re =
        rx(r"(?:[^?!.,:;*_'\x22~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'\x22~)]+(?!$))+");
    match re.captures(url).ok().flatten() {
        Some(c) => c.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
        None => String::new(),
    }
}

fn prev_char_is_alnum(s: &str) -> bool {
    let re = rx(r"[\p{L}\p{N}]");
    is_match(&re, s)
}

/// marked `codeRemoveIndent`: /^(?: {1,4}| {0,3}\t)/gm on the raw code block.
fn code_remove_indent(s: &str) -> String {
    let re = rx(r"(?m)^(?: {1,4}| {0,3}\t)");
    re.replace_all(s, "").into_owned()
}

/// marked `_tabToSpaces` (`me`): expand tabs to a 4-column stop.
fn tabs_to_spaces(s: &str, start_col: usize) -> String {
    let mut col = start_col;
    let mut out = String::new();
    for ch in s.chars() {
        if ch == '\t' {
            let n = 4 - col % 4;
            out.push_str(&" ".repeat(n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Index (char count) of first non-space char, matching `String.search(nonSpaceChar)`.
fn search_non_space(s: &str) -> usize {
    for (i, ch) in s.chars().enumerate() {
        if ch != ' ' {
            return i;
        }
    }
    s.chars().count()
}

fn search_non_space_full(s: &str) -> i64 {
    // JS String.search(/[^ ]/) returns -1 when all spaces; callers compare `>= n`.
    for (i, ch) in s.chars().enumerate() {
        if ch != ' ' {
            return i as i64;
        }
    }
    -1
}
