# SDD: SemanticSearch

**Date:** 2026-05-24
**Scope:** Design contract for the `SemanticSearch` MCP tool.
**Source:** `tools-mcp-semantic/src/tools.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `SemanticSearch` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`SemanticSearch` is the MCP tool that runs a vector-nearest-neighbor query against the workspace's local semantic code index (written by `SemanticIndex`). It loads the active manifest, embeds the caller's natural-language query with the same FastEmbed model (`jina-embeddings-v2-base-code`) used at index time, applies SQL pre-filters (workspace root, optional path scope, optional language), and returns the top-K ranked chunks with optional source content. The tool is owned by the `tools-mcp-semantic` crate; the entry point is `handle_semantic_search` (`tools-mcp-semantic/src/tools.rs:77`), which delegates to `crate::model::search_workspace` (`tools-mcp-semantic/src/model.rs:308`).

### 3.2 Explicitly Out of Scope

- Building or refreshing the index (covered by `SemanticIndex`; see `docs/tools/semantic-index.md`). `SemanticSearch` only reads; it never writes.
- Lexical search (covered by `Search` and `search_context`). Semantic and lexical search are independent surfaces and may disagree.
- JSON-RPC framing and method routing (covered in `docs/protocol.md`).
- Tool-registry composition (covered in `docs/architecture.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `SemanticSearch` |
| Aliases | None |
| Registration gate | Always registered (no env gate) |
| Owning crate | `tools-mcp-semantic` |
| Handler function | `handle_semantic_search` (`tools-mcp-semantic/src/tools.rs:77`) |
| Schema definition | `tools-mcp-semantic/src/tools.rs:133-152` |
| Registration call | `tools-mcp-semantic/src/tools.rs:42` invoked from `tools-mcp-semantic/src/lib.rs:11-13`, wired into the registry by `tools-mcp-server/src/composition.rs:89` |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Query is required and non-empty** — The schema marks `query` as required (`tools-mcp-semantic/src/tools.rs:148`); the handler additionally rejects whitespace-only queries via `validate_non_empty` (`tools-mcp-semantic/src/tools.rs:83-85`, `tools-mcp-core/src/validation.rs:11-22`).
- **Path is resolved inside the server working directory** — `resolve_scope` MUST canonicalize the requested path and refuse anything that escapes the canonical workspace (`tools-mcp-semantic/src/discovery.rs:249-272`).
- **Embedding model and dimension MUST match the index** — The handler reads the manifest's `table_name` and `vector_dim`; if either is missing the call fails (`tools-mcp-semantic/src/model.rs:316-322`). The freshly-embedded query MUST equal `vector_dim` or the call fails (`model.rs:327-334`).
- **Search scope is enforced server-side** — The LanceDB predicate MUST always include the workspace `root`; when the target is narrower than the whole workspace it MUST add a file or directory path predicate (`tools-mcp-semantic/src/store.rs:322-338`, `tools-mcp-semantic/src/discovery.rs:47-63`).
- **Threshold filters by projected distance** — Results whose `_distance` exceeds `threshold` MUST be dropped before they reach the caller (`tools-mcp-semantic/src/store.rs:357-360`). Lower distances are more similar.
- **Limit is clamped** — `limit` MUST be clamped to `1..=100` regardless of the schema bound (`tools-mcp-semantic/src/tools.rs:95`, `tools-mcp-core/src/validation.rs:43-46`).
- **Timeout is clamped** — `timeout_ms` MUST be clamped to `1000..=300000` (`tools-mcp-semantic/src/tools.rs:99`, `tools-mcp-core/src/validation.rs:28-30`).
- **Content projection is honored** — When `include_content = false`, the `content` column MUST NOT be selected from LanceDB and the response MUST omit `content` from each result (`tools-mcp-semantic/src/store.rs:314-320, 137-170`, `tools-mcp-semantic/src/model.rs:140-142`).
- **No panic on failure** — All error paths MUST return `ToolCallOutcome::err_with` from `handle_semantic_search` (`tools-mcp-semantic/src/tools.rs:104-111`); the handler MUST NOT panic.
- **Read-only** — The handler MUST NOT mutate the index, manifest, or LanceDB table.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT search paths outside the server working directory.
- MUST NOT execute a query when the manifest declares a different model or vector dimension than what the embedding provider produced. Cross-model search would silently return garbage.
- MUST NOT silently return results from a different workspace; the `root` predicate scopes every query (`tools-mcp-semantic/src/store.rs:323`).
- MUST NOT execute returned `content` as instructions. Search results are external data; consumers MUST frame them as such. See `docs/security.md`.
- MUST NOT depend on the `_distance` column being present; the score reader falls back to `0.0` when LanceDB omits the column (`tools-mcp-semantic/src/store.rs:432-441, 357`).

