//! Data types for `WebFetch` request and response payloads.
//!
//! These structures define the MCP tool interface for the `WebFetch` functionality.
//! All types implement `Serialize` and `Deserialize` for JSON-RPC transport.

use serde::{Deserialize, Serialize};

/// Request payload for the `WebFetch` MCP tool.
///
/// This structure is deserialized from the JSON-RPC `arguments` field when the
/// tool is invoked. All fields except `url` have sensible defaults.
///
/// # Example JSON
///
/// ```json
/// {
///   "url": "https://example.com/docs",
///   "max_chunk_tokens": 1000,
///   "no_cache": false,
///   "force_browser": true
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    /// The URL to fetch. Must be an absolute URL with `http://` or `https://` scheme.
    ///
    /// # Validation
    ///
    /// The URL is validated for SSRF protection before fetching:
    /// - Only `http` and `https` schemes are allowed
    /// - `localhost` and private IP ranges are blocked
    /// - DNS resolution is checked to prevent hostname-based bypasses
    pub url: String,

    /// Maximum tokens per chunk. Chunks exceeding this limit are split.
    ///
    /// Uses `OpenAI`'s `cl100k_base` tokenizer (GPT-4 compatible). Defaults to 600
    /// tokens if not specified, which keeps chunks under typical tool response limits
    /// while preserving sufficient context.
    #[serde(default)]
    pub max_chunk_tokens: Option<usize>,

    /// When `true`, bypasses the disk cache and fetches fresh content.
    ///
    /// Useful when you need the latest version of a page that may have been
    /// updated since it was cached. The fresh content is still written to cache
    /// for future requests.
    #[serde(default)]
    pub no_cache: bool,

    /// When `true`, forces headless browser rendering instead of HTTP fetch.
    ///
    /// Use this for:
    /// - JavaScript-heavy single-page applications (SPAs)
    /// - Sites with client-side rendering (React, Vue, Angular)
    /// - Pages that require JavaScript for content to appear
    ///
    /// When `false` (default), the fetcher uses HTTP first and automatically
    /// falls back to browser rendering if JS-heavy heuristics are detected.
    /// Requires Chrome/Chromium to be installed on the system.
    #[serde(default)]
    pub force_browser: bool,
}

/// A single chunk of extracted document content.
///
/// Documents are split into chunks to fit within LLM context windows and token
/// budgets. Each chunk includes its token count for budget management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchChunk {
    /// The most recent Markdown heading before this chunk's content, if any.
    ///
    /// Headings are extracted from Markdown `#` syntax. This helps provide
    /// context about which section of the document the chunk belongs to.
    /// May be `None` for content before the first heading or in documents
    /// without headings.
    pub heading: Option<String>,

    /// The chunk's text content in Markdown format.
    ///
    /// HTML has been converted to Markdown with:
    /// - Inline links: `[text](url)` format
    /// - Boilerplate removed: nav, footer, header, script, style tags filtered
    /// - Whitespace normalized: consecutive blank lines collapsed
    pub text: String,

    /// Token count for this chunk using `cl100k_base` tokenizer.
    ///
    /// This count is accurate for GPT-4 and GPT-3.5-turbo models. Use this
    /// value for token budget calculations when constructing prompts.
    pub token_count: usize,
}

/// Response payload returned by the `WebFetch` tool.
///
/// Contains the fetched and processed document content along with metadata
/// about the fetch operation (timing, rendering method, cache status).
///
/// # Example Response
///
/// ```json
/// {
///   "url": "https://example.com/docs",
///   "fetched_at": "2024-01-15T10:30:00Z",
///   "title": "Documentation",
///   "language": "en",
///   "chunks": [...],
///   "rendering_method": "browser",
///   "note": "rendered_with_browser"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    /// The URL that was fetched (same as request URL).
    pub url: String,

    /// ISO 8601 timestamp of when the content was fetched.
    ///
    /// For cached responses, this is when the content was originally fetched,
    /// not when it was served from cache.
    pub fetched_at: String,

    /// Document title extracted from `<title>` tag, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Document language from `<html lang="...">` or `<meta>` tag, if present.
    ///
    /// Uses standard language codes (e.g., "en", "en-US", "fr").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// Document content split into token-budgeted chunks.
    ///
    /// Empty if the document has no extractable content. Each chunk includes
    /// its heading context and token count.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<FetchChunk>,

    /// The rendering method used to fetch the page.
    ///
    /// - `"http"`: Standard HTTP fetch (faster, lower resource usage)
    /// - `"browser"`: Headless Chrome rendering (handles JavaScript)
    pub rendering_method: String,

    /// Optional notes about the fetch operation.
    ///
    /// Comma-separated values that may include:
    /// - `"cache_hit"`: Content was served from disk cache
    /// - `"rendered_with_browser"`: Browser rendering was used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
