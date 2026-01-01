use crate::RpcResponse;
use crate::define_mcp_tool;
use serde_json::{Value, json};

async fn handle_ping(id: Option<Value>, _args: Value) -> RpcResponse<'static> {
    RpcResponse::ok(
        id,
        json!({
            "content": [{"type": "text", "text": "pong"}],
            "isError": false
        }),
    )
}

define_mcp_tool! {
    /// Simple ping tool for health checks.
    PingTool,
    name: "ping",
    aliases: ["Ping"],
    description: "Returns 'pong' to verify the server is running",
    schema: {
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false
    },
    handler: handle_ping
}