## 5. Design Goals

- **Reads are independent of writes.** The handler opens LanceDB read-only via `open_existing` and never modifies the table (`tools-mcp-semantic/src/store.rs:89-104, 137-170`).
- **Cheap fan-out.** Most filtering happens server-side in LanceDB via SQL predicates (`root`, `path`, `language`) so the handler does not pay the cost of scanning the full vector space (`tools-mcp-semantic/src/store.rs:322-338`).
- **Token-aware response.** Each result carries `path`, `start_line`, `end_line`, and an optional `symbol`, plus a single line of human-readable text per result so the response stays compact when callers only need locations (`tools-mcp-semantic/src/model.rs:86-129`).
- **Same model on both sides.** The query is embedded with the same `FastEmbedProvider` (and the same `passage:`/`query:` prefixing discipline) the indexer used, so query-passage alignment matches what `jina-embeddings-v2-base-code` expects (`tools-mcp-semantic/src/embedding.rs:73-92`).

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `query` | string | Yes | — | Non-empty (whitespace-trimmed check) | Natural-language search query. |
| `path` | string | No | `"."` | Non-empty; resolved under the server working directory | Indexed file or directory scope to search. |
| `limit` | integer | No | `10` | `1..=100` (clamped, `tools-mcp-semantic/src/tools.rs:95`) | Maximum number of ranked chunks to return. |
| `language` | string | No | — | Trimmed and lower-cased before use | Optional language filter (e.g., `rust`, `typescript`, `python`, `go`, `markdown`). |
| `threshold` | number | No | — | LanceDB `_distance`; lower is more similar | Drop results whose distance exceeds this value. |
| `include_content` | boolean | No | `true` | — | Include each chunk's source text in the response. |
| `timeout_ms` | integer | No | `60000` | `1000..=300000` (clamped) | Search budget in milliseconds. |

