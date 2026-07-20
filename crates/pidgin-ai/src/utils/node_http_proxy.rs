// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `node-http-proxy.ts`: the http/https proxy-resolution arms are near-identical
// by design.
//! HTTP(S) proxy resolution, ported from pi-ai's
//! `packages/ai/src/utils/node-http-proxy.ts` at pinned commit `3da591ab`.
//!
//! [`resolve_http_proxy_url_for_target`] decides which proxy — if any — applies
//! to a target URL, honouring the standard `*_proxy` / `no_proxy` environment
//! variables (lower- and upper-case, scoped overrides first), and rejecting
//! non-HTTP(S) proxy schemes (SOCKS, PAC) that this transport cannot use.
//!
//! # URL parsing
//!
//! pi relies on the WHATWG `URL` object for parsing and normalization. The port
//! uses the [`url`] crate, which implements the same WHATWG standard, so target
//! parsing, default-port handling, and the proxy URL's `toString()` match pi
//! byte-for-byte (e.g. `http://proxy.example:8080` → `http://proxy.example:8080/`).
//!
//! # Error model
//!
//! pi throws for an unparseable proxy URL and for an unsupported proxy protocol.
//! The port returns [`Result`]: [`Ok(None)`] when no proxy applies, [`Ok(Some)`]
//! with the resolved proxy URL, or [`Err`] carrying a [`ProxyResolveError`] whose
//! `Display` mirrors pi's thrown messages (including
//! [`UNSUPPORTED_PROXY_PROTOCOL_MESSAGE`]).

use url::Url;

use super::provider_env::{get_provider_env_value, ProviderEnv};

/// pi's `UNSUPPORTED_PROXY_PROTOCOL_MESSAGE` (`node-http-proxy.ts:89`).
pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str =
    "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

/// Failure resolving a proxy URL (pi throws these as `Error`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyResolveError {
    /// The resolved proxy string could not be parsed as a URL.
    InvalidProxyUrl { proxy: String, reason: String },
    /// The proxy URL used a scheme other than `http`/`https`.
    UnsupportedProtocol { protocol: String },
}

impl std::fmt::Display for ProxyResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // pi: `Invalid proxy URL ${JSON.stringify(proxy)}: ${message}`.
            ProxyResolveError::InvalidProxyUrl { proxy, reason } => {
                write!(f, "Invalid proxy URL {proxy:?}: {reason}")
            }
            // pi: `${UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got ${protocol}`.
            ProxyResolveError::UnsupportedProtocol { protocol } => {
                write!(f, "{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {protocol}")
            }
        }
    }
}

impl std::error::Error for ProxyResolveError {}

/// pi's `DEFAULT_PROXY_PORTS` (`node-http-proxy.ts:4`); `0` means "unknown".
fn default_proxy_port(protocol: &str) -> u16 {
    match protocol {
        "ftp" => 21,
        "gopher" => 70,
        "http" => 80,
        "https" => 443,
        "ws" => 80,
        "wss" => 443,
        _ => 0,
    }
}

/// pi's `getProxyEnv` (`node-http-proxy.ts:13`): scoped lower/upper overrides,
/// then the process environment lower/upper, then `""`.
fn get_proxy_env(key: &str, env: Option<&ProviderEnv>) -> String {
    let lowercase_key = key.to_lowercase();
    let uppercase_key = key.to_uppercase();

    if let Some(value) = env.and_then(|env| env.get(&lowercase_key)) {
        if !value.is_empty() {
            return value.clone();
        }
    }
    if let Some(value) = env.and_then(|env| env.get(&uppercase_key)) {
        if !value.is_empty() {
            return value.clone();
        }
    }
    if let Some(value) = get_provider_env_value(&lowercase_key, None) {
        return value;
    }
    if let Some(value) = get_provider_env_value(&uppercase_key, None) {
        return value;
    }
    String::new()
}

/// Parse `host:port` per pi's `/^(.+):(\d+)$/`: a non-empty host, a colon, and an
/// all-digit port at the end.
fn parse_host_port(entry: &str) -> Option<(&str, u64)> {
    let idx = entry.rfind(':')?;
    let (host, rest) = (&entry[..idx], &entry[idx + 1..]);
    if host.is_empty() || rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u64>().ok().map(|port| (host, port))
}

/// pi's `shouldProxyHostname` (`node-http-proxy.ts:37`).
fn should_proxy_hostname(hostname: &str, port: u64, env: Option<&ProviderEnv>) -> bool {
    let no_proxy = get_proxy_env("no_proxy", env).to_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }

    no_proxy
        .split(|c: char| c == ',' || c.is_whitespace())
        .all(|entry| {
            if entry.is_empty() {
                return true;
            }
            let (mut proxy_hostname, proxy_port) = match parse_host_port(entry) {
                Some((host, port)) => (host, port),
                None => (entry, 0),
            };
            if proxy_port != 0 && proxy_port != port {
                return true;
            }
            if !(proxy_hostname.starts_with('.') || proxy_hostname.starts_with('*')) {
                return hostname != proxy_hostname;
            }
            if let Some(stripped) = proxy_hostname.strip_prefix('*') {
                proxy_hostname = stripped;
            }
            !hostname.ends_with(proxy_hostname)
        })
}

