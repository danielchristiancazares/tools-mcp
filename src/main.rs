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

use anyhow::{Context, Result};
use file_search_core as core;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tracing::{error, info};

/// Cached value of `MCP_SKIP_HEADERS` environment variable.
///
/// This static is initialized lazily on first access via [`should_skip_headers`].
/// Once set, the value is immutable for the lifetime of the process.
static SKIP_HEADERS: OnceLock<bool> = OnceLock::new();

/// Maximum allowed size for inbound MCP messages (10 MiB).
///
/// This limit applies to both Content-Length framed messages and raw JSON lines.
/// It serves as a defense against memory exhaustion attacks where a malicious
/// client could send an arbitrarily large `Content-Length` header.
///
/// # Rationale
///
/// 10 MiB is chosen to accommodate large tool arguments (e.g., file contents)
/// while still providing meaningful protection against resource exhaustion.
const MAX_MCP_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

/// Determines whether to skip Content-Length headers in responses.
///
/// When `MCP_SKIP_HEADERS=true` is set in the environment, the server outputs
/// raw JSON without HTTP-style Content-Length framing. This mode is required
/// for compatibility with certain clients (e.g., Codex) that expect newline-
/// delimited JSON.
///
/// # Caching
///
/// The environment variable is read only once (on first call) and cached
/// for the lifetime of the process. This avoids repeated syscalls and
/// ensures consistent behavior.
///
/// # Returns
///
/// `true` if responses should omit Content-Length headers, `false` otherwise.
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
mod git_utils;
mod process_utils;
mod read_file;
mod ripgrep;
mod script_runner;
mod smart_file_edit;
mod tool_registry;
mod tools;
mod validation;
mod webfetch;

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
/// - `Search` - Ripgrep-based file content search
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
pub(crate) struct RpcResponse<'a> {
    /// JSON-RPC version (always "2.0").
    pub(crate) jsonrpc: &'a str,

    /// Request identifier from the corresponding request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<Value>,

    /// Success payload. Mutually exclusive with `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,

    /// Error payload for protocol-level failures. Mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<RpcError>,
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
pub(crate) struct RpcError {
    /// Numeric error code (see table above).
    pub(crate) code: i64,

    /// Human-readable error description.
    pub(crate) message: String,

    /// Optional additional error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
}

impl RpcResponse<'static> {
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
    pub fn ok(id: Option<Value>, result: Value) -> RpcResponse<'static> {
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
    pub fn err(id: Option<Value>, msg: impl std::fmt::Display) -> RpcResponse<'static> {
        RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(serde_json::json!({"content":[{"type":"text","text": msg.to_string()}], "isError": true})),
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

    /// Creates a success response with pretty-printed JSON as text content.
    ///
    /// Useful for tools that return structured data that should be human-readable
    /// in the response. The JSON is pretty-printed for readability.
    ///
    /// # Arguments
    ///
    /// * `id` - Request identifier to echo back
    /// * `json_value` - The JSON value to serialize and return as text
    /// * `is_error` - Whether this represents an error condition
    ///
    /// # Note
    ///
    /// If serialization fails, a fallback error message is returned instead.
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
    /// * `Err(RpcResponse)` - Pre-built error response ready to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// let args: MyToolArgs = RpcResponse::parse(id.clone(), args)?;
    /// ```
    pub fn parse<T: serde::de::DeserializeOwned>(
        id: Option<Value>,
        args: Value,
    ) -> Result<T, RpcResponse<'static>> {
        serde_json::from_value::<T>(args)
            .map_err(|e| RpcResponse::err(id, format!("invalid arguments: {e}")))
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

