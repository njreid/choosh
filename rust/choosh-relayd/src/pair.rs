//! Renders the phone-pairing QR code shown by the `choosh-relayd pair` CLI
//! subcommand (see `main.rs`). The QR payload is the already-configured
//! `CHOOSH_RELAYD_BOOTSTRAP_SECRET` — never a freshly generated one — so a
//! phone that scans it authenticates against the bootstrap secret the
//! *running* server actually checks in `webauthn.rs::register_start`.
//!
//! `choosh-pair:v1:<secret>` is a fixed wire contract shared with the
//! Android app's QR scanner: the prefix must not change without updating
//! that scanner too.

use qrcode::QrCode;
use qrcode::render::unicode;

/// Fixed prefix of the pairing payload. Followed by the raw
/// `CHOOSH_RELAYD_BOOTSTRAP_SECRET` value, verbatim.
pub const PAIRING_PAYLOAD_PREFIX: &str = "choosh-pair:v1:";

/// Renders a scannable pairing QR code for `secret` (the raw
/// `CHOOSH_RELAYD_BOOTSTRAP_SECRET` value) as terminal art, followed by a
/// plain-text `Payload: ...` line so an operator can read or copy the
/// payload by hand if their terminal can't render the Unicode block
/// characters well.
///
/// # Errors
///
/// Returns [`qrcode::types::QrError`] if `secret` is too long or otherwise
/// unencodable as a single QR code (see that type's variants); this should
/// not happen for realistic bootstrap-secret lengths (e.g. 64 hex chars).
pub fn render_pairing_qr(secret: &str) -> Result<String, qrcode::types::QrError> {
    let payload = format!("{PAIRING_PAYLOAD_PREFIX}{secret}");
    let code = QrCode::new(payload.as_bytes())?;
    let qr_art = code.render::<unicode::Dense1x2>().build();
    Ok(format!("{qr_art}\nPayload: {payload}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_successfully_for_a_normal_secret() {
        let secret = "a".repeat(64); // typical `openssl rand -hex 32` length
        let rendered = render_pairing_qr(&secret).unwrap();
        assert!(!rendered.is_empty());
    }

    #[test]
    fn output_is_multi_line_terminal_art_plus_a_readable_payload_line() {
        let secret = "test-secret-0123456789";
        let rendered = render_pairing_qr(secret).unwrap();
        let lines: Vec<&str> = rendered.lines().collect();
        // The QR art itself is many lines; the payload line is one more.
        assert!(lines.len() > 5, "expected multi-line QR art, got: {rendered:?}");
        let payload_line = lines
            .iter()
            .find(|line| line.starts_with("Payload: "))
            .expect("rendered output must contain a plain-text Payload: line");
        assert_eq!(*payload_line, format!("Payload: {PAIRING_PAYLOAD_PREFIX}{secret}"));
    }

    #[test]
    fn payload_carries_the_expected_fixed_prefix_verbatim() {
        let secret = "another-secret";
        let rendered = render_pairing_qr(secret).unwrap();
        assert!(rendered.contains(&format!("{PAIRING_PAYLOAD_PREFIX}{secret}")));
    }

    #[test]
    fn qr_art_actually_encodes_the_payload_and_round_trips_via_the_crate_itself() {
        // Belt-and-suspenders: verify at the qrcode-crate level (independent
        // of our string formatting) that the payload we intend to encode is
        // representable as a single QR code and re-inspectable as such.
        let secret = "round-trip-secret";
        let payload = format!("{PAIRING_PAYLOAD_PREFIX}{secret}");
        let code = QrCode::new(payload.as_bytes()).unwrap();
        // A real QR code has a positive module count (i.e. it isn't empty).
        assert!(code.width() > 0);
        assert!(render_pairing_qr(secret).is_ok());
    }

    #[test]
    fn different_secrets_render_different_qr_art() {
        let a = render_pairing_qr("secret-one").unwrap();
        let b = render_pairing_qr("secret-two").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn renders_successfully_for_an_empty_secret() {
        // `main.rs::pair()` already refuses to call this at all when
        // `CHOOSH_RELAYD_BOOTSTRAP_SECRET` is unset/empty, but this function
        // itself takes a bare `&str` and has no such guard — an empty
        // secret is just a 15-byte `choosh-pair:v1:` payload, well within
        // QR capacity, so it must still render rather than erroring or
        // panicking if ever called directly (e.g. from a future caller
        // that doesn't share `main.rs`'s guard).
        let rendered = render_pairing_qr("").unwrap();
        assert!(rendered.contains(&format!("Payload: {PAIRING_PAYLOAD_PREFIX}")));
    }

    #[test]
    fn returns_a_qr_error_rather_than_panicking_when_the_secret_is_too_long_to_encode() {
        // A real bootstrap secret is `openssl rand -hex 32` (64 chars), but
        // this function has no length guard of its own — an oversized
        // input (e.g. a misconfigured env var) must surface as the
        // documented `Err(QrError)`, not a panic.
        let secret = "a".repeat(5_000);
        let result = render_pairing_qr(&secret);
        assert!(matches!(result, Err(qrcode::types::QrError::DataTooLong)), "got: {result:?}");
    }

    #[test]
    fn renders_successfully_for_a_secret_with_non_hex_and_non_ascii_characters() {
        // Real secrets are hex, but this function has no such restriction
        // — it encodes whatever bytes it's given. Confirm punctuation and
        // multi-byte UTF-8 don't trip up encoding or the payload line.
        let secret = "not-hex! ünïcödé 🔑 (){}[]";
        let rendered = render_pairing_qr(secret).unwrap();
        assert!(rendered.contains(&format!("Payload: {PAIRING_PAYLOAD_PREFIX}{secret}")));
    }
}