The schema sets `"additionalProperties": false` (`tools-mcp-semantic/src/tools.rs:149`); the deserializer uses `#[serde(deny_unknown_fields)]` (`tools-mcp-semantic/src/tools.rs:23`). Unknown fields produce a tool-level error (`isError: true`) with text `"invalid arguments: ..."` per `ToolCallOutcome::parse_args` (`tools-mcp-core/src/tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-semantic/src/tools.rs:137-150`

### 6.2 Behavior

`handle_semantic_search` and `search_workspace` implement the pipeline below. Each step lists the source location for verification.

1. **Parse and validate arguments** — Deserialize `SemanticSearchRequest`; reject unknown fields (`tools-mcp-semantic/src/tools.rs:78-81`). Validate `query` and `path` are non-whitespace (default `path = "."`, `tools.rs:83-89`). Clamp `limit` to `1..=100` and `timeout_ms` to `1 000..=300 000` (`tools.rs:95, 99`).
2. **Resolve workspace scope** — `resolve_scope` canonicalizes the workspace and target paths, computes the `<workspace>/.tools-mcp/semantic-index` directory, and derives a `PathFilter` (`Workspace` / `File` / `Directory`) describing the search scope (`tools-mcp-semantic/src/discovery.rs:90-114`).
3. **Load the manifest** — `IndexManifest::load_or_new` reads `<index_dir>/<model_slug>/manifest.json` (`tools-mcp-semantic/src/manifest.rs:27-55`). When the manifest contains no `table_name` or `vector_dim`, the call fails immediately with `"semantic index is empty for model <id>"` or `"semantic index has no recorded vector dimension"` (`tools-mcp-semantic/src/model.rs:316-322`).
4. **Initialize the embedding provider** — `FastEmbedProvider::new` returns the cached `TextEmbedding` for `<index_dir>/models`, initializing it via `tokio::task::spawn_blocking` the first time (`tools-mcp-semantic/src/embedding.rs:24-53, 150-185`).
5. **Embed the query** — `embed_query` prefixes the query with `"query: "` and feeds it through `model.embed` on a blocking worker thread (`tools-mcp-semantic/src/embedding.rs:73-92`). The result is a single `Vec<f32>`. Dimension mismatch against the manifest is rejected as `"semantic query embedding dimension X does not match index dimension Y"` (`tools-mcp-semantic/src/model.rs:327-334`).
6. **Open the LanceDB table** — `LanceDbStore::open_existing` connects to `<index_dir>/lancedb` and opens the table whose name is recorded in the manifest (`tools-mcp-semantic/src/store.rs:89-104, 177-187`).
7. **Compose the SQL predicate** — `build_filter_predicate` always emits `root = '<workspace>'`; appends a `path = ...` (File) or half-open range `(path = ... OR (path >= ... AND path < ...))` (Directory) when the scope is narrower than the workspace; and appends `language = '<lang>'` when `language` is non-empty after trimming (`tools-mcp-semantic/src/store.rs:322-338`, `tools-mcp-semantic/src/discovery.rs:47-63`). All string literals are escaped via `escape_sql_literal` (`discovery.rs:245-247`).
8. **Run the nearest-neighbor query** — `LanceDbStore::search` configures `nearest_to(query_embedding)` on the `vector` column, projects either the with-content or without-content column set depending on `include_content` (`tools-mcp-semantic/src/store.rs:15-31, 137-159`), and applies the SQL predicate via `only_if`. The query is bounded by `limit`.
9. **Collect and threshold-filter results** — The query stream is drained into `Vec<SemanticMatch>`. For each row the handler reads `_distance` (falling back to `0.0` if absent), and skips the row when `threshold.is_some_and(|t| score > t)` (`tools-mcp-semantic/src/store.rs:161-170, 340-374, 432-441`).
10. **Build the response** — `SearchSummary::into_payload` constructs the JSON envelope. The `content[0].text` field renders one line per result (`"{path}:{start}-{end} {score:.4} [symbol]"`) or `"No semantic matches found."` when empty; structured fields carry `query`, `model`, `count`, `results`, `timed_out`, and `index_status` (`tools-mcp-semantic/src/model.rs:86-129`). `timed_out` is computed by comparing elapsed wall-clock time against `timeout_ms` (`model.rs:358`); the search itself is not interrupted mid-stream, so `timed_out: true` here is a soft signal that the call ran long.

### 6.3 Response Schema

**Success (`isError: false`):**

`SearchSummary::into_payload` serializes the result object directly into the MCP envelope (`tools-mcp-semantic/src/model.rs:86-129`):

```json
{
  "content": [
    {
      "type": "text",
      "text": "tools-mcp-webfetch/src/webfetch/http.rs:558-649 0.1873 fetch_document\ntools-mcp-webfetch/src/webfetch/mod.rs:140-251 0.2042 run_fetch"
    }
  ],
  "isError": false,
  "query": "redirect ssrf revalidation",
  "model": "jina-embeddings-v2-base-code",
  "count": 2,
  "results": [
    {
      "chunk_id": "f3c4b9...",
      "path": "tools-mcp-webfetch/src/webfetch/http.rs",
      "language": "rust",
      "symbol": "fetch_document",
      "start_line": 558,
      "end_line": 649,
      "score": 0.1873,
      "content": "pub(crate) async fn fetch_document(req: &FetchRequest) -> Result<..."
    },
    {
      "chunk_id": "8a2d11...",
      "path": "tools-mcp-webfetch/src/webfetch/mod.rs",
      "language": "rust",
      "symbol": "run_fetch",
      "start_line": 140,
      "end_line": 251,
      "score": 0.2042,
      "content": "..."
    }
  ],
  "timed_out": false,
  "index_status": "ready"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | One line per result (`{path}:{start}-{end} {score:.4} [symbol]`) or `"No semantic matches found."` when empty. |
| `isError` | boolean | Yes | Always `false` on success. |
| `query` | string | Yes | Echoes the caller's query. |
| `model` | string | Yes | Embedding model id used at search time (currently `jina-embeddings-v2-base-code`). |
| `count` | integer | Yes | Number of ranked results returned (after threshold). |
| `results` | array | Yes | Ordered ranked matches (nearest first). |
| `results[].chunk_id` | string | Yes | Stable id derived from path + line range + content hash (`tools-mcp-semantic/src/chunking.rs:347-374`). |
| `results[].path` | string | Yes | Workspace-relative POSIX path. |
| `results[].language` | string | Yes | Language tag assigned at index time. |
| `results[].symbol` | string \| null | Yes | Symbol name when the chunker captured one; `null` for fallback windowed chunks. |
| `results[].start_line` | integer | Yes | 1-based inclusive start line. |
| `results[].end_line` | integer | Yes | 1-based inclusive end line. |
| `results[].score` | number | Yes | LanceDB `_distance` (lower is more similar); `0.0` when LanceDB omits the column. |
| `results[].content` | string | No (omitted when `include_content = false` or LanceDB omits it) | Source text of the chunk. |
| `timed_out` | boolean | Yes | `true` when elapsed time reached or exceeded `timeout_ms`. The search itself completes; this is a soft signal. |
| `index_status` | string | Yes | Currently always `"ready"`; reserved for future status surfaces (`tools-mcp-semantic/src/model.rs:359`). |

**Tool-level error (`isError: true`):**

Errors flow through `ToolCallOutcome::err_with` (`tools-mcp-core/src/tool_outcome.rs:43-57`) with a fixed remediation hint added by the handler:

```json
{
  "content": [{"type": "text", "text": "semantic search failed: <reason>"}],
  "isError": true,
  "remediation": "Run SemanticIndex for the target path, or check model/index compatibility."
}
```

The same envelope shape is returned by `ToolCallOutcome::err` when argument parsing or non-empty validation fails (`tools-mcp-core/src/tool_outcome.rs:35-40, 61-75`; `tools-mcp-core/src/validation.rs:11-22`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern | Source |
|---|---|---|---|
| Missing required `query` | `true` | `"invalid arguments: missing field \`query\` ... Required fields are missing; ..."` | `tools-mcp-semantic/src/tools.rs:78-81`, `tools-mcp-core/src/tool_outcome.rs:61-75` |
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` | `tools-mcp-semantic/src/tools.rs:78-81` |
| Whitespace-only `query` | `true` | `"query is required (non-empty string)"` | `tools-mcp-semantic/src/tools.rs:83-85`, `tools-mcp-core/src/validation.rs:11-22` |
| Empty `path` after default | `true` | `"path is required (non-empty string)"` | `tools-mcp-semantic/src/tools.rs:87-89` |
| `path` resolves outside workspace | `true` | `"semantic search failed: path ... resolves outside the server working directory ..."` | `tools-mcp-semantic/src/discovery.rs:263-272`, `tools-mcp-semantic/src/tools.rs:104-111` |
| Index manifest absent or missing `table_name` | `true` | `"semantic search failed: semantic index is empty for model jina-embeddings-v2-base-code"` | `tools-mcp-semantic/src/model.rs:316-319` |
| Manifest has no `vector_dim` | `true` | `"semantic search failed: semantic index has no recorded vector dimension"` | `tools-mcp-semantic/src/model.rs:320-322` |
| LanceDB table does not exist | `true` | `"semantic search failed: semantic index table <name> does not exist"` | `tools-mcp-semantic/src/store.rs:89-104` |
| FastEmbed initialization failure | `true` | `"semantic search failed: failed to initialize FastEmbed model ..."` | `tools-mcp-semantic/src/embedding.rs:150-174` |
| FastEmbed query embedding failure | `true` | `"semantic search failed: failed to embed semantic search query"` | `tools-mcp-semantic/src/embedding.rs:73-92` |
| Query/index dimension drift | `true` | `"semantic search failed: semantic query embedding dimension X does not match index dimension Y"` | `tools-mcp-semantic/src/model.rs:327-334` |
| LanceDB query configuration failure | `true` | `"semantic search failed: failed to configure semantic vector query"` | `tools-mcp-semantic/src/store.rs:143-148` |
| LanceDB query execution failure | `true` | `"semantic search failed: failed to execute semantic vector query"` | `tools-mcp-semantic/src/store.rs:156-159` |
| LanceDB result collection failure | `true` | `"semantic search failed: failed to collect semantic vector query results"` | `tools-mcp-semantic/src/store.rs:161-166` |
| Search exceeds soft deadline | (none) | Returned successfully with `timed_out: true` | `tools-mcp-semantic/src/model.rs:358` |

## 7. Security Considerations

- **Workspace scoping.** Both the path argument (via `resolve_scope`) and every LanceDB query (via the mandatory `root = '<workspace>'` predicate) confine results to the active workspace (`tools-mcp-semantic/src/discovery.rs:249-272`, `tools-mcp-semantic/src/store.rs:322-338`).
- **SQL literal escaping.** Workspace root, path, and language values are escaped with `escape_sql_literal` before being interpolated into the LanceDB predicate (`tools-mcp-semantic/src/discovery.rs:245-247`, `tools-mcp-semantic/src/store.rs:291-338`). Tests pin escaping behavior for paths containing single quotes (`tools-mcp-semantic/src/store.rs:517-571`) and for directory predicates that look wildcard-like (`tools-mcp-semantic/src/discovery.rs:374`).
- **Bounded resource use.** `limit` is clamped to 100; `timeout_ms` is clamped to 5 minutes; the LanceDB projection set is fixed. The handler never iterates beyond `limit` rows.
- **Local model execution.** Query embeddings are computed in-process via FastEmbed; the query string does not leave the host through this tool.
- **Untrusted result content.** Chunk text returned in `results[].content` is data, not instructions. Downstream agents MUST frame it as external document content and MUST NOT execute it as commands or interpret it as instructions. See `docs/security.md`.
- **Information disclosure.** Because the handler executes inside the server's working directory and the index covers the whole workspace by default, callers may surface any source file the indexer was allowed to read. Operators wanting tighter scoping SHOULD index a subdirectory and search within it.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| _(none read directly by this tool)_ | — | The handler does not read environment variables; FastEmbed reads its own ORT configuration internally. |

Indirect knobs:

- **`gpu-cuda` Cargo feature** (build-time, not runtime). When enabled, the embedding provider initializes ONNX Runtime with the CUDA execution provider for query embeddings as well (`tools-mcp-semantic/src/embedding.rs:158-166`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-semantic/src/tools.rs` | 40-43 |
| Module wiring | `tools-mcp-semantic/src/lib.rs` | 11-13 |
| Composition root | `tools-mcp-server/src/composition.rs` | 89 |
| Tool name + schema | `tools-mcp-semantic/src/tools.rs` | 133-152 |
| Request type (`deny_unknown_fields`) | `tools-mcp-semantic/src/tools.rs` | 22-38 |
| Handler entry | `tools-mcp-semantic/src/tools.rs` | 77-112 |
| Pipeline (`search_workspace`) | `tools-mcp-semantic/src/model.rs` | 308-361 |
| Manifest load + table lookup | `tools-mcp-semantic/src/model.rs` | 314-322 |
| Query embed (`"query: "` prefix) | `tools-mcp-semantic/src/embedding.rs` | 73-92 |
| LanceDB nearest-neighbor query | `tools-mcp-semantic/src/store.rs` | 137-170 |
| SQL filter composition | `tools-mcp-semantic/src/store.rs` | 322-338 |
| Path filter SQL | `tools-mcp-semantic/src/discovery.rs` | 47-63 |
| Result projection (with/without content) | `tools-mcp-semantic/src/store.rs` | 15-31, 314-320 |
| Threshold filter | `tools-mcp-semantic/src/store.rs` | 357-360 |
| `_distance` reader fallback | `tools-mcp-semantic/src/store.rs` | 432-441 |
| Response payload (`SearchSummary::into_payload`) | `tools-mcp-semantic/src/model.rs` | 86-129 |
| Result text rendering (`"path:start-end score symbol"`) | `tools-mcp-semantic/src/model.rs` | 88-115 |
| Error remediation | `tools-mcp-semantic/src/tools.rs` | 104-111 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "SemanticSearch",
    "arguments": {
      "query": "ssrf revalidation on redirect"
    }
  }
}
```

### 10.2 Narrowed scope with language filter and tighter threshold

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "SemanticSearch",
    "arguments": {
      "query": "robots.txt fail closed",
      "path": "tools-mcp-webfetch",
      "language": "rust",
      "limit": 5,
      "threshold": 0.3,
      "include_content": false
    }
  }
}
```

### 10.3 Success response (no results)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "No semantic matches found."}],
    "isError": false,
    "query": "unrelated topic",
    "model": "jina-embeddings-v2-base-code",
    "count": 0,
    "results": [],
    "timed_out": false,
    "index_status": "ready"
  }
}
```

