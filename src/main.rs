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

fn should_skip_headers() -> bool {
    *SKIP_HEADERS.get_or_init(|| {
        std::env::var("MCP_SKIP_HEADERS")
            .unwrap_or_default()
            .eq_ignore_ascii_case("true")
    })
}

mod codequery;
mod read_file;
mod ripgrep;
mod smart_file_edit;
mod webfetch;

use crate::codequery::handle_code_query;

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

#[derive(Serialize, Clone)]
struct ToolDef {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
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

    let tools = vec![
        ToolDef {
            name: "WebFetch".into(),
            description: "Fetch and summarize external web content with caching".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "url": {"type":"string", "description":"Absolute URL to fetch"},
                    "max_chunk_tokens": {"type":"integer","minimum":200,"description":"Approximate token budget per chunk"},
                    "no_cache": {"type":"boolean","description":"Bypass on-disk cache if true"}
                },
                "required":["url"]
            }),
        },
        ToolDef {
            name: "ping".into(),
            description: "Health check tool: responds with pong".into(),
            input_schema: serde_json::json!({ "type":"object", "properties":{}, "required":[] }),
        },
        ToolDef {
            name: "Bash".into(),
            description: "Run shell commands via bash.exe on Windows (or bash on Unix) with timeout and stdout/stderr capture.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "command": {
                        "type":"string",
                        "description":"Shell command to run inside bash.exe / bash (executed as: bash -lc \"<command>\")"
                    },
                    "timeout_ms": {
                        "type":"integer",
                        "minimum":100,
                        "default":30000,
                        "description":"Timeout in milliseconds before the command is aborted"
                    },
                    "working_dir": {
                        "type":"string",
                        "description":"Optional working directory for the command"
                    }
                },
                "required":["command"]
            }),
        },
        ToolDef {
            name: "RipGrep".into(),
            description: "Fast regex search via ripgrep (rg). Returns line-numbered output and structured match records.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "pattern":{"type":"string","description":"ripgrep pattern (regex by default)"},
                    "path":{"type":"string","description":"File or directory to search (default: current working directory)"},
                    "case":{"type":"string","enum":["smart","sensitive","insensitive"],"default":"smart","description":"Case handling mode"},
                    "fixed_strings":{"type":"boolean","default":false,"description":"Treat pattern as a literal string (rg -F)"},
                    "word_regexp":{"type":"boolean","default":false,"description":"Match on word boundaries only (rg -w)"},
                    "glob":{"type":"array","items":{"type":"string"},"description":"Optional glob filters (repeats rg --glob)"},
                    "hidden":{"type":"boolean","default":false,"description":"Search hidden files/directories (rg --hidden)"},
                    "follow":{"type":"boolean","default":false,"description":"Follow symlinks (rg --follow)"},
                    "no_ignore":{"type":"boolean","default":false,"description":"Do not respect ignore files like .gitignore (rg --no-ignore)"},
                    "context":{"type":"integer","minimum":0,"default":0,"description":"Lines of context on both sides (rg -C)"},
                    "max_results":{"type":"integer","minimum":1,"maximum":10000,"default":200,"description":"Maximum match/context events to return"},
                    "timeout_ms":{"type":"integer","minimum":100,"default":20000,"description":"Overall timeout in milliseconds"}
                },
                "required":["pattern"]
            }),
        },
        ToolDef {
            name: "CodeQuery".into(),
            description: "Index codebase files and run semantic search in one operation. Automatically syncs changed files (if file_paths provided) and queries the vector store.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "vector_store_id": {"type":"string", "description":"Target vector store ID"},
                    "vector_store_name": {"type":"string", "description":"Target vector store name"},
                    "query": {"type":"string", "description":"Natural language code question"},
                    "file_paths": {"type":"array", "items":{"type":"string"}, "description":"Optional local file paths to sync before querying"},
                    "concurrent_limit": {"type":"integer", "minimum":1, "maximum":20, "default":5, "description":"Max concurrent operations (default: 5)"},
                    "timeout_ms": {"type":"integer", "minimum":1000, "default":60000, "description":"Overall indexing wait timeout in milliseconds"},
                    "model": {"type":"string", "description":"Override model (defaults to ApiConfig default)"},
                    "max_num_results": {"type":"integer", "minimum":1, "description":"Limit vector search matches"},
                    "include_results": {"type":"boolean", "default":false, "description":"Include retrieved snippets in the response payload"}
                },
                "required":["query"]
            }),
        },
        ToolDef {
            name: "ReadFile".into(),
            description: "Read a file (optionally a line range) for quick inspection without uploading. Output is line-numbered.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","description":"Filesystem path to read"},
                    "start_line":{"type":"integer","minimum":1,"description":"Optional 1-based start line"},
                    "end_line":{"type":"integer","minimum":1,"description":"Optional 1-based end line (inclusive)"}
                },
                "required":["path"]
            }),
        },
        ToolDef {
            name: "SmartFileEdit".into(),
            description: "Read and edit files via a canonical LF view while preserving original newline bytes and whitespace.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "required":["action","path"],
                "properties":{
                    "action":{
                        "type":"string",
                        "enum":["get_region","apply_snippet_edit","apply_unified_diff"],
                        "description":"Operation to perform"
                    },
                    "path":{"type":"string","description":"Filesystem path to inspect or edit"},
                    "start_line":{"type":"integer","minimum":1,"description":"Start line for get_region"},
                    "end_line":{"type":"integer","minimum":1,"description":"End line for get_region"},
                    "old_snippet":{"type":"string","description":"Existing canonical snippet to replace"},
                    "new_snippet":{"type":"string","description":"Replacement snippet using LF newlines"},
                    "file_hash":{"type":"string","description":"sha256 hash returned by get_region to detect stale files"},
                    "region_id":{"type":"string","description":"Region identifier returned by get_region"},
                    "match_hint":{
                        "type":"object",
                        "properties":{
                            "start_line":{"type":"integer","minimum":1},
                            "end_line":{"type":"integer","minimum":1}
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": true
            }),
        },
    ];

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
            // Tools listing
            "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
            | "capabilities" => (
                RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({"tools": tools})),
                    error: None,
                },
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

                let resp = match name {
                    // webfetch tool
                    "WebFetch" => handle_webfetch(req.id.clone(), args).await,
                    // ping
                    "ping" => RpcResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: Some(serde_json::json!({
                            "content": [ { "type": "text", "text": "pong" } ], "isError": false
                        })),
                        error: None,
                    },
                    // Bash wrapper
                    "Bash" | "bash" => handle_bash(req.id.clone(), args).await,
                    // ripgrep search
                    "RipGrep" | "ripgrep" | "rg" => {
                        ripgrep::handle_ripgrep(req.id.clone(), args).await
                    }
                    // code query
                    "CodeQuery" | "code_query" | "code-query" => {
                        handle_code_query(req.id.clone(), args).await
                    }
                    // read file
                    "ReadFile" | "read_file" | "read-file" => {
                        read_file::handle_read_file(req.id.clone(), args).await
                    }
                    "smart_file_edit" | "SmartFileEdit" => {
                        smart_file_edit::handle_smart_file_edit(req.id.clone(), args).await
                    }
                    other => RpcResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: None,
                        error: Some(RpcError {
                            code: -32601,
                            message: format!("Unknown tool: {}", other),
                            data: None,
                        }),
                    },
                };
                (resp, false)
            }
            // Shutdown aliases
            "mcp/shutdown" | "shutdown" | "server/shutdown" => {
                let resp = RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({"ok": true})),
                    error: None,
                };
                (resp, true)
            }
            // Top-level ping/health
            "ping" | "health" | "mcp/health" => (
                RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({
                        "content": [ { "type": "text", "text": "pong" } ], "isError": false
                    })),
                    error: None,
                },
                false,
            ),
            _ => {
                // Log unknown method for debugging
                error!("Unknown method: {}", req.method);
                (
                    RpcResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: None,
                        error: Some(RpcError {
                            code: -32601,
                            message: format!("Method not found: {}", req.method),
                            data: None,
                        }),
                    },
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

async fn handle_webfetch(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req_result = serde_json::from_value::<webfetch::FetchRequest>(args);
    let request = match req_result {
        Ok(req) => req,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("invalid arguments: {}", e))),
                error: None,
            };
        }
    };

    match webfetch::run_fetch(request).await {
        Ok(response) => {
            match serde_json::to_value(&response) {
                Ok(json_value) => {
                    let json_text =
                        serde_json::to_string_pretty(&json_value).unwrap_or_else(|e| {
                            format!("{{\"error\": \"serialization failed: {}\"}}", e)
                        });
                    RpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: Some(serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": json_text
                            }],
                            "isError": false
                        })),
                        error: None,
                    }
                }
                Err(e) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "webfetch succeeded but response serialization failed: {}",
                        e
                    ))),
                    error: None,
                },
            }
        }
        Err(e) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text(&format!("webfetch error: {:#}", e))),
            error: None,
        },
    }
}

