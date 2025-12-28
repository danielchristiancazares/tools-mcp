use crate::script_runner::{run_script_tool, ScriptConfig};
use crate::tool_registry::McpTool;
use crate::RpcResponse;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub struct BuildTool;

impl McpTool for BuildTool {
    const NAME: &'static str = "Build";
    const ALIASES: &'static [&'static str] = &["build"];
    const DESCRIPTION: &'static str = "Run build.ps1 (Windows) or build.sh (Unix) script";

    fn input_schema() -> Value {
        json!({
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
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move {
            run_script_tool(
                id,
                args,
                ScriptConfig {
                    script_base: "build",
                    tool_label: "Build",
                },
            )
            .await
        })
    }
}
