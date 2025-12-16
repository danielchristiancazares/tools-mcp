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

/// Global browser pool instance (lazily initialized)
static BROWSER_POOL: OnceCell<Arc<browser::BrowserPool>> = OnceCell::const_new();

/// Main entry point for the `webfetch.fetch` tool.
pub async fn run_fetch(req: FetchRequest) -> Result<FetchResponse> {
    // Decision: Use browser if force_browser=true OR if URL is whitelisted
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
    let client = http::build_http_client().context("construct http client")?;

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
            let fetched = http::fetch_document(&client, &req)
                .await
                .context("fetch remote document")?;
            let entry = cache::CachedFetch {
                content_type: fetched.content_type.clone(),
                body: fetched.body.clone(),
                fetched_at: fetched.fetched_at,
            };
            cache::write_cache(&cache_key, &entry).context("write cache")?;
            (
                fetched.body,
                fetched.content_type,
                fetched.fetched_at,
                false,
            )
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

/// Attempt to render page using headless browser
async fn try_browser_render(req: &FetchRequest) -> Result<FetchResponse> {
    // Validate URL for SSRF BEFORE checking cache to prevent cache poisoning attacks
    http::validate_url_ssrf(&req.url)
        .await
        .context("SSRF validation failed")?;

    // Check browser cache (works even if Chrome isn't installed)
    let cache_key = format!("{}_browser", req.url);
    if !req.no_cache {
        if let Some(entry) = cache::read_cache(&cache_key).context("read browser cache")? {
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

    // Cache browser-rendered content (cache_key already defined above)
    let fetched_at = Utc::now();
    if !req.no_cache {
        let entry = cache::CachedFetch {
            content_type: Some("text/html".to_string()),
            body: html.as_bytes().to_vec(),
            fetched_at,
        };
        cache::write_cache(&cache_key, &entry).context("write browser cache")?;
    }

    // Extract content from rendered HTML
    let extracted = extract::extract(html.as_bytes(), Some("text/html"), &req.url)
        .context("extract browser-rendered content")?;

    build_response(
        req.url.clone(),
        fetched_at,
        extracted,
        false,
        "browser".to_string(),
        req.max_chunk_tokens,
    )
}

/// Build FetchResponse from extracted content
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

    let mut chunks: Vec<FetchChunk> = Vec::new();
    for (heading, text, tokens) in &chunks_raw {
        chunks.push(FetchChunk {
            heading: heading.clone(),
            text: text.trim().to_string(),
            token_count: *tokens,
        });
    }

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
