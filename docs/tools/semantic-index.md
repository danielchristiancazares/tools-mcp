# SDD: SemanticIndex

**Date:** 2026-05-24
**Scope:** Design contract for the `SemanticIndex` MCP tool.
**Source:** `tools-mcp-semantic/src/tools.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `SemanticIndex` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`SemanticIndex` is the MCP tool that builds (or incrementally refreshes) the on-disk semantic code-search index for a workspace path. It discovers indexable files, splits them into symbol- or line-based chunks via tree-sitter (or a 100-line fallback window with 15-line overlap), embeds the chunks with a local FastEmbed model (`jina-embeddings-v2-base-code`), and persists the resulting vectors and metadata into a LanceDB table backed by a per-model JSON manifest. The tool is owned by the `tools-mcp-semantic` crate; the entry point is `handle_semantic_index` (`tools-mcp-semantic/src/tools.rs:45`), which delegates to `crate::model::index_workspace` (`tools-mcp-semantic/src/model.rs:144`).

### 3.2 Explicitly Out of Scope

- Querying the index (covered by `SemanticSearch`; see `docs/tools/semantic-search.md`). `SemanticIndex` only writes; it never executes vector queries.
- JSON-RPC framing and method routing (covered in `docs/protocol.md`).
- Tool-registry composition (covered in `docs/architecture.md`).
- Cross-cutting environment variables (full catalog in `docs/configuration.md`).
- Model selection at runtime: the model id (`jina-embeddings-v2-base-code`) is a build-time constant (`tools-mcp-semantic/src/embedding.rs:7-8`); switching models is not exposed through the tool schema.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `SemanticIndex` |
| Aliases | None |
| Registration gate | Always registered (no env gate) |
| Owning crate | `tools-mcp-semantic` |
| Handler function | `handle_semantic_index` (`tools-mcp-semantic/src/tools.rs:45`) |
| Schema definition | `tools-mcp-semantic/src/tools.rs:114-131` |
| Registration call | `tools-mcp-semantic/src/tools.rs:41` invoked from `tools-mcp-semantic/src/lib.rs:11-13`, wired into the registry by `tools-mcp-server/src/composition.rs:89` |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Path is resolved inside the server working directory** — `resolve_scope` MUST canonicalize the requested path and reject anything that does not equal or start with the canonicalized current working directory (`tools-mcp-semantic/src/discovery.rs:249-272`).
- **Discovery is bounded** — File walking MUST honor `limit` (default 10 000, max 100 000 per the schema) and the per-call `timeout_ms` deadline (`tools-mcp-semantic/src/discovery.rs:168-204`, `tools-mcp-semantic/src/model.rs:147-153`). The walker MUST mark `truncated = true` when `limit` is hit and `timed_out = true` when the deadline elapses (`tools-mcp-semantic/src/discovery.rs:170-172, 200-203`).
- **Cancellation is observed** — When a `CancellationToken` is present in the task-local scope, the walker MUST bail with `"semantic indexing cancelled"` if cancellation has fired (`tools-mcp-semantic/src/discovery.rs:174-179`).
- **Excluded directories never enter the index** — Walk entries whose file name matches `.git`, `.svn`, `.hg`, `target`, `node_modules`, `dist`, `build`, or `.tools-mcp` MUST be pruned and counted toward `skipped_files` (`tools-mcp-semantic/src/discovery.rs:160-166, 301-306, 205`).
- **Per-file size cap** — Discovery MUST skip files larger than 1 MiB (`MAX_FILE_BYTES`, `tools-mcp-semantic/src/discovery.rs:15, 280-283`).
- **Binary content is skipped, not failed** — Files whose contents are not valid UTF-8 MUST be counted as `skipped_files` and not embedded (`tools-mcp-semantic/src/model.rs:182-188`).
- **Incremental by default** — Unless `force = true`, files whose stored `file_hash` (SHA-256 of the raw bytes; `tools-mcp-semantic/src/chunking.rs:106-110`) matches the manifest MUST be left unchanged and omitted from `updated_files`; unchanged indexed files MUST NOT be counted as `skipped_files` (`tools-mcp-semantic/src/model.rs:173-180`).
- **Stale rows are removed** — Manifest entries that fall under the target filter but are no longer present on disk MUST be removed from both LanceDB (`store.delete_paths`) and the manifest (`tools-mcp-semantic/src/model.rs:159-160, 196-207, 234-243, 289`).
- **Vector dimensions are consistent** — All embeddings in a single index call MUST share a dimension; mixing dimensions MUST fail with `"FastEmbed returned inconsistent document dimensions"` (`tools-mcp-semantic/src/model.rs:226-229`, `tools-mcp-semantic/src/embedding.rs:107-131`, `tools-mcp-semantic/src/store.rs:218-225`).
- **Manifest reflects only persisted state** — `IndexManifest::save` MUST run after `store.add_chunks` succeeds; `table_name` and `vector_dim` MUST be written so a subsequent `SemanticSearch` can locate the table (`tools-mcp-semantic/src/model.rs:276, 290-292`).
- **No panic on failure** — All error paths MUST return `ToolCallOutcome::err_with` from `handle_semantic_index` (`tools-mcp-semantic/src/tools.rs:67-74`); the handler MUST NOT panic.
- **Index location is repo-local** — Index files MUST live under `<workspace>/.tools-mcp/semantic-index` (`tools-mcp-semantic/src/discovery.rs:14, 96`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT index paths outside the server working directory.
- MUST NOT silently swap the embedding model between calls; the model id is a fixed constant (`tools-mcp-semantic/src/embedding.rs:7`). Reindexing under a different model MUST produce a new table name (`tools-mcp-semantic/src/store.rs:173-175`).
- MUST NOT follow symlinks during discovery (`WalkBuilder::follow_links(false)`, `tools-mcp-semantic/src/discovery.rs:159`).
- MUST NOT write stale embeddings: when re-embedding a file, the prior rows for that `(root, path)` pair MUST be deleted before new rows are appended (`tools-mcp-semantic/src/model.rs:240-243`).
- MUST NOT execute fetched content as commands or otherwise interpret file contents as instructions. Indexed text is data.

## 5. Design Goals

- **Local-only embeddings.** FastEmbed runs in-process with ONNX Runtime; no network round trip per chunk. Model weights are cached under `<index_dir>/models` (`tools-mcp-semantic/src/embedding.rs:27, 150-174`).
- **Incremental and idempotent.** Re-running `SemanticIndex` with no changes is cheap: the manifest's `file_hash` short-circuits embedding for unchanged files (`tools-mcp-semantic/src/model.rs:170-173`).
- **Symbol-aware chunks where possible.** Tree-sitter tag queries split Rust, TypeScript/TSX, JavaScript, Python, and Go by definition node so embeddings preserve symbol-level locality (`tools-mcp-semantic/src/chunking.rs:113-213`). Markdown is split by heading; everything else falls back to a 100-line / 15-line-overlap window.
- **Bounded everywhere.** File size, walk count, timeout, and per-chunk byte cap all clamp the cost of a single call so the tool stays predictable under hostile inputs.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | No | `"."` | Non-empty; resolved under the server working directory | File or directory to index. |
| `force` | boolean | No | `false` | — | Reindex files even when the stored `file_hash` has not changed. |
| `hidden` | boolean | No | `false` | — | Include hidden files and directories (default skips them via `WalkBuilder::hidden(true)`). |
| `no_ignore` | boolean | No | `false` | — | Bypass ignore files (`.gitignore`, `.ignore`, global excludes). |
| `limit` | integer | No | `10000` | `1..=100000` (clamped by `validation::clamp_limit`, `tools-mcp-semantic/src/tools.rs:61`) | Maximum number of files to consider during discovery. |
| `timeout_ms` | integer | No | `300000` | `1000..=1800000` (clamped by `validation::clamp_timeout`, `tools-mcp-semantic/src/tools.rs:62`) | Indexing budget in milliseconds. |

The schema sets `"additionalProperties": false` (`tools-mcp-semantic/src/tools.rs:128`); the deserializer uses `#[serde(deny_unknown_fields)]` (`tools-mcp-semantic/src/tools.rs:6`). Unknown fields produce a tool-level error (`isError: true`) with text `"invalid arguments: ..."` per `ToolCallOutcome::parse_args` (`tools-mcp-core/src/tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-semantic/src/tools.rs:118-129`

