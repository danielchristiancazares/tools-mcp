//! Outbound port traits (Clean Architecture). Implementations live in [`crate::adapters::outbound`].
//!
//! These traits isolate application logic from concrete infrastructure (`reqwest`, `OpenAI` HTTP, etc.)
//! while preserving behavior: default adapters delegate to `file_search_core` unchanged.

pub mod web_fetcher;

pub use web_fetcher::WebFetcher;

use anyhow::Result;
use file_search_core::{ApiConfig, CodeQueryOptions};

/// Semantic code search + reindex orchestration against a vector store (OpenAI-backed).
#[allow(clippy::type_complexity)]
pub trait CodeQueryEngine: Send + Sync {
    fn execute<'a>(
        &'a self,
        client: &'a reqwest::Client,
        cfg: &'a ApiConfig,
        vector_store_id: &'a str,
        file_paths: &'a [String],
        query: &'a str,
        options: CodeQueryOptions<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(String, Option<serde_json::Value>)>>
                + Send
                + 'a,
        >,
    >;
}
