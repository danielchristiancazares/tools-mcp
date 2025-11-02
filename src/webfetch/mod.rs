use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

pub mod cache;
pub mod chunker;
pub mod extract;
pub mod http;
pub mod types;

pub use types::{FetchChunk, FetchRequest, FetchResponse};

/// Main entry point for the `webfetch.fetch` tool.
pub async fn run_fetch(req: FetchRequest) -> Result<FetchResponse> {
    let client = http::build_http_client().context("construct http client")?;

    // Web retrieval is comparatively slow, so re-use the sanitized payload when the caller allows it.
    let cached = if req.no_cache {
        None
    } else {
        cache::read_cache(&req.url).context("read cache")?
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
            cache::write_cache(&req.url, &entry).context("write cache")?;
            (
                fetched.body,
                fetched.content_type,
                fetched.fetched_at,
                false,
            )
        }
    };

    let extracted = extract::extract(&body, content_type.as_deref(), &req.url)
        .context("extract document content")?;

    let chunks_raw =
        chunker::chunk_markdown(&extracted.markdown, req.max_chunk_tokens).context("chunk text")?;

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

    Ok(FetchResponse {
        url: req.url,
        fetched_at: fetched_at_str,
        title: extracted.title,
        language: extracted.language,
        chunks,
        note: cache_note(cache_hit),
    })
}

fn cache_note(hit: bool) -> Option<String> {
    if hit {
        Some("cache_hit".to_string())
    } else {
        None
    }
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
