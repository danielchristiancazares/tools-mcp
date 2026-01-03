//! # MCP File Search Server
//!
//! This module implements a Model Context Protocol (MCP) server that provides file search
//! functionality using OpenAI's vector stores API. The server communicates via JSON-RPC 2.0
//! over stdin/stdout, making it suitable for integration with various MCP clients.
//!
//! ## Architecture Overview
//!
//! The server follows a request-response pattern over stdin/stdout:
//!
//! ```text
//! ┌─────────────────┐                      ┌─────────────────┐
//! │   MCP Client    │  ── JSON-RPC ──>     │   MCP Server    │
//! │  (e.g., Codex)  │  <── JSON-RPC ──     │  (this binary)  │
//! └─────────────────┘                      └─────────────────┘
//!                                                   │
//!                                                   ▼
//!                                          ┌─────────────────┐
//!                                          │  Tool Registry  │
//!                                          │  (dispatches to │
//!                                          │   tool impls)   │
//!                                          └─────────────────┘
//! ```
//!
//! ### Message Flow
//!
//! 1. **Initialization**: Client sends `mcp/initialize`, server responds with capabilities
//! 2. **Tool Discovery**: Client calls `mcp/tools/list` to enumerate available tools
//! 3. **Tool Execution**: Client invokes tools via `mcp/tools/call` with tool name and arguments
//! 4. **Shutdown**: Client sends `mcp/shutdown` to gracefully terminate the server
//!
//! ### Protocol Details
//!
//! Messages can be framed in two ways:
//! - **Content-Length headers**: Standard HTTP-style framing (`Content-Length: N\r\n\r\n{...}`)
//! - **Raw JSON lines**: When `MCP_SKIP_HEADERS=true`, messages are newline-delimited JSON
//!
//! The server auto-detects the framing style based on whether the input starts with `{` or
//! contains a `Content-Length` header.
//!
//! ## Features
//!
//! - **File Upload**: Upload files to OpenAI vector stores with automatic format conversion
//! - **Vector Store Management**: Create, list, and query vector stores
//! - **Semantic Search**: Perform semantic searches across uploaded documents
//! - **File Management**: List and delete files from vector stores
//! - **Protocol Compatibility**: Supports multiple MCP protocol aliases for broad compatibility
//!
//! ## Tool Registration
//!
//! Tools are registered at startup via the [`ToolRegistry`]. Each tool implements the
//! [`McpTool`](crate::tool_registry::McpTool) trait, which defines:
//! - Tool name and optional aliases
//! - Description and JSON schema for input parameters
//! - Async execution logic
//!
//! ## JSON-RPC Method Aliases
//!
//! For compatibility with different MCP client implementations, the server accepts multiple
//! aliases for each method:
//!
//! | Operation    | Accepted Methods                                       |
//! |--------------|--------------------------------------------------------|
//! | Initialize   | `mcp/initialize`, `initialize`, `server/initialize`   |
//! | List Tools   | `mcp/tools/list`, `tools/list`, `server/tools/list`   |
//! | Call Tool    | `mcp/tools/call`, `tools/call`, `server/tools/call`   |
//! | Shutdown     | `mcp/shutdown`, `shutdown`, `server/shutdown`          |
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
//!
//! ```bash
//! # With Content-Length headers (default)
//! echo 'Content-Length: 58\r\n\r\n{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}' | cargo run
//!
//! # Without headers (Codex-compatible mode)
//! MCP_SKIP_HEADERS=true cargo run <<< '{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}'
//! ```
//!
//! ## Error Handling
//!
//! The server distinguishes between two error types:
//! - **Protocol errors**: Invalid JSON-RPC format, unknown methods (uses `error` field)
//! - **Tool errors**: Tool execution failures (uses `result` with `isError: true`)
//!
//! This distinction allows clients to differentiate between transport-level issues and
//! application-level failures.

use anyhow::Result;
use file_search_core as core;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{self, BufReader};
use tracing::{error, info};

use mcp_protocol::{read_mcp_message, should_skip_headers, write_mcp_response_with_mode};

mod codequery;
mod config;
mod git;
mod mcp_protocol;
mod process_utils;
mod response;
mod smart_file_edit;
mod tool_registry;
mod tools;
mod validation;
mod webfetch;

pub use response::{RpcError, RpcResponse};

use crate::tool_registry::{ToolDef, ToolRegistry};

