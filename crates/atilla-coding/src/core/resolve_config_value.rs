//! Resolve configuration values that may be shell commands, environment
//! variables, or literals.
//!
//! Ported from pi's `core/resolve-config-value.ts`. Used by `auth-storage` and
//! `model-registry` to turn a stored config string (an API key, a header value,
//! etc.) into its actual value:
//!
//! - A value starting with `!` is a shell command; its trimmed stdout is used
//!   (successful and failed results are cached for the process lifetime).
//! - `$ENV_VAR` and `${ENV_VAR}` references interpolate the named environment
//!   variable, preferring a caller-supplied credential-scoped `env` map over the
//!   process environment.
//! - In non-command values, `$$` escapes a literal `$` and `$!` escapes a
//!   literal `!`.
//! - Anything else is a literal.
//!
//! NOTE: pi's Windows-only configured-shell / stdin transport (`getShellConfig`,
//! `executeWithConfiguredShell`) is not ported here — the crate's `utils::shell`
//! process layer is still deferred, so command execution always uses the default
//! `sh -c` path (pi's Unix behavior). The `execSync` 10s timeout is likewise not
//! reproduced; a command that never exits would block, but no config value in
//! practice does. Both are environment-shaped and out of scope for this port.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex, OnceLock};

use regex::Regex;

/// Cache of shell-command results, keyed by the full `!command` config string.
/// Persists for the process lifetime, mirroring pi's `commandResultCache`.
static COMMAND_RESULT_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn env_var_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid env-var-name regex"))
}

fn env_var_name_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*").expect("valid env-var-name-prefix regex")
    })
}

/// One segment of a parsed non-command config value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

/// A config value parsed into either a shell command or an interpolation
/// template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigValueReference {
    /// A `!`-prefixed shell command; carries the full config string (including
    /// the leading `!`), matching pi's `{ type: "command", config }`.
    Command(String),
    Template(Vec<TemplatePart>),
}

/// Append a literal, coalescing with a trailing literal part (and dropping the
/// empty string), exactly like pi's `appendLiteral`.
fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(prev)) = parts.last_mut() {
        prev.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_string()));
}

fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    // `$`, `{`, `}`, `!` are all single-byte ASCII, so byte indices used for
    // scanning always fall on char boundaries; multibyte literals are copied
    // verbatim inside the slices.
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut index = 0usize;

    while index < config.len() {
        let Some(rel) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar_index = index + rel;
        append_literal(&mut parts, &config[index..dollar_index]);
        let next_char = config[dollar_index + 1..].chars().next();

        match next_char {
            Some('$') | Some('!') => {
                let ch = next_char.unwrap();
                append_literal(&mut parts, ch.encode_utf8(&mut [0u8; 1]));
                index = dollar_index + 2;
            }
            Some('{') => {
                let search_start = dollar_index + 2;
                match config[search_start..].find('}') {
                    None => {
                        append_literal(&mut parts, "$");
                        index = dollar_index + 1;
                    }
                    Some(rel_end) => {
                        let end_index = search_start + rel_end;
                        let name = &config[search_start..end_index];
                        if env_var_name_re().is_match(name) {
                            parts.push(TemplatePart::Env(name.to_string()));
                        } else {
                            append_literal(&mut parts, &config[dollar_index..end_index + 1]);
                        }
                        index = end_index + 1;
                    }
                }
            }
            _ => {
                let rest = &config[dollar_index + 1..];
                if let Some(m) = env_var_name_prefix_re().find(rest) {
                    let name = m.as_str();
                    parts.push(TemplatePart::Env(name.to_string()));
                    index = dollar_index + 1 + name.len();
                } else {
                    append_literal(&mut parts, "$");
                    index = dollar_index + 1;
                }
            }
        }
    }

    parts
}

fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        ConfigValueReference::Command(config.to_string())
    } else {
        ConfigValueReference::Template(parse_config_value_template(config))
    }
}

