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

- **Web Content Fetching**: Retrieve and process web pages with SSRF protection and robots.txt compliance
- **File Operations**: Read, write, edit, and delete files with newline-aware processing
- **Git Integration**: Execute git commands (status, diff, restore, add, commit)
- **Code Search**: Fast in-memory search for eligible literal-looking, seeded-regex, fixed-string, and fuzzy fixed-string queries with ugrep fallback for unsupported regex and search modes
- **Code Structure Extraction**: Extract C++ class/method signatures using tree-sitter

### Highlights

- **WebFetch** - HTTP + optional headless-browser fetcher with caching, robots.txt enforcement, SSRF hardening, and token-aware Markdown chunking.
- **Search** - Fast local search with an automatic in-memory path for eligible literal-looking, seeded-regex, fixed-string, and fuzzy fixed-string queries, plus ugrep fallback for unsupported regex and search modes.
- **Read** - Raw file reader (optionally a line range) for quick inspection, with opt-in line numbers.
- **Edit** - Simple snippet-based file editing. Finds `old_snippet` and replaces with `new_snippet`, preserving the file's original line endings (LF, CRLF, or CR).
- **Pwsh** - Run PowerShell commands via pwsh with timeout and stdout/stderr capture.
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

- Rust toolchain 1.94 or newer (edition 2024).
- Cargo in PATH (for running the MCP binary).
- `ugrep` in PATH (required for `Search` unsupported regex, unsupported fuzzy modes, and fallback behavior).
- Git in PATH (required for all `Git*` tools).
- **WebFetch browser support** (optional)
  - Chrome or Chromium installed and discoverable on PATH to enable headless rendering of JavaScript-heavy sites. Without it, WebFetch still works in HTTP-only mode.

### Getting Started

```bash
# Clone and enter the repo
git clone https://github.com/danielchristiancazares/tools-mcp.git
cd tools-mcp

# Build the workspace
cargo build --workspace --release

# Run the MCP server
cargo run -p tools-mcp-server --release
```

When running under an MCP client, the server reads JSON-RPC messages from stdin and writes responses to stdout. Set `MCP_SKIP_HEADERS=true` to omit `Content-Length` headers if your client expects raw JSON lines.

---

## Tool Selection Guide for LLM Agents

| Task | Tool |
|------|------|
| Find code by pattern/regex | Search |
| Fetch web content | WebFetch |
| Read file contents | Read |
| Edit existing files | Edit (simple snippet replacement) |
| Create new files | Write |
| Delete files | Delete |
| Move/rename files | Move |
| Copy files | Copy |
| List directory contents | ListDir |
| Find files by pattern | Glob |
| Run shell commands | Pwsh |
| Check git status | GitStatus |
| View diffs | GitDiff |
| Revert changes | GitRestore |
| Stage files | GitAdd |
| Commit changes | GitCommit |
| View commit history | GitLog |
| Manage branches | GitBranch |
| Switch branches | GitCheckout |
| Stash changes | GitStash |
| Show commit details | GitShow |
| Show line authors | GitBlame |
| Extract C++ structure | Outline |

---

## Architecture

### High-Level Component Diagram

```
+------------------+     +-------------------+     +-------------------+
|   MCP Client     |     |   tools           |     |   External Web    |
|   (Codex Agent)  |<--->|   (JSON-RPC 2.0)  |<--->|                   |
+------------------+     +-------------------+     +-------------------+
        |                        |
        v                        v
   stdin/stdout           +------+------+------+------+
                          |      |      |      |      |
                     WebFetch     Git  Search   Edit
                          |        |      |      |
                     +-----+------+------+------+------+
                     | HTTP/Browser |      Local FS       |
                     +--------------+---------------------+
```

### Module Organization

```text
tools-mcp-server/        # Binary crate: stdin/stdout loop, routing, composition
tools-mcp-core/          # Shared MCP/runtime support and tool registry
tools-mcp-webfetch/      # WebFetch tool and fetch pipeline
tools-mcp-local/         # Read/Edit/Write/Delete/Glob/Search/Outline/Pwsh and smart_file_edit
tools-mcp-git/           # Git tool implementations
```

