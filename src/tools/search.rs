use crate::tool_registry::McpTool;
use crate::RpcResponse;
use crate::ripgrep::handle_ripgrep;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub struct SearchTool;

impl McpTool for SearchTool {
    const NAME: &'static str = "Search";
    const ALIASES: &'static [&'static str] = &["search", "RipGrep", "ripgrep", "rg"];
    const DESCRIPTION: &'static str = "Search file contents using ripgrep/ugrep with regex support";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Search pattern (regex by default)"},
                "path": {"type": "string", "description": "File or directory to search (default: current working directory)"},
                "case": {"type": "string", "enum": ["smart", "sensitive", "insensitive"], "default": "smart", "description": "Case handling mode"},
                "fixed_strings": {"type": "boolean", "default": false, "description": "Treat pattern as a literal string (-F)"},
                "word_regexp": {"type": "boolean", "default": false, "description": "Match on word boundaries only (-w)"},
                "glob": {"type": "array", "items": {"type": "string"}, "description": "Optional glob filters"},
                "hidden": {"type": "boolean", "default": false, "description": "Search hidden files/directories"},
                "follow": {"type": "boolean", "default": false, "description": "Follow symlinks"},
                "no_ignore": {"type": "boolean", "default": false, "description": "Do not respect ignore files like .gitignore"},
                "context": {"type": "integer", "minimum": 0, "default": 0, "description": "Lines of context on both sides"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 200, "description": "Maximum match/context events to return"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 20000, "description": "Overall timeout in milliseconds"},
                "fuzzy": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Fuzzy match tolerance (1-4 edits)"}
            },
            "required": ["pattern"]
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_ripgrep(id, args).await })
    }
}
