//! MCP tool execution result without JSON-RPC envelope (`id` is applied by the registry).
//!
//! Inbound routing maps [`ToolCallOutcome`] to [`crate::response::RpcResponse`] so tool code
//! stays independent of JSON-RPC framing.

use crate::response::{text_content_result, text_content_result_with_extra};
use serde_json::Value;

/// Inner MCP `result` object for a tool call (success or tool-level error).
#[derive(Debug, Clone)]
pub struct ToolCallOutcome(pub Value);

#[derive(Debug, Clone)]
pub enum DispatchOutcome {
    Respond(ToolCallOutcome),
    Cancelled,
}

impl DispatchOutcome {
    pub fn into_rpc_response(self, id: Option<Value>) -> Option<crate::RpcResponse> {
        match self {
            Self::Respond(outcome) => Some(outcome.into_rpc_response(id)),
            Self::Cancelled => None,
        }
    }
}

impl ToolCallOutcome {
    /// Wraps a successful MCP tool `result` payload (`content`, `isError: false`, etc.).
    pub fn ok(result: Value) -> Self {
        ToolCallOutcome(result)
    }

    /// Tool-level error using MCP content format (`isError: true`).
    pub fn err(msg: impl std::fmt::Display) -> Self {
        ToolCallOutcome(text_content_result(msg.to_string(), true))
    }

    /// Tool-level error with structured fields merged into the result object.
    pub fn err_with(
        msg: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> Self {
        ToolCallOutcome(text_content_result_with_extra(msg.into(), true, extra))
    }

    /// Deserialize tool arguments; on failure returns a tool-level error (same strings as
    /// [`crate::response::RpcResponse::parse`]).
    pub fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, Self> {
        T::deserialize(args).map_err(|e| {
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

    pub fn into_rpc_response(self, id: Option<Value>) -> crate::RpcResponse {
        crate::RpcResponse::ok(id, self.0)
    }

    /// Success with text content and optional extra fields (same behavior as [`crate::response::RpcResponse::ok_text_with`]).
    pub fn ok_text_with(
        text: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> Self {
        ToolCallOutcome(text_content_result_with_extra(text.into(), false, extra))
    }

    /// JSON serialized as compact text content to minimize token usage.
    pub fn ok_json_content(json_value: &Value, is_error: bool) -> Self {
        let json_text = serde_json::to_string(json_value)
            .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
        ToolCallOutcome(serde_json::json!({
            "content": [{"type": "text", "text": json_text}],
            "isError": is_error
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{DispatchOutcome, ToolCallOutcome};
    use serde_json::json;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct ParseArgs {
        count: usize,
    }

    #[test]
    fn parse_args_deserializes_arguments_without_consuming_source_value() {
        let args = json!({"count": 3});
        let parsed: ParseArgs = ToolCallOutcome::parse_args(&args).expect("args parse");

        assert_eq!(parsed, ParseArgs { count: 3 });
        assert_eq!(args, json!({"count": 3}));
    }

    #[test]
    fn cancelled_dispatch_outcome_suppresses_rpc_response() {
        assert!(
            DispatchOutcome::Cancelled
                .into_rpc_response(Some(json!(1)))
                .is_none()
        );
    }

    #[test]
    fn ok_json_content_serializes_compact_json_text() {
        let outcome = ToolCallOutcome::ok_json_content(&json!(["alpha", "beta"]), false);

        assert_eq!(
            outcome.0,
            json!({
                "content": [{"type": "text", "text": "[\"alpha\",\"beta\"]"}],
                "isError": false
            })
        );
    }
}
