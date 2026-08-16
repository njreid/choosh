//! Sanitizing Markdown → HTML rendering, decoupled from any live file
//! source (see the crate root docs for why). Given already-fetched
//! Markdown source and a resolver for local asset references, produces
//! HTML safe to embed in the locked-down `WebView` `DESIGN.md` §11
//! describes: raw HTML in the source never passes through unescaped, and
//! every relative link/image reference is gated through
//! [`crate::is_safe_relative_asset`] before a caller-supplied
//! [`AssetResolver`] decides what it becomes.
//!
//! Built on `pulldown-cmark`'s HTML generator rather than reimplementing
//! HTML escaping and tag nesting by hand — the one thing done differently
//! from that crate's own [`pulldown_cmark::html::push_html`] is a
//! transform pass over the event stream first: raw-HTML events become text
//! events (so they're escaped like any other text, not emitted verbatim),
//! and link/image destinations are rewritten or dropped per the rules
//! above.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};

/// Above this many input bytes, [`render_markdown`] rejects the input
/// outright rather than parsing it — chosen well above any real document
/// this app would render (Markdown previews are project docs, not
/// multi-megabyte data dumps) and small enough that parsing up to the
/// limit is cheap.
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

/// Above this many rendered output bytes, [`render_markdown`] fails rather
/// than returning a partial or unbounded string. Markup and escaping can
/// expand source size somewhat (e.g. every `&`, `<`, `>` in code blocks
/// grows on escape), so this is generously larger than
/// [`MAX_INPUT_BYTES`] rather than equal to it — but still bounded, so a
/// pathological input (e.g. a single line that expands into a huge
/// element count) can't produce unbounded output from bounded input.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderError {
    InputTooLarge { limit: usize, actual: usize },
    OutputTooLarge { limit: usize },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputTooLarge { limit, actual } => {
                write!(f, "markdown source is {actual} bytes, over the {limit}-byte limit")
            }
            Self::OutputTooLarge { limit } => {
                write!(f, "rendered output exceeded the {limit}-byte limit")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// Decides what a relative link/image reference becomes once it has
/// already passed [`crate::is_safe_relative_asset`]. Returning `None`
/// neutralizes the reference (per [`render_markdown`]'s docs) rather than
/// failing the whole render.
pub trait AssetResolver {
    fn resolve(&self, reference: &str) -> Option<String>;
}

/// Resolves nothing — every relative reference is neutralized. Useful for
/// tests, and for rendering contexts with no live workspace connection to
/// resolve assets against.
pub struct NoAssetResolver;

impl AssetResolver for NoAssetResolver {
    fn resolve(&self, _reference: &str) -> Option<String> {
        None
    }
}

/// Renders `source` to sanitized HTML, applying [`MAX_INPUT_BYTES`] and
/// [`MAX_OUTPUT_BYTES`].
///
/// # Errors
///
/// Returns [`RenderError::InputTooLarge`] before parsing if `source`
/// exceeds [`MAX_INPUT_BYTES`], or [`RenderError::OutputTooLarge`] after
/// rendering if the result exceeds [`MAX_OUTPUT_BYTES`].
pub fn render_markdown(source: &str, resolver: &dyn AssetResolver) -> Result<String, RenderError> {
    render_markdown_bounded(source, resolver, MAX_INPUT_BYTES, MAX_OUTPUT_BYTES)
}

/// [`render_markdown`] with caller-chosen bounds instead of the crate
/// defaults — mainly for tests that need to exercise the bound behavior
/// itself without allocating multi-megabyte fixtures.
///
/// # Errors
///
/// See [`render_markdown`].
pub fn render_markdown_bounded(
    source: &str,
    resolver: &dyn AssetResolver,
    max_input_bytes: usize,
    max_output_bytes: usize,
) -> Result<String, RenderError> {
    if source.len() > max_input_bytes {
        return Err(RenderError::InputTooLarge { limit: max_input_bytes, actual: source.len() });
    }

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(source, options);
    let events = parser.map(|event| sanitize_event(event, resolver));

    let mut html_out = String::new();
    pulldown_cmark::html::push_html(&mut html_out, events);

    if html_out.len() > max_output_bytes {
        return Err(RenderError::OutputTooLarge { limit: max_output_bytes });
    }
    Ok(html_out)
}

fn sanitize_event<'a>(event: Event<'a>, resolver: &dyn AssetResolver) -> Event<'a> {
    match event {
        // Raw HTML from the source becomes a text node, so pulldown-cmark's
        // own (well-tested) text escaping neutralizes it instead of the
        // generator emitting it verbatim. `Tag::HtmlBlock`'s Start/End
        // wrapper events carry no content of their own (pulldown-cmark's
        // HTML generator treats both as no-ops) — only the `Event::Html`
        // events nested inside carry the actual raw bytes, which this
        // catches regardless of whether they arrived as a block or inline.
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Link { link_type, dest_url: sanitize_reference(&dest_url, resolver), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Image { link_type, dest_url: sanitize_reference(&dest_url, resolver), title, id })
        }
        other => other,
    }
}

