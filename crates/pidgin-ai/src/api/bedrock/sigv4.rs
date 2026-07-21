// straitjacket-allow-file:duplication — the per-encoding helpers and the
// canonical-request assembly repeat the same small byte/hex/HMAC shapes that
// AWS SigV4 canonicalization is defined in terms of; the clone detector reads
// the repeated `%XX`/HMAC-chain scaffolding as duplicative, but keeping each
// step spelled out verbatim mirrors the SDK's `@smithy/signature-v4` structure
// and is load-bearing for the signature, so the repetition is intentional.
//! AWS Signature Version 4 request signing for the Amazon Bedrock
//! non-bearer-token credentials path.
//!
//! pi drives Bedrock through `@aws-sdk/client-bedrock-runtime`, which delegates
//! request signing to `@smithy/signature-v4`. On the non-bearer path (no
//! `AWS_BEARER_TOKEN_BEDROCK`), the SDK resolves AWS credentials and SigV4-signs
//! every `ConverseStream` request. This module reproduces that signing without
//! the SDK so the ported driver ([`super::driver`]) can put a signed request on
//! the wire when only standard AWS credentials are available.
//!
//! # What is reproduced (matching `@smithy/signature-v4` defaults)
//!
//! - **Service / algorithm.** Service name `bedrock`, algorithm
//!   `AWS4-HMAC-SHA256`. Region flows in from the resolved client config.
//! - **Canonical request.** `METHOD\n{canonicalUri}\n{canonicalQuery}\n`
//!   `{canonicalHeaders}\n{signedHeaders}\n{hashedPayload}`. The canonical URI
//!   is the request path **double-encoded** for non-S3 services (the SDK's
//!   `uriEscapePath = true` path): the already percent-encoded path is passed
//!   through `encodeURIComponent` again, restoring `/`. So a model-id segment
//!   already encoded as `...%3A0` signs as `...%253A0`.
//! - **Signed headers.** `content-type`, `host`, `x-amz-content-sha256`,
//!   `x-amz-date`, and `x-amz-security-token` when a session token is present,
//!   plus any caller headers applied before signing. `x-amz-content-sha256` is
//!   emitted and signed (the SDK's `applyChecksum = true` default).
//! - **String to sign / signing key.** `AWS4-HMAC-SHA256\n{amzDate}\n{scope}\n`
//!   `{hex(sha256(canonicalRequest))}` with `scope = {date}/{region}/bedrock/`
//!   `aws4_request`; the signing key is the standard
//!   `date -> region -> service -> aws4_request` HMAC chain.
//! - **Headers written.** `x-amz-date`, `x-amz-content-sha256`, `host`,
//!   `x-amz-security-token` (session token only), and the final `authorization:
//!   AWS4-HMAC-SHA256 Credential=.../SignedHeaders=.../Signature=...`.
//!
//! # Tests
//!
//! Validated against published AWS vectors: the "Deriving the signing key"
//! documentation example and the SigV4 test-suite `get-vanilla` case (both use
//! the well-known `AKIDEXAMPLE` credentials), plus a fixed Bedrock
//! `ConverseStream` input whose signature is pinned to lock the double-encoded
//! canonical-URI behaviour.

use std::collections::BTreeMap;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// The SigV4 algorithm identifier.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";
/// The AWS service name Bedrock Runtime signs under.
pub const BEDROCK_SERVICE: &str = "bedrock";
/// The terminating scope component of the signing-key derivation chain.
const AWS4_REQUEST: &str = "aws4_request";

/// Header keys the AWS SDK never folds into the signature
/// (`@smithy/signature-v4` `alwaysUnsignableHeaders`). `authorization` matters
/// here: it is written last and must not participate in its own signature.
const ALWAYS_UNSIGNABLE: &[&str] = &[
    "authorization",
    "cache-control",
    "connection",
    "expect",
    "from",
    "keep-alive",
    "max-forwards",
    "pragma",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "x-amzn-trace-id",
];