### 6.2 Behavior

`handle_semantic_index` and `index_workspace` implement the pipeline below. Each step lists the source location for verification.

1. **Parse and validate arguments** — Deserialize `SemanticIndexRequest`; reject unknown fields (`tools-mcp-semantic/src/tools.rs:46-49`). Validate that `path` is non-whitespace (default `"."`, `tools-mcp-semantic/src/tools.rs:51-54`). Clamp `limit` and `timeout_ms` to schema bounds (`tools-mcp-semantic/src/tools.rs:61-62`).
2. **Resolve workspace scope** — `resolve_scope` canonicalizes the server working directory and the requested path, refusing any target that resolves outside the workspace (`tools-mcp-semantic/src/discovery.rs:90-114, 249-272`). Computes the index directory `<workspace>/.tools-mcp/semantic-index` and a `PathFilter` (`Workspace`, `File`, or `Directory`) describing what subset of the index this call operates on.
3. **Open or initialize the manifest** — `IndexManifest::load_or_new` reads `<index_dir>/<model_slug>/manifest.json` if present or constructs a fresh manifest for the current workspace and model id (`tools-mcp-semantic/src/manifest.rs:27-55, 118-132`).
4. **Discover candidate files** — `discover_files` walks the target with `ignore::WalkBuilder`, honoring `hidden`/`no_ignore` flags (`tools-mcp-semantic/src/discovery.rs:152-167`), pruning excluded directory names (`.git`, `.svn`, `.hg`, `target`, `node_modules`, `dist`, `build`, `.tools-mcp` — `discovery.rs:301-306`), and stopping at the deadline or `limit` (`discovery.rs:168-204`). Files larger than 1 MiB and files with unrecognized extensions are skipped (`discovery.rs:280-283, 297-346`).
5. **Detect stale paths** — `stale_paths_under` returns manifest entries under the current filter that no longer appear on disk (`tools-mcp-semantic/src/manifest.rs:85-96`, `tools-mcp-semantic/src/model.rs:160`).
6. **Hash and short-circuit unchanged files** — For each discovered file: read bytes, compute SHA-256 (`tools-mcp-semantic/src/chunking.rs:106-110`), and if `force = false` and the manifest's `file_hash` matches, leave the existing manifest entry untouched and continue without adding to `updated_files` or `skipped_files` (`tools-mcp-semantic/src/model.rs:173-180`).
7. **Chunk the source** — `chunk_source` dispatches by language: tree-sitter tag queries for Rust, TypeScript, TSX, JavaScript, Python, Go (`tools-mcp-semantic/src/chunking.rs:113-122, 130-213`); heading split for Markdown (`chunking.rs:240-284`); a 100-line / 15-line-overlap fallback for everything else (`chunking.rs:286-327`). Symbol chunks larger than 32 KiB are themselves broken down via the fallback (`chunking.rs:9, 188-191`). Empty chunk sets count the file as skipped (`model.rs:189-193`).
8. **Early-exit when nothing changed** — If no files require embedding, the handler still deletes any stale rows (when a table already exists), persists the manifest, and returns the summary without spinning up the embedding model (`tools-mcp-semantic/src/model.rs:206-230`).
9. **Initialize the embedding provider** — `FastEmbedProvider::new` returns a cached `TextEmbedding` for `<index_dir>/models`, initializing it via `tokio::task::spawn_blocking` the first time (`tools-mcp-semantic/src/embedding.rs:24-53, 150-174`). Subsequent calls in the same process reuse the cached model (`embedding.rs:14, 29-35, 176-185`).
10. **Embed in batches** — `embed_index_chunks` flushes batches of `default_embedding_batch_size()` (CPU = 32, CUDA = 128 when the `gpu-cuda` feature is enabled; `tools-mcp-semantic/src/embedding.rs:9-10, 142-148`). Each input is prefixed with `"passage: "` and structured with `path`, `language`, optional `symbol`, and `code` sections (`tools-mcp-semantic/src/model.rs:363-390`, `tools-mcp-semantic/src/embedding.rs:63-71, 107-131`). After every batch and around each blocking call the handler checks the deadline (`model.rs:407-431, 480-485`).
11. **Open or create the LanceDB table** — Table name is `semantic_chunks_v1_{model_slug}_{vector_dim}` (`tools-mcp-semantic/src/store.rs:13, 173-175`); the LanceDB connection points at `<index_dir>/lancedb` (`store.rs:177-187`). Schema is fixed at 13 fields including the `FixedSizeList<f32, vector_dim>` vector column (`store.rs:189-211`).
12. **Replace existing rows for changed paths** — Before appending, `store.delete_paths` removes any prior `(root, path)` rows belonging to changed or stale paths (`tools-mcp-semantic/src/model.rs:234-243`, `tools-mcp-semantic/src/store.rs:106-121`). `deleted_chunks` is computed from the manifest's chunk-id lists for those paths (`model.rs:239`, `manifest.rs:98-107`).
13. **Append new chunks** — `store.add_chunks` builds an Arrow `RecordBatch` and writes it (`tools-mcp-semantic/src/store.rs:123-135, 214-289`). The manifest is updated in lockstep, then saved with the new `table_name` and `vector_dim` (`tools-mcp-semantic/src/model.rs:253-292`).
14. **Build the response** — Returns an `IndexSummary` whose `into_payload` joins the human-readable summary text with structured fields (`tools-mcp-semantic/src/model.rs:53-73`, `tools-mcp-semantic/src/tools.rs:66`).

