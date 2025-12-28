//! # MCP File Search Server
//!
//! This module implements a Model Context Protocol (MCP) server that provides file search
//! functionality using OpenAI's vector stores API. The server communicates via JSON-RPC 2.0
//! over stdin/stdout, making it suitable for integration with various MCP clients.
//!
//! ## Features
//!
//! - **File Upload**: Upload files to OpenAI vector stores with automatic format conversion
//! - **Vector Store Management**: Create, list, and query vector stores
//! - **Semantic Search**: Perform semantic searches across uploaded documents
//! - **File Management**: List and delete files from vector stores
//! - **Protocol Compatibility**: Supports multiple MCP protocol aliases for broad compatibility
//!
//! ## Protocol
//!
//! The server implements the MCP protocol with support for:
//! - JSON-RPC 2.0 message format
//! - Content-Length headers (optional via MCP_SKIP_HEADERS env var)
//! - Protocol version negotiation
//! - Notification handling (e.g., notifications/initialized)
//!
//! ## Environment Variables
//!
//! - `OPENAI_API_KEY`: Required. Your OpenAI API key for vector store operations
//! - `MCP_SKIP_HEADERS`: Optional. Set to "true" to output raw JSON without Content-Length headers
//! - `RUST_LOG`: Optional. Logging level (debug, info, warn, error)
//!
//! ## Usage
//!
//! The server is typically run as a subprocess by an MCP client:
//! ```bash
//! echo '{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}' | cargo run
//! ```

use anyhow::{Context, Result};
use file_search_core as core;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tracing::{error, info};

/// Cached value of MCP_SKIP_HEADERS env var (read once at first use)
static SKIP_HEADERS: OnceLock<bool> = OnceLock::new();

/// Hard cap for inbound MCP message bodies (Content-Length framing) and headerless JSON lines.
/// Prevents unbounded allocations / memory DoS.
const MAX_MCP_MESSAGE_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

fn should_skip_headers() -> bool {
    *SKIP_HEADERS.get_or_init(|| {
        std::env::var("MCP_SKIP_HEADERS")
            .unwrap_or_default()
            .eq_ignore_ascii_case("true")
    })
}

mod codequery;
mod config;
mod git_tools;
mod process_utils;
mod read_file;
mod ripgrep;
mod script_runner;
mod smart_file_edit;
mod tool_registry;
mod tools;
mod webfetch;

use crate::tool_registry::{ToolDef, ToolRegistry};

fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register::<tools::PingTool>();
    registry.register::<tools::WebFetchTool>();
    registry.register::<tools::SearchTool>();
    registry.register::<tools::CodeQueryTool>();
    registry.register::<tools::ReadTool>();
    registry.register::<tools::EditTool>();
    registry.register::<tools::WriteTool>();
    registry.register::<tools::DeleteTool>();
    registry.register::<tools::GlobTool>();
    registry.register::<tools::BuildTool>();
    registry.register::<tools::TestTool>();
    registry.register::<tools::OutlineTool>();
    registry.register::<tools::PwshTool>();
    registry.register::<tools::GitStatusTool>();
    registry.register::<tools::GitDiffTool>();
    registry.register::<tools::GitRestoreTool>();
    registry.register::<tools::GitAddTool>();
    registry.register::<tools::GitCommitTool>();
    registry
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct RpcResponse<'a> {
    pub(crate) jsonrpc: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
}

impl RpcResponse<'static> {
    /// Success response with a result payload
    pub fn ok(id: Option<Value>, result: Value) -> RpcResponse<'static> {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response using MCP content format (result with isError: true)
    pub fn err(id: Option<Value>, msg: impl std::fmt::Display) -> RpcResponse<'static> {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(serde_json::json!({"content":[{"type":"text","text": msg.to_string()}], "isError": true})),
            error: None,
        }
    }

    /// Success response with text content and additional metadata fields
    pub fn ok_text_with(
        id: Option<Value>,
        text: impl Into<String>,
        extra: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> RpcResponse<'static> {
        let mut payload = serde_json::json!({
            "content": [{"type": "text", "text": text.into()}],
            "isError": false
        });
        if let Some(obj) = payload.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k.to_string(), v);
            }
        }
        RpcResponse::ok(id, payload)
    }

    /// Success response with pretty-printed JSON as text content
    pub fn ok_json_content(id: Option<Value>, json_value: Value, is_error: bool) -> RpcResponse<'static> {
        let json_text = serde_json::to_string_pretty(&json_value).unwrap_or_else(|e| {
            format!("{{\"error\": \"serialization failed: {}\"}}", e)
        });
        RpcResponse::ok(
            id,
            serde_json::json!({
                "content": [{"type": "text", "text": json_text}],
                "isError": is_error
            }),
        )
    }

    /// Parse request arguments, returning error response on failure
    pub fn parse<T: serde::de::DeserializeOwned>(
        id: Option<Value>,
        args: Value,
    ) -> Result<T, RpcResponse<'static>> {
        serde_json::from_value::<T>(args)
            .map_err(|e| RpcResponse::err(id, format!("invalid arguments: {e}")))
    }

    /// JSON-RPC protocol error (uses error field, not result)
    pub fn protocol_error(id: Option<Value>, code: i64, msg: impl Into<String>) -> RpcResponse<'static> {
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

