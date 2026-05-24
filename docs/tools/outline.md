# SDD: Outline

**Date:** 2026-05-24
**Scope:** Design contract for the `Outline` MCP tool.
**Source:** `tools-mcp-local/src/tools/outline.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Outline` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Outline` returns a structural outline of one source file. It uses tree-sitter parsers for C++, Rust, TypeScript, TSX, JavaScript, Python, and Go (with the language's standard `TAGS_QUERY` for non-C++ languages) and a heading-extractor for Markdown. The C++ path renders a header-style outline including namespaces, classes (with visibility modifiers), enums, function signatures, and conditional preprocessor guards. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_outline` in `tools-mcp-local/src/tools/outline.rs:141`.

### 3.2 Explicitly Out of Scope

- Cross-file analysis or symbol references. The tool processes one file in isolation.
- Pretty-printing or code formatting.
- Type inference. C++ output uses raw declaration text; non-C++ uses captured names from the language's tag query.
- Path policy enforcement on the file path. `Outline` uses the path verbatim, like `Read` and `ListDir`. Mutation tools enforce the workspace policy.
- Languages outside the documented supported list (`SUPPORTED_OUTLINE_EXTENSIONS`, `outline.rs:31-50`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Outline` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_outline` (`tools-mcp-local/src/tools/outline.rs:141`) |
| Schema definition | `tools-mcp-local/src/tools/outline.rs:866-886` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:28`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err` or `err_with` (`outline.rs:144,154,163,259-266`). The handler MUST NOT panic.
- **`deny_unknown_fields`.** `OutlineRequest` rejects any property outside `path` / `include_private` (`outline.rs:134`).
- **Supported extensions only.** Files whose extension does not appear in `SUPPORTED_OUTLINE_EXTENSIONS` MUST return `ToolCallOutcome::err_with("unsupported language for outline", {path, extension, supported})` (`outline.rs:259-267`). Locked in by `unsupported_extension_returns_structured_error` (`outline.rs:933`).
- **Language dispatch by extension (case-insensitive).** Extension is lowercased before lookup (`outline.rs:270-273,276-288`); `.CPP` is treated as `.cpp`.
- **Tree-sitter parsers are thread-local cached.** Each language has a `ParserCache` slot in a `thread_local!` `OUTLINE_PARSERS` (`outline.rs:24-26,101-125`). Repeated calls in the same thread reuse the parser instance, after `parser.set_language(&L)`.
- **Tags queries are compiled once per process.** Each language has a `static OnceLock<CachedTagsQuery>` so the `Query::new(...)` compilation runs at most once per language per process (`outline.rs:17-22,404-415`). Locked in by `tags_query_cache_reuses_compiled_query_for_language_variant` (`outline.rs:972`).
- **Outline AST cache is keyed by `(path, language, modified, len, content_hash)`.** Re-running `Outline` on an unchanged file MUST return the cached rendered string and MUST NOT re-parse (`outline.rs:179-190,219-223`). When `(modified, len)` look the same but the bytes changed, the `content_hash` mismatch causes a re-parse. Locked in by `cache_invalidates_same_len_same_mtime_when_content_hash_changes` (`outline.rs:1088`).
- **`include_private` is part of the cache key for C++ only.** `cache_language` appends `+private` to the language string for C++ when `include_private=true` (`outline.rs:79-99`); for other languages the flag is ignored and the cache key is independent of it. This avoids serving a C++ outline missing private members when the caller asked for them, while preserving cache reuse for unaffected languages.
- **Markdown outline is heading-only and depth-indented.** Lines starting with 1-6 `#`s followed by a space MUST produce one outline entry indented `(depth-1) * 2` spaces, prefixed with `"# "`, and ending in a trailing newline removal pass (`outline.rs:456-486`). `###No-space-after-hashes` MUST NOT produce an entry. Locked in by `markdown_heading_extraction_respects_depth` (`outline.rs:985`).
- **Non-C++ outline uses the language's `TAGS_QUERY`.** Each match emits one line `"<kind> <name>"` where `<kind>` is the most-specific `definition.*` capture name (falling back to the first capture if no `definition.*` is present) (`outline.rs:417-454`). Locked in by `tags_query_outline_preserves_order_and_utf8_names` (`outline.rs:960`).
- **C++ outline is structural.** The C++ extractor walks the AST and emits namespaces, classes/structs with `class X { ... };` braces and access labels, enums (including `enum class`), templates, declarations (excluding initializers that look like values), function signatures (stripped to before the body), and `#include` / `#ifdef` blocks (`outline.rs:341-355,512-711,713-814`).
- **Symbol-name UTF-8 preserved.** Captured names that contain non-ASCII characters MUST be returned verbatim (`outline.rs:430-433`). Locked in by `tags_query_outline_preserves_order_and_utf8_names` (`outline.rs:960`).
- **Source bytes loaded with `tokio::fs::read_to_string`.** Files containing invalid UTF-8 MUST fail with `"failed to read file: <io error>"` (`outline.rs:160-165`); unlike `Read`, the Outline parser cannot operate on lossy bytes because tree-sitter requires valid UTF-8.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT emit outline entries for unsupported file types; the response MUST be a structured error with the `supported` array enumerated.
- MUST NOT cache a stale rendered outline. The cache key includes `(modified, len, content_hash)` so any change invalidates the entry.
- MUST NOT serve a C++ outline missing private members when the caller asked for them via `include_private=true` and a previous call did not. `cache_language` segregates the two cases.
- MUST NOT panic on parser failure; a failed parse returns `"failed to parse file"` (`outline.rs:338`).

## 5. Design Goals

- **One file, one render, deterministic.** Outlines are a navigation aid; they must be stable so an agent can issue `Outline` to retrieve a structural index that won't change between calls on the same bytes.
- **Cache at the right grain.** Cache key includes content hash so re-runs are cheap; thread-local parsers and process-static queries avoid re-initializing tree-sitter machinery.
- **Language-agnostic line format for non-C++.** A uniform `"<kind> <name>"` per line is easy for downstream tooling to filter or grep, regardless of source language.
- **Honest about unsupported files.** A structured error with `supported` list lets a caller route the request elsewhere instead of getting a silent empty outline.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Must exist; must have an extension in `SUPPORTED_OUTLINE_EXTENSIONS` | Source file to outline. |
| `include_private` | boolean | No | `false` | — | C++ only. When `true`, private class/struct members are included in the rendered outline. Ignored for other languages. |

Supported extensions (case-insensitive): `cpp`, `cxx`, `cc`, `h`, `hpp`, `hxx`, `rs`, `ts`, `tsx`, `js`, `mjs`, `cjs`, `jsx`, `py`, `pyi`, `go`, `md`, `markdown` (`outline.rs:31-50`).

The schema sets `"additionalProperties": false` (`outline.rs:883`); the request type uses `#[serde(deny_unknown_fields)]` (`outline.rs:134`).

> Schema source: `tools-mcp-local/src/tools/outline.rs:870-884`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<OutlineRequest>` (`outline.rs:142-145`).
2. **Stat the file** — `std::fs::metadata(path)`. On error: `"file not found: <path>"` (`outline.rs:151-155`).
3. **Detect language** — `normalized_extension` lowercases the file extension; `language_for_extension` maps it to an `OutlineLanguage` variant or `Unsupported` (`outline.rs:157-158,270-288`).
4. **Read source** — `tokio::fs::read_to_string(path).await`. On error: `"failed to read file: <io error>"` (`outline.rs:160-165`).
5. **Build cache key (supported languages only)** — `cache_language(include_private)` returns a string key including `+private` for C++ when applicable; an `OutlineKey {path, language, modified, len, content_hash}` is built (`outline.rs:169-177`). For `Unsupported`, no cache key is built.
6. **Cache lookup** — `outline_ast_cache().get(&key)`. On hit, return the cached rendered string with `bytes` set to `key.len` and `outline_bytes` to the cached length (`outline.rs:179-190`).
7. **Render outline** — `render_outline_for_language(...)` dispatches by language:
   - **C++**: `extract_cpp_outline` walks the parse tree and produces a header-style render including namespaces, classes with access labels, enums, type aliases, declarations, function signatures, and templates (`outline.rs:341-355,512-711`).
   - **Rust / TypeScript / Tsx / JavaScript / Python / Go**: `extract_outline_with_tags_query` runs the language's TAGS_QUERY and emits `"<kind> <name>"` per match (`outline.rs:358-366,417-454`).
   - **Markdown**: `extract_markdown_outline` walks lines, extracts 1-6 `#`-headers, indents by depth (`outline.rs:258,456-486`).
   - **Unsupported**: `ToolCallOutcome::err_with(...)` with `{path, extension, supported}` (`outline.rs:259-266`).
8. **Cache the render (supported)** — `outline_ast_cache().insert(key, arc.clone())` (`outline.rs:203-206`).
9. **Build success payload** — `{content: [{type: "text", text: rendered}], isError: false, path, bytes, outline_bytes}` (`outline.rs:208-216`). Returned via `ToolCallOutcome::ok(payload)`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "definition.function greet\ndefinition.class Greeter"}],
  "isError": false,
  "path": "sample.rs",
  "bytes": 35,
  "outline_bytes": 52
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Rendered outline. Format depends on language (see §6.2 step 7). |
| `isError` | boolean | Yes | Always `false` on success. |
| `path` | string | Yes | The path as supplied by the caller. |
| `bytes` | integer | Yes | Source byte length (from metadata on cache hit, from read source length on cache miss). |
| `outline_bytes` | integer | Yes | Byte length of the rendered outline string. |

