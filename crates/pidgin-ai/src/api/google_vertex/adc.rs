//! Google Vertex AI ADC / service-account OAuth2 token minting — the
//! Bearer-token half of pi-ai's `google-vertex.ts` (the `createClient` /
//! `googleAuthOptions.keyFilename` path), ported at pinned commit `3da591ab`.
//!
//! pi does not mint the token itself: when no Vertex API key is present it hands
//! `new GoogleGenAI({ vertexai: true, project, location, googleAuthOptions: {
//! keyFilename } })` and the `@google/genai` SDK delegates to
//! `google-auth-library`, which for a service-account keyfile mints a Google
//! OAuth2 access token and sends it as `Authorization: Bearer <token>` against
//! the regional Vertex AI endpoint. This module reproduces that token mint
//! without the SDK so the ported driver ([`super::driver`]) can put a Bearer
//! request on the wire when only a service-account keyfile is resolvable.
//!
//! # What is reproduced (the RFC 7523 JWT-bearer flow google-auth-library runs
//! for a service-account keyfile)
//!
//! - **Credential.** A service-account JSON keyfile (pi's `keyFilename` from
//!   `GOOGLE_APPLICATION_CREDENTIALS`) carrying `client_email`, `private_key`,
//!   an optional `token_uri` (default `https://oauth2.googleapis.com/token`),
//!   and an optional `project_id`.
//! - **Assertion.** A JWT signed `RS256` with the service-account RSA
//!   `private_key`: header `{ alg: "RS256", typ: "JWT" }`; claims `iss` =
//!   `client_email`, `scope` = `https://www.googleapis.com/auth/cloud-platform`,
//!   `aud` = the `token_uri`, `iat` = now, `exp` = now + 3600s.
//! - **Exchange.** `POST {token_uri}` form-encoded with
//!   `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer` + `assertion=<jwt>`,
//!   parsing `access_token` + `expires_in` from the JSON response.
//! - **Caching.** A minted token is reused until it is within the eager-refresh
//!   threshold of its expiry (google-auth-library's
//!   `DEFAULT_EAGER_REFRESH_THRESHOLD_MILLIS`, 5 minutes), then re-minted.
//!
//! # Resolution subset
//!
//! Only the `GOOGLE_APPLICATION_CREDENTIALS` keyfile is resolved here — the one
//! ADC source pi's Vertex runtime actually wires (`buildGoogleAuthOptions` reads
//! exactly that env var). google-auth-library additionally supports several ADC
//! sources this port does NOT implement; see the `resolve_service_account_key`
//! follow-up note in [`super::super::super::providers::google_vertex_backend`].
//! When no service-account keyfile is resolvable the caller surfaces pi's
//! "No API key for provider" error rather than silently attempting another source.

use std::collections::BTreeMap;
use std::sync::Mutex;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::seams::http::{HttpRequest, HttpTransport};

/// The OAuth2 scope pi/google-auth-library request for Vertex (`cloud-platform`).
pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// The default OAuth2 token endpoint when the keyfile omits `token_uri`.
pub const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// The RFC 7523 grant type for a signed-JWT bearer assertion.
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// The assertion lifetime in seconds (google-auth-library signs a 1-hour JWT).
const ASSERTION_LIFETIME_SECS: i64 = 3600;
/// Reuse a cached token while more than this many milliseconds remain before its
/// expiry (google-auth-library `DEFAULT_EAGER_REFRESH_THRESHOLD_MILLIS`, 5 min).
const EAGER_REFRESH_THRESHOLD_MS: i64 = 5 * 60 * 1000;

/// A parsed service-account keyfile: the subset of a GCP service-account JSON key
/// the OAuth2 JWT-bearer flow reads. Unknown fields (`private_key_id`,
/// `client_id`, `auth_uri`, ...) are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccountKey {
    /// The service-account email, the assertion's `iss` (and token-exchange
    /// subject).
    pub client_email: String,
    /// The PEM-encoded RSA private key the assertion is signed with.
    pub private_key: String,
    /// The OAuth2 token endpoint; [`DEFAULT_TOKEN_URI`] when absent.
    #[serde(default)]
    pub token_uri: Option<String>,
    /// The GCP project the key belongs to (unused for the mint; available as a
    /// project fallback).
    #[serde(default)]
    pub project_id: Option<String>,
}

