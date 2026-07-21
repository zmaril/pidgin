// straitjacket-allow-file:duplication — this module is a faithful, byte-for-byte
// mirror of pi's `parseArgs` argv loop (vendor/pi/packages/coding-agent/src/cli/
// args.ts), which is itself already ported to Rust in
// crates/pidgin-cli/src/cli/args.rs. pidgin-cli is a bin-only crate (no `lib`
// target), so its `parse_args` cannot be imported across the crate boundary; the
// flip re-ports the same pure argv loop here so the napi surface can drive it
// directly. The duplication is the parser's control flow (the long flag `else
// if` chain), which the straitjacket duplication rule flags against the
// pidgin-cli copy — both are deliberate, independently-compiled faithful ports
// of the single pi source of truth.
//
//! Node-API surface for pi's CLI argument parser (`cli/args.ts`).
//!
//! pi's `parseArgs(args: string[]): Args` is a manual argv loop (not a
//! declarative parser): it walks the token list once, consuming values for
//! value-taking flags, capturing unknown `--long` flags, collecting `@files`
//! and bare messages, and recording diagnostics. It is a pure function of its
//! input — no env, no argv access, no I/O — so it crosses the boundary cleanly.
//!
//! # Marshaling
//!
//! [`parse_args_native`] takes the argv vector and returns pi's `Args` object
//! serialized to a JSON string, in pi's exact camelCase field shape. The JS
//! shim parses it and rebuilds the one non-JSON member — `unknownFlags`, which
//! pi models as a `Map<string, boolean | string>` — from an ordered array of
//! `[key, value]` pairs (preserving insertion order like pi's `Map`).
//!
//! Every field pi's `Args` interface declares as optional (`field?: T`) is
//! *omitted* from the JSON object when unset, so reading it in JS yields
//! `undefined` — matching pi, whose `Args` has no `T | null` fields (absent
//! optionals are always `undefined`, never `null`). Boolean flags pi only ever
//! sets to `true` are emitted only when true; `projectTrustOverride`, the sole
//! tri-state, is emitted as `true`/`false`/absent for `--approve`/`--no-approve`
//! /neither. `listModels` crosses as its string search or the boolean `true`.

use indexmap::IndexMap;
use napi_derive::napi;
use serde_json::{Map, Value};

const VALID_THINKING_LEVELS: [&str; 7] =
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn is_valid_thinking_level(level: &str) -> bool {
    VALID_THINKING_LEVELS.contains(&level)
}

fn split_csv(value: &str) -> Vec<Value> {
    value
        .split(',')
        .map(|s| Value::String(s.trim().to_string()))
        .collect()
}

fn split_csv_nonempty(value: &str) -> Vec<Value> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(Value::String)
        .collect()
}

