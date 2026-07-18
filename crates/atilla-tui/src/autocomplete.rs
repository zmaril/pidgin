// straitjacket-allow-file:duplication — the posix path helpers (`normalize_string`,
// `pjoin`, `pdirname`, `pbasename`) faithfully mirror Node's own `path` module
// implementation, and the repeated `slice`/`ends_with` idioms in
// `get_file_suggestions` / `apply_completion` mirror pi's `autocomplete.ts`
// line-for-line; collapsing them would diverge from the byte-exact source.
//! Byte-exact port of pi's autocomplete provider
//! (`vendor/pi/packages/tui/src/autocomplete.ts`).
//!
//! Ports `CombinedAutocompleteProvider` — the `@`-file / path / slash-command
//! completion engine used by the editor. The pure logic (prefix extraction,
//! path parsing, scoring, ranking, quoting, completion application, and the
//! `fd` argument builder + output parser) is reproduced line-for-line from pi.
//!
//! The two host operations pi performs — walking the filesystem
//! (`readdirSync`/`statSync`, `os.homedir()`) and shelling out to the `fd`
//! binary (`spawn`) — sit behind the [`FileProvider`] seam so the pure logic is
//! testable with an injected deterministic provider, exactly as pi's own tests
//! use real temp dirs and the `fd` binary. Reuses the already-ported
//! [`crate::fuzzy::fuzzy_filter`] for slash-command filtering.
//!
//! String-offset note: every input in pi's autocomplete corpus (lines, queries,
//! filesystem paths) is ASCII, so string offsets (`cursorCol`, `prefix.length`,
//! `item.value.length`) are treated as `char` offsets, which coincide with pi's
//! JavaScript UTF-16 offsets for ASCII. Non-ASCII autocomplete input is outside
//! pi's tested contract for this module.

use crate::fuzzy::fuzzy_filter;

// ===========================================================================
// Host seam
// ===========================================================================

/// A directory entry as returned by `readdirSync(dir, { withFileTypes: true })`.
///
/// `is_directory` and `is_symbolic_link` reflect an `lstat`-style classification
/// (symlinks are *not* followed), matching Node's `Dirent`.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// The entry's base name.
    pub name: String,
    /// `entry.isDirectory()` (does not follow symlinks).
    pub is_directory: bool,
    /// `entry.isSymbolicLink()`.
    pub is_symbolic_link: bool,
}

/// The raw result of spawning `fd` (`spawn(fdPath, args)`): its captured stdout
/// and exit code (`null` when the process was killed before exiting).
#[derive(Debug, Clone)]
pub struct FdOutput {
    /// Everything `fd` wrote to stdout, decoded as UTF-8.
    pub stdout: String,
    /// The process exit code, or `None` if it did not exit normally.
    pub code: Option<i32>,
}

/// Error returned by a [`FileProvider`] filesystem call — mirrors a thrown
/// `readdirSync`/`statSync` exception (e.g. `ENOENT`), which pi catches.
#[derive(Debug, Clone)]
pub struct ProviderError;

/// The filesystem / process host seam. pi performs these operations inline via
/// `node:fs`, `node:os`, and `node:child_process`; modelling them as a trait
/// keeps the pure completion logic deterministic and testable.
pub trait FileProvider {
    /// `readdirSync(dir, { withFileTypes: true })`. Errors (e.g. missing
    /// directory) are surfaced as `Err` and caught by the caller, matching pi.
    fn read_dir(&self, dir: &str) -> Result<Vec<DirEntry>, ProviderError>;

    /// `statSync(path).isDirectory()` (follows symlinks). Errors (broken
    /// symlink, permission denied, missing path) are surfaced as `Err`.
    fn stat_is_directory(&self, path: &str) -> Result<bool, ProviderError>;

    /// `os.homedir()`.
    fn home_dir(&self) -> String;

    /// Spawn `fd` with `args` and collect its stdout / exit code.
    fn run_fd(&self, args: &[String]) -> FdOutput;
}

// ===========================================================================
// Data types
// ===========================================================================

/// A single autocomplete suggestion (`AutocompleteItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// The text inserted when the item is chosen.
    pub value: String,
    /// The label shown in the dropdown.
    pub label: String,
    /// Optional secondary description.
    pub description: Option<String>,
}