fn resolve_env_config_value(name: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    // Mirror pi's `env?.[name] || process.env[name] || undefined`: an empty
    // string is falsy and falls through to the next source.
    if let Some(value) = env.and_then(|map| map.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part {
            if !names.iter().any(|n| n == name) {
                names.push(name.clone());
            }
        }
    }
    names
}

fn resolve_template(
    parts: &[TemplatePart],
    env: Option<&HashMap<String, String>>,
) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => {
                let value = resolve_env_config_value(name, env)?;
                resolved.push_str(&value);
            }
        }
    }
    Some(resolved)
}

/// If the whole config value is a single `$VAR` / `${VAR}` reference, return the
/// variable name; otherwise `None`.
pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => match parts.as_slice() {
            [TemplatePart::Env(name)] => Some(name.clone()),
            _ => None,
        },
        ConfigValueReference::Command(_) => None,
    }
}

/// All distinct environment variable names referenced by a config value (empty
/// for command values), in first-seen order.
pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => template_env_var_names(&parts),
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

/// The referenced environment variable names that do not currently resolve.
pub fn get_missing_config_value_env_var_names(
    config: &str,
    env: Option<&HashMap<String, String>>,
) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

/// Whether a config value is a `!`-prefixed shell command.
pub fn is_command_config_value(config: &str) -> bool {
    matches!(
        parse_config_value_reference(config),
        ConfigValueReference::Command(_)
    )
}

/// Whether every environment variable a config value references is set.
pub fn is_config_value_configured(config: &str, env: Option<&HashMap<String, String>>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

/// Resolve a config value, executing (and caching) shell commands and
/// interpolating environment references. Returns `None` when resolution fails.
pub fn resolve_config_value(config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command_config) => execute_command(&command_config),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// Like [`resolve_config_value`], but re-executes shell commands on every call
/// instead of consulting the cache.
pub fn resolve_config_value_uncached(
    config: &str,
    env: Option<&HashMap<String, String>>,
) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command_config) => execute_command_uncached(&command_config),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// Error raised when a config value cannot be resolved, mirroring the messages
/// pi throws from `resolveConfigValueOrThrow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValueError(pub String);

impl std::fmt::Display for ConfigValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigValueError {}

/// Resolve a config value or produce a descriptive error. Commands are executed
/// uncached (matching pi's `resolveConfigValueUncached` call).
pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<String, ConfigValueError> {
    if let Some(resolved) = resolve_config_value_uncached(config, env) {
        return Ok(resolved);
    }

    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command_config) => Err(ConfigValueError(format!(
            "Failed to resolve {description} from shell command: {}",
            &command_config[1..]
        ))),
        ConfigValueReference::Template(_) => {
            let missing = get_missing_config_value_env_var_names(config, env);
            match missing.len() {
                1 => Err(ConfigValueError(format!(
                    "Failed to resolve {description} from environment variable: {}",
                    missing[0]
                ))),
                n if n > 1 => Err(ConfigValueError(format!(
                    "Failed to resolve {description} from environment variables: {}",
                    missing.join(", ")
                ))),
                _ => Err(ConfigValueError(format!("Failed to resolve {description}"))),
            }
        }
    }
}

/// Resolve every header value using the same logic as [`resolve_config_value`],
/// dropping entries that resolve empty. Returns `None` when nothing resolves.
pub fn resolve_headers(
    headers: Option<&HashMap<String, String>>,
    env: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let headers = headers?;
    let mut resolved = HashMap::new();
    for (key, value) in headers {
        if let Some(resolved_value) = resolve_config_value(value, env) {
            if !resolved_value.is_empty() {
                resolved.insert(key.clone(), resolved_value);
            }
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// Like [`resolve_headers`], but fails with a descriptive error if any header
/// value cannot be resolved.
pub fn resolve_headers_or_throw(
    headers: Option<&HashMap<String, String>>,
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<Option<HashMap<String, String>>, ConfigValueError> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved = HashMap::new();
    for (key, value) in headers {
        let resolved_value =
            resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), env)?;
        resolved.insert(key.clone(), resolved_value);
    }
    Ok(if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    })
}