impl ServiceAccountKey {
    /// The token endpoint to exchange the assertion at ([`DEFAULT_TOKEN_URI`]
    /// when the keyfile omits or blanks `token_uri`).
    pub fn token_uri(&self) -> &str {
        self.token_uri
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_TOKEN_URI)
    }
}

/// Parse a service-account keyfile's JSON contents. `Err` on malformed JSON or a
/// key missing the `client_email` / `private_key` the mint requires.
pub fn parse_service_account_key(json: &str) -> Result<ServiceAccountKey, String> {
    let key: ServiceAccountKey =
        serde_json::from_str(json).map_err(|e| format!("invalid service account key: {e}"))?;
    if key.client_email.trim().is_empty() {
        return Err("service account key missing client_email".to_string());
    }
    if key.private_key.trim().is_empty() {
        return Err("service account key missing private_key".to_string());
    }
    Ok(key)
}

/// The signed-JWT assertion claims (RFC 7523 / google-auth-library): `iss` =
/// service-account email, `scope` = cloud-platform, `aud` = token endpoint,
/// `iat`/`exp` bounding a 1-hour window.
#[derive(Debug, Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

/// Mint the signed `RS256` JWT assertion for `key` at `now_secs` (Unix seconds).
/// The header is `{ alg: "RS256", typ: "JWT" }` (jsonwebtoken's default `typ`);
/// the claims bound a `now_secs .. now_secs + 3600` window. `Err` when the
/// keyfile's `private_key` is not a usable RSA PEM.
pub fn build_assertion(key: &ServiceAccountKey, now_secs: i64) -> Result<String, String> {
    let claims = Claims {
        iss: &key.client_email,
        scope: CLOUD_PLATFORM_SCOPE,
        aud: key.token_uri(),
        iat: now_secs,
        exp: now_secs + ASSERTION_LIFETIME_SECS,
    };
    let encoding_key = EncodingKey::from_rsa_pem(key.private_key.as_bytes())
        .map_err(|e| format!("invalid service account private_key: {e}"))?;
    encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .map_err(|e| format!("failed to sign vertex assertion: {e}"))
}

/// Build the token-exchange request: `POST {token_uri}` with the RFC 7523
/// form body `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=<jwt>`
/// under `content-type: application/x-www-form-urlencoded`.
pub fn build_token_request(key: &ServiceAccountKey, assertion: &str) -> HttpRequest {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", JWT_BEARER_GRANT_TYPE)
        .append_pair("assertion", assertion)
        .finish();
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    HttpRequest {
        method: "POST".to_string(),
        url: key.token_uri().to_string(),
        headers,
        body: Some(body),
    }
}

/// The token endpoint's JSON response body (the fields the flow reads).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// A minted OAuth2 access token and the wall-clock (ms) at which it expires.
#[derive(Debug, Clone)]
pub struct AccessToken {
    /// The bearer token value to send as `Authorization: Bearer <token>`.
    pub token: String,
    /// The absolute expiry instant in epoch milliseconds.
    pub expires_at_ms: i64,
}

impl AccessToken {
    /// Whether the token is reusable at `now_ms`: it is valid while more than the
    /// eager-refresh threshold remains before expiry, matching
    /// google-auth-library's early refresh so an in-flight request never carries
    /// a token about to expire.
    pub fn is_valid(&self, now_ms: i64) -> bool {
        now_ms + EAGER_REFRESH_THRESHOLD_MS < self.expires_at_ms
    }
}

/// Parse the token endpoint's JSON response into an [`AccessToken`], stamping its
/// absolute expiry from `now_ms + expires_in`. `Err` on malformed JSON or a
/// missing/empty `access_token`; a response omitting `expires_in` falls back to
/// the 1-hour assertion lifetime.
pub fn parse_token_response(body: &str, now_ms: i64) -> Result<AccessToken, String> {
    let response: TokenResponse =
        serde_json::from_str(body).map_err(|e| format!("invalid token response: {e}"))?;
    if response.access_token.is_empty() {
        return Err("token response missing access_token".to_string());
    }
    let expires_in = response.expires_in.unwrap_or(ASSERTION_LIFETIME_SECS);
    Ok(AccessToken {
        token: response.access_token,
        expires_at_ms: now_ms + expires_in * 1000,
    })
}