pub(crate) fn err_text(msg: &str) -> Value {
    serde_json::json!({"content":[{"type":"text","text": msg}], "isError": true})
}

async fn handle_bash(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    use serde::Deserialize;
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::time;

    #[derive(Deserialize)]
    struct BashRequest {
        command: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        working_dir: Option<String>,
    }

    let req_result = serde_json::from_value::<BashRequest>(args);
    let request = match req_result {
        Ok(req) => req,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("invalid arguments: {}", e))),
                error: None,
            };
        }
    };

    let shell_binary = if cfg!(target_os = "windows") {
        "bash.exe"
    } else {
        "bash"
    };

    info!(
        "Bash tool: running command via {}: {}",
        shell_binary, request.command
    );

    let timeout_ms = request.timeout_ms.unwrap_or(30_000);

    let mut cmd = Command::new(shell_binary);
    cmd.arg("-lc").arg(&request.command);

    if let Some(dir) = &request.working_dir {
        cmd.current_dir(dir);
    }

    let output_result = time::timeout(Duration::from_millis(timeout_ms), cmd.output()).await;

    match output_result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit_code = output.status.code();
            let success = output.status.success();

            if !success {
                error!("Bash tool: command failed (exit_code={:?})", exit_code);
            }

            let result = serde_json::json!({
                "command": request.command,
                "shell": shell_binary,
                "timeout_ms": timeout_ms,
                "exit_code": exit_code,
                "success": success,
                "stdout": stdout.clone(),
                "stderr": stderr.clone(),
            });

            let json_text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| {
                format!(
                    "success: {}\nexit_code: {:?}\n\nstdout:\n{}\n\nstderr:\n{}",
                    success, exit_code, stdout, stderr
                )
            });

            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": json_text
                    }],
                    "isError": !success
                })),
                error: None,
            }
        }
        Ok(Err(e)) => {
            error!("Bash tool: failed to spawn {}: {}", shell_binary, e);
            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "Failed to spawn {}: {}",
                    shell_binary, e
                ))),
                error: None,
            }
        }
        Err(_) => {
            error!("Bash tool: command timed out after {} ms", timeout_ms);
            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "Bash command timed out after {} ms",
                    timeout_ms
                ))),
                error: None,
            }
        }
    }
}
