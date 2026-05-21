use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use tools_mcp_core::{McpTool, ToolCallOutcome};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PingRequest {}

#[allow(clippy::unused_async)]
async fn handle_ping(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let _req = match ToolCallOutcome::parse_args::<PingRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };
    ToolCallOutcome::ok(json!({
        "content": [{"type": "text", "text": "pong"}],
        "isError": false
    }))
}

/// Simple ping tool for health checks.
pub struct PingTool;

impl McpTool for PingTool {
    const NAME: &'static str = "Ping";
    const ALIASES: &'static [&'static str] = &["ping"];
    const DESCRIPTION: &'static str = "Returns 'pong' to verify the server is running";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>> {
        Box::pin(handle_ping(id, args))
    }
}