### 6.3 Response Schema

**Success (`isError: false`):**

`IndexSummary::into_payload` serializes the result object directly into the MCP envelope (`tools-mcp-semantic/src/model.rs:57-82`):

```json
{
  "content": [
    {
      "type": "text",
      "text": "Indexed 15 file(s), updated 12 file(s); 101 chunk(s) indexed, 87 chunk(s) written; removed 1 stale/replaced chunk(s)."
    }
  ],
  "isError": false,
  "indexed_files": 15,
  "indexed_chunks": 101,
  "updated_files": 12,
  "updated_chunks": 87,
  "skipped_files": 3,
  "deleted_chunks": 1,
  "model": "jina-embeddings-v2-base-code",
  "store_path": "C:\\Users\\Daniel\\tools-mcp\\.tools-mcp\\semantic-index",
  "duration_ms": 4218,
  "incremental": true,
  "truncated": false,
  "timed_out": false
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Human-readable summary of the run. |
| `isError` | boolean | Yes | Always `false` on success. |
| `indexed_files` | integer | Yes | Files currently indexed under the requested target after the call completes. |
| `indexed_chunks` | integer | Yes | Chunks currently indexed under the requested target after the call completes. |
| `updated_files` | integer | Yes | Files embedded and written during this call. |
| `updated_chunks` | integer | Yes | Chunks written to LanceDB during this call. |
| `skipped_files` | integer | Yes | Files skipped because they could not be indexed: non-UTF-8, empty chunk set, oversize, excluded extension, or pruned directory entry. Unchanged indexed files are not counted here. |
| `deleted_chunks` | integer | Yes | Number of previously-stored chunks belonging to changed or stale files that were deleted before reindex/removal. |
| `model` | string | Yes | Embedding model id (currently `jina-embeddings-v2-base-code`). |
| `store_path` | string | Yes | Absolute path to `<workspace>/.tools-mcp/semantic-index`. |
| `duration_ms` | integer | Yes | Total wall-clock duration of the call. |
| `incremental` | boolean | Yes | `!force`; `true` when manifest-hash short-circuiting was allowed. |
| `truncated` | boolean | Yes | `true` when the walker hit `limit` before exhausting the target. |
| `timed_out` | boolean | Yes | `true` when the walker stopped because the deadline elapsed. |

**Tool-level error (`isError: true`):**

Errors flow through `ToolCallOutcome::err_with` (`tools-mcp-core/src/tool_outcome.rs:43-57`) with a fixed remediation hint added by the handler:

```json
{
  "content": [{"type": "text", "text": "semantic index failed: <reason>"}],
  "isError": true,
  "remediation": "Check the path, local model availability, and index directory permissions."
}
```

The same envelope shape is returned by `ToolCallOutcome::err` when argument parsing or non-empty validation fails (`tools-mcp-core/src/tool_outcome.rs:35-40, 61-75`; `tools-mcp-core/src/validation.rs:11-22`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern | Source |
|---|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` (with parser hint) | `tools-mcp-semantic/src/tools.rs:46-49`, `tools-mcp-core/src/tool_outcome.rs:61-75` |
| Empty `path` after default | `true` | `"path is required (non-empty string)"` | `tools-mcp-semantic/src/tools.rs:52-54`, `tools-mcp-core/src/validation.rs:11-22` |
| `path` resolves outside workspace | `true` | `"semantic index failed: path ... resolves outside the server working directory ..."` | `tools-mcp-semantic/src/discovery.rs:263-272`, `tools-mcp-semantic/src/tools.rs:67-74` |
| Path cannot be canonicalized | `true` | `"semantic index failed: failed to resolve path <input>: ..."` | `tools-mcp-semantic/src/discovery.rs:259-262` |
| Empty `path` literal at lower layer | `true` | `"semantic index failed: path is required"` | `tools-mcp-semantic/src/discovery.rs:250-252` |
| Walk failure (I/O / permissions) | `true` | `"semantic index failed: failed to walk index path under ..."` | `tools-mcp-semantic/src/discovery.rs:181-186` |
| Cancellation observed | `true` | `"semantic index failed: semantic indexing cancelled"` | `tools-mcp-semantic/src/discovery.rs:174-179` |
| Deadline exceeded before walk completes | (none) | Returned successfully with `timed_out: true, truncated: true` | `tools-mcp-semantic/src/discovery.rs:169-172` |
| File read failure | `true` | `"semantic index failed: failed to read <path>: ..."` | `tools-mcp-semantic/src/model.rs:166-168` |
| FastEmbed initialization failure | `true` | `"semantic index failed: failed to initialize FastEmbed model ..."` | `tools-mcp-semantic/src/embedding.rs:150-174` |
| FastEmbed embedding failure | `true` | `"semantic index failed: failed to embed semantic index documents: ..."` | `tools-mcp-semantic/src/embedding.rs:107-131` |
| FastEmbed dimension drift | `true` | `"semantic index failed: FastEmbed returned inconsistent document dimensions: expected N, got M"` | `tools-mcp-semantic/src/model.rs:226-229` |
| FastEmbed batch size mismatch | `true` | `"semantic index failed: FastEmbed returned N document embeddings for M input document(s)"` | `tools-mcp-semantic/src/model.rs:449-454` |
| LanceDB open / create failure | `true` | `"semantic index failed: failed to create LanceDB table ..."` / `"failed to open LanceDB semantic index"` | `tools-mcp-semantic/src/store.rs:69-87, 177-187` |
| LanceDB delete failure | `true` | `"semantic index failed: failed to delete replaced semantic chunks"` | `tools-mcp-semantic/src/model.rs:240-243` |
| LanceDB append failure | `true` | `"semantic index failed: failed to add semantic chunks to LanceDB"` | `tools-mcp-semantic/src/store.rs:123-135` |
| Manifest write failure | `true` | `"semantic index failed: failed to write semantic index manifest ..."` | `tools-mcp-semantic/src/manifest.rs:57-77` |

