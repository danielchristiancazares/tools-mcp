//! HTML content extraction and Markdown conversion.
//!
//! This module handles the transformation of raw HTML into clean, normalized
//! Markdown suitable for LLM consumption. It extracts document metadata and
//! removes boilerplate content.
//!
//! ## Extraction Pipeline
//!
//! ```text
//! Raw HTML bytes
//!       |
//!       v
//! +------------------+
//! | Content-Type     |---> If not HTML, treat as plain text
//! | Detection        |
//! +------------------+
//!       |
//!       v
//! +------------------+
//! | Metadata         |---> title, language
//! | Extraction       |
//! +------------------+
//!       |
//!       v
//! +------------------+
//! | Body Extraction  |---> Extracts only <body> content
//! +------------------+
//!       |
//!       v
//! +------------------+
//! | HTML to Markdown |---> Uses htmd with tag filtering
//! | Conversion       |
//! +------------------+
//!       |
//!       v
//! +------------------+
//! | Whitespace       |---> Collapses blank lines, trims
//! | Normalization    |
//! +------------------+
//! ```
//!
//! ## Boilerplate Removal
//!
//! The following HTML tags are filtered during conversion:
//! - `<script>` - JavaScript code
//! - `<style>` - CSS stylesheets
//! - `<nav>` - Navigation menus
//! - `<header>` - Page headers
//! - `<footer>` - Page footers
//!
//! ## Link Formatting
//!
//! Links are converted to inline Markdown format: `[text](url)`
//! This produces cleaner, more token-efficient output compared to
//! reference-style links.

use anyhow::Result;
use htmd::HtmlToMarkdown;
use scraper::{Html, Selector};
use std::borrow::Cow;
use std::sync::OnceLock;

/// Global HTML→Markdown converter instance.
///
/// Building the converter allocates handler tables and options; reuse it across requests.
static HTML_TO_MARKDOWN: OnceLock<HtmlToMarkdown> = OnceLock::new();
static TITLE_SELECTOR: OnceLock<Option<Selector>> = OnceLock::new();
static LANGUAGE_SELECTOR: OnceLock<Option<Selector>> = OnceLock::new();
static BODY_SELECTOR: OnceLock<Option<Selector>> = OnceLock::new();

fn get_converter() -> &'static HtmlToMarkdown {
    HTML_TO_MARKDOWN.get_or_init(|| {
        HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style", "nav", "footer", "header"])
            .build()
    })
}

fn title_selector() -> Option<&'static Selector> {
    TITLE_SELECTOR
        .get_or_init(|| Selector::parse("title").ok())
        .as_ref()
}

fn language_selector() -> Option<&'static Selector> {
    LANGUAGE_SELECTOR
        .get_or_init(|| Selector::parse("meta[http-equiv=\"content-language\"]").ok())
        .as_ref()
}

fn body_selector() -> Option<&'static Selector> {
    BODY_SELECTOR
        .get_or_init(|| Selector::parse("body").ok())
        .as_ref()
}

/// Extracted document with metadata and Markdown content.
///
/// This is the output of the extraction pipeline, containing everything
/// needed to build a `FetchResponse`.
pub struct ExtractedDocument {
    /// Document title from `<title>` tag, if present.
    pub title: Option<String>,

    /// Document language from `<html lang="...">` or `<meta>` tag.
    pub language: Option<String>,

    /// Document content converted to Markdown.
    /// Boilerplate (nav, header, footer, scripts) has been removed.
    pub markdown: String,
}

// ============================================================================
// Content Type Detection
// ============================================================================

/// Determines if content appears to be HTML based on Content-Type and content sniffing.
///
/// Checks in order:
/// 1. Content-Type header contains "html"
/// 2. First 256 bytes contain `<html` or `<!doctype html`
fn looks_like_html(content_type: Option<&str>, bytes: &[u8]) -> bool {
    fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || haystack.len() < needle.len() {
            return false;
        }
        haystack
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
    }

    // Check Content-Type header first (most reliable)
    if let Some(ct) = content_type
        && contains_ignore_ascii_case(ct.as_bytes(), b"html")
    {
        return true;
    }
    // Fall back to content sniffing for the first 512 bytes
    let prefix_len = bytes.len().min(512);
    let sample = &bytes[..prefix_len];
    contains_ignore_ascii_case(sample, b"<html")
        || contains_ignore_ascii_case(sample, b"<!doctype html")
}