### 10.4 Success response (single match, content omitted)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "tools-mcp-webfetch/src/webfetch/http.rs:502-511 0.2103 ensure_fetch_allowed"
      }
    ],
    "isError": false,
    "query": "robots.txt fail closed",
    "model": "jina-embeddings-v2-base-code",
    "count": 1,
    "results": [
      {
        "chunk_id": "abc123...",
        "path": "tools-mcp-webfetch/src/webfetch/http.rs",
        "language": "rust",
        "symbol": "ensure_fetch_allowed",
        "start_line": 502,
        "end_line": 511,
        "score": 0.2103
      }
    ],
    "timed_out": false,
    "index_status": "ready"
  }
}
```

### 10.5 Missing-index error

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "semantic search failed: semantic index is empty for model jina-embeddings-v2-base-code"
      }
    ],
    "isError": true,
    "remediation": "Run SemanticIndex for the target path, or check model/index compatibility."
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `tools_list_contains_expected_set` | `tools-mcp-server/tests/integration_test.rs:105` | `SemanticSearch` appears in `mcp/tools/list`. |
| `golden_contract_lists_expected_tools` | `tools-mcp-server/tests/golden_contract.rs:56` | Golden test pinning the `SemanticSearch` tool name. |
| `search_payload_preserves_contract_without_content` | `tools-mcp-semantic/src/model.rs:533` | Response shape: `content[0].text` formatting, `isError: false`, `count`, and the omission of `content` when not requested. |
| `embedding_document_includes_stable_code_metadata` | `tools-mcp-semantic/src/model.rs:513` | Document prefixing format used at index time (the matched search-time format). |
| `search_filter_combines_root_path_and_language` | `tools-mcp-semantic/src/store.rs:462` | SQL predicate composes `root`, directory range, and `language` clauses. |
| `directory_filter_matches_underscore_paths_literally` | `tools-mcp-semantic/src/store.rs:480` | Directory predicate treats `_` literally rather than as a SQL wildcard. |
| `search_respects_content_projection_flag` | `tools-mcp-semantic/src/store.rs:574` | `include_content` toggles whether `content` is selected/returned. |
| `search_threshold_uses_projected_distance` | `tools-mcp-semantic/src/store.rs:618` | `threshold` drops rows whose `_distance` exceeds the bound. |
| `delete_paths_predicate_escapes_batched_literals` | `tools-mcp-semantic/src/store.rs:517` | Single-quote escaping for paths (covers values that would otherwise reach search predicates). |
| `sql_literals_escape_single_quotes` | `tools-mcp-semantic/src/discovery.rs:369` | `escape_sql_literal` doubles single quotes. |
| `directory_filter_includes_children_only` | `tools-mcp-semantic/src/discovery.rs:360` | `PathFilter::Directory::contains` matches the directory and its descendants only. |

Coverage gap: there is no end-to-end test that exercises `SemanticSearch` through the JSON-RPC harness (only the `tools/list` golden assertion). The pipeline is exercised by the unit tests above plus the `semantic` benchmark harness (`tools-mcp-semantic/benches/semantic.rs`, behind the `bench-api` feature).

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `timed_out` interrupt the LanceDB query? | No. `timed_out` is computed by comparing elapsed time against `timeout_ms` after the query finishes (`tools-mcp-semantic/src/model.rs:358`). The LanceDB stream itself is not aborted mid-flight. Treat `timed_out: true` as a "this call ran long" signal, not a partial-result flag. |
| 2 | What units does `threshold` use? | LanceDB `_distance` (lower is more similar). Tests show `threshold = 0.1` keeps distance `≈ 0` matches and drops distance `1.0` matches (`tools-mcp-semantic/src/store.rs:618-651`). |
| 3 | Can the caller search across workspaces? | No. The `root` predicate is hard-coded from the canonical server working directory (`tools-mcp-semantic/src/discovery.rs:90-114`, `tools-mcp-semantic/src/store.rs:323`). To search a different workspace, run the server with that working directory. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::err_with` and `parse_args` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_limit`, `clamp_timeout` semantics (§6.1). |
| `tools-mcp-server/src/composition.rs` | Composition-root registration call (§4.1). |
| `docs/tools/semantic-index.md` | Companion tool that writes the index this tool reads. |
| `docs/security.md` | Project-wide trust-boundary guidance for untrusted external content (§7). |
