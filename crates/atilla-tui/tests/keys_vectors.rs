//! Drives the Rust key parser against vectors extracted from pi itself
//! (`crates/atilla-tui/vectors/gen/generate_keys.mjs`). Every assertion is
//! byte-identical: pi is the source of truth, and any disagreement is a bug in
//! the port, not the vectors.
//!
//! All key vectors run inside a single test function. pi's parser has two
//! pieces of ambient global state that change results — the Kitty protocol
//! flag and `isWindowsTerminalSession()` (derived from `WT_SESSION` / `SSH_*`
//! env vars). The port models the first as a settable global and reads the
//! second from the process environment, so these vectors must run serially
//! (setting that state per vector) rather than in parallel test threads.

use std::path::PathBuf;

use serde::Deserialize;

use atilla_tui::{
    decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat, matches_key,
    parse_key, set_kitty_protocol_active,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// Realize a desired isWindowsTerminalSession() result by setting WT_SESSION
// (with SSH_* cleared). Matches how the generator produced the `wt` flag.
fn set_env_for_wt(wt: bool) {
    for v in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
        std::env::remove_var(v);
    }
    if wt {
        std::env::set_var("WT_SESSION", "test-session");
    } else {
        std::env::remove_var("WT_SESSION");
    }
}

fn set_env_raw(env: &EnvSpec) {
    for (name, value) in [
        ("WT_SESSION", &env.wt_session),
        ("SSH_CONNECTION", &env.ssh_connection),
        ("SSH_CLIENT", &env.ssh_client),
        ("SSH_TTY", &env.ssh_tty),
    ] {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
}

#[derive(Deserialize)]
struct MatchesVec {
    input: String,
    #[serde(rename = "keyId")]
    key_id: String,
    kitty: bool,
    wt: bool,
    expected: bool,
}

#[derive(Deserialize)]
struct ParseVec {
    input: String,
    kitty: bool,
    wt: bool,
    expected: Option<String>,
}

#[derive(Deserialize)]
struct DecodeVec {
    input: String,
    expected: Option<String>,
}

#[derive(Deserialize)]
struct BoolVec {
    input: String,
    expected: bool,
}

#[derive(Deserialize)]
struct EnvSpec {
    #[serde(rename = "WT_SESSION")]
    wt_session: Option<String>,
    #[serde(rename = "SSH_CONNECTION")]
    ssh_connection: Option<String>,
    #[serde(rename = "SSH_CLIENT")]
    ssh_client: Option<String>,
    #[serde(rename = "SSH_TTY")]
    ssh_tty: Option<String>,
}

#[derive(Deserialize)]
struct WtVec {
    env: EnvSpec,
    expected: bool,
}

fn report(name: &str, total: usize, fails: Vec<String>) {
    if !fails.is_empty() {
        let shown: Vec<_> = fails.iter().take(30).cloned().collect();
        panic!(
            "{name}: {}/{total} vectors FAILED\n{}",
            fails.len(),
            shown.join("\n")
        );
    }
    eprintln!("{name}: {total}/{total} vectors passed");
}

#[test]
fn keys_vectors() {
    // --- matchesKey ------------------------------------------------------
    let matches: Vec<MatchesVec> = load("keys_matches_key");
    assert!(!matches.is_empty());
    let mut fails = Vec::new();
    for v in &matches {
        set_kitty_protocol_active(v.kitty);
        set_env_for_wt(v.wt);
        let got = matches_key(&v.input, &v.key_id);
        if got != v.expected {
            fails.push(format!(
                "matches_key({:?}, {:?}, kitty={}, wt={}) = {got}, want {}",
                v.input, v.key_id, v.kitty, v.wt, v.expected
            ));
        }
    }
    report("keys_matches_key", matches.len(), fails);

    // --- parseKey --------------------------------------------------------
    let parses: Vec<ParseVec> = load("keys_parse_key");
    let mut fails = Vec::new();
    for v in &parses {
        set_kitty_protocol_active(v.kitty);
        set_env_for_wt(v.wt);
        let got = parse_key(&v.input);
        if got != v.expected {
            fails.push(format!(
                "parse_key({:?}, kitty={}, wt={}) = {:?}, want {:?}",
                v.input, v.kitty, v.wt, got, v.expected
            ));
        }
    }
    report("keys_parse_key", parses.len(), fails);

    // --- decodeKittyPrintable (state independent) ------------------------
    let dk: Vec<DecodeVec> = load("keys_decode_kitty_printable");
    let mut fails = Vec::new();
    for v in &dk {
        let got = decode_kitty_printable(&v.input);
        if got != v.expected {
            fails.push(format!(
                "decode_kitty_printable({:?}) = {:?}, want {:?}",
                v.input, got, v.expected
            ));
        }
    }
    report("keys_decode_kitty_printable", dk.len(), fails);

    // --- decodePrintableKey (state independent) --------------------------
    let dp: Vec<DecodeVec> = load("keys_decode_printable_key");
    let mut fails = Vec::new();
    for v in &dp {
        let got = decode_printable_key(&v.input);
        if got != v.expected {
            fails.push(format!(
                "decode_printable_key({:?}) = {:?}, want {:?}",
                v.input, got, v.expected
            ));
        }
    }
    report("keys_decode_printable_key", dp.len(), fails);

    // --- isKeyRelease / isKeyRepeat --------------------------------------
    let rel: Vec<BoolVec> = load("keys_is_key_release");
    let mut fails = Vec::new();
    for v in &rel {
        let got = is_key_release(&v.input);
        if got != v.expected {
            fails.push(format!(
                "is_key_release({:?}) = {got}, want {}",
                v.input, v.expected
            ));
        }
    }
    report("keys_is_key_release", rel.len(), fails);

    let rep: Vec<BoolVec> = load("keys_is_key_repeat");
    let mut fails = Vec::new();
    for v in &rep {
        let got = is_key_repeat(&v.input);
        if got != v.expected {
            fails.push(format!(
                "is_key_repeat({:?}) = {got}, want {}",
                v.input, v.expected
            ));
        }
    }
    report("keys_is_key_repeat", rep.len(), fails);

    // --- isWindowsTerminalSession env derivation -------------------------
    // Exercised through matchesKey("\x08", "ctrl+backspace") with kitty off,
    // which is true iff isWindowsTerminalSession(), matching the generator.
    let wt: Vec<WtVec> = load("keys_windows_terminal_session");
    let mut fails = Vec::new();
    set_kitty_protocol_active(false);
    for v in &wt {
        set_env_raw(&v.env);
        let got = matches_key("\x08", "ctrl+backspace");
        if got != v.expected {
            fails.push(format!(
                "windows_terminal_session env case = {got}, want {}",
                v.expected
            ));
        }
    }
    report("keys_windows_terminal_session", wt.len(), fails);

    // Restore deterministic defaults.
    set_kitty_protocol_active(false);
    set_env_for_wt(false);
}
