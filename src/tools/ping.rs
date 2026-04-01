use crate::define_mcp_tool;
use crate::tool_outcome::ToolCallOutcome;
use serde::Deserialize;
use serde_json::{Value, json};

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

define_mcp_tool! {
    /// Simple ping tool for health checks.
    PingTool,
    name: "Ping",
    description: "Returns 'pong' to verify the server is running",
    schema: {
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false
    },
    handler: handle_ping
}
