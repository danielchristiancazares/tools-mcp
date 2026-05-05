//! MCP JSON-RPC routing and tool dispatch (inbound adapter).

use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use tools_mcp_core::{RpcError, RpcResponse, ToolDef, ToolRegistry};

#[derive(Debug)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub is_notification: bool,
    pub method: String,
    pub params: Value,
}

#[derive(Debug)]
pub enum ParseRpcRequestError {
    Parse(serde_json::Error),
    InvalidRequest { id: Option<Value>, message: String },
}

#[derive(Debug)]
pub enum RpcMessage {
    Request(RpcRequest),
    Response,
    Batch(Vec<RpcBatchItem>),
}

#[derive(Debug)]
pub enum RpcBatchItem {
    Request(RpcRequest),
    Response,
    InvalidRequest { id: Option<Value>, message: String },
}

pub fn parse_rpc_message(input: &str) -> Result<RpcMessage, ParseRpcRequestError> {
    let value: Value = serde_json::from_str(input).map_err(ParseRpcRequestError::Parse)?;
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                return Err(ParseRpcRequestError::InvalidRequest {
                    id: None,
                    message: "Invalid Request: batch must contain at least one message".into(),
                });
            }

            let mut batch = Vec::with_capacity(items.len());
            for item in items {
                if is_rpc_response_value(&item) {
                    batch.push(RpcBatchItem::Response);
                    continue;
                }

                match parse_rpc_request_value(item) {
                    Ok(req) => batch.push(RpcBatchItem::Request(req)),
                    Err(ParseRpcRequestError::InvalidRequest { id, message }) => {
                        batch.push(RpcBatchItem::InvalidRequest { id, message });
                    }
                    Err(ParseRpcRequestError::Parse(_)) => unreachable!(
                        "batch items are already parsed JSON values, so parse errors are impossible"
                    ),
                }
            }

            Ok(RpcMessage::Batch(batch))
        }
        value if is_rpc_response_value(&value) => Ok(RpcMessage::Response),
        value => parse_rpc_request_value(value).map(RpcMessage::Request),
    }
}

fn parse_rpc_request_value(value: Value) -> Result<RpcRequest, ParseRpcRequestError> {
    let Some(obj) = value.as_object() else {
        return Err(ParseRpcRequestError::InvalidRequest {
            id: None,
            message: "Invalid Request: request must be a JSON object".into(),
        });
    };

    let id = match extract_request_id(obj.get("id")) {
        Ok(id) => id,
        Err(()) => {
            return Err(ParseRpcRequestError::InvalidRequest {
                id: None,
                message: "Invalid Request: id must be a string, number, null, or omitted".into(),
            });
        }
    };

    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ParseRpcRequestError::InvalidRequest {
            id,
            message: "Invalid Request: jsonrpc must be \"2.0\"".into(),
        });
    }

    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Err(ParseRpcRequestError::InvalidRequest {
            id,
            message: "Invalid Request: method must be a string".into(),
        });
    };

    Ok(RpcRequest {
        id,
        is_notification: !obj.contains_key("id"),
        method: method.to_string(),
        params: obj
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(Map::new())),
    })
}

fn is_rpc_response_value(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && !obj.contains_key("method")
        && (obj.contains_key("result") ^ obj.contains_key("error"))
        && obj.contains_key("id")
        && extract_request_id(obj.get("id")).is_ok()
}

fn extract_request_id(id: Option<&Value>) -> Result<Option<Value>, ()> {
    match id {
        None => Ok(None),
        Some(Value::Null | Value::String(_) | Value::Number(_)) => Ok(id.cloned()),
        Some(_) => Err(()),
    }
}

#[derive(Serialize)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
    capabilities: Capabilities,
    tools: Vec<ToolDef>,
}

#[derive(Serialize, Default)]
struct Capabilities {
    tools: ServerCapabilitiesTools,
}

#[derive(Serialize, Default)]
struct ServerCapabilitiesTools {
    list: bool,
    call: bool,
}

#[derive(Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

