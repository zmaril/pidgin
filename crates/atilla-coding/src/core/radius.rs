//! Radius provider identity.
//!
//! Ported from pi's `core/radius.ts`, which exports a single constant naming the
//! built-in Radius model gateway provider.
//!
//! NOTE: pi's `radius.test.ts` exercises `ModelRuntime` / `AuthStorage` catalog
//! restoration, network fetch, and custom-gateway resolution — collaborators
//! that are not part of this cohort. Those assertions are out of scope here;
//! only the provider-id constant this module actually defines is pinned below.

/// Identifier of the built-in Radius model gateway provider.
pub const RADIUS_PROVIDER_ID: &str = "radius";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_is_radius() {
        assert_eq!(RADIUS_PROVIDER_ID, "radius");
    }
}
