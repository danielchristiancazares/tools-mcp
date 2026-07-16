# tools-mcp

A Rust workspace for a Model Context Protocol (MCP) server that exposes web fetching, local code search, semantic code search, newline-aware file editing, and Git operations over JSON-RPC 2.0 on stdin/stdout.

The full design surface lives in [`docs/`](./docs/README.md). Every tool has its own Spec-Driven Design (SDD) document; this README is the entry point and quick start only.

## Quick Start

```bash
# Build
cargo build --workspace --release

# Run (reads JSON-RPC on stdin, writes responses to stdout)
cargo run -p tools-mcp-server --release
```

### Requirements

- Rust toolchain 1.94 or newer (edition 2024).
- `ugrep` in `PATH` (used by the `Search` tool when the in-memory fast path is not eligible).
- `protoc` in `PATH`, or `PROTOC` set to a `protoc` executable, to build LanceDB/Lance for the semantic-search dependencies.
- Linux only: `libssl-dev` for HTTPS support.
- Optional: Chrome or Chromium for `WebFetch` browser rendering. Without it (or without setting `WEBFETCH_ENABLE_BROWSER_UNSAFE=true`), `WebFetch` runs HTTP-only.
- Optional: Azure CLI for `AdoWorkItems`. Sign in with `az login`; the tool mints a short-lived Azure DevOps token via `az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 --query accessToken -o tsv`. No PAT or token env vars are used; the token audience is overridable per call via the tool's `resource` argument.

