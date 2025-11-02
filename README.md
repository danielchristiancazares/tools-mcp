# tools-mcp

Rust-based Model Context Protocol (MCP) server that wraps OpenAI's vector store APIs and exposes a small toolbox for code agents. It lets agents sync local files, run semantic search over vector stores, and fetch external web content in a token-aware way.

## Highlights
- **CodeQuery**: One-shot helper that optionally reindexes local files (hash-based diffing with retries) and runs a semantic search query against an OpenAI vector store.
- **WebFetch**: HTTP fetcher with caching, robots.txt compliance, SSRF hardening, and Markdown chunking sized for GPT-4 class models.
- **Ping**: Lightweight health check for MCP clients.
- JSON-RPC 2.0 transport over stdin/stdout with optional `Content-Length` framing for Codex-compatible MCP clients.

## Requirements
- Rust toolchain (edition 2021; tested with latest stable).
- OpenAI API key with access to the Assistants / Vector Store APIs.

## Getting Started
```bash
# Clone and enter the repo
git clone https://github.com/your-org/tools-mcp.git
cd tools-mcp

# Build the binary
cargo build --release

# Run the MCP server (OPENAI_API_KEY must be set)
export OPENAI_API_KEY="sk-..."
cargo run --release
```

When running under an MCP client, the server reads JSON-RPC messages from stdin and writes responses to stdout. Set `MCP_SKIP_HEADERS=true` to omit `Content-Length` headers if your client expects raw JSON lines.

## Environment Variables
- `OPENAI_API_KEY` *(required)*: Used for all OpenAI REST calls.
- `MCP_SKIP_HEADERS` *(optional)*: When `true`, the server writes newline-delimited JSON without HTTP-style headers.
- `RUST_LOG` *(optional)*: Controls logging (`info` by default). Example: `RUST_LOG=debug`.
- `APP_VERSION` *(optional)*: If provided during build (e.g., `APP_VERSION=1.0.0 cargo build --release`), the value is surfaced in initialization responses.

## MCP Tools
- `CodeQuery`
  - **Required**: `query`, and either `vector_store_id` or `vector_store_name`.
  - **Optional**: `file_paths`, `concurrent_limit` (1-20, default 5), `timeout_ms` (>=1000, default 60000), `model`, `max_num_results`, `include_results`.
  - Performs hash-based reindexing of provided files (uploads changed files, deletes removed ones), waits for indexing, and issues a Responses API call with `file_search` tool attachments.
- `WebFetch`
  - **Required**: `url`.
  - **Optional**: `max_chunk_tokens` (default 2000), `no_cache` (skip disk cache).
  - Fetches remote content with SSRF protections, respects robots.txt, and returns token-counted Markdown chunks.
- `ping`
  - Health check returning “pong”.

Older aliases (e.g., `create-store`, `query`) still resolve, but the names above are preferred.

## MCP Client Configuration Example
```toml
[mcp_servers.vector-store]
command = "/path/to/tools-mcp/target/release/tools-mcp"
env = { OPENAI_API_KEY = "${OPENAI_API_KEY}", MCP_SKIP_HEADERS = "true", RUST_LOG = "error" }
```

## Development Notes
- The core OpenAI integration lives in `src/lib.rs` (`file_search_core`).
- MCP protocol handling is in `src/main.rs`.
- Web content fetching utilities are in `src/webfetch/`.
- Cached WebFetch responses live under `/tmp/tools-mcp-webfetch/`.

## Testing
```bash
# Unit and integration tests (networked tests are `#[ignore]` by default)
cargo test
```

Integration tests spawn the binary via `cargo run --release` and exercise the MCP protocol, so the release build must succeed locally.


