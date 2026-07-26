//! Deterministic, escaped rendering for a deliberately small Markdown subset.
//!
//! Supported blocks are headings, paragraphs, fenced code, task-list items, and
//! simple pipe tables. This is not a complete `CommonMark` implementation.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderLimits {
    pub max_source_bytes: usize,
    pub max_blocks: usize,
    pub max_lines: usize,
    pub max_rendered_bytes: usize,
    pub max_inline_depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedDocument {
    pub generation: u64,
    pub html: String,
    pub blocks: Vec<RenderedBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedBlock {
    pub id: String,
    pub kind: BlockKind,
    pub html: String,
}

/// Stable, bounded identity for a rendered Markdown snapshot.
///
/// The identity intentionally includes the workspace and document key as well
/// as the revision and content digest, preventing annotations from leaking
/// across hosts or being applied to a different revision of the same path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownDocumentIdentity {
    pub workspace: String,
    pub document: String,
    pub revision: u64,
    pub content_digest: u64,
}

/// Computes a deterministic identity without relying on platform or crypto
/// providers. Callers must still authenticate the workspace/document keys.
#[must_use]
pub fn document_identity(
    workspace: &str,
    document: &str,
    revision: u64,
    source: &str,
    max_key_bytes: usize,
) -> Option<MarkdownDocumentIdentity> {
    if workspace.is_empty()
        || document.is_empty()
        || workspace.len() > max_key_bytes
        || document.len() > max_key_bytes
        || workspace.bytes().any(|b| b.is_ascii_control())
        || document.bytes().any(|b| b.is_ascii_control())
    {
        return None;
    }
    Some(MarkdownDocumentIdentity {
        workspace: workspace.to_owned(),
        document: document.to_owned(),
        revision,
        content_digest: fnv1a64(source.as_bytes()),
    })
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Heading,
    Paragraph,
    Code,
    Task,
    Table,
}

/// Renders a bounded, safe Markdown subset into deterministic HTML fragments.
///
/// Raw HTML is always text. Only confined root-relative targets produce active
/// links or images; schemes, traversal, fragments, and encoded paths are inert.
///
/// # Errors
///
/// Rejects invalid limits, oversized source, excessive lines/blocks/nesting,
/// unterminated code fences, and rendered-byte overflow.
pub fn render_safe_subset(
    source: &str,
    generation: u64,
    limits: RenderLimits,
) -> Result<RenderedDocument, MarkdownError> {
    validate_limits(limits)?;
    if source.len() > limits.max_source_bytes {
        return Err(MarkdownError::SourceLimit);
    }
    let lines: Vec<&str> = source.lines().collect();
    if lines.len() > limits.max_lines {
        return Err(MarkdownError::LineLimit);
    }
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(info) = line.strip_prefix("```") {
            let start = index;
            let (next, html) = render_code_block(&lines, index, info)?;
            index = next;
            push_block(&mut blocks, BlockKind::Code, html, start, limits)?;
            continue;
        }
        if let Some((level, text)) = heading(line) {
            push_block(
                &mut blocks,
                BlockKind::Heading,
                format!("<h{level}>{}</h{level}>", render_inline(text, limits)?),
                index,
                limits,
            )?;
            index += 1;
            continue;
        }
        if let Some((checked, text)) = task(line) {
            let state = if checked { " checked" } else { "" };
            push_block(
                &mut blocks,
                BlockKind::Task,
                format!(
                    "<div class=\"task\"><input type=\"checkbox\" disabled{state}>{}</div>",
                    render_inline(text, limits)?
                ),
                index,
                limits,
            )?;
            index += 1;
            continue;
        }
        if index + 1 < lines.len() && is_table_separator(lines[index + 1]) {
            let start = index;
            let (next, html) = render_table_block(&lines, index, limits)?;
            index = next;
            push_block(&mut blocks, BlockKind::Table, html, start, limits)?;
            continue;
        }

        let start = index;
        let mut paragraph = String::new();
        while index < lines.len() && !lines[index].trim().is_empty() {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(lines[index].trim());
            index += 1;
        }
        push_block(
            &mut blocks,
            BlockKind::Paragraph,
            format!("<p>{}</p>", render_inline(&paragraph, limits)?),
            start,
            limits,
        )?;
    }

    let mut html = String::new();
    for block in &blocks {
        html.push_str("<section id=\"");
        html.push_str(&block.id);
        html.push_str("\">");
        html.push_str(&block.html);
        html.push_str("</section>");
        if html.len() > limits.max_rendered_bytes {
            return Err(MarkdownError::RenderedLimit);
        }
    }
    Ok(RenderedDocument {
        generation,
        html,
        blocks,
    })
}

