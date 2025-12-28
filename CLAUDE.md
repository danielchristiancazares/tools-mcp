# === USER INSTRUCTIONS ===
# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Tool Preferences

When tools-mcp is available, prefer its dedicated tools over Bash equivalents:

- Prefer `Read` over `cat`, `head`, `tail` for reading files
- Prefer `Search` over `grep`, `rg` for searching file contents
- Prefer `Glob` over `find`, `ls` for finding files by pattern
- Prefer `Edit` over `sed`, `awk` for modifying files
- Prefer `Write` over `echo >` or heredocs for creating new files
- Prefer `WebFetch` over `curl` for fetching URLs
- Prefer `GitStatus`, `GitDiff`, `GitRestore`, `GitAdd`, `GitCommit` over raw `git` commands
- Use `CodeQuery` for semantic/architectural questions ("how does X work?")
- Use `Search` for exact pattern matching ("find all calls to foo()")
- Avoid `Bash` when a dedicated tool exists for the operation

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
   - Retrieves remote pages with HTTP or headless browser rendering
   - Caches under `/tmp/tools-mcp-webfetch` (separate cache keys for http vs browser)
   - **Respects robots.txt** - Fetches and caches robots.txt per domain, blocks disallowed URLs [robotstxt v0.3.0]
   - **SSRF protection** - Validates URLs, blocks file://, localhost, private IPs, and non-HTTP(S) schemes (applied to both HTTP and browser paths)
   - User agent: `tools-mcp-webfetch/0.1`
   - **Hybrid rendering strategy**:
     - HTTP-first with automatic browser fallback for JS-heavy sites
     - Whitelisted domains (e.g., medium.com, notion.so) automatically use browser
     - Heuristics detect client-side rendering (React, Vue, Angular patterns)
     - Explicit `force_browser` parameter available
   - **Headless browser** (optional, requires Chrome/Chromium):
     - Uses chromiumoxide v0.7 with Chrome DevTools Protocol [chromiumoxide, https://docs.rs/chromiumoxide/]
     - Stealth configuration to avoid detection
     - Browser pool with automatic restart (every 100 requests or 1 hour)
     - 15s navigation timeout, 2s network idle wait
     - Resource blocking for performance: images, web fonts, video/audio autoplay
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
   - Optional: `max_chunk_tokens` (default: 2000), `no_cache` (default: false), `force_browser` (default: false)
   - Returns JSON with `chunks[]`, `title`, `language`, `fetched_at`, `rendering_method` ("http" or "browser"), cache metadata, `note`
   - Respects robots.txt and includes SSRF protection
   - Produces inline markdown links for clean output
   - **Rendering behavior**:
     - `force_browser=true`: Always use headless browser (requires Chrome/Chromium)
     - `force_browser=false` (default): HTTP-first, automatic browser fallback for whitelisted domains or JS-heavy heuristics
     - If Chrome not installed: Falls back to HTTP-only mode with warning

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

## System Requirements

### Required
- **Rust toolchain** (edition 2021+)
- **OPENAI_API_KEY** environment variable

### Optional (for WebFetch browser rendering)
- **Chrome or Chromium** browser installed
  - Common paths: `/usr/bin/google-chrome`, `/usr/bin/chromium`, `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
  - If not found: WebFetch operates in HTTP-only mode with warning log
  - Install on Ubuntu/Debian: `sudo apt install chromium-browser`
  - Install on macOS: `brew install --cask google-chrome`

## Environment Variables

- **OPENAI_API_KEY** (required): OpenAI API key for vector store operations
- **APP_VERSION** (optional): Version string exposed in server info
- **RUST_LOG** (optional): Logging level configuration (default: "info")

## Dependencies

- **anyhow**: Error handling
- **reqwest**: HTTP client for OpenAI API
- **chromiumoxide**: Headless Chrome automation via Chrome DevTools Protocol [chromiumoxide v0.7, https://docs.rs/chromiumoxide/]
- **readability**: Boilerplate removal for HTML extraction [readability v0.3.0, https://docs.rs/readability/0.3.0/readability/]
- **htmd**: HTML to Markdown conversion with inline links and tag filtering [htmd v0.3.2, https://docs.rs/htmd/]
- **scraper**: DOM parsing helpers for metadata [scraper v0.24.0, https://docs.rs/scraper/0.24.0/scraper/]
- **robotstxt**: robots.txt parsing and compliance checking [robotstxt v0.3.0]
- **tiktoken-rs**: Token counting aligned with OpenAI models [tiktoken-rs v0.7.0, https://docs.rs/tiktoken-rs/0.7.0/tiktoken_rs/]
- **serde/serde_json**: JSON serialization
- **tokio**: Async runtime
- **tracing/tracing-subscriber**: Logging

## Security Considerations

### Prompt Injection from Web Content
WebFetch extracts content from untrusted external websites that may contain adversarial text designed to manipulate LLM behavior (e.g., "Ignore previous instructions..."). Key mitigations:

1. **Content is clearly marked as external**: The `rendering_method` field distinguishes between "http" and "browser" sources
2. **Consuming agents must contextualize**: LLM systems should treat fetched content as untrusted user input, not system instructions
3. **No automatic command execution**: WebFetch returns structured data; it never executes commands based on scraped content
4. **SSRF protection**: URL validation prevents fetching from private networks, even if instructed by web content

**Best practice**: Consuming systems should frame WebFetch responses as "external document content" in their prompts to maintain instruction/data separation.

### SSRF Protection
- Both HTTP and browser rendering paths validate URLs before fetch
- DNS resolution checks prevent hostname-based bypasses (e.g., `malicious.com` → `127.0.0.1`)
- Blocks: `file://`, `localhost`, private IPs (10.x, 172.16-31.x, 192.168.x), reserved IPs
- robots.txt compliance prevents unauthorized crawling

### Browser Security
- Chrome runs with `--no-sandbox` flag (required for containerized environments)
- Browser process lifecycle managed to prevent resource leaks
- Network idle timeout prevents infinite loops (max 20s wait)
- Resource blocking reduces attack surface: images, web fonts, video/audio autoplay

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
# === END USER INSTRUCTIONS ===


# main-overview

## Development Guidelines

- Only modify code directly relevant to the specific request. Avoid changing unrelated functionality.
- Never replace code with placeholders like `# ... rest of the processing ...`. Always include complete code.
- Break problems into smaller steps. Think through each step separately before implementing.
- Always provide a complete PLAN with REASONING based on evidence from code and logs before making changes.
- Explain your OBSERVATIONS clearly, then provide REASONING to identify the exact issue. Add console logs when needed to gather more information.

## Primary Components

1. **MCP Server** (`src/main.rs`, `src/tool_registry.rs`)
   - JSON-RPC protocol over stdin/stdout
   - Tool registration and dispatch system
   - Protocol alias handling for compatibility

2. **OpenAI Vector Store Client** (`src/lib.rs`)
   - File upload with automatic extension handling
   - Vector store creation and management
   - Semantic search via Responses API
   - Hash-based reindexing with change detection

3. **CodeQuery** (`src/codequery/`)
   - Semantic code search orchestration
   - Automatic file discovery respecting .gitignore
   - Vector store caching and resolution
   - Incremental update processing

4. **WebFetch** (`src/webfetch/`)
   - HTTP-first with automatic browser fallback
   - SSRF protection and robots.txt compliance
   - HTML to Markdown conversion
   - Token-aware chunking for LLM consumption

5. **Smart File Edit** (`src/smart_file_edit/`)
   - Line ending preservation (LF/CRLF/CR)
   - Canonical LF processing with original format retention
   - Snippet replacement and unified diff support

6. **Git Tools** (`src/git_tools.rs`)
   - GitStatus, GitDiff, GitRestore, GitAdd, GitCommit
   - Porcelain output parsing
   - Timeout and output limit enforcement

7. **Process Utilities** (`src/process_utils.rs`)
   - Shell script execution with timeouts
   - PowerShell command support
   - ANSI code stripping