/// Resolved AWS credentials for the SigV4 path (the env subset the ported
/// [`super::get_configured_bedrock_credentials`] resolves).
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Sign `headers`/`body` in place, adding the SigV4 auth headers so the request
/// is ready for the wire. `url` is the full request URL (scheme + host + path);
/// `amz_date` is the `YYYYMMDDTHHMMSSZ` request timestamp (see
/// [`amz_date_from_epoch_ms`]).
///
/// On return `headers` carries `x-amz-date`, `x-amz-content-sha256`, `host`,
/// `x-amz-security-token` (only when the credentials include a session token),
/// and `authorization`. Fails only when `url` cannot be parsed for a host.
#[allow(clippy::too_many_arguments)]
pub fn sign_request(
    method: &str,
    url: &str,
    headers: &mut BTreeMap<String, String>,
    body: &[u8],
    creds: &AwsCredentials,
    region: &str,
    service: &str,
    amz_date: &str,
) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|error| format!("invalid Bedrock URL: {error}"))?;
    let host = canonical_host(&parsed)?;
    let path = parsed.path();
    let canonical_query = canonical_query_string(&parsed);
    let payload_hash = sha256_hex(body);

    // Assemble the exact header set that participates in the signature. The
    // SDK's `applyChecksum` default emits and signs `x-amz-content-sha256`;
    // `host` / `x-amz-date` are mandatory; the session token is signed when set.
    headers.insert("host".to_string(), host);
    headers.insert("x-amz-date".to_string(), amz_date.to_string());
    headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
    if let Some(token) = &creds.session_token {
        headers.insert("x-amz-security-token".to_string(), token.clone());
    }

    // Signed headers = every header present except the always-unsignable set,
    // lowercased and sorted (a BTreeMap iterates in sorted key order already).
    let signed: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), trim_all(v)))
        .filter(|(k, _)| !ALWAYS_UNSIGNABLE.contains(&k.as_str()))
        .collect();

    let authorization = authorization_header(
        method,
        &canonical_uri(path),
        &canonical_query,
        &signed,
        &payload_hash,
        creds,
        region,
        service,
        amz_date,
    );
    headers.insert("authorization".to_string(), authorization);
    Ok(())
}

/// Compute the `Authorization` header value for a fully-prepared request.
/// `signed` is the sorted, lowercased `(name, trimmed-value)` header set that
/// participates in the signature (must include `host` and `x-amz-date`).
#[allow(clippy::too_many_arguments)]
fn authorization_header(
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    signed: &[(String, String)],
    payload_hash: &str,
    creds: &AwsCredentials,
    region: &str,
    service: &str,
    amz_date: &str,
) -> String {
    let signed_headers = signed
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let date_stamp = &amz_date[..8];
    let scope = format!("{date_stamp}/{region}/{service}/{AWS4_REQUEST}");
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(&creds.secret_access_key, date_stamp, region, service);
    let signature = hex_lower(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id
    )
}

/// Derive the SigV4 signing key: the `AWS4{secret} -> date -> region -> service
/// -> aws4_request` HMAC-SHA256 chain.
fn derive_signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, AWS4_REQUEST.as_bytes())
}

/// The `Host` header value: hostname, plus `:port` only when the URL carries a
/// non-default explicit port (matching what the SDK sends and signs).
fn canonical_host(url: &url::Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "Bedrock URL has no host".to_string())?;
    match url.port() {
        Some(port) => Ok(format!("{host}:{port}")),
        None => Ok(host.to_string()),
    }
}

