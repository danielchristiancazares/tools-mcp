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

use anyhow::{anyhow, Context, Result};
use file_search_core as core;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, fs, path::PathBuf};
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tracing::{error, info, warn};

mod webfetch;

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
struct RpcResponse<'a> {
    jsonrpc: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
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
    let skip_headers = std::env::var("MCP_SKIP_HEADERS")
        .unwrap_or_default()
        .to_lowercase()
        == "true";

    if !skip_headers {
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
                (
                    RpcResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: Some(serde_json::to_value(init).unwrap()),
                        error: None,
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
                    // code query
                    "CodeQuery" | "code_query" | "code-query" => {
                        handle_code_query(req.id.clone(), args).await
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

async fn handle_code_query(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("OPENAI_API_KEY not set")),
            error: None,
        };
    }

    let vector_store_id_arg = args
        .get("vector_store_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let vector_store_name = args
        .get("vector_store_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

    if query.trim().is_empty() || (vector_store_id_arg.is_none() && vector_store_name.is_none()) {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text(
                "query and either vector_store_id or vector_store_name are required",
            )),
            error: None,
        };
    }

    let file_paths: Vec<String> = args
        .get("file_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|val| val.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let concurrent_limit = args
        .get("concurrent_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    if !(1..=20).contains(&concurrent_limit) {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("concurrent_limit must be between 1 and 20")),
            error: None,
        };
    }

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    if timeout_ms < 1_000 {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("timeout_ms must be at least 1000 milliseconds")),
            error: None,
        };
    }

    let include_results = args
        .get("include_results")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_num_results = args
        .get("max_num_results")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let model_override = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let client = reqwest::Client::new();
    let cfg =
        file_search_core::ApiConfig::new(api_key, model_override.as_deref().unwrap_or("gpt-4o"));

    let vector_store_id = match vector_store_id_arg {
        Some(id) => id,
        None => match resolve_vector_store_id(&client, &cfg, vector_store_name.as_deref().unwrap())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "failed to resolve vector store name: {}",
                        e
                    ))),
                    error: None,
                }
            }
        },
    };

    match file_search_core::code_query(
        &client,
        &cfg,
        &vector_store_id,
        &file_paths,
        query,
        file_search_core::CodeQueryOptions {
            concurrent_limit,
            timeout_ms,
            model: model_override.as_deref(),
            max_num_results,
            include_results,
        },
    )
    .await
    {
        Ok((text, reindex_summary)) => {
            let mut content = vec![serde_json::json!({
                "type": "text",
                "text": text
            })];

            if let Some(summary) = reindex_summary {
                let summary_text =
                    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| summary.to_string());
                content.push(serde_json::json!({
                    "type": "text",
                    "text": summary_text
                }));
            }

            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(serde_json::json!({
                    "content": content,
                    "isError": false
                })),
                error: None,
            }
        }
        Err(e) => {
            let error_message = e.to_string();
            let (client_message, log_message) = if error_message
                .contains("code_query reindex failed")
            {
                (
                    "Codebase indexing failed after 3 attempts. Please try manual searching heuristics."
                        .to_string(),
                    format!("CodeQuery reindex failed: {}", error_message),
                )
            } else {
                (
                    format!("CodeQuery failed: {}", error_message),
                    format!("CodeQuery error: {}", error_message),
                )
            };

            error!("{}", log_message);

            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&client_message)),
                error: None,
            }
        }
    }
}

async fn resolve_vector_store_id(
    client: &Client,
    cfg: &core::ApiConfig,
    name: &str,
) -> Result<String> {
    if let Some(id) = load_store_id_from_cache(name) {
        return Ok(id);
    }

    // We fall back to the API when the cache misses so the happy-path stays fast after the
    // first lookup without requiring manual list-stores calls.
    let stores = core::list_vector_stores(client, cfg).await?;
    if let Some(entry) = stores
        .into_iter()
        .find(|entry| entry.name.as_deref() == Some(name))
    {
        cache_store_id(name, &entry.id);
        return Ok(entry.id);
    }

    Err(anyhow!("vector store '{}' not found", name))
}

fn load_store_id_from_cache(name: &str) -> Option<String> {
    // Assumption: removing a vector store is rare; if the cached ID becomes stale the subsequent
    // CodeQuery call will surface the error, which keeps the common-case lookup cheap.
    let cache = load_store_cache();
    cache.get(name).cloned()
}

fn cache_store_id(name: &str, id: &str) {
    let mut cache = load_store_cache();
    if cache
        .get(name)
        .map(|existing| existing == id)
        .unwrap_or(false)
    {
        return;
    }
    cache.insert(name.to_string(), id.to_string());
    if let Err(err) = write_store_cache(&cache) {
        warn!("Failed to persist CodeQuery store cache: {}", err);
    }
}

fn load_store_cache() -> HashMap<String, String> {
    let Some(path) = stores_cache_path() else {
        return HashMap::new();
    };

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "Failed to read CodeQuery store cache at {}: {}",
                    path.display(),
                    err
                );
            }
            return HashMap::new();
        }
    };

    match serde_json::from_str::<HashMap<String, String>>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                "Ignoring invalid CodeQuery store cache at {}: {}",
                path.display(),
                err
            );
            HashMap::new()
        }
    }
}

fn write_store_cache(cache: &HashMap<String, String>) -> Result<()> {
    let Some(path) = stores_cache_path() else {
        warn!("Skipping CodeQuery store cache write because HOME is unset");
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create CodeQuery store cache directory {}",
                parent.display()
            )
        })?;
    }

    let payload =
        serde_json::to_string_pretty(cache).context("failed to serialize CodeQuery store cache")?;
    fs::write(&path, payload).with_context(|| {
        format!(
            "failed to write CodeQuery store cache at {}",
            path.display()
        )
    })?;
    Ok(())
}

fn stores_cache_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".codex");
        path.push("mcp");
        path.push("stores.json");
        path
    })
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
            }
        }
    };

    match webfetch::run_fetch(request).await {
        Ok(response) => {
            let json_value =
                serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({}));
            let json_text = serde_json::to_string(&json_value).unwrap_or_else(|_| "{}".to_string());
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
            result: Some(err_text(&format!("webfetch error: {:#}", e))),
            error: None,
        },
    }
}

fn err_text(msg: &str) -> Value {
    serde_json::json!({"content":[{"type":"text","text": msg}], "isError": true})
}