/// pi's `getProxyForUrl` (`node-http-proxy.ts:69`): resolve the proxy string for
/// a target URL, or `""` when none applies.
fn get_proxy_for_url(target_url: &str, env: Option<&ProviderEnv>) -> String {
    let parsed = match Url::parse(target_url) {
        Ok(url) => url,
        Err(_) => return String::new(),
    };
    let hostname = match parsed.host_str() {
        Some(host) if !host.is_empty() => host.to_string(),
        _ => return String::new(),
    };

    let protocol = parsed.scheme().to_string();
    let port = parsed
        .port()
        .unwrap_or_else(|| default_proxy_port(&protocol)) as u64;

    if !should_proxy_hostname(&hostname, port, env) {
        return String::new();
    }

    let mut proxy = get_proxy_env(&format!("{protocol}_proxy"), env);
    if proxy.is_empty() {
        proxy = get_proxy_env("all_proxy", env);
    }
    if !proxy.is_empty() && !proxy.contains("://") {
        proxy = format!("{protocol}://{proxy}");
    }
    proxy
}

/// Resolve the HTTP(S) proxy URL for a target, or `None` when no proxy applies
/// (`node-http-proxy.ts:92`).
///
/// Errors when the resolved proxy string is unparseable, or when it uses a
/// scheme other than `http`/`https`.
pub fn resolve_http_proxy_url_for_target(
    target_url: &str,
    env: Option<&ProviderEnv>,
) -> Result<Option<Url>, ProxyResolveError> {
    let proxy = get_proxy_for_url(target_url, env);
    if proxy.is_empty() {
        return Ok(None);
    }

    let proxy_url = Url::parse(&proxy).map_err(|error| ProxyResolveError::InvalidProxyUrl {
        proxy: proxy.clone(),
        reason: error.to_string(),
    })?;

    if proxy_url.scheme() != "http" && proxy_url.scheme() != "https" {
        return Err(ProxyResolveError::UnsupportedProtocol {
            // pi surfaces `URL.protocol`, which includes the trailing colon.
            protocol: format!("{}:", proxy_url.scheme()),
        });
    }

    Ok(Some(proxy_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serializes the process-env-mutating tests in this module: Rust runs tests
    /// in parallel threads, and these share the same `*_PROXY` variables.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// A guard that snapshots and clears the proxy env vars this module reads,
    /// restoring them on drop so process-env tests do not leak. Holds the
    /// [`env_lock`] for its lifetime so no two guards are ever live at once.
    struct ProxyEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    const PROXY_KEYS: &[&str] = &[
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
    ];

    impl ProxyEnvGuard {
        fn new() -> Self {
            let lock = env_lock();
            let saved = PROXY_KEYS
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in PROXY_KEYS {
                std::env::remove_var(key);
            }
            ProxyEnvGuard { _lock: lock, saved }
        }
    }

    impl Drop for ProxyEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn respects_no_proxy_exclusions() {
        let _guard = ProxyEnvGuard::new();
        std::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");
        std::env::set_var("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com");
        let resolved = resolve_http_proxy_url_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            None,
        )
        .unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolves_https_target_through_http_proxy() {
        let _guard = ProxyEnvGuard::new();
        std::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");
        let resolved = resolve_http_proxy_url_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.to_string(), "http://proxy.example:8080/");
    }

    #[test]
    fn prefers_scoped_proxy_before_process_env() {
        let _guard = ProxyEnvGuard::new();
        std::env::set_var("https_proxy", "http://process-proxy.example:8080");
        let scoped = env_of(&[("HTTPS_PROXY", "http://scoped-proxy.example:8080")]);
        let resolved = resolve_http_proxy_url_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&scoped),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.to_string(), "http://scoped-proxy.example:8080/");
    }

    #[test]
    fn rejects_socks_and_pac_proxy_urls() {
        let _guard = ProxyEnvGuard::new();
        std::env::set_var("HTTPS_PROXY", "socks5://proxy.example:1080");
        let error = resolve_http_proxy_url_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE));
        assert_eq!(
            error,
            ProxyResolveError::UnsupportedProtocol {
                protocol: "socks5:".to_string()
            }
        );
    }

    #[test]
    fn no_proxy_configured_returns_none() {
        let _guard = ProxyEnvGuard::new();
        let resolved = resolve_http_proxy_url_for_target("https://api.example.com", None).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn no_proxy_wildcard_disables_proxy() {
        let _guard = ProxyEnvGuard::new();
        std::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");
        std::env::set_var("NO_PROXY", "*");
        let resolved = resolve_http_proxy_url_for_target("https://api.example.com", None).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn bare_proxy_host_gets_target_scheme_prefixed() {
        let _guard = ProxyEnvGuard::new();
        // No scheme on the proxy → the target's scheme is prepended.
        let scoped = env_of(&[("HTTP_PROXY", "proxy.example:3128")]);
        let resolved = resolve_http_proxy_url_for_target("http://api.example.com", Some(&scoped))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.to_string(), "http://proxy.example:3128/");
    }

    #[test]
    fn parse_host_port_matches_anchored_pattern() {
        assert_eq!(
            parse_host_port("example.com:8080"),
            Some(("example.com", 8080))
        );
        assert_eq!(parse_host_port("example.com"), None);
        assert_eq!(parse_host_port(":8080"), None);
        assert_eq!(parse_host_port("example.com:"), None);
        assert_eq!(parse_host_port("example.com:80x"), None);
    }
}