---

## Core Modules

### main.rs - MCP Server Implementation

**Location**: `tools-mcp-server/src/main.rs`

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

/// Tool execution outcome structure
struct ToolCallOutcome {
    result: Value,
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
| Ping | `ping`, `mcp/ping` |
| Initialize | `mcp/initialize`, `initialize`, `server/initialize` |
| Tools List | `mcp/tools/list`, `tools/list`, `server/tools/list`, `mcp/capabilities`, `capabilities` |
| Tool Call | `mcp/tools/call`, `tools/call`, `server/tools/call` |
| Shutdown | `mcp/shutdown`, `shutdown`, `server/shutdown` |

---

### webfetch/mod.rs - Web Content Fetching

**Location**: `tools-mcp-webfetch/src/webfetch/mod.rs`

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

**Location**: `tools-mcp-webfetch/src/webfetch/http.rs`

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

**Location**: `tools-mcp-webfetch/src/webfetch/heuristics.rs`

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

**Location**: `tools-mcp-local/src/smart_file_edit/mod.rs`

Internal implementation for the `Edit` tool that provides surgical file editing while preserving original line endings (LF, CRLF, or CR).

#### Internal Architecture

The module handles the complexity of cross-platform text editing:

1. **Canonical LF Processing**: File content is normalized to LF internally for consistent string matching
2. **Offset Mapping**: Maintains bidirectional mappings between canonical positions and file byte positions
3. **Line Ending Detection**: Tracks statistics to determine the dominant newline style
4. **Byte-Precise Replacement**: Writes replacement bytes while preserving the original file's line ending style

#### Key Internal Types

```rust
/// Detected newline styles
enum NewlineKind {
    Lf,    // Unix: \n
    CrLf,  // Windows: \r\n
    Cr,    // Classic Mac: \r
    None,  // No newlines in file
}

/// Complete in-memory file representation
struct FileModel {
    bytes: Vec<u8>,           // Raw file content
    hash: String,             // SHA-256 hash for staleness detection
    canonical: CanonicalData, // LF-normalized view
    newline_stats: NewlineStats,
}
```

#### Public Interface

```rust
/// Simplified edit handler used by the Edit tool
/// Replaces old_snippet with new_snippet while preserving newlines
pub async fn handle_edit(_id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### git/mod.rs - Git Operations

**Location**: `tools-mcp-git/src/git/mod.rs`

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
pub async fn handle_git_status(id: Option<Value>, args: Value) -> ToolCallOutcome
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
pub async fn handle_git_diff(id: Option<Value>, args: Value) -> ToolCallOutcome
```

#### GitRestore

```rust
/// Discards uncommitted changes (DESTRUCTIVE)
///
/// # Parameters
/// - paths: String[] (required) - Files to restore
/// - staged: bool (default: false) - Restore staging area
/// - worktree: bool (default: true) - Restore working tree
pub async fn handle_git_restore(id: Option<Value>, args: Value) -> ToolCallOutcome
```

#### GitAdd

```rust
/// Stages files for commit
///
/// # Parameters
/// - paths: String[]? - Files to stage
/// - all: bool (default: false) - Stage all changes (-A)
/// - update: bool (default: false) - Stage modified/deleted only (-u)
pub async fn handle_git_add(id: Option<Value>, args: Value) -> ToolCallOutcome
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
pub async fn handle_git_commit(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### search.rs - File Search

**Location**: `tools-mcp-local/src/tools/search.rs`

Provides file content search using an automatic in-memory fast path for eligible literal-looking, seeded-regex, fixed-string, and fuzzy fixed-string queries, with ugrep fallback for unsupported regex and search modes.

```rust
/// Searches files using regex or fuzzy patterns
///
/// # Backend Selection
/// - Uses the in-memory POC for eligible literal-looking, seeded-regex, fixed-string, and fuzzy fixed-string queries
/// - Falls back to ugrep for unsupported regex and unsupported cases
/// - Fuzzy parameter present (1-4): memory-backed only for eligible fixed-string searches; otherwise falls back to ugrep -Z<N>
/// - Regex metacharacter patterns are memory-backed only when they are case-sensitive, line-oriented, compile with the Rust byte regex verifier, and have a proven required literal seed of at least three bytes
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
pub async fn handle_search(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### read_file.rs - File Reading

**Location**: `tools-mcp-local/src/tools/handlers/read_file.rs`

```rust
/// Reads file contents with optional line range
///
/// # Parameters
/// - path: String (required) - File path to read
/// - start_line: usize (1-based) - First line to read
/// - end_line: usize (1-based, inclusive) - Last line to read
/// - show_line_numbers: bool (default: false) - Prefix lines with numbers
///
/// # Response
/// - content: File text (with optional line numbers)
/// - total_lines: Total lines in file
/// - start_line/end_line: Actual range returned
pub async fn handle_read_file(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### write.rs - File Creation

**Location**: `tools-mcp-local/src/tools/write.rs`

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
pub async fn handle_write(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### delete.rs - File Deletion

**Location**: `tools-mcp-local/src/tools/delete.rs`

```rust
/// Deletes a file (DESTRUCTIVE)
///
/// # Parameters
/// - path: String (required) - File to delete
///
/// # Restrictions
/// - Only files can be deleted (not directories)
/// - Fails if file doesn't exist
pub async fn handle_delete(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### glob.rs - File Globbing

**Location**: `tools-mcp-local/src/tools/glob.rs`

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
pub async fn handle_glob(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

### outline.rs - C++ Structure Extraction

**Location**: `tools-mcp-local/src/tools/outline.rs`

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
pub async fn handle_outline(id: Option<Value>, args: Value) -> ToolCallOutcome
```

---

## MCP Tools

### WebFetch

Fetch and normalize external web content with caching and JS-aware rendering.

- **Tool name**: `WebFetch`
- **Required**:
  - `url` – absolute URL to fetch.
- **Optional**:
  - `max_chunk_tokens` (integer, default 600) – approximate token budget per chunk. Uses OpenAI's `cl100k_base` tokenizer (GPT-4 compatible).
  - `no_cache` (boolean) – when true, bypasses the on-disk cache and forces a fresh fetch.
  - `force_browser` (boolean) – when true, forces headless browser rendering even if heuristics do not flag the page as JS-heavy.
- **Behavior**:
  - Builds a hardened HTTP client, validates the URL against SSRF rules, and enforces `robots.txt`.
  - Caches responses under the system temp directory (`/tmp/tools-webfetch` on Unix, `%TEMP%\tools-webfetch` on Windows) keyed by URL + method.
  - Extracts readable content and produces Markdown.
  - Uses heuristics to detect JavaScript-heavy pages and, where possible, re-renders them via a headless Chrome/Chromium browser. Browser rendering is disabled by default unless the `WEBFETCH_ENABLE_BROWSER_UNSAFE=true` environment variable is set.
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

Fast local search that automatically uses the in-memory POC for common literal searches, including literal-looking default patterns and eligible fuzzy fixed-string searches, and delegates regex and unsupported cases to ugrep.

- **Tool name**: `Search`
- **Required**:
  - `pattern` - regex pattern to search for.
- **Optional**:
  - `path` - file or directory root (default: current working directory).
  - `case` - `"smart"` (default), `"sensitive"`, or `"insensitive"`.
  - `fixed_strings` (boolean, default `false`) - treat pattern as a literal string (`-F`).
  - `word_regexp` (boolean, default `false`) - match on word boundaries only (`-w`).
  - `glob` (string[]) - glob filters to include files.
  - `hidden` (boolean, default `false`) - search hidden files/directories.
  - `follow` (boolean, default `false`) - follow symlinks.
  - `no_ignore` (boolean, default `false`) - do not respect ignore files like `.gitignore`.
  - `context` (integer, minimum `0`, default `0`) - lines of context on both sides.
  - `max_results` (integer, minimum `1`, maximum `10000`, default `200`) - maximum match/context events to return.
  - `timeout_ms` (integer, minimum `100`, default `20000`) - overall timeout in milliseconds.
  - `fuzzy` (integer, minimum `1`, maximum `4`) - fuzzy match tolerance (1-4 edits).
- **Response**:
  - `content[0].text` - readable text output.
  - `matches` - structured match results.
- **Notes**:
  - Requires `ugrep` to be installed and discoverable on PATH on the machine running the MCP server for unseeded or unsupported regexes, unsupported fuzzy modes, and fallback behavior.
  - Backend selection is automatic; there is no environment flag or MCP request parameter for choosing the backend.
  - When the MCP server starts inside a Git worktree, it starts a best-effort background warm-cache thread for the repository root using the default file-selection shape (`hidden=false`, `follow=false`, `no_ignore=false`, no globs). This warmup does not block stdin/stdout startup and logs only to stderr.
  - Eligible literal, seeded-regex, and narrow fuzzy fixed-string requests use the in-memory POC. Unsupported or ambiguous requests fall back to ugrep.
  - The in-memory eligible subset is conservative:
    - Exact literals: `word_regexp=false`, no `fuzzy`, at least three bytes, no newlines, and either `fixed_strings=true` or a plain regex pattern with no regex metacharacters.
    - Exact literal case handling: `case=sensitive` is byte-exact; `case=insensitive` and lowercase `case=smart` use ugrep-compatible ASCII case folding for fixed strings. Plain regex literals use memory for ASCII case-insensitive matching and fall back for Unicode regex case folding.
    - Fuzzy literals: `fixed_strings=true`, `case=sensitive`, `word_regexp=false`, and `fuzzy` set to `1` through `4`, when the pattern has no newline and can be partitioned into `fuzzy + 1` contiguous UTF-8 seed segments of at least three bytes each.
    - Seeded regexes: `fixed_strings=false`, `case=sensitive`, `word_regexp=false`, no `fuzzy`, no symlink following, valid UTF-8 text scope, line-oriented syntax, successful Rust byte-regex compilation, and at least one proven required literal seed of three or more bytes. Common default-ugrep ERE constructs such as anchors, character classes, grouping, alternation, greedy repetition, and lazy repetition may be memory-backed when the seed planner can prove coverage.
    - Glob include filters can remain memory-backed for eligible fixed-string searches when file-selection semantics can be preserved; otherwise the request falls back to ugrep.
    - File-selection semantics (`path`, `glob`, `hidden`, `follow`, `no_ignore`, binary handling, and size limits) must be preserved exactly; otherwise the request falls back to ugrep.
  - The in-memory POC falls back for regex fuzzy searches, word-regexp searches, symlink-following searches, unseeded regexes, unsupported regex dialect constructs such as `(?...)`, multiline-capable regexes, Unicode regex case-folding cases, fuzzy patterns that cannot produce the required seeds, incomplete index coverage, stale verification, or exceeded index limits.
  - Additional response fields are additive and do not replace existing fields. Memory-backed responses include `backend: "memory"` and may include diagnostics such as `index_cache`, `index_generation`, `indexed_files`, `indexed_bytes`, `candidate_count`, `fuzzy_seed_count`, `fuzzy_verified_lines`, `phase_one_ms`, and `phase_two_ms`. Fallback responses may include `backend: "ugrep"` and `fallback_reason`.
  - Resource limits are configurable with `TOOLS_SEARCH_INDEX_MAX_FILE_BYTES` (default 1 MiB), `TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES` (default 256 MiB per file-selection key), `TOOLS_SEARCH_INDEX_MAX_FILES` (default 50,000), `TOOLS_SEARCH_MAX_CANDIDATES` (default 20,000), `TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS` (default 300,000), and the fuzzy verifier limits `TOOLS_SEARCH_MAX_FUZZY_PATTERN_CHARS` (default 512), `TOOLS_SEARCH_MAX_FUZZY_VERIFIED_LINES` (default 200,000), and `TOOLS_SEARCH_MAX_FUZZY_LINE_CHARS` (default 16,384). Existing `timeout_ms` and `max_results` still apply.
  - Limitations: this POC is not semantic search, does not provide ranking or embeddings, does not persist an on-disk index across server processes, does not require file-system watchers, does not accelerate `Read`, and is not a full ugrep replacement or final Hauberk design.

### Read

Read a local file (optionally a line range) for quick inspection without uploads.

- **Tool name**: `Read`
- **Required**:
  - `path` - filesystem path to read.
- **Optional**:
  - `start_line`, `end_line` (1-based, inclusive).
  - `show_line_numbers` (default: false) - set to `true` to include line numbers.
- **Response**:
  - `content[0].text` is raw file content by default (set `show_line_numbers: true` for numbered output similar to `nl -ba` / `cat -n`).
  - Includes `path`, `start_line`, `end_line`, and `total_lines`.

### Edit

Edit files by replacing a snippet, preserving original newline bytes and whitespace.

- **Tool name**: `Edit`
- **Required**:
  - `path` - filesystem path to edit.
  - `old_snippet` - exact text to find and replace (must use LF newlines).
  - `new_snippet` - replacement text (use LF newlines; file's original line endings are preserved).
- **Optional**:
  - `match_hint` - `{ start_line, end_line }` to restrict the search range when multiple matches exist.
  - `file_hash` - expected current file hash; stale hashes return `status: "stale_file"` without writing.
  - `region_id` - caller-supplied region identifier echoed in successful edit metadata.
- **Behavior**:
  - Searches for `old_snippet` in the file (within `match_hint` range if provided).
  - Replaces with `new_snippet` while preserving the file's dominant line ending style (LF, CRLF, or CR).
  - Returns structured result with status, byte range replaced, and hash values.
- **Response**:
  - `status`: `"ok"`, `"no_match"`, or `"stale_file"`.
  - On success: `replaced_byte_range`, `lines`, `bytes_written`, `file_hash_before`, `file_hash_after`, `newline_kind`, `action`, `region_id`.
  - On no_match: includes suggested candidate ranges with similarity scores.

### Pwsh

Run PowerShell commands via pwsh with timeout and stdout/stderr capture.

- **Tool name**: `Pwsh`
- **Required**:
  - `command` - PowerShell command to execute.
- **Optional**:
  - `timeout_ms` - timeout in milliseconds (default 60000, max: 300000).
  - `working_dir` - working directory for the command (default: current directory).
- **Response**:
  - Returns a JSON summary (including `stdout`, `stderr`, `exit_code`, `timed_out`) in `content[0].text` (compact by default; set `TOOLS_PRETTY_JSON=true` to pretty-print).

### Move

Move or rename a file or directory.

- **Tool name**: `Move`
- **Required**:
  - `source` - Source path to move.
  - `destination` - Destination path (or directory to move into).
- **Optional**:
  - `overwrite` - Overwrite destination if it exists (default: false).
- **Response**:
  - Returns success/failure status with source and destination paths.

### Copy

Copy a file or directory.

- **Tool name**: `Copy`
- **Required**:
  - `source` - Source path to copy.
  - `destination` - Destination path (or directory to copy into).
- **Optional**:
  - `overwrite` - Overwrite destination if it exists (default: false).
  - `recursive` - Copy directories recursively (default: false).
- **Response**:
  - Returns success/failure status with source and destination paths.

### ListDir

List directory contents with file metadata.

- **Tool name**: `ListDir`
- **Required**:
  - `path` - Directory path to list.
- **Optional**:
  - `all` (boolean, default `false`) - include hidden files (starting with `.`).
  - `long` (boolean, default `false`) - show detailed information (size, modified time).
- **Response**:
  - `content[0].text` - human-readable listing (filenames with `/` suffix for dirs, `@` for symlinks; or `d <size> <name>` format when `long` is true).
  - `path` - the directory path listed.
  - `count` - number of entries returned.
  - `entries` - array of objects with `name`, `type` (`file`/ `dir`/ `symlink`), and optionally `size` and `modified` (Unix epoch seconds) when `long` is true.

### Ping

Simple health check for MCP clients.

- **Tool name**: `Ping`
- **Behavior**:
  - Always returns `pong` in a JSON `content` array.
- Useful for MCP client connectivity tests or keepalive pings.

### GeminiGate

Approve only Gemini phases 1 through 4.

- **Tool name**: `GeminiGate`
- **Required**:
  - `phase` - phase string to evaluate.
- **Behavior**:
  - Returns `Approved` when `phase` is exactly `"1"`, `"2"`, `"3"`, or `"4"`.
  - Returns `Rejected` for any other string value.
- **Response**:
  - Always returns `content[0].text` as either `Approved` or `Rejected` with `isError: false`.

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
  - `content[0].text` - same as command output (trimmed trailing newlines); use `clean` when you only need a boolean working-tree summary.
  - Includes `stdout`, `stderr`, `exit_code`, `timed_out`, `clean`, and the executed `args`.

### GitDiff

Run `git diff` with optional flags and output truncation. When `from_ref`, `to_ref`, and `output_dir` are provided, writes per-file patches to the directory.

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
  - `from_ref` (string) - starting ref (tag/branch/commit) for ref-to-ref comparison.
  - `to_ref` (string) - ending ref (tag/branch/commit) for ref-to-ref comparison.
  - `output_dir` (string) - directory to write per-file patches (creates if missing). Required with `from_ref`/`to_ref`.
- **Response**:
  - `content[0].text` - diff output (or `no diff`; or summary text when using ref-to-ref mode).
  - Includes `stdout`, `stderr`, `exit_code`, `timed_out`, `truncated_stdout`, and the executed `args`.
  - In ref-to-ref mode: also includes `from_ref`, `to_ref`, `output_dir`, `summary`, and `files`.

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

### GitLog

Show commit history with configurable format and filters.

- **Tool name**: `GitLog`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `max_count` (integer) - Limit number of commits to show.
  - `oneline` (boolean, default false) - Show each commit on a single line.
  - `format` (string) - Pretty-print format (e.g., '%H %s' for hash and subject).
  - `author` (string) - Filter commits by author.
  - `since` (string) - Show commits after date (e.g., '2024-01-01', '2 weeks ago').
  - `until` (string) - Show commits before date.
  - `grep` (string) - Filter commits by message pattern.
  - `path` (string) - Show commits affecting this path.
  - `max_bytes` (integer, default 200000) - Maximum output bytes.
- **Response**:
  - Returns commit history output.

### GitBranch

List, create, rename, or delete branches.

- **Tool name**: `GitBranch`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `list_all` (boolean, default false) - List both local and remote branches (`-a`).
  - `list_remote` (boolean, default false) - List only remote branches (`-r`).
  - `create` (string) - Create a new branch with this name.
  - `delete` (string) - Delete this branch (`-d`, must be merged).
  - `force_delete` (string) - Force delete this branch (`-D`).
  - `rename` (string) - Rename this branch (requires `new_name`).
  - `new_name` (string) - New name when renaming a branch.
- **Response**:
  - Returns branch operation result.

### GitCheckout

Switch branches or restore working tree files.

- **Tool name**: `GitCheckout`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `branch` (string) - Branch to switch to.
  - `create_branch` (string) - Create and switch to a new branch (`-b`).
  - `commit` (string) - Checkout a specific commit (detached HEAD).
  - `paths` (string[]) - Restore these paths from HEAD or specified commit.
- **Response**:
  - Returns checkout operation result.

### GitStash

Stash changes in a dirty working directory.

- **Tool name**: `GitStash`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `action` (string, enum: ["push", "pop", "apply", "drop", "list", "show", "clear"], default: "push") - Stash action to perform. Note: "save" is also accepted as an alias for "push".
  - `message` (string) - Message for the stash (with push).
  - `index` (integer) - Stash index for pop/apply/drop/show.
  - `include_untracked` (boolean, default false) - Include untracked files (with push).
- **Response**:
  - Returns stash operation result.

### GitShow

Show commit details and diff.

- **Tool name**: `GitShow`
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `commit` (string) - Commit to show (default: HEAD).
  - `stat` (boolean, default false) - Show diffstat only.
  - `name_only` (boolean, default false) - Show only changed file names.
  - `format` (string) - Pretty-print format.
  - `max_bytes` (integer, default 200000) - Maximum output bytes.
- **Response**:
  - Returns commit details and diff output.

### GitBlame

Show line-by-line author information for a file.

- **Tool name**: `GitBlame`
- **Required**:
  - `path` (string) - File path to show blame for.
- **Optional**:
  - `working_dir` (string) - working directory for the command.
  - `timeout_ms` (integer, default 30000) - timeout in milliseconds.
  - `commit` (string) - Blame at specific commit (default: HEAD).
  - `start_line` (integer, minimum `1`) - Start line number for range (1-indexed).
  - `end_line` (integer, minimum `1`) - End line number for range (inclusive).
  - `max_bytes` (integer, default 200000) - Maximum output bytes.
- **Response**:
  - Returns blame output showing line authors.

### Write

Create a new file (fails if the file already exists).

- **Tool name**: `Write`
- **Required**:
  - `path` (string) - file path to create
  - `content` (string) - file content
- **Response**:
  - `content[0].text` - human-readable message (e.g., `"Created /path (123 bytes)"`).
  - `path` - the file path created.
  - `bytes` - number of bytes written.

### Delete

Delete a file (symlinks and directories are explicitly rejected for safety).

- **Tool name**: `Delete`
- **Required**:
  - `path` (string) - file to delete
- **Behavior**:
  - Rejects symlinks to prevent TOCTOU races and deletion of files outside the workspace.
  - Rejects directories; this tool only deletes regular files.
- **Response**:
  - `content[0].text` - human-readable message (e.g., `"Deleted /path"`).
  - `path` - the file path deleted.

### Glob

List files matching a glob pattern.

- **Tool name**: `Glob`
- **Required**:
  - `pattern` (string) - glob pattern with brace expansion (e.g., `**/*.rs`, `src/*.{ts,tsx}`)
- **Optional**:
  - `path` (string, default: ".") - base directory
  - `hidden` (boolean, default: false) - include hidden files
  - `limit` (integer, default: 1000, max: 10000) - maximum files to return
- **Response**:
  - `content[0].text` - newline-separated list of matching files (or message if none match).
  - `pattern` - the glob pattern used.
  - `base_path` - the base directory searched.
  - `count` - number of files returned.
  - `files` - array of matching file paths.
  - `truncated` (boolean, optional) - true if the result was truncated by the limit.

### Outline

Extract C++ structure.

- **Tool name**: `Outline`
- **Required**:
  - `path` (string) - C++ file path
- **Optional**:
  - `include_private` (boolean, default: false) - include private members
- **Response**:
  - `content[0].text` - extracted class/struct signatures and function declarations.
  - `path` - the file path processed.
  - `bytes` - size of the source file.
  - `outline_bytes` - size of the extracted outline.

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
| -32700 | Parse error (invalid JSON or malformed Content-Length frame) |
| -32600 | Invalid Request (malformed JSON-RPC envelope or invalid batch item) |
| -32601 | Method not found / Unknown tool |
| -32602 | Invalid params (for example, malformed `tools/call` parameters) |
| -32603 | Internal error |

### Initialize Response

```json
{
  "protocolVersion": "2025-03-26",
  "serverInfo": {
    "name": "tools-mcp-server",
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
| MCP_SKIP_HEADERS | No | Set to "true" for raw JSON output (no Content-Length headers) |
| RUST_LOG | No | Logging level (debug, info, warn, error) |
| APP_VERSION | No | Version string exposed in server info |
| HOME | No | Home directory for cache storage |
| TOOLS_PRETTY_JSON | No | Set to "true" (or 1/yes/on) to pretty-print JSON payloads returned as text (default: compact) |
| TOOLS_SEARCH_INDEX_MAX_FILE_BYTES | No | Maximum file size for the in-memory Search POC index (default: 1 MiB) |
| TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES | No | Maximum indexed bytes per Search file-selection key (default: 256 MiB) |
| TOOLS_SEARCH_INDEX_MAX_FILES | No | Maximum indexed files per Search file-selection key (default: 50,000) |
| TOOLS_SEARCH_MAX_CANDIDATES | No | Maximum candidate files verified by the in-memory Search POC (default: 20,000) |
| TOOLS_SEARCH_MAX_FUZZY_PATTERN_CHARS | No | Maximum fuzzy fixed-string pattern length verified by the in-memory Search POC (default: 512 Unicode scalar values) |
| TOOLS_SEARCH_MAX_FUZZY_VERIFIED_LINES | No | Maximum candidate lines verified by the in-memory fuzzy Search POC (default: 200,000) |
| TOOLS_SEARCH_MAX_FUZZY_LINE_CHARS | No | Maximum line length verified by the in-memory fuzzy Search POC (default: 16,384 Unicode scalar values) |
| TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES | No | Maximum compiled regex verifier size for eligible in-memory seeded regex searches (default: 10 MiB) |
| WEBFETCH_ENABLE_BROWSER_UNSAFE | No | Set to "true" to enable headless browser rendering in WebFetch (disabled by default) |

### Cache Locations

| Cache | Path | Purpose |
|-------|------|---------|
| WebFetch content | System temp dir (`/tmp/tools-webfetch` on Unix, `%TEMP%\tools-webfetch` on Windows) | HTTP response cache |

### MCP Client Configuration Example

```toml
[mcp_servers.tools]
command = "/path/to/tools-mcp/target/release/tools-mcp-server"
env = {
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
- Browser pool lifecycle management prevents resource leaks (restarts after 100 requests or 1 hour)
- Resource blocking: images, fonts, video/audio autoplay
- Network idle detection timeout: 2s (with 20s safety cap on total wait)

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
| base64 | 0.22 | Base64 encoding/decoding |
| chromiumoxide | 0.7 | Headless Chrome via CDP |
| htmd | 0.4 | HTML to Markdown conversion |
| scraper | 0.24 | HTML DOM parsing |
| robotstxt | 0.3 | robots.txt parsing |
| tiktoken-rs | 0.9 | OpenAI tokenizer |
| url | 2.5 | URL parsing |

### File Operations

| Crate | Version | Purpose |
|-------|---------|---------|
| glob | 0.3 | Glob pattern matching |
| ignore | 0.4 | .gitignore-aware walking |
| regex | 1.12 | Byte regex verification for eligible in-memory Search regex queries |
| regex-syntax | 0.8 | Regex HIR parsing and required literal seed analysis for Search |
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
4. **Process Errors**: Command spawn failed, timeout, non-zero exit

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

### Benchmarks

```bash
# Run the Read tool micro-benchmark
READ_FILE_BENCH_ITERS=100 cargo bench -p tools-mcp-local --bench read_file
```

### Coverage (Local HTML)

This repo supports local code coverage via `cargo llvm-cov` (HTML output).

Prerequisites:
- Install the Rust LLVM tools: `rustup component add llvm-tools-preview`
- Install the cargo subcommand: `cargo install cargo-llvm-cov`

Run directly:
```bash
cargo llvm-cov --workspace --html --output-dir coverage
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

The server can be tested via stdin/stdout. Use `MCP_SKIP_HEADERS=true` for raw JSON line output:

```bash
# Initialize
echo '{"jsonrpc":"2.0","id":1,"method":"mcp/initialize","params":{}}' | MCP_SKIP_HEADERS=true cargo run -p tools-mcp-server

# List tools
echo '{"jsonrpc":"2.0","id":2,"method":"mcp/tools/list","params":{}}' | MCP_SKIP_HEADERS=true cargo run -p tools-mcp-server

# Read file
echo '{"jsonrpc":"2.0","id":3,"method":"mcp/tools/call","params":{"name":"Read","arguments":{"path":"Cargo.toml"}}}' | MCP_SKIP_HEADERS=true cargo run -p tools-mcp-server
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
- ugrep for Search baseline and fallback behavior

**Optional**:
- Chrome/Chromium for browser rendering (WebFetch)
- Git for git tools

---

*Generated documentation for tools v1.0.0*
