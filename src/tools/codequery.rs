use crate::codequery::handle_code_query;
use crate::tool_registry::McpTool;
use crate::RpcResponse;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub struct CodeQueryTool;

impl McpTool for CodeQueryTool {
    const NAME: &'static str = "CodeQuery";
    const ALIASES: &'static [&'static str] = &["code_query", "code-query"];
    const DESCRIPTION: &'static str =
        "Semantic code search. Ask natural language questions about code behavior, architecture, or patterns.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "vector_store_id": {"type": "string", "description": "Target vector store ID"},
                "vector_store_name": {"type": "string", "description": "Target vector store name"},
                "query": {"type": "string", "description": "Natural language code question"},
                "file_paths": {"type": "array", "items": {"type": "string"}, "description": "Optional local file paths to sync before querying"},
                "concurrent_limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 5, "description": "Max concurrent operations (default: 5)"},
                "timeout_ms": {"type": "integer", "minimum": 1000, "default": 60000, "description": "Overall indexing wait timeout in milliseconds"},
                "model": {"type": "string", "description": "Override model (defaults to ApiConfig default)"},
                "max_num_results": {"type": "integer", "minimum": 1, "description": "Limit vector search matches"},
                "include_results": {"type": "boolean", "default": false, "description": "Include retrieved snippets in the response payload"}
            },
            "required": ["query"]
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_code_query(id, args).await })
    }
}
