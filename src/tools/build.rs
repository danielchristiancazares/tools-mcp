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
    description: "Build the project. Auto-detects build system (Cargo, npm, pnpm, yarn, Make, Just, Go, CMake) or runs build.ps1/build.sh if present.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {
                "type": "string",
                "description": "Directory to build in (default: current directory)"
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 120000)"
            },
            "build_system": {
                "type": "string",
                "enum": ["cargo", "npm", "pnpm", "yarn", "make", "just", "go", "cmake", "script"],
                "description": "Force a specific build system instead of auto-detecting"
            }
        },
        "required": []
    },
    handler: handle_build
}