async fn read_mcp_message<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            if content_length.is_some() {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading headers",
                ));
            }
            return Ok(None);
        }
        if line.len() > MAX_MCP_MESSAGE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "message line exceeds maximum allowed size",
            ));
        }
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }

        let trimmed_start = trimmed.trim_start();
        if content_length.is_none()
            && (trimmed_start.starts_with('{') || trimmed_start.starts_with('['))
        {
            return Ok(Some(trimmed.to_owned()));
        }

        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let len = value.trim().parse::<usize>().map_err(|_| {
                    io::Error::new(ErrorKind::InvalidData, "invalid Content-Length header")
                })?;
                if len > MAX_MCP_MESSAGE_BYTES {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "Content-Length exceeds maximum allowed size",
                    ));
                }
                content_length = Some(len);
            }
        }
    }

    let len = content_length
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"))?;

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let message = String::from_utf8(buf)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "message body not valid UTF-8"))?;

    let trailing = reader.fill_buf().await?;
    if trailing.starts_with(b"\r\n") {
        reader.consume(2);
    } else if trailing.starts_with(b"\n") {
        reader.consume(1);
    }

    Ok(Some(message))
}

async fn write_mcp_response<W>(writer: &mut W, resp: &RpcResponse<'_>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_vec(resp).context("serialize response")?;

    if !payload.ends_with(b"\n") {
        payload.push(b'\n');
    }

    let payload_len = payload.len();

    // Check if we should skip Content-Length headers (for Codex compatibility)
    if !should_skip_headers() {
        let header = format!("Content-Length: {}\r\n\r\n", payload_len);
        writer
            .write_all(header.as_bytes())
            .await
            .context("write Content-Length header")?;
    }

    writer
        .write_all(&payload)
        .await
        .context("write response payload")?;

    // Force immediate flush - critical for Codex compatibility
    writer.flush().await.context("flush stdout")?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout;
    let reader = BufReader::new(stdin);
    let mut reader = reader;

    let registry = build_tool_registry();
    let tools = registry.list();

    while let Some(line) = match read_mcp_message(&mut reader).await {
        Ok(v) => v,
        Err(e) => {
            error!("failed to read MCP message: {}", e);
            None
        }
    } {
        if line.trim().is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(r) => {
                info!("Received request: method={}, id={:?}", &r.method, &r.id);
                r
            }
            Err(e) => {
                error!("invalid json: {}", e);
                continue;
            }
        };

        // Check if this is a notification (no id field) - notifications don't get responses
        if req.id.is_none() {
            match req.method.as_str() {
                "notifications/initialized" | "initialized" => {
                    // Acknowledged silently, no response per JSON-RPC spec
                    info!("Received initialized notification");
                    continue;
                }
                _ => {
                    error!("Unknown notification: {}", req.method);
                    continue;
                }
            }
        }

        let (resp, should_exit): (RpcResponse, bool) = match req.method.as_str() {
            // Simple ping (JSON-RPC method)
            "ping" | "mcp/ping" => (
                RpcResponse::ok(req.id, serde_json::json!({
                    "content": [{"type": "text", "text": "pong"}],
                    "isError": false
                })),
                false,
            ),
            // Initialization aliases (Codex-compatible)
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
                    tools: tools.clone(),
                };
                let (result, error) = match serde_json::to_value(init) {
                    Ok(v) => (Some(v), None),
                    Err(e) => (
                        None,
                        Some(RpcError {
                            code: -32603,
                            message: "Internal error: failed to serialize initialize payload"
                                .into(),
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
            // Tools listing
            "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
            | "capabilities" => (
                RpcResponse::ok(req.id, serde_json::json!({"tools": tools})),
                false,
            ),
            // Tool call
            "mcp/tools/call" | "tools/call" | "server/tools/call" => {
                // Accept param shapes {name,arguments} | {toolName,args} | {call:{name,arguments}}
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
                    RpcResponse::protocol_error(req.id, -32601, format!("Unknown tool: {}", name))
                };
                (resp, false)
            }
            // Shutdown aliases
            "mcp/shutdown" | "shutdown" | "server/shutdown" => {
                (RpcResponse::ok(req.id, serde_json::json!({"ok": true})), true)
            }
            _ => {
                // Log unknown method for debugging
                error!("Unknown method: {}", req.method);
                (
                    RpcResponse::protocol_error(req.id, -32601, format!("Method not found: {}", req.method)),
                    false,
                )
            }
        };

        info!("Sending response for request id: {:?}", resp.id);
        if let Err(e) = write_mcp_response(&mut writer, &resp).await {
            error!("failed to write MCP response: {}", e);
            break;
        }
        info!("Response sent successfully");

        if should_exit {
            info!("shutdown requested");
            break;
        }
    }

    Ok(())
}


