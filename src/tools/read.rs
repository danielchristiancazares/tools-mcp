use crate::tool_registry::McpTool;
use crate::RpcResponse;
use crate::read_file::handle_read_file;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub struct ReadTool;

impl McpTool for ReadTool {
    const NAME: &'static str = "Read";
    const ALIASES: &'static [&'static str] = &["read", "ReadFile", "read_file", "read-file"];
    const DESCRIPTION: &'static str = "Read file contents with optional line range";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to read (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to read (inclusive)"
                },
                "show_line_numbers": {
                    "type": "boolean",
                    "description": "Include line numbers in output"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_read_file(id, args).await })
    }
}