/// Routes one JSON-RPC request. Returns `None` when no response should be sent (parse failure or notification).
pub async fn dispatch_jsonrpc_request(
    req: RpcRequest,
    registry: &ToolRegistry,
    tools: &[ToolDef],
) -> Option<(RpcResponse, bool)> {
    if req.is_notification {
        match req.method.as_str() {
            "notifications/initialized" | "initialized" => {
                tracing::info!("Received initialized notification");
                return None;
            }
            _ => {
                tracing::error!("Unknown notification: {}", req.method);
                return None;
            }
        }
    }

    let out: (RpcResponse, bool) = match req.method.as_str() {
        "ping" | "mcp/ping" => (
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "content": [{"type": "text", "text": "pong"}],
                    "isError": false
                }),
            ),
            false,
        ),

        "mcp/initialize" | "initialize" | "server/initialize" => {
            let init = InitializeResult {
                protocol_version: "2025-03-26".into(),
                server_info: ServerInfo {
                    name: "tools-mcp-server".into(),
                    version: option_env!("APP_VERSION").unwrap_or("1.0.0").into(),
                },
                capabilities: Capabilities {
                    tools: ServerCapabilitiesTools {
                        list: true,
                        call: true,
                    },
                },
                tools: tools.to_vec(),
            };
            let (result, error) = match serde_json::to_value(init) {
                Ok(v) => (Some(v), None),
                Err(e) => (
                    None,
                    Some(RpcError {
                        code: -32603,
                        message: "Internal error: failed to serialize initialize payload".into(),
                        data: Some(serde_json::json!({"details": e.to_string()})),
                    }),
                ),
            };
            (
                RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result,
                    error,
                },
                false,
            )
        }

        "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
        | "capabilities" => (
            RpcResponse::ok(req.id, serde_json::json!({"tools": tools})),
            false,
        ),

        "mcp/tools/call" | "tools/call" | "server/tools/call" => {
            let params = &req.params;
            let Some(params_obj) = params.as_object() else {
                return Some((
                    RpcResponse::protocol_error(
                        req.id,
                        -32602,
                        "Invalid params: tools/call params must be an object",
                    ),
                    false,
                ));
            };
            let name = params_obj
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| params_obj.get("toolName").and_then(|v| v.as_str()))
                .or_else(|| {
                    params_obj
                        .get("call")
                        .and_then(|c| c.get("name"))
                        .and_then(|v| v.as_str())
                });
            let Some(name) = name.filter(|name| !name.is_empty()) else {
                return Some((
                    RpcResponse::protocol_error(
                        req.id,
                        -32602,
                        "Invalid params: tools/call requires a non-empty tool name",
                    ),
                    false,
                ));
            };

            let args = params
                .get("arguments")
                .cloned()
                .or_else(|| params.get("args").cloned())
                .or_else(|| params.get("call").and_then(|c| c.get("arguments")).cloned())
                .unwrap_or(Value::Object(Map::new()));

            let resp = if let Some(result) = registry.call(name, req.id.clone(), args).await {
                result
            } else {
                RpcResponse::protocol_error(
                    req.id,
                    -32601,
                    format!(
                        "Unknown tool: {name}. Call mcp/tools/list to see available tool names."
                    ),
                )
            };
            (resp, false)
        }

        "mcp/shutdown" | "shutdown" | "server/shutdown" => (
            RpcResponse::ok(req.id, serde_json::json!({"ok": true})),
            true,
        ),

        _ => {
            tracing::error!("Unknown method: {}", req.method);
            (
                RpcResponse::protocol_error(
                    req.id,
                    -32601,
                    format!("Method not found: {}", req.method),
                ),
                false,
            )
        }
    };

    Some(out)
}

pub async fn dispatch_jsonrpc_batch(
    items: Vec<RpcBatchItem>,
    registry: &ToolRegistry,
    tools: &[ToolDef],
) -> Option<(Vec<RpcResponse>, bool)> {
    let mut responses = Vec::new();
    let mut should_exit = false;

    for item in items {
        match item {
            RpcBatchItem::Request(req) => {
                if let Some((response, exit)) = dispatch_jsonrpc_request(req, registry, tools).await
                {
                    responses.push(response);
                    should_exit |= exit;
                }
            }
            RpcBatchItem::Response => {}
            RpcBatchItem::InvalidRequest { id, message } => {
                responses.push(RpcResponse::protocol_error(id, -32600, message));
            }
        }
    }

    if responses.is_empty() {
        None
    } else {
        Some((responses, should_exit))
    }
}
