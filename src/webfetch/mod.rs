//! # WebFetch Module
//!
//! A token-aware web content fetcher designed for LLM consumption. This module provides
//! a complete pipeline for fetching, rendering, extracting, and chunking web content
//! while respecting robots.txt and protecting against SSRF attacks.
//!
//! ## Architecture Overview
//!
//! The WebFetch pipeline consists of several stages:
//!
//! ```text
//! +------------------+     +------------------+     +------------------+
//! |   URL Validation |---->|   Content Fetch  |---->|   HTML Extract   |
//! |   (SSRF check)   |     |   (HTTP/Browser) |     |   (to Markdown)  |
//! +------------------+     +------------------+     +------------------+
//!                                   |                        |
//!                          +--------v--------+      +--------v--------+
//!                          |  robots.txt     |      |  Token Chunking |
//!                          |  Compliance     |      |  (cl100k_base)  |
//!                          +-----------------+      +-----------------+
//! ```
//!
//! ## Hybrid Rendering Strategy
//!
//! The module implements a smart HTTP-first approach with automatic browser fallback:
//!
//! 1. **Whitelisted domains**: Known JS-heavy sites (React, Vue, Angular apps) immediately
//!    use headless browser rendering. See [`whitelist`] for the domain list.
//!
//! 2. **HTTP-first with heuristics**: For other URLs, HTTP fetch is attempted first.
//!    The response is analyzed for JS-heavy indicators (SPA shells, framework signatures,
//!    high script density). If detected, browser rendering is triggered automatically.
//!    See [`heuristics`] for detection algorithms.
//!
//! 3. **Explicit browser mode**: Callers can force browser rendering via `force_browser=true`.
//!
//! ## Security Features
//!
//! - **SSRF Protection**: URLs are validated before fetch. Blocked: `file://`, `localhost`,
//!   private IPs (10.x, 172.16-31.x, 192.168.x), and reserved ranges. DNS resolution is
//!   checked to prevent hostname-based bypasses. See [`http::validate_url_ssrf`].
//!
//! - **robots.txt Compliance**: The fetcher respects robots.txt directives. Disallowed
//!   URLs are rejected with an error. Results are cached per-domain to minimize overhead.
//!
//! - **DNS Rebinding Mitigation**: Resolved addresses are pinned for the HTTP request
//!   to prevent time-of-check/time-of-use attacks.
//!
//! ## Caching
//!
//! Fetched content is cached on disk under the system temp directory (`/tmp/tools-webfetch`
//! on Unix, `%TEMP%\tools-webfetch` on Windows). Cache keys include the rendering method
//! to keep HTTP and browser-rendered content separate. See [`cache`] for implementation.
//!
//! ## Token-Aware Chunking
//!
//! Content is chunked using OpenAI's `cl100k_base` tokenizer (GPT-4 compatible). Chunks
//! respect heading boundaries when possible and include token counts for budget management.
//! Default chunk size is 600 tokens. See [`chunker`] for details.
//!
//! ## Submodules
//!
//! - [`browser`]: Headless Chrome pool with automatic lifecycle management
//! - [`cache`]: Disk-based response caching with SHA-256 key hashing
//! - [`chunker`]: Token-aware Markdown splitting using tiktoken
//! - [`extract`]: HTML to Markdown conversion with boilerplate removal
//! - [`heuristics`]: JS-heavy site detection algorithms
//! - [`http`]: HTTP client with SSRF protection and robots.txt
//! - [`types`]: Request/response data structures
//! - [`whitelist`]: Known JS-heavy domain patterns
//!
//! ## Example Usage
//!
//! ```ignore
//! use tools_mcp::webfetch::{run_fetch, FetchRequest};
//!
//! let request = FetchRequest {
//!     url: "https://example.com".to_string(),
//!     max_chunk_tokens: Some(1000),
//!     no_cache: false,
//!     force_browser: false,
//! };
//!
//! let response = run_fetch(request).await?;
//! for chunk in response.chunks {
//!     println!("Heading: {:?}, Tokens: {}", chunk.heading, chunk.token_count);
//!     println!("{}", chunk.text);
//! }
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

pub mod browser;
pub mod cache;
pub mod chunker;
pub mod extract;
pub mod heuristics;
pub mod http;
pub mod types;
pub mod whitelist;

pub use types::{FetchChunk, FetchRequest, FetchResponse};

/// Global browser pool instance, lazily initialized on first browser render request.
///
/// The pool manages a single Chrome/Chromium process with automatic restart after
/// 100 requests or 1 hour of uptime to prevent memory leaks. Thread-safe via
/// `tokio::sync::OnceCell`.
static BROWSER_POOL: OnceCell<Arc<browser::BrowserPool>> = OnceCell::const_new();

