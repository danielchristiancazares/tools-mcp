use crate::tool_handler::handle_code_query;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    CodeQueryTool,
    name: "CodeQuery",
    description: "Semantic code search. Ask natural language questions about code behavior, architecture, or patterns.",
    schema: {
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
        "required": ["query"],
        "additionalProperties": false
    },
    handler: handle_code_query
}