## 7. Security Considerations

- **Workspace scoping.** `resolve_scope` canonicalizes both the workspace and the target before checking that the target equals or is a child of the workspace (`tools-mcp-semantic/src/discovery.rs:249-272`). This blocks `..`-style traversal and absolute paths that escape the project root.
- **No symlink following.** `WalkBuilder::follow_links(false)` ensures symlinks inside the workspace cannot redirect indexing onto files outside it (`tools-mcp-semantic/src/discovery.rs:159`).
- **Path normalization for storage.** Relative paths stored in LanceDB and the manifest use forward-slash components; absolute, parent, or root components are rejected (`tools-mcp-semantic/src/discovery.rs:216-243`).
- **SQL literal escaping.** Path and root predicates passed to LanceDB are escaped via `escape_sql_literal`, replacing `'` with `''` (`tools-mcp-semantic/src/discovery.rs:245-247`, `tools-mcp-semantic/src/store.rs:291-312`). Tests pin the escaping behavior (`tools-mcp-semantic/src/store.rs:517-571`).
- **Bounded resource use.** Per-file size cap (1 MiB), per-call discovery limit (default 10 000 files, max 100 000), and per-call timeout (default 5 min, max 30 min) bound CPU, memory, and I/O.
- **Local model execution.** FastEmbed runs in-process; no chunk contents leave the host through this tool. Model weights are downloaded on first use into `<index_dir>/models` (`tools-mcp-semantic/src/embedding.rs:27, 150-174`).
- **Untrusted file contents.** Indexed source code is data, not instructions. Downstream consumers of `SemanticSearch` results MUST continue to treat the embedded text as untrusted user input.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| _(none read directly by this tool)_ | — | The handler does not read environment variables; FastEmbed reads its own ORT configuration internally. |