// ============================================================================
// Metadata Extraction
// ============================================================================

/// Extracts the document title from the `<title>` tag.
///
/// Concatenates all text nodes within the title element and trims whitespace.
/// Returns `None` if no title tag exists or if it's empty.
fn extract_title(document: &Html) -> Option<String> {
    let selector = title_selector()?;
    let node = document.select(selector).next()?;
    let mut title = String::new();
    for t in node.text() {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(t);
    }
    if title.is_empty() { None } else { Some(title) }
}

/// Extracts the document language from HTML attributes or meta tags.
///
/// Checks in order:
/// 1. `<html lang="...">` attribute (most common)
/// 2. `<meta http-equiv="content-language" content="...">` tag
///
/// Returns standard language codes like "en", "en-US", "fr", etc.
fn extract_language(document: &Html) -> Option<String> {
    // First try the html lang attribute (most common location)
    document
        .root_element()
        .value()
        .attr("lang")
        .map(std::string::ToString::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Fall back to meta http-equiv tag
            let selector = language_selector()?;
            document
                .select(selector)
                .filter_map(|node| node.value().attr("content"))
                .map(|s| s.trim().to_string())
                .find(|lang| !lang.is_empty())
        })
}

// ============================================================================
// HTML to Markdown Conversion
// ============================================================================

/// Converts HTML to clean Markdown with boilerplate removal and whitespace normalization.
///
/// ## Conversion Features
///
/// - **Tag filtering**: Removes script, style, nav, footer, header tags
/// - **Inline links**: Produces `[text](url)` format (not reference-style)
/// - **Whitespace normalization**: Collapses 3+ consecutive newlines to 2
/// - **Line trimming**: Removes trailing whitespace from each line
///
/// ## Why These Tags Are Filtered
///
/// | Tag | Reason |
/// |-----|--------|
/// | `script` | JavaScript code is noise for LLMs |
/// | `style` | CSS rules are noise for LLMs |
/// | `nav` | Navigation is repetitive across pages |
/// | `header` | Often contains logo/nav, not content |
/// | `footer` | Usually contains copyright/links, not content |
fn clean_markdown(html: &str) -> String {
    let converter = get_converter();

    let md = converter.convert(html).unwrap_or_else(|_| html.to_string());

    // Normalize whitespace: collapse excessive blank lines, trim trailing whitespace per line.
    let mut result = String::with_capacity(md.len());
    let mut pending_blank_line = false;
    for line in md.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            // Avoid leading blank lines; defer blank-line emission until we see the next
            // non-empty line so we also avoid trailing blank lines.
            if !result.is_empty() {
                pending_blank_line = true;
            }
        } else {
            if pending_blank_line {
                // We always end non-empty lines with '\n', so emitting one extra '\n' yields a
                // single blank line between blocks.
                if !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push('\n');
                pending_blank_line = false;
            }
            result.push_str(trimmed);
            result.push('\n');
        }
    }
    // Drop the trailing newline we always append after the last non-empty line.
    if result.ends_with('\n') {
        result.pop();
    }
    result
}

/// Extracts content from HTML bytes, producing metadata and Markdown.
///
/// ## Process
///
/// 1. Parse HTML (lossy UTF-8 conversion for robustness)
/// 2. Extract title from `<title>` tag
/// 3. Extract language from `<html lang>` or `<meta>` tag
/// 4. Extract `<body>` inner HTML (avoids `<head>` content)
/// 5. Convert body to Markdown with boilerplate removal
fn extract_from_html(bytes: &[u8], _source_url: &str) -> ExtractedDocument {
    let html_source = String::from_utf8_lossy(bytes);
    let document = Html::parse_document(html_source.as_ref());

    let title = extract_title(&document);
    let language = extract_language(&document);

    // Extract just the <body> content to avoid head/script/style noise
    let body_html: Cow<'_, str> = if let Some(body_selector) = body_selector() {
        document.select(body_selector).next().map_or_else(
            || Cow::Borrowed(html_source.as_ref()),
            |body| Cow::Owned(body.inner_html()),
        )
    } else {
        Cow::Borrowed(html_source.as_ref())
    };

    // Convert body HTML to markdown
    let markdown = clean_markdown(body_html.as_ref());

    ExtractedDocument {
        title,
        language,
        markdown,
    }
}