**Tool-level error (`isError: true`):**

For unsupported extensions:

```json
{
  "content": [{"type": "text", "text": "unsupported language for outline"}],
  "isError": true,
  "path": "readme.txt",
  "extension": "txt",
  "supported": [".cpp", ".cxx", ".cc", ".h", ".hpp", ".hxx", ".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".py", ".pyi", ".go", ".md", ".markdown"]
}
```

Built via `ToolCallOutcome::err_with` (`tools-mcp-core/src/tool_outcome.rs:43-57`). Locked in by `unsupported_extension_returns_structured_error` (`outline.rs:933`).

For other errors:

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors use `ToolCallOutcome::err` (`tool_outcome.rs:35`). The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Argument deserialization failure | `true` | `"invalid arguments: ..."` plus class hint (`tool_outcome.rs:62-74`) |
| File not found (or `metadata` failure) | `true` | `"file not found: <path>"` (`outline.rs:154`) |
| Source read failure (e.g., invalid UTF-8, permission) | `true` | `"failed to read file: <io error>"` (`outline.rs:163`) |
| Tree-sitter `set_language` failure | `true` | `"failed to set language: <error>"` (`outline.rs:329`) |
| Tree-sitter parse failure | `true` | `"failed to parse file"` (`outline.rs:338`) |
| Tags query compilation failure | `true` | `"failed to compile tags query: <error>"` (`outline.rs:410`) |
| Unsupported extension | `true` (with extras) | `"unsupported language for outline"` plus `path`, `extension`, `supported` extras (`outline.rs:259-266`) |

