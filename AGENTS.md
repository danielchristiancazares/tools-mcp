# Repository Guidelines

## Project Overview
Rust Cargo workspace for an MCP server (JSON-RPC 2.0 over stdin/stdout) with tools for code search (OpenAI vector stores), web fetching, git operations, and newline-safe file editing.

## Project Structure
- `tools-mcp-server/` — binary crate with stdin/stdout loop, JSON-RPC routing, and feature-crate composition.
- `tools-mcp-core/` — shared MCP/runtime support (`mcp_protocol`, `response`, `tool_registry`, `validation`, `process`, `text`, config).
- `openai-file-search-core/` — OpenAI/vector-store client library.
- `tools-mcp-codequery/` — CodeQuery tool and vector-store cache/orchestration.
- `tools-mcp-webfetch/` — WebFetch pipeline and tool registration.
- `tools-mcp-local/` — local file/search/edit tools, including `smart_file_edit`.
- `tools-mcp-git/` — git tool implementations.
- `tools-mcp-server/tests/` — server integration and golden contract tests.
- `target/` — build output (generated).

## Commands
- `cargo build --workspace --release` — build the full workspace.
- `cargo run -p tools-mcp-server --release` — run the server locally.
- `cargo test --workspace` — run tests (some are `#[ignore]`).
- `cargo fmt --all` / `cargo clippy --workspace --all-targets` — format/lint.

Env vars:
- `OPENAI_API_KEY` — required for OpenAI-backed tools.
- `MCP_SKIP_HEADERS=true` — no `Content-Length` framing.
- `RUST_LOG=debug` — verbose logs.
- `APP_VERSION=...` - baked into init responses.

## Style & Testing
- Keep changes `cargo fmt`-clean; follow standard Rust naming (`snake_case`, `CamelCase`).
- Keep network/OpenAI tests ignored by default; run with `OPENAI_API_KEY` via `cargo test --workspace -- --ignored`.
- If you change tool schemas or response shapes, update `README.md` and `tools-mcp-server/tests/integration_test.rs`.

## Commits & Pull Requests
- Prefer Conventional Commits (e.g., `feat(webfetch): ...`, `perf(webfetch): ...`).
- PRs: include what/why, how to test, and note behavior/security impacts.

## Security Notes
- Don’t weaken WebFetch SSRF/robots.txt protections without strong justification and tests.
- Never commit secrets; use environment variables/local config.

## Cursor Cloud specific instructions

### System dependencies
The VM needs **Rust stable ≥ 1.94** (edition 2024), **libssl-dev**, and **ugrep**. The update script handles `cargo build --workspace --release`; system packages are pre-installed in the snapshot.

### Running the MCP server
The server is a stdin/stdout binary — pipe JSON-RPC messages into it. Use `MCP_SKIP_HEADERS=true` for raw JSON (no Content-Length framing). Example:
```
echo '{"jsonrpc":"2.0","id":1,"method":"mcp/tools/call","params":{"name":"ping","arguments":{}}}' \
  | MCP_SKIP_HEADERS=true RUST_LOG=error cargo run -p tools-mcp-server --release 2>/dev/null
```

### Testing caveats
- `cargo test --workspace` runs all non-ignored tests across the workspace. No external services required.
- Tests tagged `#[ignore]` need `OPENAI_API_KEY`; run with `cargo test --workspace -- --ignored`.
- The `Search` tool uses **ugrep** (not ripgrep) as its backend; some integration tests exercise it.
- The app integration tests spawn the compiled `tools-mcp-server` binary via Cargo's `CARGO_BIN_EXE_...` support.