/// Constructs the tool registry with all available MCP tools.
///
/// This function creates and populates a [`ToolRegistry`] with all tools
/// exposed by the server. Each tool is registered with its name, aliases,
/// description, and JSON schema for input validation.
///
/// # Tool Categories
///
/// The registered tools fall into several categories:
///
/// ## System Tools
/// - `ping` - Health check (returns "pong")
///
/// ## Web Tools
/// - `WebFetch` - Fetch and process web content with token-aware chunking
///
/// ## Code Analysis Tools
/// - `Search` - File content search using ugrep
/// - `CodeQuery` - Semantic code search via OpenAI vector stores
/// - `Outline` - Extract structural outline from C++ source files
///
/// ## File Operations
/// - `Read` - Read file contents with optional line range
/// - `Edit` - Apply surgical text replacements
/// - `Write` - Create or overwrite files
/// - `Delete` - Remove files
/// - `Glob` - Find files matching glob patterns
///
/// ## Build/Test Tools
/// - `Build` - Run build scripts (build.sh/build.ps1)
/// - `Test` - Run test scripts (test.sh/test.ps1)
/// - `Pwsh` - Execute PowerShell commands
///
/// ## Git Tools
/// - `GitStatus` - Show working tree status
/// - `GitDiff` - Show file changes
/// - `GitRestore` - Discard uncommitted changes
/// - `GitAdd` - Stage files for commit
/// - `GitCommit` - Create conventional commits
///
/// # Returns
///
/// A populated [`ToolRegistry`] ready for tool dispatch.
fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // System tools
    registry.register::<tools::PingTool>();

    // Web tools
    registry.register::<tools::WebFetchTool>();

    // Code analysis tools
    registry.register::<tools::SearchTool>();
    registry.register::<tools::CodeQueryTool>();

    // File operations
    registry.register::<tools::ReadTool>();
    registry.register::<tools::EditTool>();
    registry.register::<tools::WriteTool>();
    registry.register::<tools::DeleteTool>();
    registry.register::<tools::GlobTool>();

    // Build/test tools
    registry.register::<tools::BuildTool>();
    registry.register::<tools::TestTool>();
    registry.register::<tools::OutlineTool>();
    registry.register::<tools::PwshTool>();

    // Git tools
    registry.register::<tools::GitStatusTool>();
    registry.register::<tools::GitDiffTool>();
    registry.register::<tools::GitRestoreTool>();
    registry.register::<tools::GitAddTool>();
    registry.register::<tools::GitCommitTool>();

    registry
}

/// Incoming JSON-RPC 2.0 request from an MCP client.
///
/// This struct deserializes the standard JSON-RPC request format:
///
/// ```json
/// {
///   "jsonrpc": "2.0",
///   "id": 1,
///   "method": "mcp/tools/call",
///   "params": { "name": "ping", "arguments": {} }
/// }
/// ```
///
/// # Notifications vs Requests
///
/// Per JSON-RPC spec, messages without an `id` field are notifications and
/// do not receive responses. The server handles `notifications/initialized`
/// silently without replying.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    /// JSON-RPC version (must be "2.0", but not validated).
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,

    /// Request identifier. `None` indicates a notification (no response expected).
    id: Option<Value>,

    /// Method name (e.g., "mcp/initialize", "mcp/tools/call").
    method: String,

    /// Method parameters. Defaults to empty object if omitted.
    #[serde(default)]
    params: Value,
}

/// Response payload for the `mcp/initialize` method.
///
/// This struct is returned when a client initiates the MCP handshake.
/// It advertises the server's protocol version, identity, and capabilities.
///
/// # MCP Initialization Flow
///
/// 1. Client sends `mcp/initialize` with its capabilities
/// 2. Server responds with `InitializeResult` (this struct)
/// 3. Client sends `notifications/initialized` to confirm
/// 4. Normal tool operations can now proceed
#[derive(Serialize)]
struct InitializeResult {
    /// MCP protocol version supported by this server.
    /// Format: "YYYY-MM-DD" (e.g., "2025-03-26").
    #[serde(rename = "protocolVersion")]
    protocol_version: String,

    /// Server identification (name and version).
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,

    /// Server capabilities (what operations are supported).
    capabilities: Capabilities,

    /// List of all available tools with their schemas.
    /// Included here for clients that don't call `tools/list` separately.
    tools: Vec<ToolDef>,
}

/// Server capability advertisement.
///
/// Indicates which MCP features this server supports.
#[derive(Serialize, Default)]
struct Capabilities {
    /// Tool-related capabilities.
    tools: ServerCapabilitiesTools,
}

/// Tool-specific capability flags.
#[derive(Serialize, Default)]
struct ServerCapabilitiesTools {
    /// Whether the server supports `tools/list` to enumerate tools.
    list: bool,
    /// Whether the server supports `tools/call` to execute tools.
    call: bool,
}

/// Server identification information.
#[derive(Serialize)]
struct ServerInfo {
    /// Human-readable server name.
    name: String,
    /// Server version (from `APP_VERSION` env var or default).
    version: String,
}