Indirect knobs:

- **`gpu-cuda` Cargo feature** (build-time, not runtime). When enabled, the embedding provider initializes ONNX Runtime with the CUDA execution provider and raises the default batch size from 32 to 128 (`tools-mcp-semantic/src/embedding.rs:9-10, 142-148, 158-166`).
- **`TOOLS_PRETTY_JSON`** (process-wide). Does NOT affect this tool, because `IndexSummary::into_payload` constructs the JSON value directly rather than routing through `ToolCallOutcome::ok_json_content` (`tools-mcp-semantic/src/model.rs:57-82`, `tools-mcp-core/src/tool_outcome.rs:98-118`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-semantic/src/tools.rs` | 40-43 |
| Module wiring | `tools-mcp-semantic/src/lib.rs` | 11-13 |
| Composition root | `tools-mcp-server/src/composition.rs` | 89 |
| Tool name + schema | `tools-mcp-semantic/src/tools.rs` | 114-131 |
| Request type (`deny_unknown_fields`) | `tools-mcp-semantic/src/tools.rs` | 5-20 |
| Handler entry | `tools-mcp-semantic/src/tools.rs` | 45-75 |
| Pipeline (`index_workspace`) | `tools-mcp-semantic/src/model.rs` | 144-306 |
| Workspace scope resolution | `tools-mcp-semantic/src/discovery.rs` | 90-114, 249-272 |
| Discovery walker | `tools-mcp-semantic/src/discovery.rs` | 116-214 |
| Excluded directory names | `tools-mcp-semantic/src/discovery.rs` | 301-306 |
| Per-file size cap (1 MiB) | `tools-mcp-semantic/src/discovery.rs` | 15, 280-283 |
| Language extension map | `tools-mcp-semantic/src/discovery.rs` | 297-346 |
| Chunker dispatch | `tools-mcp-semantic/src/chunking.rs` | 112-128 |
| Symbol chunk cap (32 KiB) | `tools-mcp-semantic/src/chunking.rs` | 9, 188-191 |
| Fallback chunk window (100 lines / 15 overlap) | `tools-mcp-semantic/src/chunking.rs` | 10-11, 286-327 |
| chunk_id derivation | `tools-mcp-semantic/src/chunking.rs` | 347-374 |
| Manifest path | `tools-mcp-semantic/src/manifest.rs` | 130-132 |
| Manifest load / save | `tools-mcp-semantic/src/manifest.rs` | 27-77 |
| Manifest staleness | `tools-mcp-semantic/src/manifest.rs` | 79-107 |
| FastEmbed model id + slug | `tools-mcp-semantic/src/embedding.rs` | 7-8, 134-140 |
| Embedding batch size | `tools-mcp-semantic/src/embedding.rs` | 9-10, 142-148 |
| FastEmbed init + cache | `tools-mcp-semantic/src/embedding.rs` | 14, 24-53, 150-185 |
| Document prefix (`passage: `) | `tools-mcp-semantic/src/embedding.rs` | 63-71 |
| LanceDB table name | `tools-mcp-semantic/src/store.rs` | 13, 173-175 |
| LanceDB schema | `tools-mcp-semantic/src/store.rs` | 189-211 |
| LanceDB open / create | `tools-mcp-semantic/src/store.rs` | 68-104, 177-187 |
| Delete paths (batched) | `tools-mcp-semantic/src/store.rs` | 14, 106-121, 291-312 |
| Add chunks | `tools-mcp-semantic/src/store.rs` | 123-135, 214-289 |
| Response payload | `tools-mcp-semantic/src/model.rs` | 53-73 |
| Error remediation | `tools-mcp-semantic/src/tools.rs` | 67-74 |

## 10. Examples

### 10.1 Minimal request (index current workspace)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "SemanticIndex",
    "arguments": {}
  }
}
```

### 10.2 Targeted incremental reindex

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "SemanticIndex",
    "arguments": {
      "path": "tools-mcp-webfetch/src",
      "limit": 500,
      "timeout_ms": 60000
    }
  }
}
```

### 10.3 Force full reindex (ignore manifest hashes)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "SemanticIndex",
    "arguments": {
      "path": ".",
      "force": true,
      "hidden": true,
      "no_ignore": false
    }
  }
}
```