fn render_code_block(
    lines: &[&str],
    mut index: usize,
    info: &str,
) -> Result<(usize, String), MarkdownError> {
    index += 1;
    let mut code = String::new();
    while index < lines.len() && !lines[index].starts_with("```") {
        if !code.is_empty() {
            code.push('\n');
        }
        code.push_str(lines[index]);
        index += 1;
    }
    if index == lines.len() {
        return Err(MarkdownError::UnterminatedFence);
    }
    let language = safe_language(info.trim());
    let class = language.map_or(String::new(), |value| {
        format!(" class=\"language-{}\"", escape_attribute(value))
    });
    Ok((
        index + 1,
        format!("<pre><code{class}>{}</code></pre>", escape_text(&code)),
    ))
}

fn render_table_block(
    lines: &[&str],
    mut index: usize,
    limits: RenderLimits,
) -> Result<(usize, String), MarkdownError> {
    let headers = table_cells(lines[index]);
    index += 2;
    let mut rows = Vec::new();
    while index < lines.len() && lines[index].contains('|') && !lines[index].trim().is_empty() {
        rows.push(table_cells(lines[index]));
        index += 1;
    }
    let mut html = String::from("<table><thead><tr>");
    for cell in headers {
        html.push_str("<th>");
        html.push_str(&render_inline(cell, limits)?);
        html.push_str("</th>");
    }
    html.push_str("</tr></thead><tbody>");
    for row in rows {
        html.push_str("<tr>");
        for cell in row {
            html.push_str("<td>");
            html.push_str(&render_inline(cell, limits)?);
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table>");
    Ok((index, html))
}

fn validate_limits(limits: RenderLimits) -> Result<(), MarkdownError> {
    if limits.max_source_bytes == 0
        || limits.max_blocks == 0
        || limits.max_lines == 0
        || limits.max_rendered_bytes == 0
        || limits.max_inline_depth == 0
    {
        return Err(MarkdownError::InvalidLimits);
    }
    Ok(())
}

fn push_block(
    blocks: &mut Vec<RenderedBlock>,
    kind: BlockKind,
    html: String,
    source_line: usize,
    limits: RenderLimits,
) -> Result<(), MarkdownError> {
    if blocks.len() >= limits.max_blocks {
        return Err(MarkdownError::BlockLimit);
    }
    let id = format!("b-{source_line}-{}", blocks.len());
    blocks.push(RenderedBlock { id, kind, html });
    Ok(())
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let count = line.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&count).then(|| {
        line.get(count..)?
            .strip_prefix(' ')
            .map(|text| (count, text))
    })?
}

fn task(line: &str) -> Option<(bool, &str)> {
    let rest = line.strip_prefix("- [")?;
    let (marker, text) = rest.split_once("] ")?;
    match marker {
        " " => Some((false, text)),
        "x" | "X" => Some((true, text)),
        _ => None,
    }
}

fn table_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim_matches(':');
            cell.len() >= 3 && cell.bytes().all(|byte| byte == b'-')
        })
}

fn render_inline(input: &str, limits: RenderLimits) -> Result<String, MarkdownError> {
    let mut output = String::new();
    let mut remaining = input;
    let mut operations = 0;
    while !remaining.is_empty() {
        operations += 1;
        if operations
            > limits
                .max_source_bytes
                .saturating_add(limits.max_inline_depth)
        {
            return Err(MarkdownError::InlineLimit);
        }
        let image = remaining.find("![");
        let link = remaining.find('[');
        let next = match (image, link) {
            (Some(image), Some(link)) => image.min(link),
            (Some(image), None) => image,
            (None, Some(link)) => link,
            (None, None) => {
                output.push_str(&escape_text(remaining));
                break;
            }
        };
        output.push_str(&escape_text(&remaining[..next]));
        remaining = &remaining[next..];
        let is_image = remaining.starts_with("![");
        let label_start = usize::from(is_image) + 1;
        let Some(close) = remaining[label_start..].find("](") else {
            output.push_str(if is_image { "!&#91;" } else { "&#91;" });
            remaining = &remaining[label_start..];
            continue;
        };
        let close = label_start + close;
        let target_start = close + 2;
        let Some(target_end) = remaining[target_start..].find(')') else {
            output.push_str(&escape_text(&remaining[..label_start]));
            remaining = &remaining[label_start..];
            continue;
        };
        let target_end = target_start + target_end;
        let label = &remaining[label_start..close];
        let target = &remaining[target_start..target_end];
        if confined_target(target) {
            if is_image {
                output.push_str("<img alt=\"");
                output.push_str(&escape_attribute(label));
                output.push_str("\" src=\"");
                output.push_str(&escape_attribute(target));
                output.push_str("\">");
            } else {
                output.push_str("<a href=\"");
                output.push_str(&escape_attribute(target));
                output.push_str("\">");
                output.push_str(&escape_text(label));
                output.push_str("</a>");
            }
        } else {
            output.push_str("<span class=\"inert-link\">");
            output.push_str(&escape_text(label));
            output.push_str("</span>");
        }
        remaining = &remaining[target_end + 1..];
        if output.len() > limits.max_rendered_bytes {
            return Err(MarkdownError::RenderedLimit);
        }
    }
    Ok(output)
}

