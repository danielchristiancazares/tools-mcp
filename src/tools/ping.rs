use crate::tool_registry::McpTool;
use crate::RpcResponse;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

/// Simple ping tool for health checks.
pub struct PingTool;

impl McpTool for PingTool {
    const NAME: &'static str = "ping";
    const ALIASES: &'static [&'static str] = &["Ping"];
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
        _args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move {
            RpcResponse::ok(id, json!({
                "content": [{"type": "text", "text": "pong"}],
                "isError": false
            }))
        })
    }
}
