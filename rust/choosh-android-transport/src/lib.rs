//! Android outer composition root for admitted native SSH transport.
//!
//! This crate is the only permitted dependency join between opaque Android
//! handles and the Russh adapter. Concrete JNI socket and Keystore callbacks
//! remain injected capabilities; no credential bytes are represented here.

#![forbid(unsafe_code)]

/// Marker for the outer Android/Russh composition root.
///
/// The runtime adapter is deliberately not implemented until its JNI stream
/// and callback contracts have deterministic generated-key acceptance tests.
pub const COMPOSITION_BOUNDARY: &str = "android-opaque-handles-to-russh";

#[cfg(test)]
mod tests {
    use super::COMPOSITION_BOUNDARY;

    #[test]
    fn keeps_the_platform_composition_boundary_explicit() {
        assert_eq!(COMPOSITION_BOUNDARY, "android-opaque-handles-to-russh");
    }
}
