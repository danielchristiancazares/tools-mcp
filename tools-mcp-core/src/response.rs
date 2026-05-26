//! JSON-RPC 2.0 response types for MCP protocol.
//!
//! This module provides the core response types used throughout the MCP server
//! for constructing JSON-RPC responses to client requests.
//!
//! # Response Types
//!
//! - [`RpcResponse`]: The main response envelope containing either a result or error
//! - [`RpcError`]: Protocol-level error object for JSON-RPC failures
//!
//! # Tool vs Protocol Errors
//!
//! The MCP protocol distinguishes between two error categories:
//!
//! - **Tool errors**: Returned via `result` with `isError: true`. Use [`RpcResponse::err`].
//! - **Protocol errors**: Returned via `error` field. Use [`RpcResponse::protocol_error`].
//!
//! This distinction allows clients to differentiate between "the tool ran but failed"
//! and "the request was malformed or the method doesn't exist".

use serde::Serialize;
use serde_json::{Map, Value};

/// Outgoing JSON-RPC 2.0 response to an MCP client.
///
/// A response contains either a `result` (success) or an `error` (failure),
/// but never both. The `id` must match the corresponding request.
///
/// # MCP Content Format
///
/// For tool responses, the `result` field uses the MCP content format:
///
/// ```json
/// {
///   "content": [{"type": "text", "text": "..."}],
///   "isError": false
/// }
/// ```
///
/// This allows tools to return structured content (text, JSON, images) while
/// signaling success/failure via the `isError` flag.
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    /// JSON-RPC version (always "2.0").
    pub jsonrpc: &'static str,

    /// Request identifier from the corresponding request.
    pub id: Option<Value>,

    /// Success payload. Mutually exclusive with `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error payload for protocol-level failures. Mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// JSON-RPC 2.0 error object for protocol-level failures.
///
/// Used for transport/protocol errors (invalid JSON, unknown method, etc.).
/// Tool-level errors use the MCP content format with `isError: true` instead.
///
/// # Standard Error Codes
///
/// | Code   | Meaning              |
/// |--------|----------------------|
/// | -32700 | Parse error          |
/// | -32600 | Invalid request      |
/// | -32601 | Method not found     |
/// | -32602 | Invalid params       |
/// | -32603 | Internal error       |
#[derive(Debug, Serialize)]
pub struct RpcError {
    /// Numeric error code (see table above).
    pub code: i64,

    /// Human-readable error description.
    pub message: String,

    /// Optional additional error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcResponse {
    /// Creates a success response with the given result payload.
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier to echo back (required for non-notification responses)
    /// * `result` - The result payload (any JSON value)
    ///
    /// # Example
    ///
    /// ```ignore
    /// RpcResponse::ok(Some(json!(1)), json!({"status": "ready"}))
    /// ```
    #[must_use]
    pub fn ok(id: Option<Value>, result: Value) -> RpcResponse {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Creates an error response using MCP content format.
    ///
    /// This is used for tool-level errors (as opposed to protocol errors).
    /// The error is conveyed via `result` with `isError: true`, allowing
    /// clients to distinguish between "the tool ran but failed" vs
    /// "the protocol/transport failed".
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier to echo back
    /// * `msg` - Human-readable error message
    ///
    /// # Output Format
    ///
    /// ```json
    /// {
    ///   "jsonrpc": "2.0",
    ///   "id": 1,
    ///   "result": {
    ///     "content": [{"type": "text", "text": "error message"}],
    ///     "isError": true
    ///   }
    /// }
    /// ```
    pub fn err(id: Option<Value>, msg: impl std::fmt::Display) -> RpcResponse {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(text_content_result(msg.to_string(), true)),
            error: None,
        }
    }

    /// Creates a tool-level error response with extra structured fields.
    ///
    /// This is useful when callers want to attach machine-readable remediation hints
    /// (e.g., `remediation`, `error_type`, `path`, `command`) while still providing
    /// a primary human-readable `content[0].text` message.
    pub fn err_with(
        id: Option<Value>,
        msg: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> RpcResponse {
        let payload = text_content_result_with_extra(msg.into(), true, extra);
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(payload),
            error: None,
        }
    }

