# Repository Review Report

## Scope

Comprehensive codebase review of `tools`, focusing on cleanliness, architecture, and minimal refactoring where clearly beneficial.

## Overall Assessment

- Code structure is clean and modular: transport (MCP) logic resides in `tools-mcp-server/src/main.rs`, local tooling in `tools-mcp-local/src/*`, git tooling in `tools-mcp-git/src/*`, and WebFetch functionality in `tools-mcp-webfetch/src/webfetch/*`.
- Error handling uses `anyhow::Result` with contextual messages across tool handlers.
- Unit and integration tests cover key behaviors; network-dependent tests are gated with `#[ignore]`.
- No overengineering observed; responsibilities are well separated.

## Issues Identified

### High Severity

- **SSRF bypass via hostname resolution** (`tools-mcp-webfetch/src/webfetch/http.rs:62`)
  - Original SSRF guard rejected localhost/private literal IPs but skipped DNS resolution, allowing hostnames that resolve to private networks. Hardened validation by resolving hostnames via `tokio::net::lookup_host` and rejecting private/reserved IPs. [Tokio v1.x lookup_host docs](https://docs.rs/tokio/1/tokio/net/fn.lookup_host.html)

### Low Severity

- **Integration test clippy warnings** (`tools-mcp-server/tests/integration_test.rs`)
  - Removed needless references in `.args`, replaced temporary `vec!` with array literal, and swapped `expect(format!(...))` for `unwrap_or_else(|| panic!(...))` to satisfy `clippy::needless-borrows-for-generic-args`, `clippy::useless-vec`, and `clippy::expect-fun-call`.
- **Robots cache ergonomics** (`tools-mcp-webfetch/src/webfetch/http.rs:132`)
  - Optional improvement: convert `Mutex<Option<HashMap<...>>>` to `once_cell::sync::Lazy`. Deferred because current approach works and change is cosmetic.

## Applied Changes

| File | Summary |
| --- | --- |
| `tools-mcp-webfetch/src/webfetch/http.rs` | Made `validate_url_ssrf` async, added DNS resolution + private IP rejection, documented rationale, and awaited validation in `fetch_document`. |
| `tools-mcp-server/tests/integration_test.rs` | Adjusted `.args` calls, converted alias list to array, replaced `expect(format!(...))` with `unwrap_or_else(|| panic!(...))` for clippy compliance. |

## Testing & Verification

| Command | Result |
| --- | --- |
| `cargo check` | ✅ |
| `cargo clippy --all-targets --all-features -- -D warnings` | ✅ |
| `cargo test` | ✅ (API-dependent tests ignored by design) |

## Assumptions & Open Questions

- WebFetch cache (`/tmp/tools-webfetch`) persists indefinitely. Introduce TTL or pruning only if disk usage becomes a concern.
- MCP responses continue to embed JSON as strings for compatibility; no change made without guidance.

## Future Enhancements (Optional)

1. Add TTL or size management for WebFetch cache entries.
2. Replace manual robots cache initialization with `once_cell::sync::Lazy` for simpler state management.
