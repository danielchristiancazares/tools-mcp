# tools-mcp

Rust-based Model Context Protocol (MCP) server that bundles code search, web scraping, and newline-safe file editing. It is designed as a toolbox for LLM agents, speaking JSON-RPC 2.0 over stdin/stdout.

## Highlights
- **CodeQuery** - One-shot helper that optionally auto-discovers and reindexes local files, then runs a semantic search query against an OpenAI vector store.
- **WebFetch** - HTTP + optional headless-browser fetcher with caching, robots.txt enforcement, SSRF hardening, and token-aware Markdown chunking.
- **RipGrep** - Fast local regex search via ripgrep (`rg`) with both line-oriented output and structured match records.
- **ReadFile** - Line-numbered file reader (optionally a line range) for quick inspection.
- **SmartFileEdit** - Canonical LF view + byte-precise patch tool (including unified diffs) that preserves original newline bytes and whitespace when editing files via MCP.
- **Bash** - Run shell commands via bash with timeout and stdout/stderr capture.
- **Ping** - Lightweight health check for MCP clients.
- JSON-RPC 2.0 transport over stdin/stdout with optional `Content-Length` framing for Codex-compatible MCP clients.

## Requirements
- Rust toolchain (edition 2021; tested with latest stable).
- Cargo in PATH (for running the MCP binary).
- `rg` (ripgrep) in PATH (required for `RipGrep`).
- **OpenAI**
  - `OPENAI_API_KEY` with access to the Assistants / Vector Store APIs (required for `CodeQuery` and other OpenAI calls).
- **WebFetch browser support** (optional)
  - Chrome or Chromium installed and discoverable on PATH to enable headless rendering of JavaScript-heavy sites. Without it, WebFetch still works in HTTP-only mode.

## Getting Started
```bash
# Clone and enter the repo
git clone https://github.com/your-org/tools-mcp.git
cd tools-mcp

# Build the binary
cargo build --release

# Run the MCP server (OpenAI only)
export OPENAI_API_KEY="sk-..."
cargo run --release
```

When running under an MCP client, the server reads JSON-RPC messages from stdin and writes responses to stdout. Set `MCP_SKIP_HEADERS=true` to omit `Content-Length` headers if your client expects raw JSON lines.

## Environment Variables
- `OPENAI_API_KEY` *(required for CodeQuery)*  
  Used for all OpenAI REST calls (vector stores, Responses API).

- `MCP_SKIP_HEADERS` *(optional)*  
  When `true`, the server writes newline-delimited JSON without HTTP-style headers.

- `RUST_LOG` *(optional)*  
  Controls logging (`info` by default). Example: `RUST_LOG=debug`.

- `APP_VERSION` *(optional)*  
  If provided during build (e.g., `APP_VERSION=1.0.0 cargo build --release`), the value is surfaced in initialization responses.

## MCP Tools

### CodeQuery

Index code and query an OpenAI vector store in a single call.

- **Tool name**: `CodeQuery`
- **Required**:
  - `query` – natural-language question.
- **Vector store selection**:
  - `vector_store_id` – use an existing store by ID, or
  - `vector_store_name` – use or create a store with this name, or
  - omit both – default to the current directory name as `vector_store_name`. If the name cannot be inferred, the call returns an error.
- **Optional**:
  - `file_paths` (string[]) – explicit local file paths to sync before querying. If omitted, CodeQuery auto-discovers indexable files under the current directory (skipping `target/`, `node_modules/`, VCS dirs, etc.).
  - `concurrent_limit` (integer, 1–20, default 5) – maximum concurrent upload/delete operations.
  - `timeout_ms` (integer, ≥1000, default 60000) – overall indexing + query timeout in milliseconds.
  - `model` (string) – override the default OpenAI model (defaults to `gpt-4o`).
  - `max_num_results` (integer) – limit the number of vector search matches.
  - `include_results` (boolean, default `false`) – when true, includes top search matches in the response text.
- **Behavior**:
  - Resolves or creates the target vector store.
  - Hashes local files, uploads new/changed ones, and deletes removed ones (change-based reindexing).
  - Waits for indexing, then calls the Responses API with a `file_search` tool to answer the query.
- **Response**:
  - `result` contains a `content` array:
    - First item: natural-language answer text.
    - Optional second item: pretty-printed JSON summary of the reindexing operations.

### WebFetch

Fetch and normalize external web content with caching and JS-aware rendering.

- **Tool name**: `WebFetch`
- **Required**:
  - `url` – absolute URL to fetch.
- **Optional**:
  - `max_chunk_tokens` (integer, ≥200) – approximate token budget per chunk. If omitted, a default budget is used.
  - `no_cache` (boolean) – when true, bypasses the on-disk cache and forces a fresh fetch.
  - `force_browser` (boolean) – when true, forces headless browser rendering even if heuristics do not flag the page as JS-heavy.
- **Behavior**:
  - Builds a hardened HTTP client, validates the URL against SSRF rules, and enforces `robots.txt`.
  - Caches responses under `/tmp/tools-mcp-webfetch` keyed by URL + method.
  - Extracts readable content and produces Markdown.
  - Uses heuristics to detect JavaScript-heavy pages and, where possible, re-renders them via a headless Chrome/Chromium browser.
  - Splits content into token-aware chunks with headings.
- **Response shape** (`FetchResponse`):
  - `url` – final URL.
  - `fetched_at` – ISO-8601 timestamp.
  - `title` – optional document title.
  - `language` – optional language code.
  - `chunks` – array of `{ heading: Option<String>, text: String, token_count: usize }`.
  - `rendering_method` – `"http"` or `"browser"`.
  - `note` – optional string such as `"cache_hit"`, `"rendered_with_browser"`, or a combination.

