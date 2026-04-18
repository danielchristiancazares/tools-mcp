# Repository Review Report

## Scope

Comprehensive codebase review of `tools`, focusing on cleanliness, architecture, and minimal refactoring where clearly beneficial.

## Overall Assessment

- Code structure is clean and modular: transport (MCP) logic resides in `tools-mcp-server/src/main.rs`, OpenAI/vector store orchestration in `openai-file-search-core/src/lib.rs`, CodeQuery logic in `tools-mcp-codequery/src/*`, and WebFetch functionality in `tools-mcp-webfetch/src/webfetch/*`.
- Error handling uses `anyhow::Result` with contextual messages; retries and backoff are in place for vector store reindexing (`openai-file-search-core/src/lib.rs:1029`).
- Unit and integration tests cover key behaviors; OpenAI-dependent tests are gated with `#[ignore]`.
- No overengineering observed; responsibilities are well separated.

## Issues Identified

### High Severity

- **SSRF bypass via hostname resolution** (`tools-mcp-webfetch/src/webfetch/http.rs:62`)
  - Original SSRF guard rejected localhost/private literal IPs but skipped DNS resolution, allowing hostnames that resolve to private networks. Hardened validation by resolving hostnames via `tokio::net::lookup_host` and rejecting private/reserved IPs. [Tokio v1.x lookup_host docs](https://docs.rs/tokio/1/tokio/net/fn.lookup_host.html)

### Medium Severity

- **Vector store wait scope** (`openai-file-search-core/src/lib.rs:646`)
  - `wait_for_vector_file_ready` waits for *all* store files to reach `completed`. In multi-tenant or legacy stores this may block indefinitely on unrelated items. Consider enhancing the API to wait only on files uploaded in the current operation.

### Low Severity

- **Clippy performance warning** (`openai-file-search-core/src/lib.rs:374`)
  - `split('/').last()` traverses entire iterator; replaced with `rsplit('/').next()` per `clippy::double-ended-iterator-last` guidance.
- **Integration test clippy warnings** (`tools-mcp-server/tests/integration_test.rs`)
  - Removed needless references in `.args`, replaced temporary `vec!` with array literal, and swapped `expect(format!(...))` for `unwrap_or_else(|| panic!(...))` to satisfy `clippy::needless-borrows-for-generic-args`, `clippy::useless-vec`, and `clippy::expect-fun-call`.
- **Robots cache ergonomics** (`tools-mcp-webfetch/src/webfetch/http.rs:132`)
  - Optional improvement: convert `Mutex<Option<HashMap<...>>>` to `once_cell::sync::Lazy`. Deferred because current approach works and change is cosmetic.

## Applied Changes

| File | Summary |
| --- | --- |
| `tools-mcp-webfetch/src/webfetch/http.rs` | Made `validate_url_ssrf` async, added DNS resolution + private IP rejection, documented rationale, and awaited validation in `fetch_document`. |
| `openai-file-search-core/src/lib.rs` | Replaced `split('/').last()` with `rsplit('/').next()` to avoid full iterator traversal. Added comment explaining choice. |
| `tools-mcp-server/tests/integration_test.rs` | Adjusted `.args` calls, converted alias list to array, replaced `expect(format!(...))` with `unwrap_or_else(|| panic!(...))` for clippy compliance. |

## Testing & Verification

| Command | Result |
| --- | --- |
| `cargo check` | ✅ |
| `cargo clippy --all-targets --all-features -- -D warnings` | ✅ |
| `cargo test` | ✅ (API-dependent tests ignored by design) |

## Assumptions & Open Questions

- Vector stores are generally scoped per repository so waiting on all files is acceptable. If not, we should adjust `wait_for_vector_file_ready` to track specific file IDs.
- WebFetch cache (`/tmp/tools-webfetch`) persists indefinitely. Introduce TTL or pruning only if disk usage becomes a concern.
- MCP responses continue to embed JSON as strings for compatibility; no change made without guidance.

## Future Enhancements (Optional)

1. Scope vector-store readiness waiting to specific uploads.
2. Add TTL or size management for WebFetch cache entries.
3. Replace manual robots cache initialization with `once_cell::sync::Lazy` for simpler state management.