/// Faithful port of pi's `parseArgs`, producing pi's `Args` as a JSON value.
fn parse_args_value(args: &[String]) -> Value {
    let mut obj = Map::new();

    // Required members (always present in pi's Args).
    let mut messages: Vec<Value> = Vec::new();
    let mut file_args: Vec<Value> = Vec::new();
    // `unknownFlags` is pi's `Map<string, boolean | string>`; an IndexMap gives
    // JS-`Map` semantics (insert keeps position, re-set updates value in place).
    let mut unknown_flags: IndexMap<String, Value> = IndexMap::new();
    let mut diagnostics: Vec<Value> = Vec::new();

    // Optional accumulating arrays (pi lazily initializes then pushes).
    let mut append_system_prompt: Option<Vec<Value>> = None;
    let mut extensions: Option<Vec<Value>> = None;
    let mut skills: Option<Vec<Value>> = None;
    let mut prompt_templates: Option<Vec<Value>> = None;
    let mut themes: Option<Vec<Value>> = None;

    let n = args.len();
    let mut i = 0usize;
    while i < n {
        let arg = args[i].as_str();
        let has_next = i + 1 < n;

        if arg == "--help" || arg == "-h" {
            obj.insert("help".into(), Value::Bool(true));
        } else if arg == "--version" || arg == "-v" {
            obj.insert("version".into(), Value::Bool(true));
        } else if arg == "--mode" && has_next {
            i += 1;
            let mode = args[i].as_str();
            if mode == "text" || mode == "json" || mode == "rpc" {
                obj.insert("mode".into(), Value::String(mode.to_string()));
            }
        } else if arg == "--continue" || arg == "-c" {
            obj.insert("continue".into(), Value::Bool(true));
        } else if arg == "--resume" || arg == "-r" {
            obj.insert("resume".into(), Value::Bool(true));
        } else if arg == "--provider" && has_next {
            i += 1;
            obj.insert("provider".into(), Value::String(args[i].clone()));
        } else if arg == "--model" && has_next {
            i += 1;
            obj.insert("model".into(), Value::String(args[i].clone()));
        } else if arg == "--api-key" && has_next {
            i += 1;
            obj.insert("apiKey".into(), Value::String(args[i].clone()));
        } else if arg == "--system-prompt" && has_next {
            i += 1;
            obj.insert("systemPrompt".into(), Value::String(args[i].clone()));
        } else if arg == "--append-system-prompt" && has_next {
            i += 1;
            append_system_prompt
                .get_or_insert_with(Vec::new)
                .push(Value::String(args[i].clone()));
        } else if arg == "--name" || arg == "-n" {
            if has_next {
                i += 1;
                obj.insert("name".into(), Value::String(args[i].clone()));
            } else {
                diagnostics.push(diagnostic("error", "--name requires a value"));
            }
        } else if arg == "--no-session" {
            obj.insert("noSession".into(), Value::Bool(true));
        } else if arg == "--session" && has_next {
            i += 1;
            obj.insert("session".into(), Value::String(args[i].clone()));
        } else if arg == "--session-id" && has_next {
            i += 1;
            obj.insert("sessionId".into(), Value::String(args[i].clone()));
        } else if arg == "--fork" && has_next {
            i += 1;
            obj.insert("fork".into(), Value::String(args[i].clone()));
        } else if arg == "--session-dir" && has_next {
            i += 1;
            obj.insert("sessionDir".into(), Value::String(args[i].clone()));
        } else if arg == "--models" && has_next {
            i += 1;
            obj.insert("models".into(), Value::Array(split_csv(&args[i])));
        } else if arg == "--no-tools" || arg == "-nt" {
            obj.insert("noTools".into(), Value::Bool(true));
        } else if arg == "--no-builtin-tools" || arg == "-nbt" {
            obj.insert("noBuiltinTools".into(), Value::Bool(true));
        } else if (arg == "--tools" || arg == "-t") && has_next {
            i += 1;
            obj.insert("tools".into(), Value::Array(split_csv_nonempty(&args[i])));
        } else if (arg == "--exclude-tools" || arg == "-xt") && has_next {
            i += 1;
            obj.insert(
                "excludeTools".into(),
                Value::Array(split_csv_nonempty(&args[i])),
            );
        } else if arg == "--thinking" && has_next {
            i += 1;
            let level = args[i].clone();
            if is_valid_thinking_level(&level) {
                obj.insert("thinking".into(), Value::String(level));
            } else {
                diagnostics.push(diagnostic(
                    "warning",
                    &format!(
                        "Invalid thinking level \"{}\". Valid values: {}",
                        level,
                        VALID_THINKING_LEVELS.join(", ")
                    ),
                ));
            }
        } else if arg == "--print" || arg == "-p" {
            obj.insert("print".into(), Value::Bool(true));
            if let Some(next) = args.get(i + 1) {
                if !next.starts_with('@') && (!next.starts_with('-') || next.starts_with("---")) {
                    messages.push(Value::String(next.clone()));
                    i += 1;
                }
            }
        } else if arg == "--export" && has_next {
            i += 1;
            obj.insert("export".into(), Value::String(args[i].clone()));
        } else if (arg == "--extension" || arg == "-e") && has_next {
            i += 1;
            extensions
                .get_or_insert_with(Vec::new)
                .push(Value::String(args[i].clone()));
        } else if arg == "--no-extensions" || arg == "-ne" {
            obj.insert("noExtensions".into(), Value::Bool(true));
        } else if arg == "--skill" && has_next {
            i += 1;
            skills
                .get_or_insert_with(Vec::new)
                .push(Value::String(args[i].clone()));
        } else if arg == "--prompt-template" && has_next {
            i += 1;
            prompt_templates
                .get_or_insert_with(Vec::new)
                .push(Value::String(args[i].clone()));
        } else if arg == "--theme" && has_next {
            i += 1;
            themes
                .get_or_insert_with(Vec::new)
                .push(Value::String(args[i].clone()));
        } else if arg == "--no-skills" || arg == "-ns" {
            obj.insert("noSkills".into(), Value::Bool(true));
        } else if arg == "--no-prompt-templates" || arg == "-np" {
            obj.insert("noPromptTemplates".into(), Value::Bool(true));
        } else if arg == "--no-themes" {
            obj.insert("noThemes".into(), Value::Bool(true));
        } else if arg == "--no-context-files" || arg == "-nc" {
            obj.insert("noContextFiles".into(), Value::Bool(true));
        } else if arg == "--list-models" {
            // Next arg is a search pattern only if it is not a flag or file arg.
            if let Some(next) = args.get(i + 1) {
                if !next.starts_with('-') && !next.starts_with('@') {
                    i += 1;
                    obj.insert("listModels".into(), Value::String(next.clone()));
                } else {
                    obj.insert("listModels".into(), Value::Bool(true));
                }
            } else {
                obj.insert("listModels".into(), Value::Bool(true));
            }
        } else if arg == "--verbose" {
            obj.insert("verbose".into(), Value::Bool(true));
        } else if arg == "--approve" || arg == "-a" {
            obj.insert("projectTrustOverride".into(), Value::Bool(true));
        } else if arg == "--no-approve" || arg == "-na" {
            obj.insert("projectTrustOverride".into(), Value::Bool(false));
        } else if arg == "--offline" {
            obj.insert("offline".into(), Value::Bool(true));
        } else if let Some(stripped) = arg.strip_prefix('@') {
            file_args.push(Value::String(stripped.to_string()));
        } else if let Some(body) = arg.strip_prefix("--") {
            if let Some(eq_index) = body.find('=') {
                unknown_flags.insert(
                    body[..eq_index].to_string(),
                    Value::String(body[eq_index + 1..].to_string()),
                );
            } else {
                let flag_name = body.to_string();
                if let Some(next) = args.get(i + 1) {
                    if !next.starts_with('-') && !next.starts_with('@') {
                        unknown_flags.insert(flag_name, Value::String(next.clone()));
                        i += 1;
                    } else {
                        unknown_flags.insert(flag_name, Value::Bool(true));
                    }
                } else {
                    unknown_flags.insert(flag_name, Value::Bool(true));
                }
            }
        } else if arg.starts_with('-') && !arg.starts_with("--") {
            diagnostics.push(diagnostic("error", &format!("Unknown option: {arg}")));
        } else if !arg.starts_with('-') {
            messages.push(Value::String(arg.to_string()));
        }

        i += 1;
    }

    if let Some(v) = append_system_prompt {
        obj.insert("appendSystemPrompt".into(), Value::Array(v));
    }
    if let Some(v) = extensions {
        obj.insert("extensions".into(), Value::Array(v));
    }
    if let Some(v) = skills {
        obj.insert("skills".into(), Value::Array(v));
    }
    if let Some(v) = prompt_templates {
        obj.insert("promptTemplates".into(), Value::Array(v));
    }
    if let Some(v) = themes {
        obj.insert("themes".into(), Value::Array(v));
    }

    obj.insert("messages".into(), Value::Array(messages));
    obj.insert("fileArgs".into(), Value::Array(file_args));
    // Ordered [key, value] pairs; the shim rebuilds pi's `Map` from them.
    let unknown_pairs: Vec<Value> = unknown_flags
        .into_iter()
        .map(|(k, v)| Value::Array(vec![Value::String(k), v]))
        .collect();
    obj.insert("unknownFlags".into(), Value::Array(unknown_pairs));
    obj.insert("diagnostics".into(), Value::Array(diagnostics));

    Value::Object(obj)
}

