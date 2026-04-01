use super::handlers::{ScriptConfig, run_script_tool};
use crate::define_mcp_tool;
use crate::tool_outcome::ToolCallOutcome;
use serde_json::Value;

async fn handle_test(id: Option<Value>, args: Value) -> ToolCallOutcome {
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
    description: "Run tests. Auto-detects build system (Cargo, npm, pnpm, yarn, Make, Just, Go) or runs test.ps1/test.sh if present.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {
                "type": "string",
                "description": "Directory to run tests in (default: current directory)"
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 120000)"
            },
            "build_system": {
                "type": "string",
                "enum": ["cargo", "npm", "pnpm", "yarn", "make", "just", "go", "script"],
                "description": "Force a specific build system instead of auto-detecting"
            }
        },
        "required": []
    },
    handler: handle_test
}
