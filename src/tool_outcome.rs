//! MCP tool execution result without JSON-RPC envelope (`id` is applied by the registry).
//!
//! Inbound routing maps [`ToolCallOutcome`] to [`crate::response::RpcResponse`] so tool code
//! stays independent of JSON-RPC framing.

use serde_json::Value;
use std::sync::OnceLock;

/// Inner MCP `result` object for a tool call (success or tool-level error).
#[derive(Debug, Clone)]
pub struct ToolCallOutcome(pub Value);

impl ToolCallOutcome {
    /// Wraps a successful MCP tool `result` payload (`content`, `isError: false`, etc.).
    pub fn ok(result: Value) -> Self {
        ToolCallOutcome(result)
    }

    /// Tool-level error using MCP content format (`isError: true`).
    pub fn err(msg: impl std::fmt::Display) -> Self {
        ToolCallOutcome(serde_json::json!({
            "content": [{"type": "text", "text": msg.to_string()}],
            "isError": true
        }))
    }

    /// Tool-level error with structured fields merged into the result object.
    pub fn err_with(
        msg: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> Self {
        let mut payload = serde_json::json!({
            "content": [{"type": "text", "text": msg.into()}],
            "isError": true
        });
        if let Some(obj) = payload.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k.to_string(), v);
            }
        }
        ToolCallOutcome(payload)
    }

    /// Deserialize tool arguments; on failure returns a tool-level error (same strings as
    /// [`crate::response::RpcResponse::parse`]).
    pub fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, Self> {
        serde_json::from_value::<T>(args).map_err(|e| {
            let msg = e.to_string();
            let hint = if msg.contains("unknown field") {
                " Unknown fields are not allowed; check argument names against the tool schema."
            } else if msg.contains("missing field") {
                " Required fields are missing; provide all required arguments per the tool schema."
            } else if msg.contains("invalid type") {
                " One or more arguments has the wrong type; check the tool schema for expected types."
            } else {
                ""
            };
            Self::err(format!("invalid arguments: {msg}.{hint}"))
        })
    }

    pub fn into_rpc_response(self, id: Option<Value>) -> crate::RpcResponse<'static> {
        crate::RpcResponse::ok(id, self.0)
    }

    /// Success with text content and optional extra fields (same behavior as [`crate::response::RpcResponse::ok_text_with`]).
    pub fn ok_text_with(
        text: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> Self {
        let mut payload = serde_json::json!({
            "content": [{"type": "text", "text": text.into()}],
            "isError": false
        });
        if let Some(obj) = payload.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k.to_string(), v);
            }
        }
        ToolCallOutcome(payload)
    }

    /// JSON serialized as text content (same behavior as [`crate::response::RpcResponse::ok_json_content`]).
    pub fn ok_json_content(json_value: Value, is_error: bool) -> Self {
        static PRETTY_JSON: OnceLock<bool> = OnceLock::new();
        let pretty = *PRETTY_JSON.get_or_init(|| {
            std::env::var("TOOLS_PRETTY_JSON")
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false)
        });

        let json_text = if pretty {
            serde_json::to_string_pretty(&json_value)
        } else {
            serde_json::to_string(&json_value)
        }
        .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
        ToolCallOutcome(serde_json::json!({
            "content": [{"type": "text", "text": json_text}],
            "isError": is_error
        }))
    }
}