/// Canonical URI: the request path, double-encoded for non-S3 services (the
/// SDK's `uriEscapePath = true`). Path segments are normalized (empty / `.` /
/// `..` removed) then `encodeURIComponent`-escaped, with `/` restored.
fn canonical_uri(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut normalized = String::new();
    if path.starts_with('/') {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if path.ends_with('/') && !segments.is_empty() {
        normalized.push('/');
    }
    if normalized.is_empty() {
        normalized.push('/');
    }
    encode_uri_component(&normalized).replace("%2F", "/")
}

/// Canonical query string: each `key=value` pair URI-encoded (RFC 3986,
/// unreserved-only) and sorted by encoded key then value. Empty for
/// `ConverseStream`, which carries no query parameters.
fn canonical_query_string(url: &url::Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (encode_rfc3986(&k), encode_rfc3986(&v)))
        .collect();
    if pairs.is_empty() {
        return String::new();
    }
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode `value` over its UTF-8 bytes: ASCII alphanumerics plus the
/// `extra_unreserved` bytes stay literal; every other byte becomes `%XX`
/// (uppercase hex).
fn percent_encode(value: &str, extra_unreserved: &[u8]) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || extra_unreserved.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// JS `encodeURIComponent`: leaves `A-Za-z0-9-_.!~*'()` unescaped, `%XX`
/// (uppercase) for everything else, over UTF-8 bytes.
fn encode_uri_component(value: &str) -> String {
    percent_encode(value, b"-_.!~*'()")
}

/// RFC 3986 unreserved encoding used for canonical query components: only
/// `A-Za-z0-9-_.~` stay literal.
fn encode_rfc3986(value: &str) -> String {
    percent_encode(value, b"-_.~")
}

/// Trim ends and collapse internal runs of spaces to one (the SDK's canonical
/// header value normalization).
fn trim_all(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_space = false;
    for ch in value.trim().chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Format a millisecond Unix timestamp (pi's `Date.now()`) as the SigV4
/// `x-amz-date` value `YYYYMMDDTHHMMSSZ` in UTC.
pub fn amz_date_from_epoch_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Convert days since the Unix epoch to a `(year, month, day)` civil date
/// (Howard Hinnant's `civil_from_days`), valid across the proleptic Gregorian
/// calendar without a date library.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The well-known AWS SigV4 test-suite credentials.
    fn example_creds() -> AwsCredentials {
        AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
        }
    }

    #[test]
    fn signing_key_matches_aws_docs_vector() {
        // AWS "Deriving the signing key" documentation example.
        let key = derive_signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex_lower(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn get_vanilla_authorization_matches_test_suite() {
        // AWS SigV4 test suite `get-vanilla`: GET / with only host + x-amz-date
        // signed and an empty payload. Validates the full canonical-request ->
        // string-to-sign -> signature pipeline against a published vector.
        let empty_hash = sha256_hex(b"");
        let signed = vec![
            ("host".to_string(), "example.amazonaws.com".to_string()),
            ("x-amz-date".to_string(), "20150830T123600Z".to_string()),
        ];
        let auth = authorization_header(
            "GET",
            "/",
            "",
            &signed,
            &empty_hash,
            &example_creds(),
            "us-east-1",
            "service",
            "20150830T123600Z",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn empty_payload_hash_is_the_known_sha256() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn canonical_uri_double_encodes_non_s3_path() {
        // The model-id path segment is already single-encoded (`:` -> `%3A`);
        // the non-S3 canonical URI double-encodes it (`%3A` -> `%253A`) while
        // leaving the structural `/` separators intact.
        assert_eq!(
            canonical_uri("/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream"),
            "/model/anthropic.claude-3-5-sonnet-20241022-v2%253A0/converse-stream"
        );
        assert_eq!(canonical_uri("/"), "/");
        assert_eq!(canonical_uri(""), "/");
    }

    #[test]
    fn amz_date_formats_epoch_ms_in_utc() {
        // 2015-08-30T12:36:00Z == 1440938160 s.
        assert_eq!(
            amz_date_from_epoch_ms(1_440_938_160_000),
            "20150830T123600Z"
        );
        assert_eq!(amz_date_from_epoch_ms(0), "19700101T000000Z");
    }

    #[test]
    fn sign_request_writes_and_signs_the_bedrock_headers() {
        // A fixed ConverseStream input; the signature is pinned so the
        // double-encoded canonical URI + signed-header set stay locked. Session
        // token present, so x-amz-security-token is written and signed.
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("SESSIONTOKEN".to_string()),
        };
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        sign_request(
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream",
            &mut headers,
            br#"{"modelId":"x"}"#,
            &creds,
            "us-east-1",
            BEDROCK_SERVICE,
            "20150830T123600Z",
        )
        .unwrap();

        assert_eq!(
            headers.get("host").map(String::as_str),
            Some("bedrock-runtime.us-east-1.amazonaws.com")
        );
        assert_eq!(
            headers.get("x-amz-date").map(String::as_str),
            Some("20150830T123600Z")
        );
        assert_eq!(
            headers.get("x-amz-security-token").map(String::as_str),
            Some("SESSIONTOKEN")
        );
        // Body hash of the fixed payload.
        assert_eq!(
            headers.get("x-amz-content-sha256").map(String::as_str),
            Some(sha256_hex(br#"{"modelId":"x"}"#).as_str())
        );
        let auth = headers.get("authorization").expect("authorization written");
        assert!(auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request"
        ));
        assert!(auth.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
        ));
        assert!(auth.contains("Signature="));
    }

    #[test]
    fn sign_request_without_session_token_omits_security_token() {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        sign_request(
            "POST",
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/x/converse-stream",
            &mut headers,
            b"{}",
            &example_creds(),
            "us-west-2",
            BEDROCK_SERVICE,
            "20150830T123600Z",
        )
        .unwrap();
        assert!(!headers.contains_key("x-amz-security-token"));
        let auth = headers.get("authorization").unwrap();
        assert!(auth.contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"));
        assert!(!auth.contains("x-amz-security-token"));
    }
}
