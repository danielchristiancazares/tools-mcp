//! MCP JSON-RPC routing and tool registry wiring (inbound adapter).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::response::{RpcError, RpcResponse};
use crate::tool_registry::{ToolDef, ToolRegistry};

/// Constructs the tool registry with all available MCP tools.
pub fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register::<crate::tools::PingTool>();
    registry.register::<crate::tools::WebFetchTool>();
    registry.register::<crate::tools::SearchTool>();
    registry.register::<crate::tools::CodeQueryTool>();
    registry.register::<crate::tools::ReadTool>();
    registry.register::<crate::tools::EditTool>();
    registry.register::<crate::tools::WriteTool>();
    registry.register::<crate::tools::DeleteTool>();
    registry.register::<crate::tools::GlobTool>();
    registry.register::<crate::tools::MoveTool>();
    registry.register::<crate::tools::CopyTool>();
    registry.register::<crate::tools::ListDirTool>();
    registry.register::<crate::tools::BuildTool>();
    registry.register::<crate::tools::TestTool>();
    registry.register::<crate::tools::OutlineTool>();
    registry.register::<crate::tools::PwshTool>();
    registry.register::<crate::tools::GitStatusTool>();
    registry.register::<crate::tools::GitDiffTool>();
    registry.register::<crate::tools::GitRestoreTool>();
    registry.register::<crate::tools::GitAddTool>();
    registry.register::<crate::tools::GitCommitTool>();
    registry.register::<crate::tools::GitLogTool>();
    registry.register::<crate::tools::GitBranchTool>();
    registry.register::<crate::tools::GitCheckoutTool>();
    registry.register::<crate::tools::GitStashTool>();
    registry.register::<crate::tools::GitShowTool>();
    registry.register::<crate::tools::GitBlameTool>();

    registry
}

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[serde(rename = "jsonrpc")]
    pub _jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
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
) -> Option<(RpcResponse<'static>, bool)> {
    if req.id.is_none() {
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

    let out: (RpcResponse<'static>, bool) = match req.method.as_str() {
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
                    name: "mcp-echo-server".into(),
                    version: option_env!("APP_VERSION").unwrap_or("0.9.0").into(),
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
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("toolName").and_then(|v| v.as_str()))
                .or_else(|| {
                    params
                        .get("call")
                        .and_then(|c| c.get("name"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("");

            let args = params
                .get("arguments")
                .cloned()
                .or_else(|| params.get("args").cloned())
                .or_else(|| params.get("call").and_then(|c| c.get("arguments")).cloned())
                .unwrap_or(Value::Object(Default::default()));

            let resp = if let Some(result) = registry.call(name, req.id.clone(), args).await {
                result
            } else {
                RpcResponse::protocol_error(
                    req.id,
                    -32601,
                    format!(
                        "Unknown tool: {}. Call mcp/tools/list to see available tool names.",
                        name
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
