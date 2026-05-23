//! MCP JSON-RPC routing and tool dispatch (inbound adapter).

use crate::composition::{InflightRegistry, JsonRpcId};
use serde::{Serialize, Serializer};
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
    pub method_kind: RpcMethodKind,
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcMethodKind {
    Ping,
    Initialize,
    ToolsList,
    ToolsCall,
    Shutdown,
    InitializedNotification,
    CancelledNotification,
    Unknown,
}

impl RpcMethodKind {
    pub fn from_name(method: &str) -> Self {
        match method {
            "ping" | "mcp/ping" => Self::Ping,
            "mcp/initialize" | "initialize" | "server/initialize" => Self::Initialize,
            "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
            | "capabilities" => Self::ToolsList,
            "mcp/tools/call" | "tools/call" | "server/tools/call" => Self::ToolsCall,
            "mcp/shutdown" | "shutdown" | "server/shutdown" => Self::Shutdown,
            "notifications/initialized" | "initialized" => Self::InitializedNotification,
            "notifications/cancelled" => Self::CancelledNotification,
            _ => Self::Unknown,
        }
    }

    pub fn is_tool_call(self) -> bool {
        matches!(self, Self::ToolsCall)
    }

    pub fn is_shutdown(self) -> bool {
        matches!(self, Self::Shutdown)
    }
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
    let Value::Object(mut obj) = value else {
        return Err(ParseRpcRequestError::InvalidRequest {
            id: None,
            message: "Invalid Request: request must be a JSON object".into(),
        });
    };

    let raw_id = obj.remove("id");
    let is_notification = raw_id.is_none();
    let id = match extract_request_id(raw_id) {
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

    let method = match obj.remove("method") {
        Some(Value::String(method)) => method,
        _ => {
            return Err(ParseRpcRequestError::InvalidRequest {
                id,
                message: "Invalid Request: method must be a string".into(),
            });
        }
    };
    let method_kind = RpcMethodKind::from_name(&method);
    let params = obj.remove("params").unwrap_or_else(empty_params);

    Ok(RpcRequest {
        id,
        is_notification,
        method,
        method_kind,
        params,
    })
}

fn empty_params() -> Value {
    Value::Object(Map::new())
}

fn is_valid_request_id_value(id: Option<&Value>) -> bool {
    matches!(id, Some(Value::Null | Value::String(_) | Value::Number(_)))
}

fn extract_request_id(id: Option<Value>) -> Result<Option<Value>, ()> {
    match id {
        None => Ok(None),
        Some(id) if is_valid_request_id_value(Some(&id)) => Ok(Some(id)),
        Some(_) => Err(()),
    }
}

fn is_rpc_response_value(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && !obj.contains_key("method")
        && (obj.contains_key("result") ^ obj.contains_key("error"))
        && obj.contains_key("id")
        && is_valid_request_id_value(obj.get("id"))
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
    ping_result: Value,
    shutdown_result: Value,
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
        let ping_result = serde_json::json!({
            "content": [{"type": "text", "text": "pong"}],
            "isError": false
        });
        let shutdown_result = serde_json::json!({"ok": true});

        Ok(Self {
            initialize_result,
            tools_list_result,
            ping_result,
            shutdown_result,
        })
    }

    fn initialize_result(&self) -> &Value {
        &self.initialize_result
    }

    fn tools_list_result(&self) -> &Value {
        &self.tools_list_result
    }

    fn ping_result(&self) -> &Value {
        &self.ping_result
    }

    fn shutdown_result(&self) -> &Value {
        &self.shutdown_result
    }
}

#[derive(Debug)]
pub enum DispatchResponse<'a> {
    Owned(RpcResponse),
    BorrowedResult(BorrowedRpcResponse<'a>),
}

#[derive(Debug, Serialize)]
pub struct BorrowedRpcResponse<'a> {
    jsonrpc: &'static str,
    id: Option<Value>,
    result: &'a Value,
}

impl<'a> DispatchResponse<'a> {
    fn owned(response: RpcResponse) -> Self {
        Self::Owned(response)
    }

    fn borrowed_result(id: Option<Value>, result: &'a Value) -> Self {
        Self::BorrowedResult(BorrowedRpcResponse {
            jsonrpc: "2.0",
            id,
            result,
        })
    }

    pub fn id(&self) -> Option<&Value> {
        match self {
            Self::Owned(response) => response.id.as_ref(),
            Self::BorrowedResult(response) => response.id.as_ref(),
        }
    }

