use crate::script_runner::{run_script_tool, ScriptConfig};
use crate::tool_registry::McpTool;
use crate::RpcResponse;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub struct TestTool;

impl McpTool for TestTool {
    const NAME: &'static str = "Test";
    const ALIASES: &'static [&'static str] = &["test"];
    const DESCRIPTION: &'static str = "Run test.ps1 (Windows) or test.sh (Unix) script";

    fn input_schema() -> Value {
        json!({
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
                    script_base: "test",
                    tool_label: "Test",
                },
            )
            .await
        })
    }
}
