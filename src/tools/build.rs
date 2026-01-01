use super::handlers::{ScriptConfig, run_script_tool};
use crate::RpcResponse;
use crate::define_mcp_tool;
use serde_json::Value;

async fn handle_build(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    run_script_tool(
        id,
        args,
        ScriptConfig {
            script_base: "build",
            tool_label: "Build",
        },
    )
    .await
}

define_mcp_tool! {
    BuildTool,
    name: "Build",
    aliases: ["build"],
    description: "Run build.ps1 (Windows) or build.sh (Unix) script",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {
                "type": "string",
                "description": "Directory containing the build script"
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 120000)"
            }
        },
        "required": [],
        "additionalProperties": false
    },
    handler: handle_build
}