/// Format an OAuth2 token-endpoint error body into a diagnostic message. The
/// endpoint returns `{ "error", "error_description" }`; this surfaces the
/// description (or error code) with the status, falling back to the raw body.
fn format_token_error(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    let detail = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            value
                .get("error_description")
                .or_else(|| value.get("error"))
                .and_then(|field| field.as_str())
                .map(str::to_string)
        });
    match detail {
        Some(message) => format!("vertex token request failed: {status} {message}"),
        None if !trimmed.is_empty() => format!("vertex token request failed: {status} {trimmed}"),
        None => format!("vertex token request failed: {status} (no body)"),
    }
}

/// Mint a fresh access token for `key` over `transport`: sign the assertion at
/// `now_ms`, exchange it at the token endpoint, and parse the response. `Err`
/// on a signing failure, a transport error, a non-2xx token response, or a
/// malformed response body.
pub fn mint_access_token<T: HttpTransport + ?Sized>(
    transport: &T,
    key: &ServiceAccountKey,
    now_ms: i64,
) -> Result<AccessToken, String> {
    let assertion = build_assertion(key, now_ms.div_euclid(1000))?;
    let request = build_token_request(key, &assertion);
    let response = transport
        .send(&request)
        .map_err(|error| error.to_string())?;
    if !response.is_ok() {
        return Err(format_token_error(response.status, &response.body));
    }
    parse_token_response(&response.body, now_ms)
}

/// A single-slot access-token cache, reproducing google-auth-library's token
/// reuse: a minted token is held and returned until it nears expiry, then a new
/// one is minted. Shared behind `&self` (a `Mutex`) so the long-lived Vertex
/// backend can reuse one token across turns for the same credential.
#[derive(Debug, Default)]
pub struct TokenCache {
    cached: Mutex<Option<AccessToken>>,
}