/// Reads a single MCP message from the input stream.
///
/// This function implements flexible message parsing that supports two framing modes:
///
/// ## Content-Length Framing (HTTP-style)
///
/// ```text
/// Content-Length: 42\r\n
/// \r\n
/// {"jsonrpc":"2.0","id":1,"method":"ping"}
/// ```
///
/// ## Raw JSON Lines
///
/// ```text
/// {"jsonrpc":"2.0","id":1,"method":"ping"}
/// ```
///
/// The function auto-detects the mode: if a line starts with `{` or `[`, it's treated
/// as raw JSON. Otherwise, it expects HTTP-style headers.
///
/// # Arguments
///
/// * `reader` - An async buffered reader (typically wrapping stdin)
///
/// # Returns
///
/// * `Ok(Some(message))` - Successfully read a complete message
/// * `Ok(None)` - Clean EOF (no more messages)
/// * `Err(...)` - I/O error or protocol violation
///
/// # Errors
///
/// Returns an error if:
/// - A line exceeds [`MAX_MCP_MESSAGE_BYTES`] (DoS protection)
/// - Content-Length header is malformed or exceeds limit
/// - EOF occurs mid-message (incomplete read)
/// - Message body is not valid UTF-8
///
/// # Protocol Details
///
/// - Handles both CRLF and LF line endings
/// - Consumes trailing newlines after the message body
/// - Ignores empty lines before headers (for robustness)
/// - Case-insensitive header name matching
async fn read_mcp_message<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    let mut content_length: Option<usize> = None;

    // Parse headers or detect raw JSON
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;

        // Clean EOF - no more messages
        if bytes_read == 0 {
            if content_length.is_some() {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading headers",
                ));
            }
            return Ok(None);
        }

        // DoS protection: reject oversized lines
        if line.len() > MAX_MCP_MESSAGE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "message line exceeds maximum allowed size",
            ));
        }

        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);

        // Empty line signals end of headers (if we have Content-Length)
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue; // Skip leading blank lines
        }

        // Auto-detect raw JSON mode (line starts with { or [)
        let trimmed_start = trimmed.trim_start();
        if content_length.is_none()
            && (trimmed_start.starts_with('{') || trimmed_start.starts_with('['))
        {
            return Ok(Some(trimmed.to_owned()));
        }

        // Parse Content-Length header (case-insensitive)
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

    // Read the message body using the Content-Length
    let len = content_length
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"))?;

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let message = String::from_utf8(buf)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "message body not valid UTF-8"))?;

    // Consume optional trailing newline after message body
    let trailing = reader.fill_buf().await?;
    if trailing.starts_with(b"\r\n") {
        reader.consume(2);
    } else if trailing.starts_with(b"\n") {
        reader.consume(1);
    }

    Ok(Some(message))
}

/// Writes an MCP response to the output stream.
///
/// Serializes the response to JSON and writes it with optional Content-Length
/// framing based on the `MCP_SKIP_HEADERS` environment variable.
///
/// # Framing Modes
///
/// ## With Headers (default)
///
/// ```text
/// Content-Length: 42\r\n
/// \r\n
/// {"jsonrpc":"2.0","id":1,"result":{...}}\n
/// ```
///
/// ## Without Headers (`MCP_SKIP_HEADERS=true`)
///
/// ```text
/// {"jsonrpc":"2.0","id":1,"result":{...}}\n
/// ```
///
/// # Arguments
///
/// * `writer` - An async writer (typically stdout)
/// * `resp` - The response to serialize and write
///
/// # Returns
///
/// * `Ok(())` - Response written and flushed successfully
/// * `Err(...)` - Serialization or I/O error
///
/// # Important
///
/// This function always flushes the writer after writing. This is critical
/// for clients that read responses synchronously, as they may block waiting
/// for data that's still in the output buffer.
async fn write_mcp_response<W>(writer: &mut W, resp: &RpcResponse<'_>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_vec(resp).context("serialize response")?;

    // Ensure payload ends with newline for clean line-based parsing
    if !payload.ends_with(b"\n") {
        payload.push(b'\n');
    }

    let payload_len = payload.len();

    // Write Content-Length header unless in raw JSON mode
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

    // Flush immediately to ensure client receives the response
    writer.flush().await.context("flush stdout")?;
    Ok(())
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
    while let Some(line) = match read_mcp_message(&mut reader).await {
        Ok(v) => v,
        Err(e) => {
            error!("failed to read MCP message: {}", e);
            None
        }
    } {
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
                RpcResponse::ok(req.id, serde_json::json!({
                    "content": [{"type": "text", "text": "pong"}],
                    "isError": false
                })),
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
            "mcp/shutdown" | "shutdown" | "server/shutdown" => {
                (RpcResponse::ok(req.id, serde_json::json!({"ok": true})), true)
            }

            // Unknown method
            _ => {
                error!("Unknown method: {}", req.method);
                (
                    RpcResponse::protocol_error(req.id, -32601, format!("Method not found: {}", req.method)),
                    false,
                )
            }
        };

        // Send response
        info!("Sending response for request id: {:?}", resp.id);
        if let Err(e) = write_mcp_response(&mut writer, &resp).await {
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


