use anyhow::Result;
use openai_file_search_core::{ApiConfig, CodeQueryOptions};

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