### 10.4 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "Indexed 9 file(s), updated 7 file(s); 53 chunk(s) indexed, 41 chunk(s) written; removed 2 stale/replaced chunk(s)."
      }
    ],
    "isError": false,
    "indexed_files": 9,
    "indexed_chunks": 53,
    "updated_files": 7,
    "updated_chunks": 41,
    "skipped_files": 12,
    "deleted_chunks": 2,
    "model": "jina-embeddings-v2-base-code",
    "store_path": "C:\\Users\\Daniel\\tools-mcp\\.tools-mcp\\semantic-index",
    "duration_ms": 1893,
    "incremental": true,
    "truncated": false,
    "timed_out": false
  }
}
```

### 10.5 Out-of-workspace path error

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "semantic index failed: path ..\\elsewhere resolves outside the server working directory C:\\Users\\Daniel\\tools-mcp"
      }
    ],
    "isError": true,
    "remediation": "Check the path, local model availability, and index directory permissions."
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `tools_list_contains_expected_set` | `tools-mcp-server/tests/integration_test.rs:104` | `SemanticIndex` appears in `mcp/tools/list`. |
| `golden_contract_lists_expected_tools` | `tools-mcp-server/tests/golden_contract.rs:55` | Golden test pinning the `SemanticIndex` tool name. |
| `directory_filter_includes_children_only` | `tools-mcp-semantic/src/discovery.rs:360` | `PathFilter::Directory` matches the directory and its descendants but not sibling prefixes. |
| `sql_literals_escape_single_quotes` | `tools-mcp-semantic/src/discovery.rs:369` | Single quotes in paths are doubled before reaching LanceDB. |
| `directory_filter_sql_treats_wildcards_literally` | `tools-mcp-semantic/src/discovery.rs:374` | Directory predicate uses a half-open range, not LIKE patterns. |
| `language_detection_is_ascii_case_insensitive` | `tools-mcp-semantic/src/discovery.rs:386` | Extension matching is case-insensitive for the supported set. |
| `skip_path_matches_complete_components_only` | `tools-mcp-semantic/src/discovery.rs:397` | Excluded directory names only fire on whole path components. |
| `chunks_rust_functions_with_line_spans` | `tools-mcp-semantic/src/chunking.rs:391` | Tree-sitter tag-based chunking captures Rust functions with correct line spans. |
| `chunks_markdown_by_heading` | `tools-mcp-semantic/src/chunking.rs:426` | Markdown chunker splits by heading and assigns titles as symbols. |
| `fallback_chunks_preserve_overlap_and_metadata` | `tools-mcp-semantic/src/chunking.rs:446` | Fallback windowing uses 100 lines with 15-line overlap. |
| `manifest_json_preserves_persisted_schema` | `tools-mcp-semantic/src/manifest.rs:140` | Manifest serializes the documented field set. |
| `default_model_slug_is_stable` | `tools-mcp-semantic/src/embedding.rs:220` | `default_model_slug()` matches the slug derivation rule. |
| `default_embedding_batch_size_matches_execution_provider` | `tools-mcp-semantic/src/embedding.rs:225` | Batch size is 128 with `gpu-cuda`, 32 otherwise. |
| `table_names_include_model_and_dimension` | `tools-mcp-semantic/src/store.rs:454` | LanceDB table name includes the model slug and vector dimension. |
| `delete_paths_predicate_escapes_batched_literals` | `tools-mcp-semantic/src/store.rs:517` | Path deletion predicate batches paths and escapes single quotes. |
| `delete_paths_removes_multiple_escaped_paths` | `tools-mcp-semantic/src/store.rs:528` | Round-trip delete against LanceDB removes the targeted rows. |

Coverage gap: there is no end-to-end test that exercises `SemanticIndex` through the JSON-RPC harness (only the `tools/list` golden assertion). The pipeline is exercised by the unit tests above plus the `semantic` benchmark harness (`tools-mcp-semantic/benches/semantic.rs`, behind the `bench-api` feature).

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Can the caller choose a different embedding model? | No. `default_model_id()` returns the compile-time constant `jina-embeddings-v2-base-code` (`tools-mcp-semantic/src/embedding.rs:7-8, 134-136`); the schema exposes no model field. |
| 2 | Does `force=true` rebuild the LanceDB table from scratch? | No. `force` only disables manifest short-circuiting; the table itself is opened or created on demand and only rows for changed paths are deleted (`tools-mcp-semantic/src/model.rs:170-173, 234-243`). |
| 3 | Where does the index live on disk? | `<workspace>/.tools-mcp/semantic-index/`; manifest at `<index_dir>/<model_slug>/manifest.json`; LanceDB at `<index_dir>/lancedb`; model weights at `<index_dir>/models` (`discovery.rs:14, 96`, `manifest.rs:130-132`, `store.rs:177-187`, `embedding.rs:27`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::err_with` and `parse_args` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_limit`, `clamp_timeout` semantics (§6.1). |
| `tools-mcp-server/src/composition.rs` | Composition-root registration call (§4.1). |
| `docs/tools/semantic-search.md` | Companion tool that reads the index this tool writes. |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