fn confined_target(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('/')
        && !target.starts_with('#')
        && !target.starts_with("//")
        && !target.contains('%')
        && !target.contains('\\')
        && !target
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        && !target
            .split('/')
            .next()
            .is_some_and(|part| part.contains(':'))
        && !target.bytes().any(|byte| byte.is_ascii_control())
}

fn safe_language(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then_some(value)
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(value: &str) -> String {
    escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownPlaceholder {
    RenderUnavailable,
    UnsafeTarget,
    LimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownError {
    InvalidLimits,
    SourceLimit,
    LineLimit,
    BlockLimit,
    InlineLimit,
    RenderedLimit,
    UnterminatedFence,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RenderLimits = RenderLimits {
        max_source_bytes: 2_048,
        max_blocks: 32,
        max_lines: 64,
        max_rendered_bytes: 8_192,
        max_inline_depth: 8,
    };

    #[test]
    fn renders_deterministic_supported_blocks() {
        let source = "# Review\n\n- [x] checked\n\n| A | B |\n| --- | --- |\n| one | two |\n\n```rust\nlet x = 1 < 2;\n```";
        let rendered = render_safe_subset(source, 7, LIMITS).unwrap();
        assert_eq!(rendered.generation, 7);
        assert_eq!(
            rendered
                .blocks
                .iter()
                .map(|block| block.kind)
                .collect::<Vec<_>>(),
            vec![
                BlockKind::Heading,
                BlockKind::Task,
                BlockKind::Table,
                BlockKind::Code
            ]
        );
        assert!(rendered.html.contains("<h1>Review</h1>"));
        assert!(rendered.html.contains("let x = 1 &lt; 2;"));
    }

    #[test]
    fn raw_html_and_script_are_only_escaped_text() {
        let rendered =
            render_safe_subset("<script>alert(1)</script> <img src=x onerror=x>", 1, LIMITS)
                .unwrap();
        assert!(!rendered.html.contains("<script>"));
        assert!(!rendered.html.contains("<img src=x"));
        assert!(rendered.html.contains("&lt;script&gt;"));
    }

    #[test]
    fn confined_links_activate_and_external_or_traversal_links_are_inert() {
        let rendered = render_safe_subset(
            "[safe](docs/a.md) [web](https://example.invalid) ![escape](../secret.png)",
            1,
            LIMITS,
        )
        .unwrap();
        assert!(rendered.html.contains("<a href=\"docs/a.md\">safe</a>"));
        assert!(
            rendered
                .html
                .contains("<span class=\"inert-link\">web</span>")
        );
        assert!(
            rendered
                .html
                .contains("<span class=\"inert-link\">escape</span>")
        );
        assert!(!rendered.html.contains("https://"));
    }

    #[test]
    fn hostile_attribute_characters_are_escaped() {
        let rendered = render_safe_subset("![a\" onerror=\"x](images/a.png)", 1, LIMITS).unwrap();
        assert!(rendered.html.contains("alt=\"a&quot; onerror=&quot;x\""));
        assert!(!rendered.html.contains(" onerror=\"x\""));
    }

    #[test]
    fn source_block_render_and_fence_limits_fail_closed() {
        let tiny = RenderLimits {
            max_source_bytes: 3,
            ..LIMITS
        };
        assert_eq!(
            render_safe_subset("long", 1, tiny),
            Err(MarkdownError::SourceLimit)
        );
        let one = RenderLimits {
            max_blocks: 1,
            ..LIMITS
        };
        assert_eq!(
            render_safe_subset("a\n\nb", 1, one),
            Err(MarkdownError::BlockLimit)
        );
        let rendered = RenderLimits {
            max_rendered_bytes: 5,
            ..LIMITS
        };
        assert_eq!(
            render_safe_subset("a", 1, rendered),
            Err(MarkdownError::RenderedLimit)
        );
        assert_eq!(
            render_safe_subset("```\ncode", 1, LIMITS),
            Err(MarkdownError::UnterminatedFence)
        );
    }

    #[test]
    fn document_identity_is_stable_and_revision_scoped() {
        let first = document_identity("ws-1", "docs/review.md", 4, "# A", 64).unwrap();
        let same = document_identity("ws-1", "docs/review.md", 4, "# A", 64).unwrap();
        let changed = document_identity("ws-1", "docs/review.md", 5, "# A", 64).unwrap();
        let edited = document_identity("ws-1", "docs/review.md", 4, "# B", 64).unwrap();
        assert_eq!(first, same);
        assert_ne!(first.revision, changed.revision);
        assert_ne!(first.content_digest, edited.content_digest);
    }

    #[test]
    fn document_identity_rejects_ambiguous_or_oversized_keys() {
        assert!(document_identity("", "doc", 1, "x", 16).is_none());
        assert!(document_identity("ws\n", "doc", 1, "x", 16).is_none());
        assert!(document_identity("workspace-too-long", "doc", 1, "x", 4).is_none());
    }
}
