//! MCP JSON-RPC routing and tool dispatch (inbound adapter).

use crate::composition::{InflightRegistry, JsonRpcId};
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tools_mcp_core::{DispatchOutcome, RpcResponse, ToolDef, ToolRegistry};

pub const MAX_JSONRPC_BATCH_ITEMS: usize = 256;

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
            if items.len() > MAX_JSONRPC_BATCH_ITEMS {
                return Err(ParseRpcRequestError::InvalidRequest {
                    id: None,
                    message: format!(
                        "Invalid Request: batch must contain at most {MAX_JSONRPC_BATCH_ITEMS} messages"
                    ),
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
struct InitializeResult<'a> {
    #[serde(rename = "protocolVersion")]
    protocol_version: &'static str,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo<'a>,
    capabilities: Capabilities,
    tools: &'a [ToolDef],
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
struct ServerInfo<'a> {
    name: &'static str,
    version: &'a str,
}

#[derive(Debug)]
pub struct StaticProtocolPayloads {
    initialize_result: Value,
    tools_list_result: Value,
}

impl StaticProtocolPayloads {
    pub fn new(tools: &[ToolDef]) -> Result<Self, serde_json::Error> {
        let initialize_result = serde_json::to_value(InitializeResult {
            protocol_version: "2025-03-26",
            server_info: ServerInfo {
                name: "tools-mcp-server",
                version: option_env!("APP_VERSION").unwrap_or("1.0.0"),
            },
            capabilities: Capabilities {
                tools: ServerCapabilitiesTools {
                    list: true,
                    call: true,
                },
            },
            tools,
        })?;
        let tools_list_result = serde_json::json!({ "tools": tools });

        Ok(Self {
            initialize_result,
            tools_list_result,
        })
    }

    fn initialize_result(&self) -> Value {
        self.initialize_result.clone()
    }

    fn tools_list_result(&self) -> Value {
        self.tools_list_result.clone()
    }
}

fn cancelled_request_id(params: &Value) -> Option<JsonRpcId> {
    params
        .as_object()
        .and_then(|params| params.get("requestId"))
        .and_then(JsonRpcId::from_value)
}

/// Routes one JSON-RPC request. Returns `None` when no response should be sent (parse failure, notification, or cancelled request).
pub async fn dispatch_jsonrpc_request(
    req: RpcRequest,
    registry: &ToolRegistry,
    static_payloads: &StaticProtocolPayloads,
    inflight: &InflightRegistry,
    cancellation_token: Option<CancellationToken>,
) -> Option<(RpcResponse, bool)> {
    if req.is_notification {
        match req.method.as_str() {
            "notifications/initialized" | "initialized" => {
                tracing::info!("Received initialized notification");
                return None;
            }
            "notifications/cancelled" => {
                let Some(request_id) = cancelled_request_id(&req.params) else {
                    tracing::debug!("Ignoring cancellation notification with invalid requestId");
                    return None;
                };

                if inflight.cancel(&request_id) {
                    tracing::info!("Cancelled in-flight request: {:?}", request_id);
                } else {
                    tracing::debug!(
                        "Ignoring cancellation for unknown or completed request: {:?}",
                        request_id
                    );
                }
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

        "mcp/initialize" | "initialize" | "server/initialize" => (
            RpcResponse::ok(req.id, static_payloads.initialize_result()),
            false,
        ),

        "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
        | "capabilities" => (
            RpcResponse::ok(req.id, static_payloads.tools_list_result()),
            false,
        ),

        "mcp/tools/call" | "tools/call" | "server/tools/call" => {
            let request_id = req.id.clone();
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

            let resp = match cancellation_token {
                Some(token) => match registry
                    .call_with_cancellation(name, request_id.clone(), args, token)
                    .await
                {
                    Some(DispatchOutcome::Respond(outcome)) => {
                        outcome.into_rpc_response(request_id)
                    }
                    Some(DispatchOutcome::Cancelled) => return None,
                    None => RpcResponse::protocol_error(
                        req.id,
                        -32601,
                        format!(
                            "Unknown tool: {name}. Call mcp/tools/list to see available tool names."
                        ),
                    ),
                },
                None => {
                    if let Some(result) = registry.call(name, request_id, args).await {
                        result
                    } else {
                        RpcResponse::protocol_error(
                            req.id,
                            -32601,
                            format!(
                                "Unknown tool: {name}. Call mcp/tools/list to see available tool names."
                            ),
                        )
                    }
                }
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
    static_payloads: &StaticProtocolPayloads,
    inflight: &InflightRegistry,
) -> Option<(Vec<RpcResponse>, bool)> {
    let mut responses = Vec::new();
    let mut should_exit = false;

    // Phase 1 limitation: batch items remain sequential and do not get per-item
    // cancellation tokens. `notifications/cancelled` can only target independently
    // tracked non-batch requests, which is acceptable for spec-compliant race handling.
    for item in items {
        match item {
            RpcBatchItem::Request(req) => {
                if let Some((response, exit)) =
                    dispatch_jsonrpc_request(req, registry, static_payloads, inflight, None).await
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

#[cfg(test)]
mod tests {
    use super::{
        Capabilities, InitializeResult, MAX_JSONRPC_BATCH_ITEMS, ParseRpcRequestError, RpcMessage,
        RpcRequest, ServerCapabilitiesTools, ServerInfo, StaticProtocolPayloads,
        dispatch_jsonrpc_request, parse_rpc_message,
    };
    use crate::composition::InflightRegistry;
    use serde_json::json;
    use tools_mcp_core::{ToolDef, ToolRegistry};

    fn request_with_id(id: usize) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "ping",
            "params": {}
        })
    }

    fn cached_payload_test_tools() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "CachedTool".to_string(),
            description: "tool used by cached protocol payload tests".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }]
    }

    #[test]
    fn parse_rpc_message_accepts_max_sized_batch() {
        let batch = (0..MAX_JSONRPC_BATCH_ITEMS)
            .map(request_with_id)
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&batch).expect("serialize batch");

        let parsed = parse_rpc_message(&input).expect("max batch should parse");

        let RpcMessage::Batch(items) = parsed else {
            panic!("expected batch message");
        };
        assert_eq!(items.len(), MAX_JSONRPC_BATCH_ITEMS);
    }

    #[test]
    fn parse_rpc_message_rejects_oversized_batch() {
        let batch = (0..=MAX_JSONRPC_BATCH_ITEMS)
            .map(request_with_id)
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&batch).expect("serialize batch");

        let err = parse_rpc_message(&input).expect_err("oversized batch should be rejected");

        match err {
            ParseRpcRequestError::InvalidRequest { id, message } => {
                assert!(id.is_none());
                assert!(message.contains("at most 256 messages"));
            }
            ParseRpcRequestError::Parse(err) => panic!("expected invalid request, got {err}"),
        }
    }

    #[tokio::test]
    async fn initialize_dispatch_returns_cached_payload_with_legacy_shape() {
        let registry = ToolRegistry::new();
        let tools = cached_payload_test_tools();
        let static_payloads =
            StaticProtocolPayloads::new(&tools).expect("static payloads should serialize");
        let expected = serde_json::to_value(InitializeResult {
            protocol_version: "2025-03-26",
            server_info: ServerInfo {
                name: "tools-mcp-server",
                version: option_env!("APP_VERSION").unwrap_or("1.0.0"),
            },
            capabilities: Capabilities {
                tools: ServerCapabilitiesTools {
                    list: true,
                    call: true,
                },
            },
            tools: &tools,
        })
        .expect("legacy initialize payload should serialize");

        let (response, should_exit) = dispatch_jsonrpc_request(
            RpcRequest {
                id: Some(json!(42)),
                is_notification: false,
                method: "mcp/initialize".to_string(),
                params: json!({"capabilities": {}}),
            },
            &registry,
            &static_payloads,
            &InflightRegistry::default(),
            None,
        )
        .await
        .expect("initialize should respond");

        assert!(!should_exit);
        assert_eq!(response.id, Some(json!(42)));
        assert_eq!(response.result, Some(expected));
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn tools_list_dispatch_returns_cached_payload_with_legacy_shape_for_aliases() {
        let registry = ToolRegistry::new();
        let tools = cached_payload_test_tools();
        let static_payloads =
            StaticProtocolPayloads::new(&tools).expect("static payloads should serialize");
        let expected = json!({ "tools": tools });

        for method in [
            "mcp/tools/list",
            "tools/list",
            "server/tools/list",
            "mcp/capabilities",
            "capabilities",
        ] {
            let (response, should_exit) = dispatch_jsonrpc_request(
                RpcRequest {
                    id: Some(json!(method)),
                    is_notification: false,
                    method: method.to_string(),
                    params: json!({}),
                },
                &registry,
                &static_payloads,
                &InflightRegistry::default(),
                None,
            )
            .await
            .expect("tools/list alias should respond");

            assert!(!should_exit);
            assert_eq!(response.id, Some(json!(method)));
            assert_eq!(response.result, Some(expected.clone()));
            assert!(response.error.is_none());
        }
    }
}
