use anyhow::Result;
use htmd::HtmlToMarkdown;
use scraper::{Html, Selector};

/// Parsed document metadata alongside normalized Markdown text.
pub struct ExtractedDocument {
    pub title: Option<String>,
    pub language: Option<String>,
    pub markdown: String,
}

fn looks_like_html(content_type: Option<&str>, bytes: &[u8]) -> bool {
    if let Some(ct) = content_type {
        if ct.to_ascii_lowercase().contains("html") {
            return true;
        }
    }
    let sample = bytes
        .iter()
        .take(256)
        .map(|c| *c as char)
        .collect::<String>()
        .to_ascii_lowercase();
    sample.contains("<html") || sample.contains("<!doctype html")
}

fn extract_title(document: &Html) -> Option<String> {
    let selector = Selector::parse("title").ok()?;
    document
        .select(&selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .filter(|s| !s.is_empty())
}

fn extract_language(document: &Html) -> Option<String> {
    document
        .root_element()
        .value()
        .attr("lang")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Try to read <meta http-equiv="content-language">
            let selector = Selector::parse("meta[http-equiv=\"content-language\"]").ok()?;
            document
                .select(&selector)
                .filter_map(|node| node.value().attr("content"))
                .map(|s| s.trim().to_string())
                .find(|lang| !lang.is_empty())
        })
}

fn clean_markdown(html: &str) -> String {
    // Configure htmd to skip common boilerplate tags and produce inline links
    let converter = HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "nav", "footer", "header"])
        .build();

    converter.convert(html).unwrap_or_else(|_| html.to_string())
}

/// Convert HTML to Markdown without readability preprocessing.
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

fn extract_from_text(bytes: &[u8]) -> Result<ExtractedDocument> {
    let text = String::from_utf8_lossy(bytes).to_string();
    Ok(ExtractedDocument {
        title: None,
        language: None,
        markdown: text,
    })
}

/// Determine the best strategy for extracting normalized Markdown from the response body.
pub fn extract(
    bytes: &[u8],
    content_type: Option<&str>,
    source_url: &str,
) -> Result<ExtractedDocument> {
    if looks_like_html(content_type, bytes) {
        extract_from_html(bytes, source_url)
    } else {
        // Assume UTF-8 text/Markdown when the payload is not HTML; callers can inspect token counts to validate.
        extract_from_text(bytes)
    }
}
