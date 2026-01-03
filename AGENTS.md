# Repository Guidelines

## Project Overview
Rust MCP server (JSON-RPC 2.0 over stdin/stdout) with tools for code search (OpenAI vector stores), web fetching, and newline-safe file editing.

## Project Structure
- `src/main.rs` — MCP protocol + tool routing.
- `src/lib.rs` — OpenAI/vector-store client.
- `src/codequery/`, `src/webfetch/`, `src/smart_file_edit/` — tool implementations.
- `src/tools/handlers/read_file.rs` — raw file reader tool.
- `tests/` — integration tests; `target/` — build output (generated).

## Commands
- `cargo build --release` — build release binary.
- `cargo run --release` — run server locally.
- `cargo test` — run tests (some are `#[ignore]`).
- `cargo fmt` / `cargo clippy --all-targets` — format/lint.

Env vars:
- `OPENAI_API_KEY` — required for OpenAI-backed tools.
- `MCP_SKIP_HEADERS=true` — no `Content-Length` framing.
- `RUST_LOG=debug` — verbose logs.
- `APP_VERSION=...` - baked into init responses.

## Style & Testing
- Keep changes `cargo fmt`-clean; follow standard Rust naming (`snake_case`, `CamelCase`).
- Keep network/OpenAI tests ignored by default; run with `OPENAI_API_KEY` via `cargo test -- --ignored`.
- If you change tool schemas or response shapes, update `README.md` and `tests/integration_test.rs`.

## Commits & Pull Requests
- Prefer Conventional Commits (e.g., `feat(webfetch): ...`, `perf(webfetch): ...`).
- PRs: include what/why, how to test, and note behavior/security impacts.

## Security Notes
- Don’t weaken WebFetch SSRF/robots.txt protections without strong justification and tests.
- Never commit secrets; use environment variables/local config.