/// Extracts content from plain text bytes (non-HTML fallback).
///
/// For non-HTML content, we simply convert bytes to UTF-8 (lossy) and
/// return as-is. No metadata extraction is possible from plain text.
fn extract_from_text(bytes: &[u8]) -> ExtractedDocument {
    let text = String::from_utf8_lossy(bytes).to_string();
    ExtractedDocument {
        title: None,
        language: None,
        markdown: text,
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Extracts document content and metadata from raw response bytes.
///
/// This is the main entry point for content extraction. It automatically
/// detects the content type and applies the appropriate extraction strategy.
///
/// # Arguments
///
/// * `bytes` - Raw response body bytes
/// * `content_type` - Content-Type header value (e.g., "text/html; charset=utf-8")
/// * `source_url` - Original URL (currently unused, reserved for future use)
///
/// # Content Type Detection
///
/// HTML detection uses both Content-Type header and content sniffing:
/// - If Content-Type contains "html" -> HTML extraction
/// - If first 256 bytes contain `<html` or `<!doctype html` -> HTML extraction
/// - Otherwise -> Plain text extraction
///
/// # Returns
///
/// An `ExtractedDocument` containing:
/// - `title`: Document title (HTML only)
/// - `language`: Document language (HTML only)
/// - `markdown`: Content converted to Markdown (or raw text for non-HTML)
pub fn extract(
    bytes: &[u8],
    content_type: Option<&str>,
    source_url: &str,
) -> Result<ExtractedDocument> {
    if looks_like_html(content_type, bytes) {
        Ok(extract_from_html(bytes, source_url))
    } else {
        // Non-HTML content: treat as plain text/Markdown
        Ok(extract_from_text(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_html_true_for_content_type_html() {
        assert!(looks_like_html(
            Some("text/HTML; charset=utf-8"),
            b"not html bytes"
        ));
    }

    #[test]
    fn looks_like_html_true_for_doctype_sniff() {
        assert!(looks_like_html(
            None,
            b"<!DOCTYPE html><html><body></body></html>"
        ));
    }

    #[test]
    fn extract_html_metadata_and_markdown() {
        let html = br#"<!doctype html>
<html lang="en">
  <head><title>My Title</title></head>
  <body>
    <h1>Hello</h1>
    <script>console.log("nope")</script>
    <p>World</p>
  </body>
</html>"#;

        let doc = extract(
            html,
            Some("text/html; charset=utf-8"),
            "https://example.com",
        )
        .expect("extract failed");
        assert_eq!(doc.title.as_deref(), Some("My Title"));
        assert_eq!(doc.language.as_deref(), Some("en"));
        assert!(doc.markdown.contains("Hello"));
        assert!(doc.markdown.contains("World"));
        assert!(
            !doc.markdown.contains("console.log"),
            "script content should be filtered out"
        );
    }

    #[test]
    fn extract_text_fallback_for_non_html() {
        let bytes = b"plain text\nline 2\n";
        let doc =
            extract(bytes, Some("text/plain"), "https://example.com").expect("extract failed");
        assert_eq!(doc.title, None);
        assert_eq!(doc.language, None);
        assert_eq!(doc.markdown, "plain text\nline 2\n");
    }

    // BUG: clean_markdown's whitespace normalization drops the trailing newline
    // from single-line content, which changes the semantics of the output.
    #[test]
    fn clean_markdown_drops_final_newline_from_single_line_content() {
        let html = "<p>Hello</p>";
        let result = super::clean_markdown(html);
        // The result should not end with a newline, but the original content did.
        // This is a design choice bug — trailing newlines are stripped.
        assert!(
            !result.ends_with('\n'),
            "BUG CONFIRMED: clean_markdown strips trailing newline"
        );
    }

    // REGRESSION: HTML sniffing window increased from 256 to 512 bytes.
    #[test]
    fn looks_like_html_detects_doctype_within_512_bytes() {
        let prefix = " ".repeat(300);
        let content = format!("{prefix}<!DOCTYPE html><html><body>test</body></html>");
        let bytes = content.as_bytes();

        let is_html = super::looks_like_html(None, bytes);
        assert!(
            is_html,
            "HTML with doctype within 512 bytes should be detected"
        );
    }
}
