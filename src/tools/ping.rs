use crate::RpcResponse;
use crate::define_mcp_tool;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PingRequest {}

async fn handle_ping(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let _req = match RpcResponse::parse::<PingRequest>(id.clone(), args) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
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