/// An absolute-scheme reference (`https://...`, `mailto:...`, a
/// protocol-relative `//host/...`, or an in-page `#fragment`) is genuinely
/// external content, not a workspace-relative path — so it never goes
/// through [`crate::is_safe_relative_asset`]/[`AssetResolver`] at all.
/// `crate::is_safe_relative_asset` doesn't itself reject every scheme form
/// (e.g. `mailto:` has no `://`), so this check runs first rather than
/// relying on that gate to draw this particular line.
///
/// This only tests whether `reference` is syntactically absolute per RFC
/// 3986's scheme grammar — it says nothing about which scheme, so every
/// caller must pair it with [`is_safe_absolute_reference`] before treating
/// the reference as safe to pass through: `javascript:...` and
/// `data:...` are syntactically absolute too.
fn is_absolute_reference(reference: &str) -> bool {
    if reference.starts_with('#') || reference.starts_with("//") {
        return true;
    }
    reference.find(':').is_some_and(|colon| {
        let scheme = &reference[..colon];
        !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Schemes an absolute reference is allowed to carry through to the
/// rendered `href`/`src` unchanged. Deliberately an explicit allowlist
/// rather than "any syntactically valid RFC 3986 scheme"
/// ([`is_absolute_reference`]): syntactic validity says nothing about
/// whether a `WebView` is safe to act on the result, and `javascript:`,
/// `vbscript:`, and `data:` are all syntactically valid schemes too.
const SAFE_ABSOLUTE_SCHEMES: [&str; 3] = ["https", "http", "mailto"];

/// True only for an [`is_absolute_reference`] reference whose scheme (or
/// lack of one, for a fragment/protocol-relative reference, neither of
/// which carries a scheme to check) is on [`SAFE_ABSOLUTE_SCHEMES`].
fn is_safe_absolute_reference(reference: &str) -> bool {
    if reference.starts_with('#') || reference.starts_with("//") {
        return true;
    }
    match reference.find(':') {
        Some(colon) => SAFE_ABSOLUTE_SCHEMES.contains(&reference[..colon].to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Rewrites (or neutralizes) one link/image destination per
/// [`render_markdown`]'s reference-handling rules.
fn sanitize_reference<'a>(reference: &str, resolver: &dyn AssetResolver) -> CowStr<'a> {
    if is_absolute_reference(reference) {
        // Syntactically absolute, but only pass it through unchanged if
        // its scheme is actually on the allowlist — an unsafe scheme
        // (`javascript:`, `vbscript:`, `data:`, ...) is neutralized here,
        // the same way an unresolved/unsafe relative reference is below,
        // rather than falling through to the relative-asset gate (which
        // has no notion of schemes to reject it on).
        return if is_safe_absolute_reference(reference) {
            CowStr::from(reference.to_string())
        } else {
            CowStr::from("")
        };
    }
    if crate::is_safe_relative_asset(reference)
        && let Some(resolved) = resolver.resolve(reference)
    {
        return CowStr::from(resolved);
    }
    // Neutralized: an empty destination renders an inert link/image
    // (`href=""`/`src=""`) rather than a working one, without dropping the
    // surrounding tag structure or the element's text/alt content.
    CowStr::from("")
}

#[cfg(test)]
mod tests {
    use super::{
        AssetResolver, MAX_INPUT_BYTES, NoAssetResolver, RenderError, render_markdown, render_markdown_bounded,
    };

    struct MapResolver;
    impl AssetResolver for MapResolver {
        fn resolve(&self, reference: &str) -> Option<String> {
            if reference == "images/logo.png" {
                Some("/assets/abc123/images/logo.png".to_string())
            } else {
                None
            }
        }
    }

    #[test]
    fn headings_lists_and_emphasis_render() {
        let html = render_markdown("# Title\n\n- one\n- two\n\n*em* and **strong**\n", &NoAssetResolver).unwrap();
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");
        assert!(html.contains("<em>em</em>"), "{html}");
        assert!(html.contains("<strong>strong</strong>"), "{html}");
    }

    #[test]
    fn tables_render_when_enabled() {
        let html = render_markdown("| a | b |\n| - | - |\n| 1 | 2 |\n", &NoAssetResolver).unwrap();
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<td>1</td>"), "{html}");
    }

    #[test]
    fn fenced_code_block_carries_a_language_class() {
        let html = render_markdown("```rust\nfn main() {}\n```\n", &NoAssetResolver).unwrap();
        assert!(html.contains("class=\"language-rust\""), "{html}");
        assert!(html.contains("fn main() {}"), "{html}");
    }

    #[test]
    fn raw_inline_html_is_escaped_not_executed() {
        let html = render_markdown("Look: <script>alert(1)</script> after text.\n", &NoAssetResolver).unwrap();
        assert!(!html.contains("<script>"), "raw tag leaked through: {html}");
        assert!(html.contains("&lt;script&gt;"), "expected escaped form: {html}");
    }

    #[test]
    fn raw_html_block_is_escaped_not_executed() {
        let html = render_markdown("<div onclick=\"evil()\">\n\nhi\n\n</div>\n", &NoAssetResolver).unwrap();
        assert!(!html.contains("<div onclick"), "raw block leaked through: {html}");
        assert!(html.contains("&lt;div"), "expected escaped form: {html}");
    }

    #[test]
    fn external_absolute_url_passes_through_unchanged() {
        let html = render_markdown("[go](https://example.test/x)\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"https://example.test/x\""), "{html}");
    }

    #[test]
    fn mailto_scheme_passes_through_unchanged() {
        let html = render_markdown("[mail](mailto:a@example.test)\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"mailto:a@example.test\""), "{html}");
    }

    #[test]
    fn safe_relative_reference_is_rewritten_via_resolver() {
        let html = render_markdown("![logo](images/logo.png)\n", &MapResolver).unwrap();
        assert!(html.contains("src=\"/assets/abc123/images/logo.png\""), "{html}");
    }

    #[test]
    fn unresolved_relative_reference_is_neutralized() {
        let html = render_markdown("![missing](images/other.png)\n", &MapResolver).unwrap();
        assert!(html.contains("src=\"\""), "expected neutralized src: {html}");
        assert!(!html.contains("images/other.png"), "raw workspace path leaked into output: {html}");
    }

    #[test]
    fn unsafe_relative_reference_is_neutralized_not_passed_through() {
        let html = render_markdown("[escape](../../etc/passwd)\n", &MapResolver).unwrap();
        assert!(html.contains("href=\"\""), "expected neutralized href: {html}");
        assert!(!html.contains("etc/passwd"), "traversal path leaked into output: {html}");
    }

    #[test]
    fn javascript_scheme_link_is_neutralized_not_passed_through() {
        let html = render_markdown("[click me](javascript:alert(document.cookie))\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"\""), "expected neutralized href: {html}");
        assert!(!html.contains("javascript:"), "javascript: scheme leaked into output: {html}");
    }

    #[test]
    fn javascript_scheme_image_is_neutralized_not_passed_through() {
        let html = render_markdown("![x](javascript:alert(1))\n", &NoAssetResolver).unwrap();
        assert!(html.contains("src=\"\""), "expected neutralized src: {html}");
        assert!(!html.contains("javascript:"), "javascript: scheme leaked into output: {html}");
    }

    #[test]
    fn vbscript_scheme_reference_is_neutralized_not_passed_through() {
        let html = render_markdown("[click me](vbscript:msgbox(1))\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"\""), "expected neutralized href: {html}");
        assert!(!html.contains("vbscript:"), "vbscript: scheme leaked into output: {html}");
    }

    #[test]
    fn data_scheme_reference_is_neutralized_not_passed_through() {
        let html =
            render_markdown("[click me](data:text/html,<script>alert(1)</script>)\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"\""), "expected neutralized href: {html}");
        assert!(!html.contains("data:text/html"), "data: scheme leaked into output: {html}");
    }

    #[test]
    fn uppercase_javascript_scheme_is_also_neutralized() {
        // Scheme comparison must be case-insensitive per RFC 3986 — a
        // WebView doesn't care about the casing of `javascript:` either.
        let html = render_markdown("[click me](JavaScript:alert(1))\n", &NoAssetResolver).unwrap();
        assert!(html.contains("href=\"\""), "expected neutralized href: {html}");
        assert!(!html.to_lowercase().contains("javascript:"), "javascript: scheme leaked into output: {html}");
    }

    #[test]
    fn allowlisted_absolute_schemes_still_pass_through_unchanged() {
        let html = render_markdown(
            "[a](https://example.test/x) [b](http://example.test/y) [c](mailto:a@example.test)\n",
            &NoAssetResolver,
        )
        .unwrap();
        assert!(html.contains("href=\"https://example.test/x\""), "{html}");
        assert!(html.contains("href=\"http://example.test/y\""), "{html}");
        assert!(html.contains("href=\"mailto:a@example.test\""), "{html}");
    }

    #[test]
    fn a_hostile_reference_does_not_fail_the_whole_render() {
        let html = render_markdown("before [escape](../x) after\n", &MapResolver).unwrap();
        assert!(html.contains("before"), "{html}");
        assert!(html.contains("after"), "{html}");
    }

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let huge = "a".repeat(MAX_INPUT_BYTES + 1);
        let error = render_markdown(&huge, &NoAssetResolver).unwrap_err();
        assert_eq!(error, RenderError::InputTooLarge { limit: MAX_INPUT_BYTES, actual: huge.len() });
    }

    #[test]
    fn oversized_output_is_rejected_with_a_small_bound() {
        // A pathological input well under a byte-count input limit can
        // still expand past a small output limit — bound both ends
        // independently rather than assuming input size bounds output size.
        let source = "* item\n".repeat(2_000);
        let error = render_markdown_bounded(&source, &NoAssetResolver, MAX_INPUT_BYTES, 64).unwrap_err();
        assert_eq!(error, RenderError::OutputTooLarge { limit: 64 });
    }

    #[test]
    fn exactly_at_the_input_bound_is_accepted() {
        let source = "a".repeat(MAX_INPUT_BYTES);
        assert!(render_markdown(&source, &NoAssetResolver).is_ok());
    }
}
