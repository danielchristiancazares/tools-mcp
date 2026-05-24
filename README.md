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
| Health check | `Ping` | [docs/tools/ping.md](./docs/tools/ping.md) |
| Fetch and chunk web content | `WebFetch` | [docs/tools/webfetch.md](./docs/tools/webfetch.md) |
| Search by regex / literal / fuzzy | `Search` | [docs/tools/search.md](./docs/tools/search.md) |
| Search and expand matches into file windows | `search_context` | [docs/tools/search-context.md](./docs/tools/search-context.md) |
| Build or refresh semantic code index | `SemanticIndex` | [docs/tools/semantic-index.md](./docs/tools/semantic-index.md) |
| Natural-language semantic code search | `SemanticSearch` | [docs/tools/semantic-search.md](./docs/tools/semantic-search.md) |
| Read a file (optionally a line range) | `Read` | [docs/tools/read.md](./docs/tools/read.md) |
| Edit a file by snippet replacement (preserves newlines) | `Edit` | [docs/tools/edit.md](./docs/tools/edit.md) |
| Create a new file | `Write` | [docs/tools/write.md](./docs/tools/write.md) |
| Delete a file | `Delete` | [docs/tools/delete.md](./docs/tools/delete.md) |
| Move or rename a file or directory | `Move` | [docs/tools/move.md](./docs/tools/move.md) |
| Copy a file or directory | `Copy` | [docs/tools/copy.md](./docs/tools/copy.md) |
| List directory entries | `ListDir` | [docs/tools/list-dir.md](./docs/tools/list-dir.md) |
| Find files by glob pattern | `Glob` | [docs/tools/glob.md](./docs/tools/glob.md) |
| Extract source structure (Rust, TS/JS, Python, Go, C++, Markdown) | `Outline` | [docs/tools/outline.md](./docs/tools/outline.md) |
| Run PowerShell commands **(gated: `MCP_ENABLE_PWSH_TOOL=true`)** | `Pwsh` | [docs/tools/pwsh.md](./docs/tools/pwsh.md) |
| Read-only worktree snapshot **(gated: `MCP_ENABLE_GIT=true`)** | `git_snapshot` | [docs/tools/git-snapshot.md](./docs/tools/git-snapshot.md) |
| `git status` **(gated)** | `GitStatus` | [docs/tools/git-status.md](./docs/tools/git-status.md) |
| `git diff` **(gated)** | `GitDiff` | [docs/tools/git-diff.md](./docs/tools/git-diff.md) |
| `git restore` **(gated)** | `GitRestore` | [docs/tools/git-restore.md](./docs/tools/git-restore.md) |
| `git add` **(gated)** | `GitAdd` | [docs/tools/git-add.md](./docs/tools/git-add.md) |
| `git commit` (Conventional Commits) **(gated)** | `GitCommit` | [docs/tools/git-commit.md](./docs/tools/git-commit.md) |
| `git log` **(gated)** | `GitLog` | [docs/tools/git-log.md](./docs/tools/git-log.md) |
| `git branch` (list/create/rename/delete) **(gated)** | `GitBranch` | [docs/tools/git-branch.md](./docs/tools/git-branch.md) |
| `git checkout` **(gated)** | `GitCheckout` | [docs/tools/git-checkout.md](./docs/tools/git-checkout.md) |
| `git stash` **(gated)** | `GitStash` | [docs/tools/git-stash.md](./docs/tools/git-stash.md) |
| `git show` **(gated)** | `GitShow` | [docs/tools/git-show.md](./docs/tools/git-show.md) |
| `git blame` **(gated)** | `GitBlame` | [docs/tools/git-blame.md](./docs/tools/git-blame.md) |

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
