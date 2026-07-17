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

#[cfg(test)]
mod tests {
    use super::boundary_name;

    #[test]
    fn foundation_boundary_has_a_stable_name() {
        assert_eq!(boundary_name(), "choosh-web");
    }
}