/// Clear the config-value command cache. Exposed for testing.
pub fn clear_config_value_cache() {
    COMMAND_RESULT_CACHE
        .lock()
        .expect("command cache mutex poisoned")
        .clear();
}

/// Execute a `!command` (the config string, including the leading `!`) through
/// the default shell, returning trimmed stdout or `None` on failure/empty.
fn execute_with_default_shell(command: &str) -> Option<String> {
    let output = Command::new("sh").arg("-c").arg(command).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn execute_command_uncached(command_config: &str) -> Option<String> {
    // Strip the leading `!`.
    let command = &command_config[1..];
    execute_with_default_shell(command)
}

fn execute_command(command_config: &str) -> Option<String> {
    {
        let cache = COMMAND_RESULT_CACHE
            .lock()
            .expect("command cache mutex poisoned");
        if let Some(cached) = cache.get(command_config) {
            return cached.clone();
        }
    }
    let result = execute_command_uncached(command_config);
    COMMAND_RESULT_CACHE
        .lock()
        .expect("command cache mutex poisoned")
        .insert(command_config.to_string(), result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Serializes tests that mutate process-global environment variables and the
    /// shared command cache so they do not race one another.
    static ENV_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn resolves_literals_environment_templates_and_escapes() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        std::env::set_var("TEST_CONFIG_LEFT", "left");
        std::env::set_var("TEST_CONFIG_RIGHT", "right");

        assert_eq!(
            resolve_config_value("literal-key", None).as_deref(),
            Some("literal-key")
        );
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_LEFT", None).as_deref(),
            Some("left")
        );
        assert_eq!(
            resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", None).as_deref(),
            Some("left_right")
        );
        assert_eq!(
            resolve_config_value("$$TEST_CONFIG_LEFT", None).as_deref(),
            Some("$TEST_CONFIG_LEFT")
        );
        assert_eq!(
            resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", None).as_deref(),
            Some("!literal-right")
        );

        std::env::remove_var("TEST_CONFIG_LEFT");
        std::env::remove_var("TEST_CONFIG_RIGHT");
    }

    #[test]
    fn uses_credential_scoped_environment_before_process_env() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        std::env::set_var("TEST_CONFIG_SCOPED", "process");

        let scoped = env_map(&[("TEST_CONFIG_SCOPED", "credential")]);
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_SCOPED", Some(&scoped)).as_deref(),
            Some("credential")
        );

        std::env::remove_var("TEST_CONFIG_SCOPED");
    }

    #[test]
    fn executes_shell_commands_and_trims_their_output() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        assert_eq!(
            resolve_config_value("!echo '  spaced-key  '", None).as_deref(),
            Some("spaced-key")
        );
        assert_eq!(
            resolve_config_value("!printf 'line1\\nline2'", None).as_deref(),
            Some("line1\nline2")
        );
        assert_eq!(
            resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
            Some("hello-world")
        );
    }

    #[test]
    fn returns_none_when_command_resolution_fails() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        for command in ["!exit 1", "!nonexistent-command-12345", "!printf ''"] {
            assert_eq!(
                resolve_config_value(command, None),
                None,
                "expected {command} to resolve to None"
            );
        }
    }

    #[test]
    fn caches_successful_and_failed_commands_until_cleared() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        let dir = std::env::temp_dir().join(format!(
            "atilla-config-value-{}-{}",
            std::process::id(),
            "cache"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("counter");
        std::fs::write(&counter, "0").unwrap();
        let path = counter.to_string_lossy().replace('"', "\\\"");
        let success = format!(
            "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
        );

        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "1");

        clear_config_value_cache();
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");

        let failure =
            format!("!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; exit 1'");
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "3");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn does_not_cache_environment_values() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        std::env::set_var("TEST_CONFIG_DYNAMIC", "first");
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
            Some("first")
        );
        std::env::set_var("TEST_CONFIG_DYNAMIC", "second");
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
            Some("second")
        );
        std::env::remove_var("TEST_CONFIG_DYNAMIC");
    }

    #[test]
    fn uncached_resolution_executes_a_command_on_every_call() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        let dir = std::env::temp_dir().join(format!(
            "atilla-config-value-{}-{}",
            std::process::id(),
            "uncached"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("uncached-counter");
        std::fs::write(&counter, "0").unwrap();
        let path = counter.to_string_lossy().replace('"', "\\\"");
        let command = format!(
            "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
        );

        assert_eq!(
            resolve_config_value_uncached(&command, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value_uncached(&command, None).as_deref(),
            Some("value")
        );
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");

        std::fs::remove_dir_all(&dir).ok();
    }

    // NOTE: pi's "uses stdin when the configured Windows shell requires it" test
    // mocks `process.platform = "win32"` and `getShellConfig` to exercise the
    // configured-shell / stdin transport. That branch is not ported (see the
    // module NOTE), so the assertion is intentionally skipped as environment-
    // shaped.

    #[test]
    fn parses_env_var_name_helpers() {
        assert_eq!(
            get_config_value_env_var_name("$FOO").as_deref(),
            Some("FOO")
        );
        assert_eq!(
            get_config_value_env_var_name("${FOO}").as_deref(),
            Some("FOO")
        );
        assert_eq!(get_config_value_env_var_name("literal"), None);
        assert_eq!(get_config_value_env_var_name("$FOO$BAR"), None);
        assert_eq!(get_config_value_env_var_name("!echo hi"), None);

        // `${FOO}` is a braced ref; the `_` after it is a literal separator. In
        // `$BAR_`, the bare-ref regex is greedy over word chars (`_` included),
        // so the name is `BAR_`, not `BAR`. `$FOO` is already seen and deduped.
        assert_eq!(
            get_config_value_env_var_names("${FOO}_$BAR_$FOO"),
            vec!["FOO".to_string(), "BAR_".to_string()]
        );
        assert!(get_config_value_env_var_names("!echo hi").is_empty());
    }

    #[test]
    fn command_and_configured_detection() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("TEST_CONFIG_SET", "value");
        assert!(is_command_config_value("!echo hi"));
        assert!(!is_command_config_value("$FOO"));
        assert!(is_config_value_configured("$TEST_CONFIG_SET", None));
        assert!(!is_config_value_configured("$TEST_CONFIG_UNSET_XYZ", None));
        assert_eq!(
            get_missing_config_value_env_var_names("$TEST_CONFIG_UNSET_XYZ", None),
            vec!["TEST_CONFIG_UNSET_XYZ".to_string()]
        );
        std::env::remove_var("TEST_CONFIG_SET");
    }

    #[test]
    fn or_throw_reports_missing_environment_variables() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        let err = resolve_config_value_or_throw("$MISSING_ONE_XYZ", "API key", None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to resolve API key from environment variable: MISSING_ONE_XYZ"
        );
        let err = resolve_config_value_or_throw("$MISSING_A_XYZ$MISSING_B_XYZ", "API key", None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to resolve API key from environment variables: MISSING_A_XYZ, MISSING_B_XYZ"
        );
        let err = resolve_config_value_or_throw("!exit 1", "token", None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to resolve token from shell command: exit 1"
        );
    }

    #[test]
    fn resolve_headers_drops_unresolved_and_returns_none_when_empty() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_config_value_cache();
        std::env::set_var("TEST_HEADER_TOKEN", "secret");
        let headers = env_map(&[
            ("Authorization", "$TEST_HEADER_TOKEN"),
            ("X-Missing", "$TEST_HEADER_MISSING_XYZ"),
        ]);
        let resolved = resolve_headers(Some(&headers), None).unwrap();
        assert_eq!(
            resolved.get("Authorization").map(String::as_str),
            Some("secret")
        );
        assert!(!resolved.contains_key("X-Missing"));

        let none_headers = env_map(&[("X-Missing", "$TEST_HEADER_MISSING_XYZ")]);
        assert!(resolve_headers(Some(&none_headers), None).is_none());
        assert!(resolve_headers(None, None).is_none());
        std::env::remove_var("TEST_HEADER_TOKEN");
    }
}
