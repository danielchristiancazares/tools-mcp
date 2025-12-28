use crate::tool_registry::McpTool;
use crate::webfetch::handle_webfetch;
use crate::RpcResponse;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub struct WebFetchTool;

impl McpTool for WebFetchTool {
    const NAME: &'static str = "WebFetch";
    const ALIASES: &'static [&'static str] = &["webfetch", "web_fetch"];
    const DESCRIPTION: &'static str = "Fetch a URL via HTTP (with optional headless-browser fallback), convert to Markdown, and return token-aware chunks for LLM consumption. Respects robots.txt.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "max_chunk_tokens": {
                    "type": "integer",
                    "description": "Max tokens per chunk (default 2000)"
                },
                "no_cache": {
                    "type": "boolean",
                    "description": "Bypass cache and fetch fresh content"
                },
                "force_browser": {
                    "type": "boolean",
                    "description": "Force headless browser rendering"
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_webfetch(id, args).await })
    }
}