/// MCP server entry point.
///
/// This is the main event loop that:
/// 1. Initializes logging (to stderr, respecting `RUST_LOG`)
/// 2. Builds the tool registry with all available tools
/// 3. Reads JSON-RPC messages from stdin
/// 4. Dispatches requests to the appropriate handler
/// 5. Writes responses to stdout
/// 6. Exits on shutdown request or EOF
///
/// # Message Routing
///
/// The server routes messages based on the `method` field:
///
/// | Method Pattern           | Handler                          |
/// |--------------------------|----------------------------------|
/// | `*initialize`            | Return capabilities and tools    |
/// | `*tools/list`            | Return available tools           |
/// | `*tools/call`            | Dispatch to tool registry        |
/// | `*shutdown`              | Acknowledge and exit             |
/// | `notifications/*`        | Acknowledge silently (no reply)  |
/// | Unknown                  | Return method-not-found error    |
///
/// # Tool Call Parameter Formats
///
/// For flexibility, `tools/call` accepts multiple parameter shapes:
///
/// ```json
/// // Standard MCP format
/// {"name": "ping", "arguments": {}}
///
/// // Alternative naming
/// {"toolName": "ping", "args": {}}
///
/// // Nested format
/// {"call": {"name": "ping", "arguments": {}}}
/// ```
///
/// # Error Handling
///
/// - I/O errors during read: logged, loop continues (tolerant of transient errors)
/// - I/O errors during write: logged, loop exits (client likely disconnected)
/// - Invalid JSON: logged, message skipped (no response)
/// - Unknown method: protocol error response (-32601)
/// - Tool not found: protocol error response (-32601)
/// - Tool execution error: tool error response (isError: true)
///
/// # Shutdown
///
/// The server exits gracefully when:
/// - Client sends `mcp/shutdown` (or alias)
/// - Stdin reaches EOF
/// - Fatal I/O error occurs during write
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging to stderr (stdout is reserved for MCP protocol)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Set up I/O channels
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout;
    let reader = BufReader::new(stdin);
    let mut reader = reader;

    // Build tool registry at startup (tools are immutable during runtime)
    let registry = build_tool_registry();
    let tools = registry.list();

    // Main message loop
    while let Some(message) = match read_mcp_message(&mut reader).await {
        Ok(v) => v,
        Err(e) => {
            error!("failed to read MCP message: {}", e);
            None
        }
    } {
        let line = message.body;
        // Skip empty lines (defensive)
        if line.trim().is_empty() {
            continue;
        }

        // Parse JSON-RPC request
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

        // Handle notifications (no id = no response per JSON-RPC spec)
        if req.id.is_none() {
            match req.method.as_str() {
                "notifications/initialized" | "initialized" => {
                    info!("Received initialized notification");
                    continue;
                }
                _ => {
                    error!("Unknown notification: {}", req.method);
                    continue;
                }
            }
        }

        // Route request to appropriate handler
        let (resp, should_exit): (RpcResponse, bool) = match req.method.as_str() {
            // Health check (JSON-RPC method, not a tool)
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

            // MCP initialization handshake
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

            // Tool enumeration
            "mcp/tools/list" | "tools/list" | "server/tools/list" | "mcp/capabilities"
            | "capabilities" => (
                RpcResponse::ok(req.id, serde_json::json!({"tools": tools})),
                false,
            ),

            // Tool execution (main workhorse)
            "mcp/tools/call" | "tools/call" | "server/tools/call" => {
                // Extract tool name from various param shapes for client compatibility
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

                // Extract arguments from various param shapes
                let args = params
                    .get("arguments")
                    .cloned()
                    .or_else(|| params.get("args").cloned())
                    .or_else(|| params.get("call").and_then(|c| c.get("arguments")).cloned())
                    .unwrap_or(Value::Object(Default::default()));

                // Dispatch to tool registry
                let resp = if let Some(result) = registry.call(name, req.id.clone(), args).await {
                    result
                } else {
                    RpcResponse::protocol_error(req.id, -32601, format!("Unknown tool: {}", name))
                };
                (resp, false)
            }

            // Graceful shutdown
            "mcp/shutdown" | "shutdown" | "server/shutdown" => (
                RpcResponse::ok(req.id, serde_json::json!({"ok": true})),
                true,
            ),

            // Unknown method
            _ => {
                error!("Unknown method: {}", req.method);
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

        // Send response
        info!("Sending response for request id: {:?}", resp.id);
        let skip_headers = if message.has_headers {
            false
        } else {
            should_skip_headers()
        };
        if let Err(e) = write_mcp_response_with_mode(&mut writer, &resp, skip_headers).await {
            error!("failed to write MCP response: {}", e);
            break;
        }
        info!("Response sent successfully");

        // Exit if shutdown was requested
        if should_exit {
            info!("shutdown requested");
            break;
        }
    }

    Ok(())
}
