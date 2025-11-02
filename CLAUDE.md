# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a **Rust-based MCP (Model Context Protocol) server** that provides file search functionality using OpenAI's vector stores API and a token-aware web content fetcher for Codex-style agents.

## Architecture

### Core Components

1. **MCP Server (`src/main.rs`)**
   - Implements JSON-RPC protocol over stdin/stdout
   - Handles MCP initialization, tool listing, and tool execution
   - Provides 6 main tools: ping, query, upload_file, create-store, get-store-by-name, list-stores
   - Accepts multiple protocol aliases for compatibility (e.g., `mcp/initialize`, `initialize`, `server/initialize`)

2. **OpenAI API Client (`src/lib.rs`)**
   - Wraps OpenAI's vector store and file APIs
   - Handles file uploads with automatic extension validation
   - Manages vector store creation and file association
   - Implements polling for file indexing completion
   - Core functions: `upload_file`, `create_vector_store`, `responses_with_file_search`

3. **WebFetch Pipeline (`src/webfetch/`)**
   - Retrieves remote pages with caching under `/tmp/tools-mcp-webfetch`
   - **Respects robots.txt** - Fetches and caches robots.txt per domain, blocks disallowed URLs [robotstxt v0.3.0]
   - **SSRF protection** - Validates URLs, blocks file://, localhost, private IPs, and non-HTTP(S) schemes
   - User agent: `tools-mcp-webfetch/0.1`
   - Extracts `<body>` content only, converts to Markdown with `htmd` [htmd v0.3.2, https://docs.rs/htmd/]
   - Filters nav/footer/header/script/style tags to reduce duplication and noise
   - Produces inline links `[text](url)` for cleaner, more token-efficient output
   - Chunks text using OpenAI's `cl100k_base` tokenizer for GPT-4 budgets [tiktoken-rs v0.7.0, https://docs.rs/tiktoken-rs/0.7.0/tiktoken_rs/]
   - Produces summaries plus per-section token counts for Codex

4. **Build Script (`src/build.rs`)**
   - Sets `APP_VERSION` environment variable during compilation if provided

### Protocol Flow

1. Client sends JSON-RPC requests via stdin
2. Server parses MCP messages with Content-Length headers
3. Server routes requests to appropriate handlers
4. Responses are sent back via stdout with proper headers

## Development Commands

### Build and Run

```bash
# Build the project
cargo build --release

# Run the MCP server (requires OPENAI_API_KEY environment variable)
export OPENAI_API_KEY="your-api-key"
cargo run

# Run with custom version
APP_VERSION="1.0.0" cargo build --release
```

### Testing MCP Protocol

```bash
# Send initialization request
echo '{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}' | cargo run

# List available tools
echo '{"jsonrpc":"2.0","id":2,"method":"mcp/tools/list","params":{}}' | cargo run

# Call ping tool
echo '{"jsonrpc":"2.0","id":3,"method":"mcp/tools/call","params":{"name":"ping","arguments":{}}}' | cargo run
```

## Configuration

### Codex Configuration

Add to your `~/.codex/config.toml`:

```toml
[mcp_servers.vector-store]
command = "/path/to/tools-mcp/target/release/tools-mcp"
env = { OPENAI_API_KEY = "${OPENAI_API_KEY}", MCP_SKIP_HEADERS = "true", RUST_LOG = "error" }
```

The `env` field allows direct configuration of environment variables without needing a wrapper script.

## MCP Tools

### User-Facing Tools

1. **CodeQuery** - High-level codebase search (primary tool)
   - Index codebase files and run semantic search in one operation
   - Automatically syncs changed files and queries the vector store
   - Required: `query`
   - Required (one of): `vector_store_id` OR `vector_store_name`
   - Optional: `file_paths` (array - files to sync), `concurrent_limit` (1-20, default: 5), `timeout_ms` (default: 60000), `model`, `max_num_results`, `include_results`
   - Handles store creation, file syncing (hash-based deletion of removed files), and querying automatically

2. **WebFetch** - Fetch and process web content
   - Required: `url`
   - Optional: `max_chunk_tokens` (default: 2000), `no_cache` (default: false)
   - Returns JSON with `chunks[]`, `title`, `language`, `fetched_at`, cache metadata
   - Respects robots.txt and includes SSRF protection
   - Produces inline markdown links for clean output

3. **ping** - Health check
   - Returns "pong"
   - No arguments required

### Internal Library Functions

These are used by CodeQuery internally but not exposed as MCP tools:
- `create_vector_store`, `list_vector_stores`, `get_store_by_name` - Store management
- `upload_file`, `upload_files_batch` - File uploads
- `reindex_files`, `reindex_with_retry` - Hash-based file syncing with deletion
- `responses_with_file_search` - Query execution
- `list_vector_store_files`, `delete_vector_store_file` - File management

### Tool Response Format

All tool responses follow the MCP content format:
```json
{
  "content": [
    {
      "type": "text" | "json",
      "text": "response text" | "json": {...}
    }
  ],
  "isError": false | true
}
```

## Key Implementation Details

### Message Reading (`read_mcp_message`)
- Handles both Content-Length header format and raw JSON
- Supports different line endings (CRLF and LF)
- Properly consumes trailing newlines after message body

### File Upload Handling
- Automatically appends `.txt` extension for unsupported file types
- Supports both local files and URLs
- Allowed extensions: c, cpp, css, csv, doc, docx, gif, go, html, java, jpeg, jpg, js, json, md, pdf, php, pkl, png, pptx, py, rb, tar, tex, ts, txt, webp, xlsx, xml, zip

### Vector Store File Management
- Implements overwrite functionality by checking existing filenames
- Waits for file indexing with configurable polling interval and timeout
- Deletes duplicate files when overwrite is enabled

### Error Handling
- Returns MCP-compatible error responses with appropriate codes
- Logs errors to stderr using tracing
- HTTP error responses include status code and body for debugging

## Environment Variables

- **OPENAI_API_KEY** (required): OpenAI API key for vector store operations
- **APP_VERSION** (optional): Version string exposed in server info
- **RUST_LOG** (optional): Logging level configuration (default: "info")

## Dependencies

- **anyhow**: Error handling
- **reqwest**: HTTP client for OpenAI API
- **readability**: Boilerplate removal for HTML extraction [readability v0.3.0, https://docs.rs/readability/0.3.0/readability/]
- **htmd**: HTML to Markdown conversion with inline links and tag filtering [htmd v0.3.2, https://docs.rs/htmd/]
- **scraper**: DOM parsing helpers for metadata [scraper v0.24.0, https://docs.rs/scraper/0.24.0/scraper/]
- **robotstxt**: robots.txt parsing and compliance checking [robotstxt v0.3.0]
- **tiktoken-rs**: Token counting aligned with OpenAI models [tiktoken-rs v0.7.0, https://docs.rs/tiktoken-rs/0.7.0/tiktoken_rs/]
- **serde/serde_json**: JSON serialization
- **tokio**: Async runtime
- **tracing/tracing-subscriber**: Logging

## Common Development Tasks

### Adding a New Tool

1. Add tool definition to `tools` vector in `main()` at src/main.rs:165
2. Add handler case in the match statement at src/main.rs:306
3. Implement handler function following the pattern of existing handlers
4. Ensure response uses proper MCP content format

### Debugging Protocol Issues

- Enable trace logging: `RUST_LOG=trace cargo run`
- Check stderr for detailed message parsing logs
- Verify Content-Length headers match payload size
- Ensure JSON-RPC id field is preserved in responses

### Testing Vector Store Operations

```rust
// Example test for upload and query flow
#[tokio::test]
async fn test_file_search_flow() {
    let client = reqwest::Client::new();
    let cfg = ApiConfig::new(env::var("OPENAI_API_KEY").unwrap(), "gpt-4");

    // Upload file and create vector store
    let result = file_search_run(
        &client,
        &cfg,
        "test.txt",
        "What does this file contain?",
        None,
        Some(5),
        true
    ).await.unwrap();

    // Verify response structure
    assert!(result["file_id"].is_string());
    assert!(result["vector_store_id"].is_string());
}
```
