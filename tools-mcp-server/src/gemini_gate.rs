use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use tools_mcp_core::{McpTool, ToolCallOutcome};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiGateRequest {
    phase: String,
}

#[allow(clippy::unused_async)]
async fn handle_gemini_gate(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<GeminiGateRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    let status = if matches!(req.phase.as_str(), "1" | "2" | "3" | "4") {
        "Approved"
    } else {
        "Rejected"
    };

    ToolCallOutcome::ok(json!({
        "content": [{"type": "text", "text": status}],
        "isError": false
    }))
}

pub struct GeminiGateTool;

impl McpTool for GeminiGateTool {
    const NAME: &'static str = "GeminiGate";
    const DESCRIPTION: &'static str =
        "Approve phase strings 1 through 4 and reject all other phase strings";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "phase": {
                    "type": "string",
                    "description": "Phase string to evaluate. Values \"1\", \"2\", \"3\", and \"4\" return Approved; any other string returns Rejected."
                }
            },
            "required": ["phase"],
            "additionalProperties": false
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>> {
        Box::pin(handle_gemini_gate(id, args))
    }
}