fn diagnostic(kind: &str, message: &str) -> Value {
    let mut d = Map::new();
    d.insert("type".into(), Value::String(kind.to_string()));
    d.insert("message".into(), Value::String(message.to_string()));
    Value::Object(d)
}

/// pi's `parseArgs`: parse an argv token list into the `Args` object, returned
/// as a JSON string in pi's exact camelCase shape (see module docs for the
/// `unknownFlags` / optional-field marshaling contract).
#[napi(js_name = "parseArgsNative")]
pub fn parse_args_native(argv: Vec<String>) -> String {
    parse_args_value(&argv).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Value {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args_value(&owned)
    }

    #[test]
    fn version_flag() {
        assert_eq!(parse(&["--version"])["version"], Value::Bool(true));
        assert_eq!(parse(&["-v"])["version"], Value::Bool(true));
    }

    #[test]
    fn print_consumes_frontmatter_prompt() {
        let v = parse(&["-p", "---\ntitle: hello\n---\nSay hi."]);
        assert_eq!(v["print"], Value::Bool(true));
        assert_eq!(
            v["messages"],
            serde_json::json!(["---\ntitle: hello\n---\nSay hi."])
        );
        assert_eq!(v["unknownFlags"], serde_json::json!([]));
    }

    #[test]
    fn print_stops_at_options() {
        let v = parse(&["-p", "--provider", "openai", "Say hi."]);
        assert_eq!(v["provider"], Value::String("openai".into()));
        assert_eq!(v["messages"], serde_json::json!(["Say hi."]));
    }

    #[test]
    fn name_empty_value_preserved() {
        let v = parse(&["--name", ""]);
        assert_eq!(v["name"], Value::String(String::new()));
    }

    #[test]
    fn name_missing_value_diagnostic() {
        let v = parse(&["--name"]);
        assert_eq!(
            v["diagnostics"],
            serde_json::json!([{"type": "error", "message": "--name requires a value"}])
        );
        assert!(v.get("name").is_none());
    }

    #[test]
    fn unknown_flags_forms() {
        let v = parse(&["--unknown-flag", "message"]);
        assert_eq!(v["messages"], serde_json::json!([]));
        assert_eq!(
            v["unknownFlags"],
            serde_json::json!([["unknown-flag", "message"]])
        );

        let v = parse(&["--unknown-flag"]);
        assert_eq!(
            v["unknownFlags"],
            serde_json::json!([["unknown-flag", true]])
        );

        let v = parse(&["--unknown-flag=value"]);
        assert_eq!(
            v["unknownFlags"],
            serde_json::json!([["unknown-flag", "value"]])
        );
    }

    #[test]
    fn tools_filters_empty() {
        let v = parse(&["--tools", "read,bash"]);
        assert_eq!(v["tools"], serde_json::json!(["read", "bash"]));
    }

    #[test]
    fn no_approve_is_false() {
        assert_eq!(
            parse(&["--no-approve"])["projectTrustOverride"],
            Value::Bool(false)
        );
        assert_eq!(
            parse(&["--approve"])["projectTrustOverride"],
            Value::Bool(true)
        );
    }

    #[test]
    fn invalid_thinking_warns() {
        let v = parse(&["--thinking", "bogus"]);
        assert!(v.get("thinking").is_none());
        assert_eq!(v["diagnostics"][0]["type"], Value::String("warning".into()));
    }
}
