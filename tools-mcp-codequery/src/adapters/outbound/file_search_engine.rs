//! Default [`crate::ports::CodeQueryEngine`] implementation delegating to
//! `openai_file_search_core`.

use crate::ports::CodeQueryEngine;
use anyhow::Result;
use openai_file_search_core::{ApiConfig, CodeQueryOptions};

/// Delegates to [`openai_file_search_core::code_query`] unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileSearchCoreEngine;

impl CodeQueryEngine for FileSearchCoreEngine {
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
    > {
        Box::pin(openai_file_search_core::code_query(
            client,
            cfg,
            vector_store_id,
            file_paths,
            query,
            options,
        ))
    }
}