impl TokenCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a valid bearer token for `key`, minting (and caching) a fresh one
    /// over `transport` when the cache is empty or the held token is within the
    /// eager-refresh threshold of expiry at `now_ms`.
    pub fn get_or_mint<T: HttpTransport + ?Sized>(
        &self,
        transport: &T,
        key: &ServiceAccountKey,
        now_ms: i64,
    ) -> Result<String, String> {
        if let Some(token) = self.cached.lock().expect("token cache poisoned").as_ref() {
            if token.is_valid(now_ms) {
                return Ok(token.token.clone());
            }
        }
        let token = mint_access_token(transport, key, now_ms)?;
        let value = token.token.clone();
        *self.cached.lock().expect("token cache poisoned") = Some(token);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde_json::json;

    use crate::seams::http::{HttpResponse, HttpStreamResponse};

    /// A test service-account RSA private key (PKCS#8 PEM, 2048-bit) generated
    /// solely for these tests — not a real credential.
    const TEST_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQC6h5NBY4KSiTly\n\
L43t2Q05DqKLCjZ55Ihf6te7lo95Lv5/hF610PqZbCK/Yy+w6rul7hpZEy2ZtE8q\n\
31cbSKGEz+25A/gusRFAzjVNbZ6sVNUpGyC0O5+uHCmOqlVv62TqSU7A3l+fYIUD\n\
zoEPjs3jpzAbD4J2nCB23Us7vEjBtXEaWos7DzzlCsIyeilSqaVtbg3MvH0j4vEN\n\
i5B4IKFIDZy0Swr4yn+dX5Lzw9Rlo4UwbIqtaFUtjz6YpKAjJ+VocLK17DIqAf0t\n\
bS4N5zW50eL1CTRtiPUe3YdeL/a7JRbuS817zeSGpfKcm7p0rBI+OZGMqryt8ZJp\n\
L2vPIP5bAgMBAAECggEAAu4dSJItG2svbVVE5/8YX5SUxhVffLOz1rnkMKyxTUiJ\n\
M+ya5kVFooJZ22LN/Xv6faVLYanU9gyoj7ZZcnLGIsV3aQggbm9Wo4t5t+EodHGS\n\
taYY8evb2srTdkvDstHiUHHdXFdB7kmAXWpxiZKHNnPKZCputLlII0XfqC0RgYV2\n\
vxf01Eu3fhXpm1ZGN0qQQSdHmZWz8wVnHvLIvhuZPBYr6Chhf5VzaDsX7FTVLT0z\n\
AxC3/h3XfdiWcepRZ2YCOAVICwwSphYi6vYtLcSl2oHpDSAafFekNSn5OXlMwVva\n\
ktaPyS5+rl5rplPlWzwiN8kDSkCNCZ+sUenoK0bW/QKBgQDwtUTnzRFkuIW2XVOC\n\
dpsBxuJARUIjD+flyleFL/R0+SCxHKo9Q9Weg6m9wxsyebzJDKiwFsPRFxcoj8/T\n\
fc4hsz8IS6amTwS1OrHnUCb+iZ7qEKYPhub5ZaeydxYghtHdF/Y5Ioq+BrqRU2fZ\n\
orcl75WDsKGdnMgtgKIZQrGilwKBgQDGYS4G1S0E27o2aqC7m1W+8yBRGzvN5mMy\n\
9iQVAbc3NBfRBxUjwOezzGAhLP1M/BhnxtTfenXdT8Q6tjWTJo20x9gBnVfmFLuh\n\
8LgCP3jxZjfOfGg20XbzsLXr6tMouLY2Dz6r6UskS6y2RHd4GZMV2rSyKSlAWDox\n\
ac0VlIOu3QKBgAmSS7Ej+GMW60o7H8z6RmOlsu13U4/tW/1JNH25UHEuTtx8FVDm\n\
V6I7/g3rqjMxoA4mkLaf0R2JW4RjY5I3WHECnakIyRGn5roGIXjfOQ26DzWjf9by\n\
OFEGd8qi7aBRfBrcjw/qjbXMsrKArIp86+d4RWu9JFAOIe+dQ9TZUBL1AoGASLxG\n\
4PB2ciqSKvOLfV3l4X5JIhPHKKZJRt0iu6UGZTIvbU+Ye6R2D+FmeaOCOCDSXfJ0\n\
CIBhCMT/YLuABzUCjf8b/vOOz+hYJ3cYMJLPKEtfONE6cKb6Yz0uZpKR24NmI4oR\n\
Y3zFNUidybJuz1UpLcEjsZMP8eynYYi2TixG3+0CgYA+3oECPLblJy/zSHUTFo1l\n\
IB8R9ow06U0cOJUlEtXnh5I0Pw/pWQZ/fLowxoZpgimyPVIB+eTny95ZRI7xs68v\n\
/9yhlc2io8j51HSYxp27iBcLJYfe+xLmYkLd/4WrAGAv3lJuR6VTt4psG83C+WHb\n\
KUQo/7EG3Rkjxp82ha99wQ==\n\
-----END PRIVATE KEY-----\n";

    /// The RSA public key matching [`TEST_PRIVATE_KEY`], used to verify assertions.
    const TEST_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuoeTQWOCkok5ci+N7dkN\n\
OQ6iiwo2eeSIX+rXu5aPeS7+f4RetdD6mWwiv2MvsOq7pe4aWRMtmbRPKt9XG0ih\n\
hM/tuQP4LrERQM41TW2erFTVKRsgtDufrhwpjqpVb+tk6klOwN5fn2CFA86BD47N\n\
46cwGw+Cdpwgdt1LO7xIwbVxGlqLOw885QrCMnopUqmlbW4NzLx9I+LxDYuQeCCh\n\
SA2ctEsK+Mp/nV+S88PUZaOFMGyKrWhVLY8+mKSgIyflaHCytewyKgH9LW0uDec1\n\
udHi9Qk0bYj1Ht2HXi/2uyUW7kvNe83khqXynJu6dKwSPjmRjKq8rfGSaS9rzyD+\n\
WwIDAQAB\n\
-----END PUBLIC KEY-----\n";

    fn test_key() -> ServiceAccountKey {
        ServiceAccountKey {
            client_email: "svc@example-project.iam.gserviceaccount.com".to_string(),
            private_key: TEST_PRIVATE_KEY.to_string(),
            token_uri: None,
            project_id: Some("example-project".to_string()),
        }
    }

    #[derive(Serialize, Deserialize)]
    struct VerifiedClaims {
        iss: String,
        scope: String,
        aud: String,
        iat: i64,
        exp: i64,
    }

    /// A transport that returns a scripted token response and records the request.
    struct RecordingTransport {
        response: HttpResponse,
        calls: AtomicUsize,
        last_body: Mutex<Option<String>>,
        last_url: Mutex<Option<String>>,
    }

    impl RecordingTransport {
        fn new(response: HttpResponse) -> Self {
            Self {
                response,
                calls: AtomicUsize::new(0),
                last_body: Mutex::new(None),
                last_url: Mutex::new(None),
            }
        }
    }

    impl HttpTransport for RecordingTransport {
        fn send(&self, request: &HttpRequest) -> std::io::Result<HttpResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_body.lock().unwrap() = request.body.clone();
            *self.last_url.lock().unwrap() = Some(request.url.clone());
            Ok(self.response.clone())
        }

        fn send_streaming(&self, request: &HttpRequest) -> std::io::Result<HttpStreamResponse<'_>> {
            let response = self.send(request)?;
            Ok(HttpStreamResponse {
                status: response.status,
                headers: response.headers,
                chunks: Box::new(std::iter::once(Ok(response.body.into_bytes()))),
            })
        }
    }

    fn token_ok(access_token: &str, expires_in: i64) -> HttpResponse {
        HttpResponse::ok(
            json!({
                "access_token": access_token,
                "expires_in": expires_in,
                "token_type": "Bearer",
            })
            .to_string(),
        )
    }

    // A missing client_email / private_key is rejected before any network call.
    #[test]
    fn parse_rejects_incomplete_key() {
        assert!(parse_service_account_key("{ not json").is_err());
        let missing_key = json!({ "client_email": "svc@example.com" }).to_string();
        assert!(parse_service_account_key(&missing_key).is_err());
        let full = json!({
            "type": "service_account",
            "client_email": "svc@example.com",
            "private_key": TEST_PRIVATE_KEY,
            "token_uri": "https://oauth2.googleapis.com/token",
            "project_id": "p",
        })
        .to_string();
        let parsed = parse_service_account_key(&full).expect("parsed");
        assert_eq!(parsed.client_email, "svc@example.com");
        assert_eq!(parsed.token_uri(), DEFAULT_TOKEN_URI);
    }

    // The assertion carries the RFC 7523 / google-auth-library claims and verifies
    // against the matching public key (RS256 signing round-trip).
    #[test]
    fn assertion_claims_and_signature_round_trip() {
        let key = test_key();
        let now_secs = 1_700_000_000;
        let assertion = build_assertion(&key, now_secs).expect("assertion");

        // Header is RS256 (a JWT with three base64url segments).
        assert_eq!(assertion.split('.').count(), 3);
        let header = jsonwebtoken::decode_header(&assertion).expect("header");
        assert_eq!(header.alg, Algorithm::RS256);

        // Verifying against the public key both checks the signature and decodes
        // the claims — proving iss/scope/aud/iat/exp were signed as constructed.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[DEFAULT_TOKEN_URI]);
        validation.set_required_spec_claims(&["exp"]);
        // The test `now` is far in the past; accept the historical `exp`.
        validation.validate_exp = false;
        let decoded = decode::<VerifiedClaims>(
            &assertion,
            &DecodingKey::from_rsa_pem(TEST_PUBLIC_KEY.as_bytes()).expect("public key"),
            &validation,
        )
        .expect("verified assertion");

        assert_eq!(decoded.claims.iss, key.client_email);
        assert_eq!(decoded.claims.scope, CLOUD_PLATFORM_SCOPE);
        assert_eq!(decoded.claims.aud, DEFAULT_TOKEN_URI);
        assert_eq!(decoded.claims.iat, now_secs);
        assert_eq!(decoded.claims.exp, now_secs + ASSERTION_LIFETIME_SECS);
    }

    // The token request is a form-encoded JWT-bearer POST to the token endpoint.
    #[test]
    fn token_request_is_jwt_bearer_form_post() {
        let key = test_key();
        let request = build_token_request(&key, "HEADER.PAYLOAD.SIGNATURE");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, DEFAULT_TOKEN_URI);
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/x-www-form-urlencoded")
        );
        let body = request.body.expect("body");
        // grant_type is percent-encoded (`:` -> %3A); the assertion is present.
        assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"));
        assert!(body.contains("assertion=HEADER.PAYLOAD.SIGNATURE"));
    }

    // A well-formed token response parses; expiry is stamped from now + expires_in.
    #[test]
    fn token_response_parses_and_stamps_expiry() {
        let now_ms = 1_700_000_000_000;
        let token = parse_token_response(
            &json!({ "access_token": "ya29.abc", "expires_in": 3600 }).to_string(),
            now_ms,
        )
        .expect("token");
        assert_eq!(token.token, "ya29.abc");
        assert_eq!(token.expires_at_ms, now_ms + 3600 * 1000);

        assert!(parse_token_response("{}", now_ms).is_err());
        assert!(parse_token_response("not json", now_ms).is_err());
    }

    // A token is valid until it enters the eager-refresh window before expiry.
    #[test]
    fn access_token_validity_honors_eager_refresh_window() {
        let token = AccessToken {
            token: "t".to_string(),
            expires_at_ms: 1_000_000,
        };
        // Comfortably before expiry: valid.
        assert!(token.is_valid(1_000_000 - EAGER_REFRESH_THRESHOLD_MS - 1));
        // Exactly at the threshold boundary: no longer valid (eager refresh).
        assert!(!token.is_valid(1_000_000 - EAGER_REFRESH_THRESHOLD_MS));
        // Past expiry: not valid.
        assert!(!token.is_valid(1_000_001));
    }

    // mint_access_token signs, exchanges, and parses over a transport.
    #[test]
    fn mint_exchanges_assertion_for_access_token() {
        let transport = RecordingTransport::new(token_ok("ya29.minted", 3600));
        let now_ms = 1_700_000_000_000;
        let token = mint_access_token(&transport, &test_key(), now_ms).expect("token");
        assert_eq!(token.token, "ya29.minted");
        assert_eq!(token.expires_at_ms, now_ms + 3600 * 1000);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            transport.last_url.lock().unwrap().as_deref(),
            Some(DEFAULT_TOKEN_URI)
        );
    }

    // A non-2xx token response surfaces the OAuth error description.
    #[test]
    fn mint_surfaces_token_endpoint_error() {
        let transport = RecordingTransport::new(HttpResponse {
            status: 400,
            headers: BTreeMap::new(),
            body: json!({ "error": "invalid_grant", "error_description": "Invalid JWT" })
                .to_string(),
        });
        let error = mint_access_token(&transport, &test_key(), 1_700_000_000_000).unwrap_err();
        assert!(error.contains("400"));
        assert!(error.contains("Invalid JWT"));
    }

    // The cache mints once, reuses while valid, and re-mints once the held token
    // enters the eager-refresh window.
    #[test]
    fn cache_reuses_until_near_expiry_then_remints() {
        let transport = RecordingTransport::new(token_ok("ya29.cached", 3600));
        let cache = TokenCache::new();
        let start = 1_700_000_000_000;

        let first = cache
            .get_or_mint(&transport, &test_key(), start)
            .expect("first");
        assert_eq!(first, "ya29.cached");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        // Still comfortably valid: served from cache, no new mint.
        let second = cache
            .get_or_mint(&transport, &test_key(), start + 60_000)
            .expect("second");
        assert_eq!(second, "ya29.cached");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        // Inside the eager-refresh window: a fresh mint is performed.
        let near_expiry = start + 3600 * 1000 - EAGER_REFRESH_THRESHOLD_MS;
        let third = cache
            .get_or_mint(&transport, &test_key(), near_expiry)
            .expect("third");
        assert_eq!(third, "ya29.cached");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    }
}