## 7. Security Considerations

- **No path policy enforcement.** `Outline` uses `Path::new(path_str)` directly (`outline.rs:151`). Consistent with `Read`, `ListDir`, and `Glob`: read-only enumeration / inspection is intentionally unsandboxed. Mutation tools enforce the workspace policy.
- **Bounded by source size.** The parser allocates outline output with a pre-allocation capped at `MAX_PREALLOCATED_OUTLINE_BYTES = 64 KiB` (`outline.rs:28,488-490`); the actual output can grow beyond that, but the cap prevents pessimistic upfront allocation on very large source files.
- **Cache is process-scoped and bounded.** `DEFAULT_OUTLINE_CACHE_MAX_ENTRIES = 256` (`scope_cache.rs:18`); least-recently-used eviction prevents unbounded growth.
- **No remote calls or process spawning.** Parsing runs entirely in-process via tree-sitter.
- **Untrusted output.** Outline content is derived from external source files. Callers MUST NOT execute the rendered outline as code. Pathological filenames or identifier names are returned verbatim.

## 8. Configuration

Not applicable. `Outline` reads no environment variables. The cache lifetime is the process lifetime.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 28 |
| Tool name + schema | `tools-mcp-local/src/tools/outline.rs` | 866-886 |
| Handler entry point | `tools-mcp-local/src/tools/outline.rs` | 141 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/outline.rs` | 133-139 |
| `SUPPORTED_OUTLINE_EXTENSIONS` | `tools-mcp-local/src/tools/outline.rs` | 31-50 |
| Language enum + tree-sitter mapping | `tools-mcp-local/src/tools/outline.rs` | 52-99 |
| `cache_language` (segregates C++ `+private`) | `tools-mcp-local/src/tools/outline.rs` | 79-99 |
| Thread-local parser cache | `tools-mcp-local/src/tools/outline.rs` | 24-26, 101-125 |
| Process-static `OnceLock` tags-query cache | `tools-mcp-local/src/tools/outline.rs` | 17-22, 404-415 |
| Outline AST cache lookup | `tools-mcp-local/src/tools/outline.rs` | 179-190 |
| Outline AST cache insert | `tools-mcp-local/src/tools/outline.rs` | 203-206 |
| `render_outline_for_language` dispatch | `tools-mcp-local/src/tools/outline.rs` | 243-268 |
| `extract_outline_with_tags_query` | `tools-mcp-local/src/tools/outline.rs` | 358-366, 417-454 |
| C++ extractor entry | `tools-mcp-local/src/tools/outline.rs` | 341-355 |
| C++ tree traversal | `tools-mcp-local/src/tools/outline.rs` | 512-711 |
| C++ class-body access label handling | `tools-mcp-local/src/tools/outline.rs` | 713-814 |
| Markdown extractor | `tools-mcp-local/src/tools/outline.rs` | 456-486 |
| `outline_content_hash` (cache invalidation) | `tools-mcp-local/src/tools/outline.rs` | 219-223 |
| Source read with `tokio::fs::read_to_string` | `tools-mcp-local/src/tools/outline.rs` | 160-165 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Outline",
    "arguments": {"path": "src/main.rs"}
  }
}
```