/// Main entry point for the WebFetch MCP tool.
///
/// Orchestrates the complete fetch pipeline:
/// 1. Determines rendering strategy (browser vs HTTP) based on whitelist and request flags
/// 2. Checks disk cache for previously fetched content
/// 3. Fetches content via HTTP or headless browser
/// 4. Extracts and converts HTML to Markdown
/// 5. Analyzes content for JS-heavy indicators (triggers browser retry if detected)
/// 6. Chunks content with token counts for LLM consumption
///
/// # Arguments
///
/// * `req` - The fetch request containing URL and optional parameters
///
/// # Returns
///
/// A `FetchResponse` containing chunked Markdown content with metadata, or an error
/// if the URL is blocked (SSRF/robots.txt), unreachable, or processing fails.
///
/// # Rendering Strategy
///
/// The function implements a tiered rendering approach:
/// - **Tier 1**: Whitelisted domains or `force_browser=true` -> immediate browser rendering
/// - **Tier 2**: HTTP fetch with JS-heavy heuristic analysis -> browser fallback if needed
/// - **Tier 3**: Degraded HTTP mode if browser unavailable or fails
pub async fn run_fetch(req: FetchRequest) -> Result<FetchResponse> {
    // Rendering decision: browser if explicitly requested OR domain is known to require JS
    let use_browser = req.force_browser || whitelist::is_whitelisted_js_heavy(&req.url);

    if use_browser {
        debug!("Browser rendering requested for {}", req.url);
        // Try browser first, fallback to HTTP if browser fails
        match try_browser_render(&req).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                warn!("Browser rendering failed, falling back to HTTP: {}", e);
                // Continue with HTTP fallback
            }
        }
    }

    // HTTP-first path (with JS-heavy detection)
    // Check cache with method-specific key
    let cache_key = format!("{}_http", req.url);
    let cached = if req.no_cache {
        None
    } else {
        cache::read_cache(&cache_key).context("read cache")?
    };

    let (body, content_type, fetched_at, cache_hit) = match cached {
        Some(entry) => (entry.body, entry.content_type, entry.fetched_at, true),
        None => {
            let fetched = http::fetch_document(&req)
                .await
                .context("fetch remote document")?;
            // Avoid cloning potentially-large bodies: move into the cache entry, write it,
            // then destructure the entry to continue processing.
            let entry = cache::CachedFetch {
                content_type: fetched.content_type,
                body: fetched.body,
                fetched_at: fetched.fetched_at,
            };
            cache::write_cache(&cache_key, &entry).context("write cache")?;
            let cache::CachedFetch {
                body,
                content_type,
                fetched_at,
            } = entry;
            (body, content_type, fetched_at, false)
        }
    };

    // Extract content from HTTP response
    let extracted = extract::extract(&body, content_type.as_deref(), &req.url)
        .context("extract document content")?;

    // Check if content appears JS-heavy (even when HTML came from cache,
    // otherwise a JS-heavy page can get "stuck" returning cached shell HTML forever)
    let rendering_method = if !req.force_browser {
        let html_str = String::from_utf8_lossy(&body);
        let analysis = heuristics::analyze_js_heavy(
            &html_str,
            &extracted.markdown,
            content_type.as_deref(),
            Some(body.len()),
        );

        if analysis.is_js_heavy {
            info!(
                "JS-heavy site detected (confidence: {:.2}): {}",
                analysis.confidence,
                analysis.reasons.join(", ")
            );

            // Retry with browser
            match try_browser_render(&req).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    warn!("Browser rendering failed after JS detection: {}", e);
                    // Continue with HTTP result (degraded mode)
                    "http"
                }
            }
        } else {
            "http"
        }
    } else {
        "http"
    };

    // Build response from HTTP content
    build_response(
        req.url,
        fetched_at,
        extracted,
        cache_hit,
        rendering_method.to_string(),
        req.max_chunk_tokens,
    )
}