    /// Creates a success response with text content and additional metadata.
    ///
    /// This is a convenience method for tools that return text output with
    /// extra metadata fields (e.g., `cached`, `rendering_method`).
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier to echo back
    /// * `text` - Primary text content
    /// * `extra` - Additional key-value pairs to merge into the response
    ///
    /// # Example
    ///
    /// ```ignore
    /// RpcResponse::ok_text_with(
    ///     req.id,
    ///     "File contents here",
    ///     [("cached", json!(true)), ("bytes", json!(1024))]
    /// )
    /// ```
    pub fn ok_text_with(
        id: Option<Value>,
        text: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> RpcResponse {
        let payload = text_content_result_with_extra(text.into(), false, extra);
        RpcResponse::ok(id, payload)
    }

    /// Parses request arguments into a typed struct.
    ///
    /// This is a convenience method for tool implementations that need to
    /// deserialize their arguments from the generic `Value` passed by the
    /// dispatcher.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The target type (must implement `DeserializeOwned`)
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier (used in error response if parsing fails)
    /// * `args` - The raw JSON arguments to parse
    ///
    /// # Returns
    ///
    /// * `Ok(T)` - Successfully parsed arguments
    /// * `Err(Box<RpcResponse>)` - Pre-built error response ready to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// let args: MyToolArgs = RpcResponse::parse(id.clone(), args)?;
    /// ```
    /// # Errors
    ///
    /// Returns an error response when argument parsing fails due to unknown fields,
    /// missing fields, or incompatible types.
    pub fn parse<T: serde::de::DeserializeOwned>(
        id: Option<Value>,
        args: &Value,
    ) -> Result<T, Box<RpcResponse>> {
        T::deserialize(args).map_err(|e| {
            let msg = e.to_string();
            // Serde's error strings are informative but not always prescriptive.
            // Add short remediation hints for the most common failure modes.
            let hint = if msg.contains("unknown field") {
                " Unknown fields are not allowed; check argument names against the tool schema."
            } else if msg.contains("missing field") {
                " Required fields are missing; provide all required arguments per the tool schema."
            } else if msg.contains("invalid type") {
                " One or more arguments has the wrong type; check the tool schema for expected types."
            } else {
                ""
            };
            Box::new(RpcResponse::err(id, format!("invalid arguments: {msg}.{hint}")))
        })
    }

    /// Creates a protocol-level error response.
    ///
    /// Unlike [`Self::err`], this uses the JSON-RPC `error` field instead of
    /// the `result` field. This is appropriate for protocol/transport errors
    /// (invalid JSON, unknown method, etc.) rather than tool execution failures.
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier to echo back
    /// * `code` - Numeric error code (use standard JSON-RPC codes, see [`RpcError`])
    /// * `msg` - Human-readable error message
    ///
    /// # Standard Error Codes
    ///
    /// * `-32601` - Method not found
    /// * `-32602` - Invalid params
    /// * `-32603` - Internal error
    pub fn protocol_error(id: Option<Value>, code: i64, msg: impl Into<String>) -> RpcResponse {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: msg.into(),
                data: None,
            }),
        }
    }
}

#[inline]
pub(crate) fn text_content_result(text: String, is_error: bool) -> Value {
    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        Value::Array(vec![text_content_item(text)]),
    );
    result.insert("isError".to_owned(), Value::Bool(is_error));
    Value::Object(result)
}

#[inline]
pub(crate) fn text_content_result_with_extra(
    text: String,
    is_error: bool,
    extra: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        Value::Array(vec![text_content_item(text)]),
    );
    result.insert("isError".to_owned(), Value::Bool(is_error));
    for (key, value) in extra {
        result.insert(key.to_owned(), value);
    }
    Value::Object(result)
}

#[inline]
fn text_content_item(text: String) -> Value {
    let mut content = Map::new();
    content.insert("type".to_owned(), Value::String("text".to_owned()));
    content.insert("text".to_owned(), Value::String(text));
    Value::Object(content)
}

#[cfg(test)]
mod tests {
    use super::RpcResponse;
    use serde_json::json;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ParseArgs {
        value: String,
    }

    #[test]
    fn protocol_error_serializes_unknown_id_as_null() {
        let response = RpcResponse::protocol_error(None, -32700, "Parse error");
        let json = serde_json::to_value(response).expect("response serializes");

        assert!(json.as_object().expect("object").contains_key("id"));
        assert!(json["id"].is_null());
    }

    #[test]
    fn parse_deserializes_arguments_without_consuming_source_value() {
        let args = json!({"value": "ok"});
        let parsed: ParseArgs = RpcResponse::parse(Some(json!(1)), &args).expect("args parse");

        assert_eq!(
            parsed,
            ParseArgs {
                value: "ok".to_string()
            }
        );
        assert_eq!(args, json!({"value": "ok"}));
    }

    #[test]
    fn parse_preserves_unknown_field_hint() {
        let args = json!({"value": "ok", "extra": true});
        let err = RpcResponse::parse::<ParseArgs>(Some(json!(1)), &args).unwrap_err();
        let result = err.result.expect("tool error result");
        let message = result["content"][0]["text"].as_str().expect("message text");

        assert!(message.contains("invalid arguments: unknown field"));
        assert!(message.contains("Unknown fields are not allowed"));
    }

    #[test]
    fn ok_text_with_preserves_extra_field_overwrites() {
        let response = RpcResponse::ok_text_with(
            Some(json!(1)),
            "hello",
            [("content", json!([])), ("isError", json!(true))],
        );
        let result = response.result.expect("tool result");

        assert_eq!(result["content"], json!([]));
        assert_eq!(result["isError"], json!(true));
    }
}
