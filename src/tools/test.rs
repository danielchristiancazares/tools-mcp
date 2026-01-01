use super::handlers::{ScriptConfig, run_script_tool};
use crate::RpcResponse;
use crate::define_mcp_tool;
use serde_json::Value;

async fn handle_test(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    run_script_tool(
        id,
        args,
        ScriptConfig {
            script_base: "test",
            tool_label: "Test",
        },
    )
    .await
}

define_mcp_tool! {
    TestTool,
    name: "Test",
    aliases: ["test"],
    description: "Run test.ps1 (Windows) or test.sh (Unix) script",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {
                "type": "string",
                "description": "Directory containing the test script"
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 120000)"
            }
        },
        "required": [],
        "additionalProperties": false
    },
    handler: handle_test
}