### MCP Client Configuration

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.tools-mcp]
command = "/absolute/path/to/tools-mcp/target/release/tools-mcp-server"
env = { MCP_SKIP_HEADERS = "true", RUST_LOG = "error" }
```

Set `MCP_SKIP_HEADERS=true` when the client expects raw JSON lines instead of `Content-Length`-framed messages.

## Tools

Each row links to the tool's design contract under [`docs/tools/`](./docs/README.md). Tools marked **gated** are disabled by default and require an explicit environment variable to register.

| Task | Tool | Spec |
|---|---|---|
| Health check | **Tool name**: `Ping` | [docs/tools/ping.md](./docs/tools/ping.md) |
| Look up Azure DevOps work items by ID, keyword, or assignee | **Tool name**: `AdoWorkItems` | [docs/tools/ado-work-items.md](./docs/tools/ado-work-items.md) |
| Fetch and chunk web content | **Tool name**: `WebFetch` | [docs/tools/webfetch.md](./docs/tools/webfetch.md) |
| Search by regex / literal / fuzzy | **Tool name**: `Search` | [docs/tools/search.md](./docs/tools/search.md) |
| Search and expand matches into file windows | **Tool name**: `search_context` | [docs/tools/search-context.md](./docs/tools/search-context.md) |
| Build or refresh semantic code index with indexed/updated counts **(gated: `MCP_SEMANTIC_BACKEND` set)** | **Tool name**: `SemanticIndex` | [docs/tools/semantic-index.md](./docs/tools/semantic-index.md) |
| Natural-language semantic code search **(gated: `MCP_SEMANTIC_BACKEND` set)** | **Tool name**: `SemanticSearch` | [docs/tools/semantic-search.md](./docs/tools/semantic-search.md) |
| Read a file (optionally a line range) | **Tool name**: `Read` | [docs/tools/read.md](./docs/tools/read.md) |
| Edit a file by snippet replacement (preserves newlines) | **Tool name**: `Edit` | [docs/tools/edit.md](./docs/tools/edit.md) |
| Create a new file | **Tool name**: `Write` | [docs/tools/write.md](./docs/tools/write.md) |
| Delete a file | **Tool name**: `Delete` | [docs/tools/delete.md](./docs/tools/delete.md) |
| Move or rename a file or directory | **Tool name**: `Move` | [docs/tools/move.md](./docs/tools/move.md) |
| Copy a file or directory | **Tool name**: `Copy` | [docs/tools/copy.md](./docs/tools/copy.md) |
| List directory entries | **Tool name**: `ListDir` | [docs/tools/list-dir.md](./docs/tools/list-dir.md) |
| Count extension files and non-empty lines by child directory | **Tool name**: `CountLines` | [docs/tools/count-lines.md](./docs/tools/count-lines.md) |
| Find files by glob pattern | **Tool name**: `Glob` | [docs/tools/glob.md](./docs/tools/glob.md) |
| Extract source structure (Rust, TS/JS, Python, Go, C++, Markdown) | **Tool name**: `Outline` | [docs/tools/outline.md](./docs/tools/outline.md) |
| Run PowerShell commands **(gated: `MCP_ENABLE_PWSH_TOOL=true`)** | **Tool name**: `Pwsh` | [docs/tools/pwsh.md](./docs/tools/pwsh.md) |
| Read-only worktree snapshot **(gated: `MCP_ENABLE_GIT=true`)** | **Tool name**: `git_snapshot` | [docs/tools/git-snapshot.md](./docs/tools/git-snapshot.md) |
| `git status` **(gated)** | **Tool name**: `GitStatus` | [docs/tools/git-status.md](./docs/tools/git-status.md) |
| `git diff` **(gated)** | **Tool name**: `GitDiff` | [docs/tools/git-diff.md](./docs/tools/git-diff.md) |
| `git apply` supported text patches **(gated)** | **Tool name**: `GitApply` | [docs/tools/git-apply.md](./docs/tools/git-apply.md) |
| Enumerate selectable git diff hunks **(gated)** | **Tool name**: `GitHunks` | [docs/tools/git-hunks.md](./docs/tools/git-hunks.md) |
| Stage or unstage selected git hunks **(gated)** | **Tool name**: `GitStageHunks` | [docs/tools/git-stage-hunks.md](./docs/tools/git-stage-hunks.md) |
| `git restore` **(gated)** | **Tool name**: `GitRestore` | [docs/tools/git-restore.md](./docs/tools/git-restore.md) |
| `git add` **(gated)** | **Tool name**: `GitAdd` | [docs/tools/git-add.md](./docs/tools/git-add.md) |
| `git commit` (Conventional Commits) **(gated)** | **Tool name**: `GitCommit` | [docs/tools/git-commit.md](./docs/tools/git-commit.md) |
| `git log` **(gated)** | **Tool name**: `GitLog` | [docs/tools/git-log.md](./docs/tools/git-log.md) |
| `git branch` (list/create/rename/delete) **(gated)** | **Tool name**: `GitBranch` | [docs/tools/git-branch.md](./docs/tools/git-branch.md) |
| `git checkout` **(gated)** | **Tool name**: `GitCheckout` | [docs/tools/git-checkout.md](./docs/tools/git-checkout.md) |
| `git stash` **(gated)** | **Tool name**: `GitStash` | [docs/tools/git-stash.md](./docs/tools/git-stash.md) |
| `git show` **(gated)** | **Tool name**: `GitShow` | [docs/tools/git-show.md](./docs/tools/git-show.md) |
| `git blame` **(gated)** | **Tool name**: `GitBlame` | [docs/tools/git-blame.md](./docs/tools/git-blame.md) |

## Protocol Error Codes

Observable JSON-RPC error codes returned by the server (full details in [`docs/protocol.md`](./docs/protocol.md)):

| Code | Meaning |
|---|---|
| `-32700` | Parse error — request body is not valid JSON |
| `-32600` | Invalid request — missing or malformed JSON-RPC fields |
| `-32601` | Method not found — unknown method or unknown tool name |
| `-32602` | Invalid params — missing required tool argument |
| `-32603` | Internal error — unexpected server-side failure |

## Cross-Cutting Documentation

| Topic | Doc |
|---|---|
| Workspace layout and tool composition | [docs/architecture.md](./docs/architecture.md) |
| JSON-RPC protocol, method aliases, framing, error codes | [docs/protocol.md](./docs/protocol.md) |
| Complete environment variable catalog | [docs/configuration.md](./docs/configuration.md) |
| Security model: SSRF, robots.txt, sandboxing, trust boundaries | [docs/security.md](./docs/security.md) |
| Workspace dependencies and rationale | [docs/dependencies.md](./docs/dependencies.md) |
| Search backend design (predates SDDs) | [docs/hauberk-in-memory-search-srd.md](./docs/hauberk-in-memory-search-srd.md) |
| Threat model | [docs/tools-mcp-threat-model.md](./docs/tools-mcp-threat-model.md) |

## Testing

```bash
cargo test --workspace                 # all non-ignored tests
cargo test --workspace -- --ignored    # network/host-dependent tests
cargo fmt --all
cargo clippy --workspace --all-targets
```

## License

See repository root for license terms.
