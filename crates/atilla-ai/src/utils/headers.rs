//! Header-to-record conversion, ported from pi-ai's
//! `packages/ai/src/utils/headers.ts` at pinned commit `3da591ab`.
//!
//! Two small adapters that flatten header collections into a plain map:
//!
//! - [`headers_to_record`] mirrors pi's `headersToRecord(headers: Headers)`. The
//!   WHATWG `Headers` object always lowercases field names as they are stored,
//!   and `Headers.entries()` yields those already-lowercased `(name, value)`
//!   pairs. Rust has no `Headers` type, so the port takes any iterable of
//!   `(name, value)` pairs and lowercases each key itself, reproducing the
//!   normalization callers depend on.
//! - [`provider_headers_to_record`] mirrors pi's `providerHeadersToRecord`,
//!   which walks a plain [`ProviderHeaders`] record (`Record<string, string |
//!   null>`), drops `null`-valued entries, and returns `undefined` (here
//!   [`None`]) when nothing survives. Keys are copied verbatim — pi iterates
//!   `Object.entries`, which does not normalize case.
//!
//! Both return a [`BTreeMap`] so iteration order is deterministic; JS object key
//! order is not observable through the record shape these feed.

use std::collections::BTreeMap;

/// Provider header overrides (`types.ts:105`,
/// `ProviderHeaders = Record<string, string | null>`); `None` marks a header
/// explicitly cleared.
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

/// Flatten a `Headers`-like iterable of `(name, value)` pairs into a record,
/// lowercasing field names as the WHATWG `Headers` object does
/// (`headers.ts:3`).
pub fn headers_to_record<I>(headers: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut result = BTreeMap::new();
    for (key, value) in headers {
        result.insert(key.to_lowercase(), value);
    }
    result
}

/// Flatten provider header overrides, dropping cleared (`null`) entries and
/// returning `None` when none remain (`headers.ts:11`).
pub fn provider_headers_to_record(
    headers: Option<&ProviderHeaders>,
) -> Option<BTreeMap<String, String>> {
    let headers = headers?;
    let mut result = BTreeMap::new();
    for (key, value) in headers {
        if let Some(value) = value {
            result.insert(key.clone(), value.clone());
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_header_names() {
        let record = headers_to_record([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Api-Key".to_string(), "secret".to_string()),
        ]);
        assert_eq!(
            record.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(record.get("x-api-key").map(String::as_str), Some("secret"));
        assert!(!record.contains_key("Content-Type"));
    }

    #[test]
    fn empty_iterable_yields_empty_record() {
        let record = headers_to_record(Vec::<(String, String)>::new());
        assert!(record.is_empty());
    }

    #[test]
    fn provider_headers_none_input_is_none() {
        assert_eq!(provider_headers_to_record(None), None);
    }

    #[test]
    fn provider_headers_drops_null_values() {
        let mut headers = ProviderHeaders::new();
        headers.insert("X-Keep".to_string(), Some("yes".to_string()));
        headers.insert("X-Drop".to_string(), None);
        let record = provider_headers_to_record(Some(&headers)).unwrap();
        assert_eq!(record.len(), 1);
        assert_eq!(record.get("X-Keep").map(String::as_str), Some("yes"));
        assert!(!record.contains_key("X-Drop"));
    }

    #[test]
    fn provider_headers_all_null_is_none() {
        let mut headers = ProviderHeaders::new();
        headers.insert("X-Drop".to_string(), None);
        assert_eq!(provider_headers_to_record(Some(&headers)), None);
    }

    #[test]
    fn provider_headers_preserves_key_case() {
        let mut headers = ProviderHeaders::new();
        headers.insert("Authorization".to_string(), Some("Bearer x".to_string()));
        let record = provider_headers_to_record(Some(&headers)).unwrap();
        assert!(record.contains_key("Authorization"));
    }
}
