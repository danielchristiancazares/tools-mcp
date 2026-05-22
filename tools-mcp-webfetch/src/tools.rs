use crate::webfetch_tool::handle_webfetch;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    WebFetchTool,
    name: "WebFetch",
    description: "Fetch a URL via HTTP (with optional headless-browser fallback), convert to Markdown, and return token-aware chunks for LLM consumption. Respects robots.txt.",
    schema: {
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "The URL to fetch"
            },
            "max_chunk_tokens": {
                "type": "integer",
                "minimum": 1,
                "description": "Max tokens per chunk (default 600)"
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
    },
    handler: handle_webfetch
}
