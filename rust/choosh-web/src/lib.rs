//! Bounded, framework-agnostic primitives for Choosh's future loopback web surfaces.
//!
//! The M0 crate intentionally exposes no listener or browser bridge. Network ownership
//! and authenticated gateway behavior arrive with M3; keeping this boundary empty makes
//! an accidental public listener impossible in the foundation milestone.

/// Returns the stable boundary name used by diagnostics and future protocol wiring.
#[must_use]
pub const fn boundary_name() -> &'static str {
    "choosh-web"
}

/// Accept only document-relative asset references for the authenticated route.
///
/// This deliberately does not resolve filesystem paths; the host-side workspace
/// authority performs component-aware resolution after this syntactic gate.
#[must_use]
pub fn is_safe_relative_asset(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('/')
        && !reference.contains(['\\', '\0'])
        && !reference.contains("://")
        && !reference.starts_with("//")
        && !reference
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
}

#[cfg(test)]
mod tests {
    use super::{boundary_name, is_safe_relative_asset};

    #[test]
    fn foundation_boundary_has_a_stable_name() {
        assert_eq!(boundary_name(), "choosh-web");
    }

    #[test]
    fn asset_gate_accepts_only_non_ambiguous_relative_references() {
        assert!(is_safe_relative_asset("images/logo.png"));
        for rejected in [
            "",
            "/etc/passwd",
            "../secret",
            "a/../b",
            "file:///tmp/x",
            "https://example.test/x",
            "\\\\host\\share",
            "a//b",
        ] {
            assert!(!is_safe_relative_asset(rejected), "accepted {rejected:?}");
        }
    }
}
