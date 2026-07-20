//! `pidgin-tui` — a bit-exact Rust port of pi's TUI pure-function layer.
//!
//! Part 1 ports the width module from `vendor/pi/packages/tui/src/utils.ts`.
//! Part 2 ports the key parser from `vendor/pi/packages/tui/src/keys.ts`.
//! Correctness means byte-identical results versus pi: pi's inline renderer
//! crashes on any width mismatch, so the port is validated against vectors
//! extracted from pi itself (see `tests/width_vectors.rs` and
//! `tests/keys_vectors.rs`).

pub mod app;
pub mod autocomplete;
pub mod components;
mod eaw_table;
pub mod editor;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod kill_ring;
pub mod markdown;
pub mod overlay;
pub mod query_terminal;
pub mod renderer;
pub mod terminal;
pub mod terminal_colors;
pub mod terminal_image;
pub mod text_util;
pub mod undo_stack;
mod unicode_tables;
pub mod widgets;
pub mod width;
pub mod word_navigation;

pub use app::{mount_focused_editor, EditorView, LoopEvent, RunLoop, StdinReader};
pub use autocomplete::{
    AppliedCompletion, AutocompleteItem, AutocompleteSuggestions, CombinedAutocompleteProvider,
    Command, DirEntry, FdOutput, FileProvider, ProviderError, SlashCommand,
};
pub use components::{
    Input, SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext, SettingItem, SettingsList, SettingsListOptions,
    SettingsListTheme, SubmenuDone, SubmenuFactory,
};
pub use editor::{
    word_wrap_line, AutocompleteProvider, Cursor, Editor, EditorOptions, EditorTheme,
    SuggestionOutcome, TextChunk,
};
pub use fuzzy::{fuzzy_filter, fuzzy_filter_indices, fuzzy_match, FuzzyMatch};
pub use keybindings::{
    tui_keybindings, KeybindingConflict, KeybindingDefinition, KeybindingsManager,
};
pub use keys::{
    decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat,
    is_kitty_protocol_active, matches_key, parse_key, set_kitty_protocol_active, KeyEventType,
};
pub use kill_ring::{KillRing, PushOpts};
pub use markdown::{
    default_markdown_theme, markdown_render, DefaultTextStyle, Markdown, MarkdownOptions,
    MarkdownTheme, StyleFn,
};
pub use overlay::{
    ComponentId, MarginSpec, OverlayAnchor, OverlayHandleId, OverlayMargin, OverlayOptions,
    ReactionAction, SizeValue,
};
pub use renderer::{
    delete_kitty_image, is_image_line, Component, Container, InputListener, InputListenerResult,
    RenderError, SharedLines, Tui, CURSOR_MARKER,
};
pub use terminal::{
    enable_virtual_terminal_input, is_native_modifier_pressed, is_negotiation_sequence_prefix,
    normalize_apple_terminal_input, parse_negotiation_sequence, CrosstermTerminal, LoggingTerminal,
    ModifierKey, NegotiationSequence, ProcessTerminal, StdinBuffer, StdinBufferOptions, StdinEvent,
    Terminal, TerminalInput,
};
pub use terminal_colors::{
    is_osc11_background_color_response, parse_osc11_background_color, parse_osc_hex_channel,
    parse_terminal_color_scheme_report, RgbColor, TerminalColorScheme,
};
pub use terminal_image::{
    allocate_image_id, calculate_image_cell_size, calculate_image_rows, delete_all_kitty_images,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_image_dimensions, hyperlink, image_fallback, render_image, reset_capabilities_cache,
    set_capabilities, set_cell_dimensions, CellDimensions, ImageDimensions, ImageProtocol,
    TerminalCapabilities,
};
pub use text_util::{
    apply_background_to_line, is_punctuation_char, is_whitespace_char, word_segment, WordSegment,
};
pub use undo_stack::UndoStack;
pub use widgets::{
    truncated_text_render, BgFn, BoxWidget, Image, ImageOptions, ImageTheme, Loader,
    LoaderIndicatorOptions, Spacer, Text, TruncatedText,
};
pub use width::{
    extract_ansi_code, extract_segments, normalize_terminal_output, slice_by_column,
    slice_with_width, truncate_to_width, visible_width, wrap_text_with_ansi, ExtractSegments,
};
pub use word_navigation::{find_word_backward, find_word_forward, WordNavOptions};
