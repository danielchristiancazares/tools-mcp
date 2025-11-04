use serde::{Deserialize, Serialize};

/// Request payload accepted by the `webfetch.fetch` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    /// The absolute or scheme-qualified URL to fetch.
    pub url: String,
    /// Optional token budget hint per chunk; defaults are applied when omitted.
    #[serde(default)]
    pub max_chunk_tokens: Option<usize>,
    /// Force bypassing the cache when set to true.
    #[serde(default)]
    pub no_cache: bool,
    /// Force using headless browser for rendering (bypasses HTTP-first heuristics).
    #[serde(default)]
    pub force_browser: bool,
}

/// A single document chunk with headings and normalized text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchChunk {
    pub heading: Option<String>,
    pub text: String,
    pub token_count: usize,
}

/// Response payload returned by the `webfetch.fetch` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub url: String,
    pub fetched_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<FetchChunk>,
    /// Rendering method used: "http" or "browser"
    pub rendering_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