### RipGrep

Fast local regex search using ripgrep (`rg`).

- **Tool name**: `RipGrep` (aliases accepted: `ripgrep`, `rg`)
- **Required**:
  - `pattern` - ripgrep pattern (regex by default).
- **Optional**:
  - `path` - file or directory root (default: current working directory).
  - `case` - `"smart"` (default), `"sensitive"`, or `"insensitive"`.
  - `fixed_strings`, `word_regexp`, `glob`, `hidden`, `follow`, `no_ignore`, `context`, `max_results`, `timeout_ms`.
- **Notes**:
  - Requires `rg` to be installed and discoverable on PATH on the machine running the MCP server.
  - Uses `rg --json` and returns both a readable `content[0].text` and structured `matches`.

### ReadFile

Read a local file (optionally a line range) for quick inspection without uploads.

- **Tool name**: `ReadFile`
- **Required**:
  - `path` - filesystem path to read.
- **Optional**:
  - `start_line`, `end_line` (1-based, inclusive).
- **Response**:
  - `content[0].text` is line-numbered (similar to `nl -ba` / `cat -n`).
  - Includes `start_line`, `end_line`, and `total_lines`.

### SmartFileEdit

Edit files while preserving original newline bytes and whitespace.

- **Tool name**: `SmartFileEdit` (alias accepted: `smart_file_edit`)
- **Required base fields**:
  - `action` - one of `"get_region"`, `"apply_snippet_edit"`, `"apply_unified_diff"`.
  - `path` - filesystem path to inspect or edit.

#### Actions

- `get_region`
  - **Optional**: `start_line`, `end_line` (1-based) to select a range; defaults to the whole file.
  - **Returns**:
    - `plain_text` – raw LF-normalized text for the selected region.
    - `canonical_text` – numbered lines (LF).
    - `file_hash` – `sha256:...` hash of the file.
    - `region_id` – opaque region identifier.
    - `canonical_range` and `byte_range` for the region.
    - `newline_style` and `file_size_bytes`.

- `apply_snippet_edit`
  - **Required**:
    - `old_snippet` – canonical LF snippet to replace.
    - `new_snippet` – canonical LF replacement.
  - **Recommended**:
    - `file_hash` – from the last `get_region`/edit; used to detect stale files.
  - **Optional**:
    - `match_hint` – `{ start_line, end_line }` to restrict the search range.
    - `region_id` – any caller-chosen identifier to help correlate edits.
  - **Behavior**:
    - Searches the canonical LF view for `old_snippet` (within `match_hint` if provided).
    - Rewrites the corresponding byte range using the file’s dominant newline style, preserving other bytes.
    - Returns:
      - `status`: `"ok"`, `"no_match"`, or `"stale_file"`.
      - `replaced_byte_range`, `lines`, `bytes_written`, `file_hash_before`, `file_hash_after`, `newline_kind`.
      - When `status = "no_match"`, includes suggested candidate ranges.

- `apply_unified_diff`
  - **Required**:
    - `diff` – unified diff hunks for a single file (e.g., output similar to `git diff` but without needing `---/+++` headers).
  - **Optional**:
    - `file_hash` – expected hash before applying the first hunk.
  - **Behavior**:
    - Parses each `@@ -old_start,old_len +new_start,new_len @@` hunk.
    - For each hunk, builds `old_snippet` / `new_snippet` and applies it via `apply_snippet_edit` using line-based hints.
    - Requires at least one context or removal line per hunk (pure additions with zero context are not supported yet).
  - **Response**:
    - On success:
      - `status: "ok"`, `hunks_applied`, `file_hash_before`, `file_hash_after`, and per-hunk results.
    - On failure:
      - `status: "no_match"` or `"stale_file"`, plus `failed_hunk` and detailed payload from the underlying snippet edit.

### Bash

Run shell commands via bash with timeout and stdout/stderr capture.

- **Tool name**: `Bash` (alias accepted: `bash`)
- **Required**:
  - `command` - shell command to run (executed as: `bash -lc "<command>"`).
- **Optional**:
  - `timeout_ms` - timeout in milliseconds (default 30000).
  - `working_dir` - optional working directory for the command.
- **Response**:
  - Returns a pretty-printed JSON summary (including `stdout`, `stderr`, `exit_code`) in `content[0].text`.

### ping

Simple health check for MCP clients.

- **Tool name**: `ping`
- **Behavior**:
  - Always returns `pong` in a JSON `content` array.
- Useful for MCP client connectivity tests or keepalive pings.

## MCP Client Configuration Example
```toml
[mcp_servers.tools-mcp]
command = "/path/to/tools-mcp/target/release/tools-mcp"
env = { 
  OPENAI_API_KEY = "${OPENAI_API_KEY}",
  MCP_SKIP_HEADERS = "true",
  RUST_LOG = "error"
}
```

## Development Notes
- The core OpenAI integration lives in `src/lib.rs` (`file_search_core`).
- MCP protocol handling and tool wiring live in `src/main.rs`.
- Code search orchestration is in `src/codequery/`.
- Web content fetching, parsing, and heuristics live in `src/webfetch/` (`browser`, `http`, `cache`, `chunker`, etc.).
- Newline-aware file editing (including unified diff support) is implemented in `src/smart_file_edit/mod.rs`.
- RipGrep tool implementation lives in `src/ripgrep.rs`.
- Raw file reader lives in `src/read_file.rs`.
- Cached WebFetch responses live under `/tmp/tools-mcp-webfetch/`.

## Testing
```bash
# Unit and integration tests (some networked tests may be `#[ignore]` by default)
cargo test
```

Integration tests may spawn the binary via `cargo run --release` and exercise the MCP protocol, so the release build must succeed locally.
