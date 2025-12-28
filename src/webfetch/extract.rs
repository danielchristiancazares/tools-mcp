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
    // Check Content-Type header first (most reliable)
    if let Some(ct) = content_type {
        if ct.to_ascii_lowercase().contains("html") {
            return true;
        }
    }
    // Fall back to content sniffing for the first 256 bytes
    let sample = bytes
        .iter()
        .take(256)
        .map(|c| *c as char)
        .collect::<String>()
        .to_ascii_lowercase();
    sample.contains("<html") || sample.contains("<!doctype html")
}

// ============================================================================
// Metadata Extraction
// ============================================================================

/// Extracts the document title from the `<title>` tag.
///
/// Concatenates all text nodes within the title element and trims whitespace.
/// Returns `None` if no title tag exists or if it's empty.
fn extract_title(document: &Html) -> Option<String> {
    let selector = Selector::parse("title").ok()?;
    document
        .select(&selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .filter(|s| !s.is_empty())
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
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Fall back to meta http-equiv tag
            let selector = Selector::parse("meta[http-equiv=\"content-language\"]").ok()?;
            document
                .select(&selector)
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
    // Configure htmd with tag filtering for boilerplate removal
    let converter = HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "nav", "footer", "header"])
        .build();

    let md = converter.convert(html).unwrap_or_else(|_| html.to_string());

    // Normalize whitespace: collapse excessive blank lines, trim line endings
    let mut result = String::with_capacity(md.len());
    let mut consecutive_newlines = 0;
    for line in md.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_newlines += 1;
            // Allow max 2 consecutive newlines (one blank line)
            if consecutive_newlines <= 2 {
                result.push('\n');
            }
        } else {
            if consecutive_newlines > 0 && !result.is_empty() {
                // Newlines already pushed above
            }
            consecutive_newlines = 0;
            result.push_str(trimmed);
            result.push('\n');
        }
    }
    result.trim().to_string()
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
fn extract_from_html(bytes: &[u8], _source_url: &str) -> Result<ExtractedDocument> {
    let html_owned = String::from_utf8_lossy(bytes).to_string();
    let document = Html::parse_document(&html_owned);

    let title = extract_title(&document);
    let language = extract_language(&document);

    // Extract just the <body> content to avoid head/script/style noise
    let body_html = if let Ok(body_selector) = Selector::parse("body") {
        document
            .select(&body_selector)
            .next()
            .map(|body| body.inner_html())
            .unwrap_or_else(|| html_owned.clone())
    } else {
        html_owned.clone()
    };

    // Convert body HTML to markdown
    let markdown = clean_markdown(&body_html);

    Ok(ExtractedDocument {
        title,
        language,
        markdown,
    })
}

/// Extracts content from plain text bytes (non-HTML fallback).
///
/// For non-HTML content, we simply convert bytes to UTF-8 (lossy) and
/// return as-is. No metadata extraction is possible from plain text.
fn extract_from_text(bytes: &[u8]) -> Result<ExtractedDocument> {
    let text = String::from_utf8_lossy(bytes).to_string();
    Ok(ExtractedDocument {
        title: None,
        language: None,
        markdown: text,
    })
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
        extract_from_html(bytes, source_url)
    } else {
        // Non-HTML content: treat as plain text/Markdown
        extract_from_text(bytes)
    }
}