/// The result of [`CombinedAutocompleteProvider::get_suggestions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    /// The matching items.
    pub items: Vec<AutocompleteItem>,
    /// What the suggestions are matched against (e.g. `"/"` or `"src/"`).
    pub prefix: String,
}

/// The new document state produced by [`CombinedAutocompleteProvider::apply_completion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedCompletion {
    /// The updated lines.
    pub lines: Vec<String>,
    /// The cursor line after applying.
    pub cursor_line: usize,
    /// The cursor column after applying.
    pub cursor_col: usize,
}

/// A slash command (`SlashCommand`) that can supply argument completions.
pub struct SlashCommand {
    /// The command name (without leading `/`).
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Optional argument hint shown alongside the description.
    pub argument_hint: Option<String>,
    /// Argument-completion callback; returns `None` when unavailable.
    #[allow(clippy::type_complexity)]
    pub get_argument_completions: Option<Box<dyn Fn(&str) -> Option<Vec<AutocompleteItem>>>>,
}

/// A registered command — either a [`SlashCommand`] or a bare [`AutocompleteItem`].
pub enum Command {
    /// A slash command with optional argument completion.
    Slash(SlashCommand),
    /// A plain item (its `value` is used as the command name).
    Item(AutocompleteItem),
}

impl Command {
    fn name(&self) -> &str {
        match self {
            Command::Slash(c) => &c.name,
            Command::Item(i) => &i.value,
        }
    }
    fn description(&self) -> Option<&str> {
        match self {
            Command::Slash(c) => c.description.as_deref(),
            Command::Item(i) => i.description.as_deref(),
        }
    }
    fn argument_hint(&self) -> Option<&str> {
        match self {
            Command::Slash(c) => c.argument_hint.as_deref(),
            Command::Item(_) => None,
        }
    }
}

// ===========================================================================
// Module-level pure helpers (byte-exact ports)
// ===========================================================================

const PATH_DELIMITERS: [char; 5] = [' ', '\t', '"', '\'', '='];

fn is_path_delimiter(c: char) -> bool {
    PATH_DELIMITERS.contains(&c)
}

fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn escape_regex(value: &str) -> String {
    // pi: value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return normalized;
    }

    let has_trailing_separator = normalized.ends_with('/');
    // normalized.replace(/^\/+|\/+$/g, "")
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return normalized;
    }

    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(escape_regex)
        .collect();
    if segments.is_empty() {
        return normalized;
    }

    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

fn find_last_delimiter(text: &str) -> i64 {
    let chars: Vec<char> = text.chars().collect();
    for i in (0..chars.len()).rev() {
        if is_path_delimiter(chars[i]) {
            return i as i64;
        }
    }
    -1
}

fn find_unclosed_quote_start(text: &str) -> Option<i64> {
    let chars: Vec<char> = text.chars().collect();
    let mut in_quotes = false;
    let mut quote_start: i64 = -1;

    for (i, &c) in chars.iter().enumerate() {
        if c == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = i as i64;
            }
        }
    }

    if in_quotes {
        Some(quote_start)
    } else {
        None
    }
}