    #[cfg(test)]
    fn result(&self) -> Option<&Value> {
        match self {
            Self::Owned(response) => response.result.as_ref(),
            Self::BorrowedResult(response) => Some(response.result),
        }
    }

    #[cfg(test)]
    fn error(&self) -> Option<&tools_mcp_core::RpcError> {
        match self {
            Self::Owned(response) => response.error.as_ref(),
            Self::BorrowedResult(_) => None,
        }
    }
}

impl Serialize for DispatchResponse<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Owned(response) => response.serialize(serializer),
            Self::BorrowedResult(response) => response.serialize(serializer),
        }
    }
}

fn cancelled_request_id(params: &Value) -> Option<JsonRpcId> {
    params
        .as_object()
        .and_then(|params| params.get("requestId"))
        .and_then(JsonRpcId::from_value)
}

fn invalid_params_response<'a>(
    id: Option<Value>,
    message: &'static str,
) -> (DispatchResponse<'a>, bool) {
    (
        DispatchResponse::owned(RpcResponse::protocol_error(id, -32602, message)),
        false,
    )
}

fn unknown_tool_response<'a>(id: Option<Value>, name: &str) -> DispatchResponse<'a> {
    DispatchResponse::owned(RpcResponse::protocol_error(
        id,
        -32601,
        format!("Unknown tool: {name}. Call mcp/tools/list to see available tool names."),
    ))
}

fn unknown_method_response<'a>(id: Option<Value>, method: &str) -> DispatchResponse<'a> {
    DispatchResponse::owned(RpcResponse::protocol_error(
        id,
        -32601,
        format!("Method not found: {method}"),
    ))
}