/// Renders a page using headless Chrome/Chromium browser.
///
/// This function handles the complete browser rendering flow:
/// 1. **SSRF validation first** - URL is validated BEFORE cache lookup to prevent
///    cache poisoning attacks where a malicious URL could poison the cache
/// 2. **Cache check** - Returns cached browser-rendered content if available
/// 3. **Browser availability** - Falls back with error if Chrome not installed
/// 4. **Page rendering** - Navigates to URL, waits for JS execution and network idle
/// 5. **Content extraction** - Extracts rendered HTML and converts to Markdown
///
/// # Security Note
///
/// SSRF validation happens before any cache operations. This prevents an attacker
/// from poisoning the cache with content from a private IP by using a URL that
/// passes initial validation but redirects to a private address.
///
/// # Errors
///
/// Returns an error if:
/// - URL fails SSRF validation (private IP, localhost, non-HTTP scheme)
/// - Chrome/Chromium is not installed on the system
/// - Browser navigation times out (15s limit)
/// - Page content extraction fails
async fn try_browser_render(req: &FetchRequest) -> Result<FetchResponse> {
    // SECURITY: Validate SSRF before cache to prevent cache poisoning attacks
    http::validate_url_ssrf(&req.url)
        .await
        .context("SSRF validation failed")?;

    // Check browser cache (works even if Chrome isn't installed)
    let cache_key = format!("{}_browser", req.url);
    if !req.no_cache
        && let Some(entry) = cache::read_cache(&cache_key).context("read browser cache")?
    {
        let extracted = extract::extract(&entry.body, entry.content_type.as_deref(), &req.url)
            .context("extract cached browser-rendered content")?;

        return build_response(
            req.url.clone(),
            entry.fetched_at,
            extracted,
            true,
            "browser".to_string(),
            req.max_chunk_tokens,
        );
    }

    // Check if browser is available
    if !browser::BrowserPool::is_available().await {
        return Err(anyhow::anyhow!(
            "Chrome/Chromium not installed. Browser rendering disabled."
        ));
    }

    // Get or create browser pool
    let pool = BROWSER_POOL
        .get_or_init(|| async { Arc::new(browser::BrowserPool::new()) })
        .await;

    // Render the page
    let html = pool
        .render_page(&req.url)
        .await
        .context("Browser page rendering failed")?;

    let fetched_at = Utc::now();
    // Cache browser-rendered content (cache_key already defined above) without copying
    // the full HTML buffer.
    let extracted = if req.no_cache {
        extract::extract(html.as_bytes(), Some("text/html"), &req.url)
            .context("extract browser-rendered content")?
    } else {
        let entry = cache::CachedFetch {
            content_type: Some("text/html".to_string()),
            body: html.into_bytes(),
            fetched_at,
        };
        cache::write_cache(&cache_key, &entry).context("write browser cache")?;
        extract::extract(&entry.body, entry.content_type.as_deref(), &req.url)
            .context("extract browser-rendered content")?
    };

    build_response(
        req.url.clone(),
        fetched_at,
        extracted,
        false,
        "browser".to_string(),
        req.max_chunk_tokens,
    )
}

/// Constructs the final `FetchResponse` from extracted document content.
///
/// This function performs the final processing stage:
/// 1. Chunks the Markdown content respecting token budgets and heading boundaries
/// 2. Ensures at least one chunk exists (even for minimal content)
/// 3. Assembles metadata notes (cache hit, rendering method)
///
/// # Arguments
///
/// * `url` - The original request URL (included in response for reference)
/// * `fetched_at` - Timestamp when content was fetched (from cache or fresh)
/// * `extracted` - The extracted document with title, language, and Markdown content
/// * `cache_hit` - Whether content was served from cache
/// * `rendering_method` - Either "http" or "browser"
/// * `max_chunk_tokens` - Optional token limit per chunk (defaults to 600)
fn build_response(
    url: String,
    fetched_at: DateTime<Utc>,
    extracted: extract::ExtractedDocument,
    cache_hit: bool,
    rendering_method: String,
    max_chunk_tokens: Option<usize>,
) -> Result<FetchResponse> {
    let chunks_raw =
        chunker::chunk_markdown(&extracted.markdown, max_chunk_tokens).context("chunk text")?;

    // Move strings directly to avoid clones.
    let mut chunks: Vec<FetchChunk> = chunks_raw
        .into_iter()
        .map(|(heading, text, token_count)| FetchChunk {
            heading,
            text,
            token_count,
        })
        .collect();

    // Guarantee at least one chunk so downstream prompts are never empty.
    if chunks.is_empty() && !extracted.markdown.trim().is_empty() {
        let text = extracted.markdown.trim().to_string();
        let tokens = chunker::estimate_tokens(&text)?;
        chunks.push(FetchChunk {
            heading: None,
            text,
            token_count: tokens,
        });
    }

    let fetched_at_str = format_timestamp(fetched_at);

    // Build note with cache and rendering info
    let mut notes = Vec::new();
    if cache_hit {
        notes.push("cache_hit".to_string());
    }
    if rendering_method == "browser" {
        notes.push("rendered_with_browser".to_string());
    }

    Ok(FetchResponse {
        url,
        fetched_at: fetched_at_str,
        title: extracted.title,
        language: extracted.language,
        chunks,
        rendering_method,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join(", "))
        },
    })
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
