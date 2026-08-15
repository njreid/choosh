//! Bounded, framework-agnostic primitives for Choosh's loopback web surfaces.
//!
//! This crate itself exposes no listener or browser bridge — no socket, no
//! HTTP framing, no JNI — only [`markdown`]'s sanitizing Markdown→HTML
//! renderer and the [`is_safe_relative_asset`] gate, both pure functions
//! over already-fetched bytes. The listener/browser-bridge role this
//! module doc used to describe as future work is real today, just built
//! one crate over: `choosh-android-bridge::markdown_gateway` is the
//! `workspace.file.read`-backed loopback HTTP server
//! (`docs/specs/service-tunnels.md`'s Markdown/Datastar `WebView` surface)
//! that fetches a document over the RPC tunnel and calls
//! [`markdown::render_markdown`] directly as its rendering core — see that
//! module's doc comment for the full architecture reasoning on why
//! rendering happens client-side there rather than in this crate. This
//! crate stays deliberately below that seam: framework-agnostic,
//! testable without a socket or an RPC connection, and reusable by a
//! server-side renderer later without change, per
//! [M5](../../../docs/milestones/M5-web-and-markdown.md) ("Web preview and
//! Markdown").

pub mod markdown;

/// Escapes arbitrary text for safe embedding in HTML, via `maud`'s
/// auto-escaping `html!` macro. `markdown::render_markdown` handles its own
/// content escaping internally (through `pulldown-cmark`'s HTML generator,
/// after neutralizing raw-HTML events — see `markdown` for why); this is
/// for embedding standalone dynamic text elsewhere, e.g. a workspace-
/// relative file name in a future page-shell `<title>`.
#[must_use]
pub fn escape_html(text: &str) -> String {
    maud::html! { (text) }.into_string()
}

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
    use super::{boundary_name, escape_html, is_safe_relative_asset};

    #[test]
    fn foundation_boundary_has_a_stable_name() {
        assert_eq!(boundary_name(), "choosh-web");
    }

    #[test]
    fn escape_html_neutralizes_markup() {
        assert_eq!(escape_html("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(escape_html("plain text"), "plain text");
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
