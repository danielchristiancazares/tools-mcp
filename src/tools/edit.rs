use crate::tool_registry::McpTool;
use crate::RpcResponse;
use crate::smart_file_edit::handle_smart_file_edit;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub struct EditTool;

impl McpTool for EditTool {
    const NAME: &'static str = "Edit";
    const ALIASES: &'static [&'static str] = &["edit", "smart_file_edit", "SmartFileEdit"];
    const DESCRIPTION: &'static str = "Apply surgical text replacements to a file, preserving line endings";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "required": ["action", "path"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get_region", "apply_snippet_edit", "apply_unified_diff"],
                    "description": "Operation to perform"
                },
                "path": {"type": "string", "description": "Filesystem path to inspect or edit"},
                "start_line": {"type": "integer", "minimum": 1, "description": "Start line for get_region"},
                "end_line": {"type": "integer", "minimum": 1, "description": "End line for get_region"},
                "old_snippet": {"type": "string", "description": "Existing canonical snippet to replace"},
                "new_snippet": {"type": "string", "description": "Replacement snippet using LF newlines"},
                "diff": {"type": "string", "description": "Unified diff to apply (for apply_unified_diff action)"},
                "file_hash": {"type": "string", "description": "sha256 hash returned by get_region to detect stale files"},
                "region_id": {"type": "string", "description": "Region identifier returned by get_region"},
                "match_hint": {
                    "type": "object",
                    "properties": {
                        "start_line": {"type": "integer", "minimum": 1},
                        "end_line": {"type": "integer", "minimum": 1}
                    },
                    "additionalProperties": false
                }
            },
            "additionalProperties": true
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_smart_file_edit(id, args).await })
    }
}
