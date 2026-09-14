use super::{content::clean_html_fragment, MAX_SAFE_HTML_SOURCE_BYTES};
use crate::domain::clipboard::{ClipboardTextFormat, ClipboardTextPreview};

fn detect_format(source: &str, is_html: bool) -> ClipboardTextFormat {
    let text = source.trim();
    let html = regex::Regex::new(r"(?is)^<(?:!doctype\s+html|html\b|(?:p|div|h[1-6]|table|ul|ol|blockquote|pre|strong|em)\b[^>]*>.*</)").unwrap();
    let markdown = regex::Regex::new(r"(?m)^(?:#{1,6} |```|~~~|> |[-*+] \S|[0-9]+\. \S)|\[[^\]\n]+\]\(https?://[^)]+\)|\*\*[^*\n]+\*\*|(?m)^\|?.+\|.+\n\|?\s*:?-{3,}").unwrap();
    if is_html || html.is_match(text) {
        ClipboardTextFormat::Html
    } else if markdown.is_match(text) {
        ClipboardTextFormat::Markdown
    } else {
        ClipboardTextFormat::Text
    }
}

pub(super) fn build_preview(
    source: String,
    is_html: bool,
    format: Option<ClipboardTextFormat>,
) -> ClipboardTextPreview {
    let format = format.unwrap_or_else(|| detect_format(&source, is_html));
    let render_limited =
        format != ClipboardTextFormat::Text && source.len() > MAX_SAFE_HTML_SOURCE_BYTES;
    let safe_html = if render_limited {
        None
    } else {
        match format {
            ClipboardTextFormat::Text => None,
            ClipboardTextFormat::Html => clean_html_fragment(&source),
            ClipboardTextFormat::Markdown => {
                let options = pulldown_cmark::Options::ENABLE_TABLES
                    | pulldown_cmark::Options::ENABLE_STRIKETHROUGH;
                let mut html = String::new();
                pulldown_cmark::html::push_html(
                    &mut html,
                    pulldown_cmark::Parser::new_ext(&source, options),
                );
                clean_html_fragment(&html)
            }
        }
    };
    ClipboardTextPreview {
        source,
        format,
        safe_html,
        render_limited,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_structure_is_rendered_and_active_content_removed() {
        let source = "# 标题\n\n**加粗**\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet a = 1;\n```\n<script>alert(1)</script><img src='https://example.com/pixel'>";
        let preview = build_preview(source.into(), false, None);
        assert_eq!(preview.format, ClipboardTextFormat::Markdown);
        assert_eq!(preview.source, source);
        let html = preview.safe_html.unwrap();
        for tag in ["<h1>", "<strong>", "<table>", "<pre><code>"] {
            assert!(html.contains(tag), "{html}");
        }
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("https://example.com"));
    }
    #[test]
    fn html_source_is_exact_and_preview_does_not_use_card_truncation() {
        let source = format!("<p onclick='bad()'>{}末尾</p>", "文".repeat(13_000));
        let preview = build_preview(source.clone(), true, None);
        assert_eq!(preview.source, source);
        let html = preview.safe_html.unwrap();
        assert!(html.contains("末尾"));
        assert!(!html.contains("onclick"));
        let preview = build_preview(source, true, Some(ClipboardTextFormat::Text));
        assert!(preview.safe_html.is_none());
    }
    #[test]
    fn oversized_source_is_preserved_with_explicit_render_limit() {
        let source = "文".repeat(MAX_SAFE_HTML_SOURCE_BYTES);
        let preview = build_preview(source.clone(), true, None);
        assert!(preview.render_limited);
        assert!(preview.safe_html.is_none());
        assert_eq!(preview.source, source);
    }
    #[test]
    fn plain_text_is_not_misclassified() {
        for source in ["hello_world", "a < b > c", "2 * 3 = 6", "some # text"] {
            assert_eq!(detect_format(source, false), ClipboardTextFormat::Text);
        }
        assert_eq!(
            detect_format("<p>Hello</p>", false),
            ClipboardTextFormat::Html
        );
    }
}