### 10.2 Rust success (tags-query format)

Given `fn greet() {}\npub struct Greeter;\n`:

```json
{
  "result": {
    "content": [{"type": "text", "text": "definition.function greet\ndefinition.class Greeter"}],
    "isError": false,
    "path": "src/main.rs",
    "bytes": 33,
    "outline_bytes": 49
  }
}
```

Locked in by `tags_query_outline_preserves_order_and_utf8_names` (`outline.rs:960`).

### 10.3 Markdown outline

Given `# Root\n## Child\n### Grandchild\n#### Great Grandchild\n###Ignored\n`:

```json
{
  "result": {
    "content": [{"type": "text", "text": "# Root\n  # Child\n    # Grandchild\n      # Great Grandchild"}],
    "isError": false,
    "path": "notes.md",
    "bytes": 60,
    "outline_bytes": 50
  }
}
```

Locked in by `markdown_heading_extraction_respects_depth` (`outline.rs:985`).

### 10.4 C++ with `include_private`

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Outline",
    "arguments": {
      "path": "include/Greeter.h",
      "include_private": true
    }
  }
}
```

Returns a render including private members. The same path with `include_private: false` (or omitted) returns a different render and uses a different cache entry (`outline.rs:79-99`).

### 10.5 Unsupported extension

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Outline",
    "arguments": {"path": "README.txt"}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "unsupported language for outline"}],
    "isError": true,
    "path": "README.txt",
    "extension": "txt",
    "supported": [".cpp", ".cxx", ".cc", ".h", ".hpp", ".hxx", ".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".py", ".pyi", ".go", ".md", ".markdown"]
  }
}
```

Locked in by `unsupported_extension_returns_structured_error` (`outline.rs:933`).