/// Routes one JSON-RPC request. Returns `None` when no response should be sent (parse failure, notification, or cancelled request).
pub async fn dispatch_jsonrpc_request<'a>(
    req: RpcRequest,
    registry: &ToolRegistry,
    static_payloads: &'a StaticProtocolPayloads,
    inflight: &InflightRegistry,
    cancellation_token: Option<CancellationToken>,
) -> Option<(DispatchResponse<'a>, bool)> {
    if req.is_notification {
        match req.method_kind {
            RpcMethodKind::InitializedNotification => {
                tracing::info!("Received initialized notification");
                return None;
            }
            RpcMethodKind::CancelledNotification => {
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

    let out: (DispatchResponse<'a>, bool) = match req.method_kind {
        RpcMethodKind::Ping => (
            DispatchResponse::borrowed_result(req.id, static_payloads.ping_result()),
            false,
        ),

        RpcMethodKind::Initialize => (
            DispatchResponse::borrowed_result(req.id, static_payloads.initialize_result()),
            false,
        ),

        RpcMethodKind::ToolsList => (
            DispatchResponse::borrowed_result(req.id, static_payloads.tools_list_result()),
            false,
        ),

        RpcMethodKind::ToolsCall => {
            let request_id = req.id.clone();
            let Some(params_obj) = req.params.as_object() else {
                return Some(invalid_params_response(
                    req.id,
                    "Invalid params: tools/call params must be an object",
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
                return Some(invalid_params_response(
                    req.id,
                    "Invalid params: tools/call requires a non-empty tool name",
                ));
            };

            let args = params_obj
                .get("arguments")
                .cloned()
                .or_else(|| params_obj.get("args").cloned())
                .or_else(|| {
                    params_obj
                        .get("call")
                        .and_then(|c| c.get("arguments"))
                        .cloned()
                })
                .unwrap_or_else(empty_params);

            let resp = match cancellation_token {
                Some(token) => match registry
                    .call_with_cancellation(name, request_id.clone(), args, token)
                    .await
                {
                    Some(DispatchOutcome::Respond(outcome)) => {
                        DispatchResponse::owned(outcome.into_rpc_response(request_id))
                    }
                    Some(DispatchOutcome::Cancelled) => return None,
                    None => unknown_tool_response(req.id, name),
                },
                None => {
                    if let Some(result) = registry.call(name, request_id, args).await {
                        DispatchResponse::owned(result)
                    } else {
                        unknown_tool_response(req.id, name)
                    }
                }
            };
            (resp, false)
        }

        RpcMethodKind::Shutdown => (
            DispatchResponse::borrowed_result(req.id, static_payloads.shutdown_result()),
            true,
        ),

        _ => {
            let method = req.method;
            tracing::error!("Unknown method: {}", method);
            (unknown_method_response(req.id, &method), false)
        }
    };

    Some(out)
}

pub async fn dispatch_jsonrpc_batch<'a>(
    items: Vec<RpcBatchItem>,
    registry: &ToolRegistry,
    static_payloads: &'a StaticProtocolPayloads,
    inflight: &InflightRegistry,
) -> Option<(Vec<DispatchResponse<'a>>, bool)> {
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
                responses.push(DispatchResponse::owned(RpcResponse::protocol_error(
                    id, -32600, message,
                )));
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
        RpcMethodKind, RpcRequest, ServerCapabilitiesTools, ServerInfo, StaticProtocolPayloads,
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

    fn rpc_request(method: &str, id: serde_json::Value, params: serde_json::Value) -> RpcRequest {
        RpcRequest {
            id: Some(id),
            is_notification: false,
            method: method.to_string(),
            method_kind: RpcMethodKind::from_name(method),
            params,
        }
    }

    #[test]
    fn borrowed_dispatch_response_serializes_legacy_success_shape() {
        let result = json!({ "tools": [] });
        let response = super::DispatchResponse::borrowed_result(Some(json!("request-id")), &result);

        let serialized = serde_json::to_value(response).expect("response should serialize");

        assert_eq!(
            serialized,
            json!({
                "jsonrpc": "2.0",
                "id": "request-id",
                "result": { "tools": [] }
            })
        );
    }

    #[test]
    fn parse_rpc_message_preserves_null_id_and_defaults_missing_params() {
        let parsed = parse_rpc_message(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();

        let RpcMessage::Request(req) = parsed else {
            panic!("expected request message");
        };
        assert_eq!(req.id, Some(json!(null)));
        assert!(!req.is_notification);
        assert_eq!(req.params, json!({}));

        let parsed =
            parse_rpc_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();

        let RpcMessage::Request(req) = parsed else {
            panic!("expected notification request message");
        };
        assert!(req.id.is_none());
        assert!(req.is_notification);
        assert_eq!(req.params, json!({}));
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
    async fn ping_dispatch_routes_aliases_with_pong_shape_and_ids() {
        let registry = ToolRegistry::new();
        let tools = Vec::<ToolDef>::new();
        let static_payloads =
            StaticProtocolPayloads::new(&tools).expect("static payloads should serialize");

        for method in ["ping", "mcp/ping"] {
            let (response, should_exit) = dispatch_jsonrpc_request(
                rpc_request(method, json!(method), json!({})),
                &registry,
                &static_payloads,
                &InflightRegistry::default(),
                None,
            )
            .await
            .expect("ping alias should respond");

            assert!(!should_exit);
            assert_eq!(response.id().cloned(), Some(json!(method)));
            assert_eq!(
                response.result().cloned(),
                Some(json!({
                    "content": [{"type": "text", "text": "pong"}],
                    "isError": false
                }))
            );
            assert!(response.error().is_none());
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
            rpc_request("mcp/initialize", json!(42), json!({"capabilities": {}})),
            &registry,
            &static_payloads,
            &InflightRegistry::default(),
            None,
        )
        .await
        .expect("initialize should respond");

        assert!(!should_exit);
        assert_eq!(response.id().cloned(), Some(json!(42)));
        assert_eq!(response.result().cloned(), Some(expected));
        assert!(response.error().is_none());
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
                rpc_request(method, json!(method), json!({})),
                &registry,
                &static_payloads,
                &InflightRegistry::default(),
                None,
            )
            .await
            .expect("tools/list alias should respond");

            assert!(!should_exit);
            assert_eq!(response.id().cloned(), Some(json!(method)));
            assert_eq!(response.result().cloned(), Some(expected.clone()));
            assert!(response.error().is_none());
        }
    }

    #[tokio::test]
    async fn shutdown_dispatch_routes_aliases_with_ok_shape_ids_and_exit_signal() {
        let registry = ToolRegistry::new();
        let tools = Vec::<ToolDef>::new();
        let static_payloads =
            StaticProtocolPayloads::new(&tools).expect("static payloads should serialize");

        for method in ["mcp/shutdown", "shutdown", "server/shutdown"] {
            let (response, should_exit) = dispatch_jsonrpc_request(
                rpc_request(method, json!(method), json!({})),
                &registry,
                &static_payloads,
                &InflightRegistry::default(),
                None,
            )
            .await
            .expect("shutdown alias should respond");

            assert!(should_exit);
            assert_eq!(response.id().cloned(), Some(json!(method)));
            assert_eq!(response.result().cloned(), Some(json!({"ok": true})));
            assert!(response.error().is_none());
        }
    }
}