fn is_token_start(text: &str, index: i64) -> bool {
    if index == 0 {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let prev = if index >= 1 && ((index - 1) as usize) < chars.len() {
        chars[(index - 1) as usize]
    } else {
        // text[index - 1] ?? "" -> undefined coerced to "" is not a delimiter.
        return false;
    };
    is_path_delimiter(prev)
}

fn char_slice_from(text: &str, start: i64) -> String {
    if start <= 0 {
        return text.to_string();
    }
    text.chars().skip(start as usize).collect()
}

fn char_at(text: &str, index: i64) -> Option<char> {
    if index < 0 {
        return None;
    }
    text.chars().nth(index as usize)
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 && char_at(text, quote_start - 1) == Some('@') {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(char_slice_from(text, quote_start - 1));
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    Some(char_slice_from(text, quote_start))
}

struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

struct CompletionValueOptions {
    // pi's `buildCompletionValue` accepts `isDirectory` in its options object but
    // never reads it; kept here to mirror the source signature call-for-call.
    #[allow(dead_code)]
    is_directory: bool,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn build_completion_value(path: &str, options: &CompletionValueOptions) -> String {
    let needs_quotes = options.is_quoted_prefix || path.contains(' ');
    let prefix = if options.is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}

// ---------------------------------------------------------------------------
// Node posix `path` helpers (byte-exact ports of Node's implementation).
// ---------------------------------------------------------------------------

const CHAR_FORWARD_SLASH: u8 = b'/';
const CHAR_DOT: u8 = b'.';

fn is_posix_path_separator(code: u8) -> bool {
    code == CHAR_FORWARD_SLASH
}

// Node's internal `normalizeString`, posix specialization.
fn normalize_string(path: &str, allow_above_root: bool) -> String {
    let bytes = path.as_bytes();
    let mut res = String::new();
    let mut last_segment_length: i64 = 0;
    let mut last_slash: i64 = -1;
    let mut dots: i64 = 0;
    let mut code: u8 = 0;
    let len = bytes.len() as i64;

    let mut i: i64 = 0;
    while i <= len {
        if i < len {
            code = bytes[i as usize];
        } else if is_posix_path_separator(code) {
            break;
        } else {
            code = CHAR_FORWARD_SLASH;
        }

        if is_posix_path_separator(code) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                let rb = res.as_bytes();
                let rlen = rb.len();
                if rlen < 2
                    || last_segment_length != 2
                    || rb[rlen - 1] != CHAR_DOT
                    || rb[rlen - 2] != CHAR_DOT
                {
                    if rlen > 2 {
                        if let Some(last_slash_index) = res.rfind('/') {
                            res.truncate(last_slash_index);
                            last_segment_length = match res.rfind('/') {
                                Some(idx) => (res.len() as i64) - 1 - (idx as i64),
                                None => (res.len() as i64) - 1 - (-1),
                            };
                        } else {
                            res.clear();
                            last_segment_length = 0;
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if rlen != 0 {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push('/');
                    }
                    res.push_str("..");
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[((last_slash + 1) as usize)..(i as usize)];
                if !res.is_empty() {
                    res.push('/');
                    res.push_str(seg);
                } else {
                    res.push_str(seg);
                }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == CHAR_DOT && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }

    res
}

fn pnormalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let bytes = path.as_bytes();
    let is_absolute = bytes[0] == CHAR_FORWARD_SLASH;
    let trailing_separator = bytes[bytes.len() - 1] == CHAR_FORWARD_SLASH;

    let mut normalized = normalize_string(path, !is_absolute);
    if normalized.is_empty() {
        if is_absolute {
            return "/".to_string();
        }
        return if trailing_separator { "./" } else { "." }.to_string();
    }
    if trailing_separator {
        normalized.push('/');
    }
    if is_absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

fn pjoin(a: &str, b: &str) -> String {
    // Node posix.join with two args.
    let mut joined: Option<String> = None;
    for arg in [a, b] {
        if !arg.is_empty() {
            match &mut joined {
                None => joined = Some(arg.to_string()),
                Some(j) => {
                    j.push('/');
                    j.push_str(arg);
                }
            }
        }
    }
    match joined {
        None => ".".to_string(),
        Some(j) => pnormalize(&j),
    }
}

fn pdirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let bytes = path.as_bytes();
    let has_root = bytes[0] == CHAR_FORWARD_SLASH;
    let mut end: i64 = -1;
    let mut matched_slash = true;
    let mut i = (bytes.len() as i64) - 1;
    while i >= 1 {
        if bytes[i as usize] == CHAR_FORWARD_SLASH {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }

    if end == -1 {
        return if has_root { "/" } else { "." }.to_string();
    }
    if has_root && end == 1 {
        return "//".to_string();
    }
    path[..(end as usize)].to_string()
}

fn pbasename(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut start: usize = 0;
    let mut end: i64 = -1;
    let mut matched_slash = true;
    let mut i = (bytes.len() as i64) - 1;
    while i >= 0 {
        if bytes[i as usize] == CHAR_FORWARD_SLASH {
            if !matched_slash {
                start = (i + 1) as usize;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }

    if end == -1 {
        return String::new();
    }
    path[start..(end as usize)].to_string()
}

// ---------------------------------------------------------------------------
// fd walk (arg building + output parsing; the spawn sits behind the seam).
// ---------------------------------------------------------------------------

struct FdEntry {
    path: String,
    is_directory: bool,
}

fn walk_directory_with_fd(
    provider: &dyn FileProvider,
    base_dir: &str,
    query: &str,
    max_results: usize,
) -> Vec<FdEntry> {
    let mut args: Vec<String> = vec![
        "--base-directory".into(),
        base_dir.to_string(),
        "--max-results".into(),
        max_results.to_string(),
        "--type".into(),
        "f".into(),
        "--type".into(),
        "d".into(),
        "--follow".into(),
        "--hidden".into(),
        "--exclude".into(),
        ".git".into(),
        "--exclude".into(),
        ".git/*".into(),
        "--exclude".into(),
        ".git/**".into(),
    ];

    if to_display_path(query).contains('/') {
        args.push("--full-path".into());
    }

    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }

    let out = provider.run_fd(&args);
    if out.code != Some(0) || out.stdout.is_empty() {
        return Vec::new();
    }

    // stdout.trim().split("\n").filter(Boolean)
    let lines: Vec<&str> = out
        .stdout
        .trim()
        .split('\n')
        .filter(|l| !l.is_empty())
        .collect();

    let mut results: Vec<FdEntry> = Vec::new();
    for line in lines {
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            display_line[..display_line.len() - 1].to_string()
        } else {
            display_line.clone()
        };
        if normalized_path == ".git"
            || normalized_path.starts_with(".git/")
            || normalized_path.contains("/.git/")
        {
            continue;
        }

        results.push(FdEntry {
            path: display_line,
            is_directory: has_trailing_separator,
        });
    }

    results
}

// ===========================================================================
// CombinedAutocompleteProvider
// ===========================================================================

/// Combined provider that handles both slash commands and file paths — a
/// byte-exact port of pi's `CombinedAutocompleteProvider`.
pub struct CombinedAutocompleteProvider<P: FileProvider> {
    commands: Vec<Command>,
    base_path: String,
    fd_path: Option<String>,
    provider: P,
}

struct ScopedFuzzyQuery {
    base_dir: String,
    query: String,
    display_base: String,
}

impl<P: FileProvider> CombinedAutocompleteProvider<P> {
    /// Construct a provider. `fd_path` is `Some` when the `fd` binary is
    /// available (gating the fuzzy `@` file walk), `None` otherwise.
    pub fn new(
        commands: Vec<Command>,
        base_path: impl Into<String>,
        fd_path: Option<String>,
        provider: P,
    ) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
            fd_path,
            provider,
        }
    }

    /// Get autocomplete suggestions for the current text/cursor position.
    /// Returns `None` when no suggestions are available.
    pub fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let empty = String::new();
        let current_line = lines.get(cursor_line).unwrap_or(&empty);
        let text_before_cursor = slice_to(current_line, cursor_col);

        let at_prefix = self.extract_at_prefix(&text_before_cursor);
        if let Some(at_prefix) = at_prefix {
            let parsed = parse_path_prefix(&at_prefix);
            let suggestions =
                self.get_fuzzy_file_suggestions(&parsed.raw_prefix, parsed.is_quoted_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        if !force && text_before_cursor.starts_with('/') {
            let space_index = index_of(&text_before_cursor, ' ');

            if space_index == -1 {
                let prefix = char_slice_from(&text_before_cursor, 1);
                // command items
                let command_items: Vec<CommandItem> = self
                    .commands
                    .iter()
                    .map(|cmd| {
                        let name = cmd.name().to_string();
                        let hint = cmd.argument_hint().filter(|h| !h.is_empty());
                        let desc = cmd.description().unwrap_or("").to_string();
                        let full_desc = match hint {
                            Some(h) => {
                                if !desc.is_empty() {
                                    format!("{h} — {desc}")
                                } else {
                                    h.to_string()
                                }
                            }
                            None => desc,
                        };
                        CommandItem {
                            name: name.clone(),
                            label: name,
                            description: if full_desc.is_empty() {
                                None
                            } else {
                                Some(full_desc)
                            },
                        }
                    })
                    .collect();

                let filtered = fuzzy_filter(command_items, &prefix, |item| item.name.clone());
                let items: Vec<AutocompleteItem> = filtered
                    .into_iter()
                    .map(|item| AutocompleteItem {
                        value: item.name,
                        label: item.label,
                        description: item.description,
                    })
                    .collect();

                if items.is_empty() {
                    return None;
                }

                return Some(AutocompleteSuggestions {
                    items,
                    prefix: text_before_cursor,
                });
            }

            let command_name = slice_range(&text_before_cursor, 1, space_index as usize);
            let argument_text = char_slice_from(&text_before_cursor, space_index + 1);

            let command = self
                .commands
                .iter()
                .find(|cmd| cmd.name() == command_name)?;

            let get_arg = match command {
                Command::Slash(c) => c.get_argument_completions.as_ref(),
                Command::Item(_) => None,
            }?;

            let argument_suggestions = get_arg(&argument_text)?;
            if argument_suggestions.is_empty() {
                return None;
            }

            return Some(AutocompleteSuggestions {
                items: argument_suggestions,
                prefix: argument_text,
            });
        }

        let path_match = self.extract_path_prefix(&text_before_cursor, force)?;

        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }

        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    /// Apply the selected item, returning the new lines and cursor position.
    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AppliedCompletion {
        let empty = String::new();
        let current_line = lines.get(cursor_line).unwrap_or(&empty);
        let prefix_len = char_len(prefix);
        let before_prefix = slice_to(current_line, cursor_col.saturating_sub(prefix_len));
        // Faithfully: currentLine.slice(0, cursorCol - prefix.length). In JS a
        // negative start clamps to 0; saturating_sub matches for cursorCol >=
        // prefix.length (always true here).
        let after_cursor = char_slice_from(current_line, cursor_col as i64);
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                char_slice_from(&after_cursor, 1)
            } else {
                after_cursor.clone()
            };

        let is_slash_command = prefix.starts_with('/')
            && before_prefix.trim().is_empty()
            && !char_slice_from(prefix, 1).contains('/');
        if is_slash_command {
            let new_line = format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;
            return AppliedCompletion {
                lines: new_lines,
                cursor_line,
                cursor_col: char_len(&before_prefix) + char_len(&item.value) + 2,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!(
                "{}{}{suffix}{adjusted_after_cursor}",
                before_prefix, item.value
            );
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;

            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                char_len(&item.value) - 1
            } else {
                char_len(&item.value)
            };
            return AppliedCompletion {
                lines: new_lines,
                cursor_line,
                cursor_col: char_len(&before_prefix) + cursor_offset + char_len(suffix),
            };
        }

        let text_before_cursor = slice_to(current_line, cursor_col);
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;

            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                char_len(&item.value) - 1
            } else {
                char_len(&item.value)
            };
            return AppliedCompletion {
                lines: new_lines,
                cursor_line,
                cursor_col: char_len(&before_prefix) + cursor_offset,
            };
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        let mut new_lines = lines.to_vec();
        new_lines[cursor_line] = new_line;

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            char_len(&item.value) - 1
        } else {
            char_len(&item.value)
        };
        AppliedCompletion {
            lines: new_lines,
            cursor_line,
            cursor_col: char_len(&before_prefix) + cursor_offset,
        }
    }

    // Extract @ prefix for fuzzy file suggestions.
    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        let quoted_prefix = extract_quoted_prefix(text);
        if let Some(ref q) = quoted_prefix {
            if q.starts_with("@\"") {
                return quoted_prefix;
            }
        }

        let last_delimiter_index = find_last_delimiter(text);
        let token_start = if last_delimiter_index == -1 {
            0
        } else {
            last_delimiter_index + 1
        };

        if char_at(text, token_start) == Some('@') {
            return Some(char_slice_from(text, token_start));
        }

        None
    }

    // Extract a path-like prefix from the text before cursor.
    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        let quoted_prefix = extract_quoted_prefix(text);
        if let Some(q) = quoted_prefix {
            return Some(q);
        }

        let last_delimiter_index = find_last_delimiter(text);
        let path_prefix = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            char_slice_from(text, last_delimiter_index + 1)
        };

        if force_extract {
            return Some(path_prefix);
        }

        if path_prefix.contains('/')
            || path_prefix.starts_with('.')
            || path_prefix.starts_with("~/")
        {
            return Some(path_prefix);
        }

        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }

        None
    }

    // Expand home directory (~/) to actual home path.
    fn expand_home_path(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let expanded_path = pjoin(&self.provider.home_dir(), rest);
            if path.ends_with('/') && !expanded_path.ends_with('/') {
                format!("{expanded_path}/")
            } else {
                expanded_path
            }
        } else if path == "~" {
            self.provider.home_dir()
        } else {
            path.to_string()
        }
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedFuzzyQuery> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;

        let display_base = normalized_query[..slash_index + 1].to_string();
        let query = normalized_query[slash_index + 1..].to_string();

        let base_dir = if display_base.starts_with("~/") {
            self.expand_home_path(&display_base)
        } else if display_base.starts_with('/') {
            display_base.clone()
        } else {
            pjoin(&self.base_path, &display_base)
        };

        match self.provider.stat_is_directory(&base_dir) {
            Ok(true) => {}
            _ => return None,
        }

        Some(ScopedFuzzyQuery {
            base_dir,
            query,
            display_base,
        })
    }

    fn scoped_path_for_display(&self, display_base: &str, relative_path: &str) -> String {
        let normalized_relative_path = to_display_path(relative_path);
        if display_base == "/" {
            return format!("/{normalized_relative_path}");
        }
        format!(
            "{}{}",
            to_display_path(display_base),
            normalized_relative_path
        )
    }

    // Get file/directory suggestions for a given path prefix.
    // pi keeps the `isRootPrefix` and `rawPrefix.endsWith("/")` cases as two
    // separate branches with identical bodies; the port mirrors that structure
    // for line-for-line traceability rather than collapsing them.
    #[allow(clippy::if_same_then_else)]
    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let raw_prefix = parsed.raw_prefix;
        let is_at_prefix = parsed.is_at_prefix;
        let is_quoted_prefix = parsed.is_quoted_prefix;
        let mut expanded_prefix = raw_prefix.clone();

        if expanded_prefix.starts_with('~') {
            expanded_prefix = self.expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (is_at_prefix && raw_prefix.is_empty());

        let search_dir: String;
        let search_prefix: String;

        if is_root_prefix {
            if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                search_dir = expanded_prefix.clone();
            } else {
                search_dir = pjoin(&self.base_path, &expanded_prefix);
            }
            search_prefix = String::new();
        } else if raw_prefix.ends_with('/') {
            if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                search_dir = expanded_prefix.clone();
            } else {
                search_dir = pjoin(&self.base_path, &expanded_prefix);
            }
            search_prefix = String::new();
        } else {
            let dir = pdirname(&expanded_prefix);
            let file = pbasename(&expanded_prefix);
            if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                search_dir = dir;
            } else {
                search_dir = pjoin(&self.base_path, &dir);
            }
            search_prefix = file;
        }

        let entries = match self.provider.read_dir(&search_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        let search_prefix_lower = search_prefix.to_lowercase();

        for entry in &entries {
            if !entry.name.to_lowercase().starts_with(&search_prefix_lower) {
                continue;
            }

            let mut is_directory = entry.is_directory;
            if !is_directory && entry.is_symbolic_link {
                let full_path = pjoin(&search_dir, &entry.name);
                if let Ok(true) = self.provider.stat_is_directory(&full_path) {
                    is_directory = true;
                }
                // Broken symlink / permission error -> treat as file (Err -> no change).
            }

            let name = &entry.name;
            let display_prefix = &raw_prefix;
            let mut relative_path: String;

            if display_prefix.ends_with('/') {
                relative_path = format!("{display_prefix}{name}");
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if let Some(home_relative_dir) = display_prefix.strip_prefix("~/") {
                    let dir = pdirname(home_relative_dir);
                    relative_path = if dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", pjoin(&dir, name))
                    };
                } else if display_prefix.starts_with('/') {
                    let dir = pdirname(display_prefix);
                    relative_path = if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    };
                } else {
                    relative_path = pjoin(&pdirname(display_prefix), name);
                    if display_prefix.starts_with("./") && !relative_path.starts_with("./") {
                        relative_path = format!("./{relative_path}");
                    }
                }
            } else if display_prefix.starts_with('~') {
                relative_path = format!("~/{name}");
            } else {
                relative_path = name.clone();
            }

            relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path.clone()
            };
            let value = build_completion_value(
                &path_value,
                &CompletionValueOptions {
                    is_directory,
                    is_at_prefix,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        // Sort directories first, then alphabetically.
        suggestions.sort_by(|a, b| {
            let a_is_dir = a.value.ends_with('/');
            let b_is_dir = b.value.ends_with('/');
            if a_is_dir && !b_is_dir {
                return std::cmp::Ordering::Less;
            }
            if !a_is_dir && b_is_dir {
                return std::cmp::Ordering::Greater;
            }
            locale_compare(&a.label, &b.label)
        });

        suggestions
    }

    // Score an entry against the query (higher = better match).
    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i64 {
        let file_name = pbasename(file_path);
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score: i64 = 0;

        if lower_file_name == lower_query {
            score = 100;
        } else if lower_file_name.starts_with(&lower_query) {
            score = 80;
        } else if lower_file_name.contains(&lower_query) {
            score = 50;
        } else if file_path.to_lowercase().contains(&lower_query) {
            score = 30;
        }

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    // Fuzzy file search using fd.
    fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        is_quoted_prefix: bool,
    ) -> Vec<AutocompleteItem> {
        if self.fd_path.is_none() {
            return Vec::new();
        }

        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let fd_base_dir = scoped_query
            .as_ref()
            .map(|s| s.base_dir.clone())
            .unwrap_or_else(|| self.base_path.clone());
        let fd_query = scoped_query
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_else(|| query.to_string());

        let entries = walk_directory_with_fd(&self.provider, &fd_base_dir, &fd_query, 100);

        // scored + filtered
        let mut scored: Vec<(FdEntry, i64)> = entries
            .into_iter()
            .map(|entry| {
                let score = if !fd_query.is_empty() {
                    self.score_entry(&entry.path, &fd_query, entry.is_directory)
                } else {
                    1
                };
                (entry, score)
            })
            .filter(|(_, score)| *score > 0)
            .collect();

        // scoredEntries.sort((a, b) => b.score - a.score) — stable descending.
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let top_entries: Vec<(FdEntry, i64)> = scored.into_iter().take(20).collect();

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for (entry, _score) in top_entries {
            let entry_path = entry.path;
            let is_directory = entry.is_directory;
            let path_without_slash = if is_directory {
                entry_path[..entry_path.len() - 1].to_string()
            } else {
                entry_path.clone()
            };
            let display_path = match &scoped_query {
                Some(s) => self.scoped_path_for_display(&s.display_base, &path_without_slash),
                None => path_without_slash.clone(),
            };
            let entry_name = pbasename(&path_without_slash);
            let completion_path = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            let value = build_completion_value(
                &completion_path,
                &CompletionValueOptions {
                    is_directory,
                    is_at_prefix: true,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{entry_name}{}", if is_directory { "/" } else { "" }),
                description: Some(display_path),
            });
        }

        suggestions
    }

    /// Whether file completion should trigger on an explicit Tab press.
    pub fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let empty = String::new();
        let current_line = lines.get(cursor_line).unwrap_or(&empty);
        let text_before_cursor = slice_to(current_line, cursor_col);

        let trimmed = text_before_cursor.trim();
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            return false;
        }

        true
    }
}

// Intermediate command item shape used for slash-command fuzzy filtering.
struct CommandItem {
    name: String,
    label: String,
    description: Option<String>,
}

// ===========================================================================
// Small JS-string helpers (char-based; ASCII corpus — see module note).
// ===========================================================================

fn char_len(s: &str) -> usize {
    s.chars().count()
}

// s.slice(0, end) with end as a char count.
fn slice_to(s: &str, end: usize) -> String {
    s.chars().take(end).collect()
}

// s.slice(start, end) with start/end as char counts.
fn slice_range(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

// s.indexOf(ch) in char units, or -1.
fn index_of(s: &str, ch: char) -> i64 {
    match s.chars().position(|c| c == ch) {
        Some(i) => i as i64,
        None => -1,
    }
}

// String.prototype.localeCompare with the default locale. For the ASCII
// filenames in pi's autocomplete corpus this coincides with byte ordering;
// ordering over arbitrary system-root entries is not part of pi's contract.
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    a.cmp(b)
}
