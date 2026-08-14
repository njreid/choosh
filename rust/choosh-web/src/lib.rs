//! Bounded, framework-agnostic primitives for Choosh's loopback web surfaces.
//!
//! This crate intentionally exposes no listener or browser bridge yet: the
//! loopback HTTP server, Datastar SSE wiring, and root-confined ranged
//! asset serving all depend on the `workspace.file.read` RPC
//! (`docs/specs/jj-integration.md`), which is part of
//! [M1](../../../docs/milestones/M1-workspace-and-jj.md) and not yet
//! landed. What's here — [`markdown`]'s sanitizing Markdown→HTML renderer
//! and the [`is_safe_relative_asset`] gate — is the decoupled rendering
//! core that increment will wire up to a real file source; see
//! [M5](../../../docs/milestones/M5-web-and-markdown.md) ("Web preview and
//! Markdown") for the full scope this crate grows into.

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
