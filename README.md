# tools-mcp Workspace Technical Documentation

## Table of Contents

1. [Project Overview](#project-overview)
2. [Quick Start](#quick-start)
3. [Tool Selection Guide for LLM Agents](#tool-selection-guide-for-llm-agents)
4. [Architecture](#architecture)
5. [Core Modules](#core-modules)
6. [MCP Tools](#mcp-tools)
7. [MCP Protocol Implementation](#mcp-protocol-implementation)
8. [Data Structures](#data-structures)
9. [Configuration](#configuration)
10. [Security Considerations](#security-considerations)
11. [Dependencies](#dependencies)
12. [Error Handling](#error-handling)
13. [Testing](#testing)
14. [Build and Release](#build-and-release)

---

## Project Overview

**tools-mcp** is a Rust Cargo workspace for a Model Context Protocol (MCP) server and its supporting feature crates. The server communicates via JSON-RPC 2.0 over stdin/stdout, enabling seamless integration with MCP-compatible clients such as Codex agents.

### Key Capabilities

- **Semantic Code Search**: Index and query codebases using OpenAI's vector stores API
- **Web Content Fetching**: Retrieve and process web pages with SSRF protection and robots.txt compliance
- **File Operations**: Read, write, edit, and delete files with newline-aware processing
- **Git Integration**: Execute git commands (status, diff, restore, add, commit)
- **Code Search**: Regex and fuzzy file search using ugrep
- **Code Structure Extraction**: Extract C++ class/method signatures using tree-sitter

### Highlights

- **CodeQuery** - One-shot helper that optionally auto-discovers and reindexes local files, then runs a semantic search query against an OpenAI vector store.
- **WebFetch** - HTTP + optional headless-browser fetcher with caching, robots.txt enforcement, SSRF hardening, and token-aware Markdown chunking.
- **Search** - Fast local regex search via ugrep with both line-oriented output and structured match records.
- **Read** - Line-numbered file reader (optionally a line range) for quick inspection.
- **SmartFileEdit** - Canonical LF view + byte-precise patch tool (including unified diffs) that preserves original newline bytes and whitespace when editing files via MCP.
- **Bash** - Run shell commands via bash with timeout and stdout/stderr capture.
- **GitStatus / GitDiff / GitRestore** - Local Git status/diff/restore helpers with timeout and output truncation.
- **Ping** - Lightweight health check for MCP clients.
- JSON-RPC 2.0 transport over stdin/stdout with optional `Content-Length` framing for Codex-compatible MCP clients.

### Design Principles

1. **Safety First**: SSRF protection, robots.txt compliance, input validation
2. **Token Efficiency**: Chunk-based content processing aligned with LLM token budgets
3. **Cross-Platform**: Windows and Unix support with platform-specific script handling
4. **Protocol Compatibility**: Multiple MCP method aliases for broad client compatibility

---

## Quick Start

### Requirements

- Rust toolchain (edition 2024; tested with latest stable).
- Cargo in PATH (for running the MCP binary).
- `ugrep` in PATH (required for `Search`).
- Git in PATH (required for `GitStatus`, `GitDiff`, and `GitRestore`).
- **OpenAI**
  - `OPENAI_API_KEY` with access to the Assistants / Vector Store APIs (required for `CodeQuery` and other OpenAI calls).
- **WebFetch browser support** (optional)
  - Chrome or Chromium installed and discoverable on PATH to enable headless rendering of JavaScript-heavy sites. Without it, WebFetch still works in HTTP-only mode.

### Getting Started

```bash
# Clone and enter the repo
git clone https://github.com/your-org/tools.git
cd tools

# Build the workspace
cargo build --workspace --release

# Run the MCP server (OpenAI only)
export OPENAI_API_KEY="sk-..."
cargo run -p tools-mcp-server --release
```

When running under an MCP client, the server reads JSON-RPC messages from stdin and writes responses to stdout. Set `MCP_SKIP_HEADERS=true` to omit `Content-Length` headers if your client expects raw JSON lines.

---

## Tool Selection Guide for LLM Agents

| Task | Tool |
|------|------|
| Semantic code search | CodeQuery |
| Find code by pattern/regex | Search |
| Fetch web content | WebFetch |
| Read file contents | Read |
| Edit existing files | Edit |
| Create new files | Write |
| Delete files | Delete |
| Find files by pattern | Glob |
| Run shell commands | Bash |
| Check git status | GitStatus |
| View diffs | GitDiff |
| Revert changes | GitRestore |
| Stage files | GitAdd |
| Commit changes | GitCommit |
| Extract C++ structure | Outline |

---

## Architecture

### High-Level Component Diagram

```
+------------------+     +-------------------+     +-------------------+
|   MCP Client     |     |   tools           |     |   External APIs   |
|   (Codex Agent)  |<--->|   (JSON-RPC 2.0)  |<--->|   (OpenAI, Web)   |
+------------------+     +-------------------+     +-------------------+
        |                        |
        v                        v
   stdin/stdout           +------+------+------+------+
                          |      |      |      |      |
                    WebFetch CodeQuery Git  Ripgrep  Edit
                          |      |      |      |      |
                    +-----+------+------+------+------+
                    | HTTP/Browser | OpenAI API | Local FS |
                    +--------------+------------+---------+
```

### Module Organization

```text
apps/
  tools-mcp-server/        # Binary crate: stdin/stdout loop, routing, composition

crates/
  tools-mcp-core/          # Shared MCP/runtime support and tool registry
  openai-file-search-core/ # OpenAI/vector-store client library
  tools-mcp-codequery/     # CodeQuery tool, cache, OpenAI integration adapter
  tools-mcp-webfetch/      # WebFetch tool and fetch pipeline
  tools-mcp-local/         # Read/Edit/Write/Delete/Glob/Search/Outline/Pwsh and smart_file_edit
  tools-mcp-git/           # Git tool implementations
```

---

## Core Modules

### main.rs - MCP Server Implementation

**Location**: `apps/tools-mcp-server/src/main.rs`

The main module implements the MCP server, handling protocol communication and routing requests to appropriate tool handlers.

#### Key Types

```rust
/// JSON-RPC request structure
struct RpcRequest {
    jsonrpc: String,        // Always "2.0"
    id: Option<Value>,      // Request ID (null for notifications)
    method: String,         // MCP method name
    params: Value,          // Method parameters
}

/// JSON-RPC response structure
struct RpcResponse<'a> {
    jsonrpc: &'a str,
    id: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
}

/// Tool definition for MCP tools/list response
struct ToolDef {
    name: String,
    description: String,
    input_schema: Value,    // JSON Schema
}
```

#### Message Framing

The server supports two message formats:

1. **Content-Length Headers**: Standard MCP framing with `Content-Length: N\r\n\r\n` prefix
2. **Raw JSON Lines**: Direct JSON messages (enabled via `MCP_SKIP_HEADERS=true`)

```rust
/// Reads MCP messages supporting both Content-Length framing and raw JSON
async fn read_mcp_message<R>(reader: &mut R) -> io::Result<Option<String>>
```

#### Method Routing

Supported method aliases for broad client compatibility:

| Method Category | Aliases |
|----------------|---------|
| Initialize | `mcp/initialize`, `initialize`, `server/initialize` |
| Tools List | `mcp/tools/list`, `tools/list`, `server/tools/list`, `mcp/capabilities`, `capabilities` |
| Tool Call | `mcp/tools/call`, `tools/call`, `server/tools/call` |
| Shutdown | `mcp/shutdown`, `shutdown`, `server/shutdown` |

---

### lib.rs - OpenAI API Client

**Location**: `crates/openai-file-search-core/src/lib.rs`
**Library Name**: `openai_file_search_core`

Provides the core functionality for interacting with OpenAI's vector stores and file search APIs.

#### Configuration

```rust
/// API configuration for OpenAI operations
pub struct ApiConfig {
    pub api_key: String,
    pub default_model: String,  // e.g., "gpt-4o"
}
```

#### File Operations

**File Upload**

```rust
/// Uploads a file to OpenAI's file storage
///
/// # Arguments
/// * `client` - HTTP client
/// * `cfg` - API configuration
/// * `path_or_url` - Local path or HTTP(S) URL
///
/// # Returns
/// File ID assigned by OpenAI (e.g., "file-abc123")
///
/// # File Format Handling
/// - Allowed extensions: c, cpp, css, csv, doc, docx, gif, go, html, java,
///   jpeg, jpg, js, json, md, pdf, php, pkl, png, pptx, py, rb, tar, tex,
///   ts, txt, webp, xlsx, xml, zip
/// - Unsupported extensions are converted to .txt
pub async fn upload_file(client: &Client, cfg: &ApiConfig, path_or_url: &str) -> Result<String>
```

**Batch Upload**

```rust
/// Uploads multiple files with concurrency control
///
/// # Returns
/// Tuple of (successes, failures) where each is a Vec of (path, id/error)
pub async fn upload_files_batch(
    client: &Client,
    cfg: &ApiConfig,
    file_paths: Vec<String>,
    vector_store_id: &str,
    concurrent_limit: usize,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>)>
```

#### Vector Store Management

```rust
/// Creates a new vector store
pub async fn create_vector_store(client: &Client, cfg: &ApiConfig, name: &str) -> Result<String>

/// Lists all vector stores
pub async fn list_vector_stores(client: &Client, cfg: &ApiConfig) -> Result<Vec<VectorStoreEntry>>

/// Gets vector store details including file counts
pub async fn get_vector_store_details(
    client: &Client, cfg: &ApiConfig, vs_id: &str
) -> Result<VectorStoreDetails>

/// Waits for all files in a vector store to finish indexing
pub async fn wait_for_vector_store_ready(
    client: &Client, cfg: &ApiConfig, vs_id: &str,
    poll_ms: u64, timeout_ms: u64
) -> Result<()>
```

#### Semantic Search

```rust
/// Executes a semantic search query against a vector store
///
/// # Arguments
/// * `model` - Model to use (e.g., "gpt-4o")
/// * `query` - Natural language search query
/// * `vector_store_id` - Target vector store ID
/// * `max_num_results` - Optional limit on returned results
/// * `include_results` - Include raw search results in response
pub async fn responses_with_file_search(
    client: &Client, cfg: &ApiConfig, model: &str, query: &str,
    vector_store_id: &str, max_num_results: Option<u32>, include_results: bool,
) -> Result<serde_json::Value>
```

#### File Reindexing

```rust
/// Reindexes files based on content hashes
///
/// This function implements incremental indexing:
/// 1. Lists existing files in vector store with their hashes
/// 2. Computes hashes for local files
/// 3. Uploads changed/new files with path and hash attributes
/// 4. Deletes files that no longer exist locally (orphan cleanup)
///
/// # Attributes Stored
/// - `path`: Full file path for matching
/// - `hash`: SHA-256 content hash for change detection
/// - `indexed_at`: ISO 8601 timestamp
pub async fn reindex_files(
    client: &Client, cfg: &ApiConfig, vector_store_id: &str,
    file_paths: &[String], concurrent_limit: usize, skip_per_file_wait: bool,
) -> Result<serde_json::Value>
```

#### CodeQuery Options

```rust
/// Configuration for CodeQuery operations
pub struct CodeQueryOptions<'a> {
    pub concurrent_limit: usize,      // Max concurrent uploads (1-20)
    pub timeout_ms: u64,              // Indexing timeout
    pub model: Option<&'a str>,       // Override default model
    pub max_num_results: Option<u32>, // Limit search results
    pub include_results: bool,        // Include raw results in response
}
```

---

### codequery/mod.rs - Semantic Code Search

**Location**: `src/codequery/mod.rs`

Orchestrates semantic code search by combining file indexing with OpenAI's vector store queries.

#### Handler Function

```rust
/// Handles CodeQuery tool invocations
///
/// # Flow
/// 1. Validates API key and parameters
/// 2. Resolves vector store (by ID, name, or auto-creates from directory name)
/// 3. Auto-discovers indexable files if none provided
/// 4. Filters out binary and non-code files
/// 5. Reindexes changed files using hash-based comparison
/// 6. Waits for indexing to complete
/// 7. Executes semantic search query
///
/// # File Discovery
/// Uses `ignore` crate to walk directory tree respecting .gitignore rules
///
/// # Indexable Extensions
/// rs, c, h, cpp, hpp, go, java, kt, kts, swift, py, rb, php, js, jsx, ts, tsx
pub async fn handle_code_query(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

#### Skip Directories

The following directories are automatically excluded from file discovery:
- `.git`, `.hg`, `.svn` (version control)
- `.idea`, `.vscode` (IDE)
- `.venv`, `__pycache__`, `node_modules` (dependencies)
- `target`, `dist`, `build`, `out` (build artifacts)
- `coverage`, `tmp` (temporary)

---

### codequery/cache.rs - Vector Store ID Caching

**Location**: `src/codequery/cache.rs`

Provides disk-based caching for vector store IDs to avoid repeated API lookups.

```rust
/// Loads a cached vector store ID by name
pub fn load_store_id_from_cache(name: &str) -> Option<String>

/// Caches a vector store ID for future lookups
pub fn cache_store_id(name: &str, id: &str)
```

**Cache Location**: `$HOME/.codex/mcp/stores.json`

---

### webfetch/mod.rs - Web Content Fetching

**Location**: `src/webfetch/mod.rs`

Orchestrates web content fetching with hybrid rendering (HTTP-first with browser fallback).

#### Main Entry Point

```rust
/// Fetches and processes web content
///
/// # Rendering Strategy
/// 1. If force_browser=true OR URL is whitelisted: use browser
/// 2. Otherwise: HTTP fetch, then check for JS-heavy heuristics
/// 3. If JS-heavy detected: retry with browser
/// 4. Fallback to HTTP content if browser fails
///
/// # Processing Pipeline
/// 1. Validate URL for SSRF protection
/// 2. Check disk cache
/// 3. Fetch content (HTTP or browser)
/// 4. Extract body content, convert to Markdown
/// 5. Chunk text by token budget
/// 6. Return structured response with metadata
pub async fn run_fetch(req: FetchRequest) -> Result<FetchResponse>
```

#### Global Browser Pool

```rust
/// Lazily initialized browser pool for headless rendering
static BROWSER_POOL: OnceCell<Arc<browser::BrowserPool>> = OnceCell::const_new();
```

---

### webfetch/http.rs - HTTP Client with Security

**Location**: `src/webfetch/http.rs`

Provides secure HTTP fetching with SSRF protection and robots.txt compliance.

#### SSRF Validation

```rust
/// Validates URL for SSRF protection
///
/// # Checks Performed
/// 1. Scheme must be http or https
/// 2. Rejects localhost and localhost.localdomain
/// 3. Rejects literal private/reserved IP addresses:
///    - IPv4: private ranges, loopback, link-local, broadcast, documentation
///    - IPv6: loopback, unspecified, unique local, link-local
///    - IPv4-mapped IPv6 addresses (e.g., ::ffff:127.0.0.1)
/// 4. Resolves DNS and blocks if any resolved IP is private/reserved
///
/// # Side Effects
/// Performs DNS resolution to detect hostname-based bypasses
pub async fn validate_url_ssrf(url: &str) -> Result<()>
```

#### Robots.txt Compliance

```rust
/// Checks if URL is allowed by robots.txt
///
/// # Caching
/// robots.txt content is cached per domain in a global RwLock-protected HashMap
///
/// # User-Agent
/// Uses "tools-webfetch/0.1" for matching
async fn is_allowed_by_robots(client: &Client, url: &str) -> Result<bool>
```

#### HTTP Client Configuration

```rust
/// Builds HTTP client with security settings
///
/// # Configuration
/// - User-Agent: "tools-webfetch/0.1"
/// - Timeout: 20 seconds
/// - Redirects: Disabled (manual handling for SSRF protection)
/// - Compression: brotli, gzip, deflate enabled
pub fn build_http_client() -> Result<Client>
```

---

### webfetch/heuristics.rs - JS-Heavy Site Detection

**Location**: `src/webfetch/heuristics.rs`

Detects JavaScript-heavy websites that require browser rendering.

#### Heuristic Analysis

```rust
/// Analyzes HTML to determine if browser rendering is needed
///
/// # Heuristics Applied (with weights)
/// 1. Empty SPA shell with root divs (0.5)
/// 2. High script tag density (>5 scripts) (0.25)
/// 3. Framework signatures (React, Vue, Angular, Next.js, Svelte) (0.3)
/// 4. Small HTML payload (<5KB) (0.15)
/// 5. Explicit noscript warnings (0.5)
///
/// # Threshold
/// Site is considered JS-heavy if confidence >= 0.5 (50%)
pub fn analyze_js_heavy(
    html_body: &str,
    extracted_markdown: &str,
    content_type: Option<&str>,
    content_length: Option<usize>,
) -> JsHeuristicResult
```

#### Framework Detection Patterns

| Framework | Signature Patterns |
|-----------|-------------------|
| React | `data-reactroot`, `data-reactid`, `__reactContainer`, `__REACT` |
| Vue | `data-v-`, `v-cloak`, `__vue`, `__VUE__` |
| Angular | `ng-app`, `ng-version`, `ng-binding`, `[ng-` |
| Next.js | `__NEXT_DATA__`, `_next/static` |
| Svelte | `svelte-`, `__SVELTE__` |

---

### smart_file_edit/mod.rs - Newline-Aware File Editing

**Location**: `src/smart_file_edit/mod.rs`

Provides surgical file editing that preserves original line endings.

#### Operations

**get_region**: Extract a file region with metadata

```rust
/// Returns file region with canonical text and metadata
///
/// # Response Fields
/// - plain_text: Raw text without line numbers
/// - canonical_text: Text with line numbers (for reference)
/// - file_hash: SHA-256 hash for staleness detection
/// - region_id: UUID for tracking
/// - newline_style: Detected line ending style (LF, CRLF, CR)
/// - byte_range: Start/end byte offsets
fn handle_get_region(req: &GetRegionRequest) -> Result<Value>
```

**apply_snippet_edit**: Replace text while preserving newlines

```rust
/// Replaces old_snippet with new_snippet
///
/// # Features
/// - Canonical LF processing for consistent matching
/// - Original newline preservation in output
/// - Staleness detection via file_hash
/// - match_hint for guided search (start_line, end_line)
///
/// # Response Statuses
/// - ok: Edit applied successfully
/// - no_match: old_snippet not found (includes candidate suggestions)
/// - stale_file: File changed since get_region
fn handle_apply_snippet_edit(req: &ApplySnippetEditRequest) -> Result<Value>
```

**apply_unified_diff**: Apply unified diff format patches

```rust
/// Applies a unified diff to a file
///
/// # Diff Format
/// Expects standard unified diff with @@ hunk headers
///
/// # Processing
/// Each hunk is converted to a snippet edit and applied sequentially
/// File hash is updated between hunks for consistency
fn handle_apply_unified_diff(req: &ApplyUnifiedDiffRequest) -> Result<Value>
```

#### Newline Handling

```rust
/// Detected newline styles
enum NewlineKind {
    Lf,    // Unix: \n
    CrLf,  // Windows: \r\n
    Cr,    // Classic Mac: \r
    None,  // No newlines in file
}

/// Statistics for mixed newline detection
struct NewlineStats {
    lf: usize,
    crlf: usize,
    cr: usize,
}

impl NewlineStats {
    /// Returns the most common newline style (dominant)
    fn dominant(&self) -> NewlineKind

    /// Returns dominant style, defaulting to LF if no newlines
    fn default_kind(&self) -> NewlineKind
}
```

---

### git/mod.rs - Git Operations

**Location**: `src/git/mod.rs`

Provides git command execution with timeout and output management.

#### Common Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| working_dir | String? | cwd | Working directory for git command |
| timeout_ms | u64 | 30000 | Command timeout in milliseconds |

#### GitStatus

```rust
/// Shows working tree status
///
/// # Parameters
/// - porcelain: bool (default: true) - Use --porcelain=1 output
/// - branch: bool (default: true) - Include branch info (-b)
/// - untracked: bool (default: true) - Include untracked files
///
/// # Response
/// - clean: bool - True if no changes
/// - stdout/stderr: Raw command output
/// - exit_code: Process exit code
pub async fn handle_git_status(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

#### GitDiff

```rust
/// Shows file changes
///
/// # Parameters
/// - cached: bool (default: false) - Diff staged changes (--cached)
/// - stat: bool (default: false) - Show diffstat only (--stat)
/// - name_only: bool (default: false) - Show only file names
/// - unified: i64? - Context lines (-U<N>)
/// - paths: String[]? - Specific paths to diff
/// - max_bytes: usize (default: 200000) - Max stdout capture
pub async fn handle_git_diff(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

#### GitRestore

```rust
/// Discards uncommitted changes (DESTRUCTIVE)
///
/// # Parameters
/// - paths: String[] (required) - Files to restore
/// - staged: bool (default: false) - Restore staging area
/// - worktree: bool (default: true) - Restore working tree
pub async fn handle_git_restore(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

#### GitAdd

```rust
/// Stages files for commit
///
/// # Parameters
/// - paths: String[]? - Files to stage
/// - all: bool (default: false) - Stage all changes (-A)
/// - update: bool (default: false) - Stage modified/deleted only (-u)
pub async fn handle_git_add(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

#### GitCommit

```rust
/// Creates a conventional commit
///
/// # Parameters
/// - type: String (required) - Commit type (feat, fix, docs, etc.)
/// - scope: String? - Optional scope/area
/// - message: String (required) - Commit description
///
/// # Commit Message Format
/// With scope: "type(scope): message"
/// Without scope: "type: message"
pub async fn handle_git_commit(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### tools/handlers/ripgrep.rs - File Search

**Location**: `src/tools/handlers/ripgrep.rs`

Provides file content search using ugrep.

```rust
/// Searches files using regex or fuzzy patterns
///
/// # Backend Selection
/// - Uses ugrep for all searches
/// - Fuzzy parameter present (1-4): Adds -Z<N> flag
///
/// # Parameters
/// - pattern: String (required) - Search pattern (regex by default)
/// - path: String (default: ".") - Root search path
/// - case: "smart"|"sensitive"|"insensitive" (default: "smart")
/// - fixed_strings: bool - Treat pattern as literal (-F)
/// - word_regexp: bool - Match word boundaries only (-w)
/// - glob: String[] - Glob filters
/// - hidden: bool - Include hidden files
/// - follow: bool - Follow symlinks
/// - no_ignore: bool - Ignore .gitignore
/// - context: usize - Context lines around matches
/// - max_results: usize (default: 200, max: 10000)
/// - timeout_ms: u64 (default: 20000)
/// - fuzzy: u8 (1-4) - Fuzzy match tolerance
///
/// # Response Format
/// Returns structured matches with file paths, line numbers, and content
pub async fn handle_ripgrep(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### read_file.rs - File Reading

**Location**: `src/tools/handlers/read_file.rs`

```rust
/// Reads file contents with optional line range
///
/// # Parameters
/// - path: String (required) - File path to read
/// - start_line: usize (1-based) - First line to read
/// - end_line: usize (1-based, inclusive) - Last line to read
/// - show_line_numbers: bool (default: true) - Prefix lines with numbers
///
/// # Response
/// - content: File text (with optional line numbers)
/// - total_lines: Total lines in file
/// - start_line/end_line: Actual range returned
pub async fn handle_read_file(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### write.rs - File Creation

**Location**: `src/tools/write.rs`

```rust
/// Creates a new file
///
/// # Parameters
/// - path: String (required) - File path to create
/// - content: String (required) - File content
///
/// # Behavior
/// - Fails if file already exists (use Edit for modifications)
/// - Creates parent directories automatically
///
/// # Response
/// - bytes: Number of bytes written
pub async fn handle_write(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### delete.rs - File Deletion

**Location**: `src/tools/delete.rs`

```rust
/// Deletes a file (DESTRUCTIVE)
///
/// # Parameters
/// - path: String (required) - File to delete
///
/// # Restrictions
/// - Only files can be deleted (not directories)
/// - Fails if file doesn't exist
pub async fn handle_delete(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### glob.rs - File Globbing

**Location**: `src/tools/glob.rs`

```rust
/// Lists files matching a glob pattern
///
/// # Parameters
/// - pattern: String (required) - Glob pattern (e.g., "**/*.rs")
/// - path: String (default: ".") - Base directory
/// - hidden: bool (default: false) - Include hidden files
/// - limit: usize (default: 1000, max: 10000)
///
/// # Features
/// - Respects .gitignore
/// - Sorted output for consistency
pub async fn handle_glob(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

### outline.rs - C++ Structure Extraction

**Location**: `src/tools/outline.rs`

```rust
/// Extracts C++ structure without implementation bodies
///
/// Uses tree-sitter for parsing (tree-sitter-cpp v0.23.4)
///
/// # Parameters
/// - path: String (required) - C++ file path
/// - include_private: bool (default: false) - Include private members
///
/// # Extracted Elements
/// - Preprocessor includes and conditionals
/// - Namespace definitions
/// - Class/struct specifiers with base classes
/// - Enum specifiers (including enum class)
/// - Type definitions and using declarations
/// - Function declarations/signatures (without bodies)
/// - Template declarations
/// - Doc comments (/// and /** */)
///
/// # Access Control
/// For classes: default private, for structs: default public
/// Access specifiers (public:, protected:, private:) are preserved
pub async fn handle_outline(id: Option<Value>, args: Value) -> RpcResponse<'static>
```

---

## MCP Tools

### CodeQuery

Index code and query an OpenAI vector store in a single call.

- **Tool name**: `CodeQuery`
- **Required**:
  - `query` – natural-language question.
- **Vector store selection**:
  - `vector_store_id` – use an existing store by ID, or
  - `vector_store_name` – use or create a store with this name, or
  - omit both – default to the git top-level directory name plus a workspace fingerprint, such as `tools-mcp [1a2b3c4d]`. If the name cannot be inferred, the call returns an error.
- **Optional**:
  - `file_paths` (string[]) - explicit local file paths to sync before querying. If omitted, CodeQuery auto-discovers indexable files under the git top level when inside a repository, otherwise under the current directory (respecting `.gitignore`, skipping `target/`, `node_modules/`, VCS dirs, etc.). CodeQuery only indexes source code files; docs (e.g. `.md`), config (e.g. `.toml`/`.yaml`), and binary/media files (e.g. images/archives) are filtered out even if explicitly listed.
  - `concurrent_limit` (integer, 1-20, default 5) - maximum concurrent upload/delete operations.
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
    - Optional second item: JSON summary of the reindexing operations (compact by default; set `TOOLS_PRETTY_JSON=true` to pretty-print).

#### CodeQuery Architecture

At a high level, CodeQuery is split across the feature crate `crates/tools-mcp-codequery/` and the reusable OpenAI/vector-store client crate `crates/openai-file-search-core/`.

**Key modules**
- `apps/tools-mcp-server/src/main.rs`: composition root and stdin/stdout loop; JSON-RPC routing lives in `apps/tools-mcp-server/src/mcp_server.rs`.
- `crates/tools-mcp-codequery/src/tool_handler.rs`: CodeQuery MCP handler (validation, defaults, file discovery, vector-store resolution, response shaping); delegates semantic search to `openai_file_search_core` via the feature crate's `CodeQueryEngine`.
- `crates/tools-mcp-codequery/src/codequery_cache.rs`: tiny on-disk cache mapping the resolved store lookup key to `vector_store_id` to avoid repeated list/create calls.
- `crates/openai-file-search-core/src/lib.rs`: OpenAI REST calls (files + vector stores + Responses API), plus the change-based reindexing algorithm.

**Data flow (single CodeQuery call)**
```text
MCP client
  -> src/adapters/inbound/mcp_server.rs (route tool)
    -> src/application/codequery_tool.rs::handle_code_query (validate args, choose store, choose files)
      -> src/lib.rs::code_query (sync files, then query with file_search)
        -> src/lib.rs::reindex_with_retry / reindex_files (optional)
        -> src/lib.rs::wait_for_vector_store_ready (poll once for batch indexing)
        -> src/lib.rs::responses_with_file_search (Responses API w/ file_search tool)
      <- returns: answer text (+ optional JSON reindex summary)
```

**Vector store selection**
- If `vector_store_id` is provided, CodeQuery uses it directly.
- Otherwise, it uses `vector_store_name`:
  - If omitted, it defaults to the git top-level directory name plus a workspace fingerprint (so same-named repos do not collide).
  - It attempts to load an ID from `~/.codex/mcp/stores.json` (via `src/codequery_cache.rs`).
  - If not cached, it lists vector stores and matches by name; if no match exists, it creates a new vector store and caches its ID.
  - Note: the cache path is based on `HOME` (so on Windows you may need `HOME` set for caching to work).

**Local file discovery and filtering**
- If `file_paths` is omitted/empty, CodeQuery walks the git top level recursively when inside a repository, otherwise the current directory, and collects indexable files.
- It skips common "noise" directories (e.g., `.git/`, `node_modules/`, `target/`, `dist/`) and hidden directories.
- It indexes:
  - source code files with allowed extensions (see `src/lib.rs` `is_codequery_indexable_ext`).

**Change-based reindexing algorithm**
When `file_paths` is non-empty, `src/lib.rs::code_query` syncs local files into the chosen vector store before asking the question:
- Lists current vector-store files and builds lookups by `attributes.path`, `attributes.hash`, and `filename` (for legacy entries).
- Computes SHA-256 for each local file.
- Decides actions:
  - **skip** when the stored hash matches the local hash for the same path/filename.
  - **upload** when new/changed, and includes `attributes = { path, hash, indexed_at }` for future comparisons.
  - **delete** when files exist in the store but are no longer present locally (orphans), plus old versions of changed files.
- Upload/attach operations are concurrency-limited (`concurrent_limit`) and wrapped in a small retry loop (`reindex_with_retry`) for transient errors.
- Instead of waiting per-file, it does a single poll for vector-store readiness (`wait_for_vector_store_ready`) to keep the call latency reasonable.

**Query execution**
- After indexing is ready, CodeQuery uses the OpenAI Responses API and enables the `file_search` tool bound to the selected vector store.
- The tool returns the assistant's answer text. If `include_results=true`, CodeQuery also includes top search matches in the extracted response text.

**Response shape**
- The MCP result always returns `content[0].text` as the assistant answer.
- If reindexing ran, it also returns `content[1].text` containing a JSON "reindex summary" with uploaded/skipped/deleted/errors counts and entries (compact by default; set `TOOLS_PRETTY_JSON=true` to pretty-print).

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
  - Caches responses under `/tmp/tools-webfetch` keyed by URL + method.
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

### Search

Fast local regex search using ugrep.

- **Tool name**: `Search`
- **Required**:
  - `pattern` - ripgrep pattern (regex by default).
- **Optional**:
  - `path` - file or directory root (default: current working directory).
  - `case` - `"smart"` (default), `"sensitive"`, or `"insensitive"`.
  - `fixed_strings`, `word_regexp`, `glob`, `hidden`, `follow`, `no_ignore`, `context`, `max_results`, `timeout_ms`.
- **Notes**:
  - Requires `ugrep` to be installed and discoverable on PATH on the machine running the MCP server.
  - Uses `ugrep` and returns both a readable `content[0].text` and structured `matches`.

### Read

Read a local file (optionally a line range) for quick inspection without uploads.

- **Tool name**: `Read`
- **Required**:
  - `path` - filesystem path to read.
- **Optional**:
  - `start_line`, `end_line` (1-based, inclusive).
  - `show_line_numbers` (default: true) - set to `false` for raw content.
- **Response**:
  - `content[0].text` is line-numbered by default (similar to `nl -ba` / `cat -n`).
  - Includes `start_line`, `end_line`, and `total_lines`.

### SmartFileEdit

Edit files while preserving original newline bytes and whitespace.

- **Tool name**: `SmartFileEdit`
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
    - Rewrites the corresponding byte range using the file's dominant newline style, preserving other bytes.
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

- **Tool name**: `Bash`
- **Required**:
  - `command` - shell command to run (executed as: `bash -lc "<command>"`).
- **Optional**:
  - `timeout_ms` - timeout in milliseconds (default 30000).
  - `working_dir` - optional working directory for the command.
- **Response**:
  - Returns a JSON summary (including `stdout`, `stderr`, `exit_code`) in `content[0].text` (compact by default; set `TOOLS_PRETTY_JSON=true` to pretty-print).

### ping

Simple health check for MCP clients.

- **Tool name**: `ping`
- **Behavior**:
  - Always returns `pong` in a JSON `content` array.
- Useful for MCP client connectivity tests or keepalive pings.

### GitStatus

Run `git status` (porcelain by default).

- **Tool name**: `GitStatus`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `porcelain` (boolean, default `true`) - when true, uses `--porcelain=1`.
  - `branch` (boolean, default `true`) - include branch header (`-b`) in porcelain mode.
  - `untracked` (boolean, default `true`) - include untracked files in porcelain mode (when false, uses `-uno`).
- **Response**:
  - `content[0].text` - porcelain output (or `clean` if there are no changes).
  - Includes `stdout`, `stderr`, `exit_code`, `timed_out`, `clean`, and the executed `args`.

### GitDiff

Run `git diff` with optional flags and output truncation.

- **Tool name**: `GitDiff`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `cached` (boolean, default `false`) - staged diff (`--cached`).
  - `stat` (boolean, default `false`) - diffstat only (`--stat`).
  - `name_only` (boolean, default `false`) - file names only (`--name-only`).
  - `unified` (integer, >= 0) - context lines (`-U<N>`).
  - `paths` (string[]) - limit diff to these paths (passed after `--`).
  - `max_bytes` (integer, default 200000) - maximum bytes captured from stdout before truncation.
- **Response**:
  - `content[0].text` - diff output (or `no diff`).
  - Includes `stdout`, `stderr`, `exit_code`, `timed_out`, `truncated_stdout`, and the executed `args`.

### GitRestore

Run `git restore` on explicit paths.

- **Tool name**: `GitRestore`
- **Required**:
  - `paths` (string[]) - paths to restore (passed after `--`).
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `staged` (boolean, default `false`) - restore the index (`--staged`).
  - `worktree` (boolean, default `true`) - restore the working tree (`--worktree`).
- **Response**:
  - `content[0].text` - `ok` on success (or stderr on failure).
  - Includes `stdout`, `stderr`, `exit_code`, `timed_out`, and the executed `args`.

### GitAdd

Stage files for commit.

- **Tool name**: `GitAdd`
- **Optional**:
  - `paths` (string[]) - files to stage
  - `all` (boolean, default `false`) - stage all changes (`-A`)
  - `update` (boolean, default `false`) - stage modified/deleted only (`-u`)
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
- **Response**:
  - Returns success/failure status with command output.

### GitCommit

Create a conventional commit.

- **Tool name**: `GitCommit`
- **Required**:
  - `type` (string) - commit type (feat, fix, docs, style, refactor, test, chore, etc.)
  - `message` (string) - commit description
- **Optional**:
  - `scope` (string) - optional scope/area of change
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
- **Response**:
  - Returns commit result with hash and message.

### Write

Create a new file.

- **Tool name**: `Write`
- **Required**:
  - `path` (string) - file path to create
  - `content` (string) - file content
- **Response**:
  - Returns number of bytes written.

### Delete

Delete a file.

- **Tool name**: `Delete`
- **Required**:
  - `path` (string) - file to delete
- **Response**:
  - Returns success/failure status.

### Glob

List files matching a glob pattern.

- **Tool name**: `Glob`
- **Required**:
  - `pattern` (string) - glob pattern (e.g., `**/*.rs`)
- **Optional**:
  - `path` (string, default: ".") - base directory
  - `hidden` (boolean, default: false) - include hidden files
  - `limit` (integer, default: 1000, max: 10000) - maximum files to return
- **Response**:
  - Returns array of matching file paths.

### Outline

Extract C++ structure.

- **Tool name**: `Outline`
- **Required**:
  - `path` (string) - C++ file path
- **Optional**:
  - `include_private` (boolean, default: false) - include private members
- **Response**:
  - Returns extracted class/struct signatures and function declarations.

---

## MCP Protocol Implementation

### Protocol Version

The server implements MCP protocol version `2025-03-26`.

### Message Format

#### Request Format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Read",
    "arguments": {
      "path": "/path/to/file.txt"
    }
  }
}
```

#### Response Format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "file contents..."
      }
    ],
    "isError": false
  }
}
```

### Notification Handling

The server handles the following notifications (no response sent):
- `notifications/initialized`
- `initialized`

### Error Codes

| Code | Meaning |
|------|---------|
| -32601 | Method not found / Unknown tool |
| -32603 | Internal error |

### Initialize Response

```json
{
  "protocolVersion": "2025-03-26",
  "serverInfo": {
    "name": "mcp-echo-server",
    "version": "1.0.0"
  },
  "capabilities": {
    "tools": {
      "list": true,
      "call": true
    }
  },
  "tools": [/* tool definitions */]
}
```

---

## Data Structures

### OpenAI Response Types

```rust
/// OpenAI Responses API response object
pub struct ResponseObject {
    pub id: String,
    pub object: String,
    pub created_at: i64,
    pub status: String,
    pub model: String,
    pub output: Vec<OutputItem>,
    pub error: Option<Value>,
    pub usage: Option<Value>,
}

/// Output item variants
pub enum OutputItem {
    Message(MessageOutput),
    FileSearchCall(FileSearchOutput),
    Other,
}

/// Message output with content
pub struct MessageOutput {
    pub id: String,
    pub role: String,
    pub status: String,
    pub content: Vec<ContentItem>,
}

/// File search results
pub struct FileSearchOutput {
    pub id: String,
    pub status: String,
    pub queries: Option<Vec<String>>,
    pub results: Option<Vec<Value>>,
}
```

### Vector Store Types

```rust
/// Vector store with file counts
pub struct VectorStoreDetails {
    pub id: String,
    pub file_counts: FileCounts,
}

pub struct FileCounts {
    pub in_progress: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub total: u64,
}

/// File metadata
pub struct FileInfo {
    pub id: String,
    pub filename: Option<String>,
    pub purpose: Option<String>,
    pub bytes: Option<u64>,
    pub created_at: Option<i64>,
    pub attributes: Option<Map<String, Value>>,
}
```

### WebFetch Types

```rust
/// Request payload for WebFetch tool
pub struct FetchRequest {
    pub url: String,
    pub max_chunk_tokens: Option<usize>,
    pub no_cache: bool,
    pub force_browser: bool,
}

/// Response payload from WebFetch tool
pub struct FetchResponse {
    pub url: String,
    pub fetched_at: String,      // ISO 8601 timestamp
    pub title: Option<String>,
    pub language: Option<String>,
    pub chunks: Vec<FetchChunk>,
    pub rendering_method: String, // "http" or "browser"
    pub note: Option<String>,     // "cache_hit", "rendered_with_browser"
}

/// Single content chunk
pub struct FetchChunk {
    pub heading: Option<String>,
    pub text: String,
    pub token_count: usize,
}
```

---

## Configuration

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| OPENAI_API_KEY | Yes | OpenAI API key for vector store operations |
| MCP_SKIP_HEADERS | No | Set to "true" for raw JSON output (no Content-Length headers) |
| RUST_LOG | No | Logging level (debug, info, warn, error) |
| APP_VERSION | No | Version string exposed in server info |
| HOME | No | Home directory for cache storage |
| TOOLS_PRETTY_JSON | No | Set to "true" (or 1/yes/on) to pretty-print JSON payloads returned as text (default: compact) |

### Cache Locations

| Cache | Path | Purpose |
|-------|------|---------|
| CodeQuery stores | `$HOME/.codex/mcp/stores.json` | Vector store ID mapping |
| WebFetch content | `/tmp/tools-webfetch/` | HTTP response cache |

### MCP Client Configuration Example

```toml
[mcp_servers.tools]
command = "/path/to/tools/target/release/tools"
env = {
  OPENAI_API_KEY = "${OPENAI_API_KEY}",
  MCP_SKIP_HEADERS = "true",
  RUST_LOG = "error"
}
```

---

## Security Considerations

### SSRF Protection

The WebFetch tool implements comprehensive SSRF protection:

1. **Scheme Validation**: Only `http://` and `https://` allowed
2. **Hostname Blocking**: Rejects `localhost` and `localhost.localdomain`
3. **IP Address Validation**:
   - IPv4: Private ranges (10.x, 172.16-31.x, 192.168.x), loopback, link-local, broadcast, documentation
   - IPv6: Loopback, unspecified, unique local, link-local
   - IPv4-mapped IPv6 (e.g., `::ffff:127.0.0.1`)
4. **DNS Resolution Check**: Resolves hostname and validates all returned IPs
5. **Redirect Validation**: Manual redirect following with SSRF check on each hop

### Robots.txt Compliance

- Fetches and caches robots.txt per domain
- Uses User-Agent `tools-webfetch/0.1` for matching
- Blocks URLs disallowed by robots.txt
- Missing robots.txt allows all paths

### Prompt Injection Mitigation

WebFetch returns structured data clearly marked as external content:

- `rendering_method` distinguishes source ("http" vs "browser")
- Consuming agents should treat content as untrusted user input
- No automatic command execution based on scraped content

### Browser Security

When using headless Chrome:
- Runs with `--no-sandbox` (required for containerized environments)
- Browser pool lifecycle management prevents resource leaks
- Resource blocking: images, fonts, video/audio autoplay
- Network idle timeout (max 20s wait)

### Input Validation

All tool handlers validate:
- Required parameters are present and non-empty
- Numeric ranges are within bounds
- File paths exist (for read operations)
- File doesn't exist (for write operations)

---

## Dependencies

### Core Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| tokio | 1.0 | Async runtime with full features |
| anyhow | 1.0 | Error handling with context |
| serde | 1.0 | Serialization/deserialization |
| serde_json | 1.0 | JSON parsing and generation |
| reqwest | 0.12 | HTTP client (TLS, compression) |
| tracing | 0.1 | Structured logging |
| tracing-subscriber | 0.3 | Log subscriber with env filter |

### Web Fetching

| Crate | Version | Purpose |
|-------|---------|---------|
| chromiumoxide | 0.7 | Headless Chrome via CDP |
| htmd | 0.4 | HTML to Markdown conversion |
| readability | 0.3 | Boilerplate removal |
| scraper | 0.24 | HTML DOM parsing |
| robotstxt | 0.3 | robots.txt parsing |
| tiktoken-rs | 0.9 | OpenAI tokenizer |
| url | 2.5 | URL parsing |

### File Operations

| Crate | Version | Purpose |
|-------|---------|---------|
| glob | 0.3 | Glob pattern matching |
| ignore | 0.4 | .gitignore-aware walking |
| sha2 | 0.10 | SHA-256 hashing |
| hex | 0.4 | Hex encoding |
| uuid | 1.0 | UUID generation |

### Code Analysis

| Crate | Version | Purpose |
|-------|---------|---------|
| tree-sitter | 0.26 | Incremental parsing |
| tree-sitter-cpp | 0.23 | C++ grammar |

### Date/Time

| Crate | Version | Purpose |
|-------|---------|---------|
| chrono | 0.4 | Date/time with serde |

### Development

| Crate | Version | Purpose |
|-------|---------|---------|
| tempfile | 3.10 | Temporary files for tests |
| futures | 0.3 | Async utilities |

---

## Error Handling

### Error Response Format

Tool errors are returned in the MCP content format:

```json
{
  "content": [
    {
      "type": "text",
      "text": "error message"
    }
  ],
  "isError": true
}
```

### Error Categories

1. **Validation Errors**: Invalid parameters, missing required fields
2. **File Errors**: Not found, permission denied, already exists
3. **Network Errors**: Connection failed, timeout, HTTP errors
4. **API Errors**: OpenAI API failures, rate limiting
5. **Process Errors**: Command spawn failed, timeout, non-zero exit

### Retry Logic

The `reindex_with_retry` function implements exponential backoff:

- Max attempts: 3
- Backoff delays: 200ms, 500ms, 1000ms
- Jitter: 50ms per attempt
- Retries on: timeout, connection reset, 429/5xx errors

---

## Testing

### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run with output
cargo test --workspace -- --nocapture

# Run specific test
cargo test --workspace test_name
```

### Coverage (Local HTML)

This repo supports local code coverage via `cargo llvm-cov` (HTML output).

Prerequisites:
- Install the Rust LLVM tools: `rustup component add llvm-tools-preview`
- Install the cargo subcommand: `cargo install cargo-llvm-cov`

Run (cross-platform):
```bash
cargo coverage
```

Run via scripts (also supports installing missing prerequisites):

Windows (PowerShell):
```powershell
.\scripts\coverage.ps1
# or:
.\scripts\coverage.ps1 -Install -Open
```

Unix/macOS:
```bash
./scripts/coverage.sh
# or:
./scripts/coverage.sh --install
```

Output:
- `coverage/index.html`

### Integration Testing

The server can be tested via stdin/stdout:

```bash
# Initialize
echo '{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}' | cargo run -p tools-mcp-server

# List tools
echo '{"jsonrpc":"2.0","id":2,"method":"mcp/tools/list","params":{}}' | cargo run -p tools-mcp-server

# Read file
echo '{"jsonrpc":"2.0","id":3,"method":"mcp/tools/call","params":{"name":"Read","arguments":{"path":"Cargo.toml"}}}' | cargo run -p tools-mcp-server
```

---

## Build and Release

### Building

```bash
# Debug build
cargo build --workspace

# Release build
cargo build --workspace --release

# With custom version
APP_VERSION="1.0.0" cargo build -p tools-mcp-server --release
```

### Binary Location

- Debug: `target/debug/tools-mcp-server`
- Release: `target/release/tools-mcp-server`

### System Requirements

**Required**:
- Rust toolchain (2024 edition)
- OPENAI_API_KEY environment variable

**Optional**:
- Chrome/Chromium for browser rendering
- ripgrep (`rg`) for Search tool
- ugrep for fuzzy search
- Git for git tools

---

*Generated documentation for tools v1.0.0*