### 10.6 Missing file

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "file not found: missing.rs"}],
    "isError": true
  }
}
```

Locked in by `missing_path_returns_file_not_found_error` (`outline.rs:1025`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `each_supported_language_emits_at_least_one_entry` | `tools-mcp-local/src/tools/outline.rs:898` | All 8 languages produce non-empty output for a minimal sample. |
| `unsupported_extension_returns_structured_error` | `tools-mcp-local/src/tools/outline.rs:933` | `.txt` → `unsupported language for outline` with `path`/`extension`/`supported`. |
| `repeated_tags_query_outline_extraction_returns_identical_output` | `tools-mcp-local/src/tools/outline.rs:948` | Idempotency of the tags-query path. |
| `tags_query_outline_preserves_order_and_utf8_names` | `tools-mcp-local/src/tools/outline.rs:960` | Non-ASCII identifiers preserved; capture-order maintained. |
| `tags_query_cache_reuses_compiled_query_for_language_variant` | `tools-mcp-local/src/tools/outline.rs:972` | `OnceLock` caches compiled query by `Arc` identity. |
| `markdown_heading_extraction_respects_depth` | `tools-mcp-local/src/tools/outline.rs:985` | 1-6 `#` produce entries; missing-space form rejected. |
| `missing_path_returns_file_not_found_error` | `tools-mcp-local/src/tools/outline.rs:1025` | Missing source → `"file not found: <path>"`. |
| `rust_outline_populates_cache_and_returns_identical_second_call` | `tools-mcp-local/src/tools/outline.rs:1040` | First call populates cache; second returns identical text. |
| `cache_invalidates_when_file_changes` | `tools-mcp-local/src/tools/outline.rs:1062` | mtime+len changes invalidate the cache. |
| `cache_invalidates_same_len_same_mtime_when_content_hash_changes` | `tools-mcp-local/src/tools/outline.rs:1088` | Same `(len, modified)` but different bytes → `content_hash` mismatch → re-parse. |
| `markdown_outline_populates_cache` | `tools-mcp-local/src/tools/outline.rs:1128` | Markdown also caches. |
| `unsupported_extension_does_not_touch_cache` | `tools-mcp-local/src/tools/outline.rs:1145` | Unsupported extensions never write to the cache. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Which file types does `Outline` support? | C++ (`.cpp/.cxx/.cc/.h/.hpp/.hxx`), Rust (`.rs`), TypeScript (`.ts/.tsx`), JavaScript (`.js/.mjs/.cjs/.jsx`), Python (`.py/.pyi`), Go (`.go`), Markdown (`.md/.markdown`). Anything else returns an `unsupported language for outline` error. |
| 2 | What does `include_private` do? | Affects only C++. When `true`, private class/struct members are emitted; for other languages the flag is accepted but ignored. The flag participates in the cache key for C++ only (`outline.rs:79-99`). |
| 3 | What backing parser is used? | tree-sitter for all languages except Markdown; Markdown uses a hand-rolled line scanner that recognizes 1-6 `#` headings with a space after the marker (`outline.rs:456-486`). |
| 4 | What is the output format for non-C++ tags-query languages? | One line per definition: `"<kind> <name>"` where `<kind>` is the most specific `definition.*` capture (e.g., `definition.function`, `definition.class`) (`outline.rs:417-454`). |
| 5 | How is the AST cache invalidated when a file changes without mtime/len changing? | The cache key includes `content_hash` (a 64-bit `DefaultHasher` of file bytes) so identical mtime+len but different content evicts the entry on next lookup (`outline.rs:219-223,1088-1125`). |
| 6 | Does `Outline` enforce path policy? | No. Read-only inspection tools (`Read`, `ListDir`, `Glob`, `Outline`) intentionally do not. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok`, `err`, `err_with` constructors (§6.3, §6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/tools/outline.rs` | Handler, schema, language dispatch, C++ traversal, tags-query rendering, Markdown extractor (§6.2). |
| `tools-mcp-local/src/tools/scope_cache.rs` | `OutlineKey` and `outline_ast_cache()` definitions (§4.2). |
| `tree-sitter-cpp` / `tree-sitter-rust` / `tree-sitter-typescript` / `tree-sitter-javascript` / `tree-sitter-python` / `tree-sitter-go` | Language grammars and (for non-C++) `TAGS_QUERY` strings consumed by `extract_outline_with_tags_query` (§6.2). |
