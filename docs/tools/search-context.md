# SDD: search_context

**Date:** 2026-05-24
**Scope:** Design contract for the `search_context` MCP tool.
**Source:** `tools-mcp-local/src/tools/search_context.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `search_context` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`search_context` is the MCP tool that searches file contents for a pattern and, for each match, returns a merged, line-numbered window of the surrounding source. It is owned by the `tools-mcp-local` crate. The handler function is `handle_search_context` (`tools-mcp-local/src/tools/handlers/search_context.rs:87`). Internally the tool delegates pattern-matching to the `Search` tool's `handle_search` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:309`) with `context: 0` and then re-reads each matched file from disk through `tokio::fs::read` to expand a configurable number of context lines per match (`search_context.rs:115-128,206-268`). Adjacent windows in the same file are merged.

### 3.2 Explicitly Out of Scope

- The `Search` tool itself. See `docs/tools/search.md` for the underlying pattern-matching contract, including the in-memory vs ugrep backend split, response shape for matches, and security properties of the path-list-injection defenses.
- The `Read` tool. `search_context` does not invoke `Read`; it reads matched files directly via `tokio::fs::read` (`search_context.rs:226`).
- JSON-RPC framing and routing, tool-registry composition, semantic search, and unrelated file tools.
- File-system mutation. `search_context` is strictly read-only.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `search_context` |
| Aliases | None |
| Registration gate | Always registered (no env gate). |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_search_context` (`tools-mcp-local/src/tools/handlers/search_context.rs:87`) |
| Schema definition | `tools-mcp-local/src/tools/search_context.rs:4-30` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:39` (invoked from `tools-mcp-server/src/composition.rs:88` via `tools_mcp_local::register_tools`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **`pattern` is required and non-whitespace.** Arguments without `pattern`, or whose `pattern` contains only whitespace, MUST be rejected with `"pattern is required (non-empty string)"` (`tools-mcp-core/src/validation.rs:11-22`, called from `search_context.rs:93-95`).
- **Optional `path` is non-whitespace when supplied.** When `path` is supplied it MUST be non-whitespace; when omitted the underlying search defaults to `"."` (`search_context.rs:96-100,121`).
- **`context_lines` clamped to `[0, 50]`, default `3`.** Out-of-range or absent values MUST be clamped, not rejected (`search_context.rs:12-13,102-107`).
- **`max_matches` clamped to `[1, 200]`, default `20`.** `max_results` is an alias; when both are present `max_matches` wins (`search_context.rs:14-15,40-42,108-113`).
- **Match expansion is bounded by the underlying `Search` cap.** `max_matches` is forwarded as `Search`'s `max_results` (`search_context.rs:136-137`); the underlying search cap (clamped to `[1, 10000]`, default `100`) is therefore overridden by `max_matches` for this tool.
- **`context: 0` always passed to `Search`.** Context lines are reconstructed locally from disk reads; `search_context` MUST NOT forward `context > 0` to `Search` because the upstream tool's context-line events would otherwise inflate `max_results` accounting (`search_context.rs:136`).
- **Search failure is propagated verbatim.** If `Search` returns `isError: true`, `search_context` MUST return the same `ToolCallOutcome` unchanged (`search_context.rs:117-119`).
- **Matched paths MUST stay under the requested search root.** Before expansion, every reported path is canonicalized and compared to the canonicalized search root via `validate_match_path` (`search_context.rs:188-204`). A path that cannot be canonicalized, or that resolves outside the canonical root, is silently dropped from the windows expansion. When the root is a file, the matched path MUST canonicalize to the same file.
- **Per-file reads use `tokio::fs::read`.** Each file is read once per call, asynchronously, with errors converted into tool-level errors `"failed to read search match path <path>: <err>"` (`search_context.rs:226-228`).
- **Lossy UTF-8 line splitting.** File contents are decoded with `String::from_utf8_lossy` (`search_context.rs:298-303`); invalid bytes are replaced with the Unicode replacement character (`U+FFFD`). Lines are split on `\n`; a trailing `\r` is dropped so CRLF-terminated files render the same as LF (`search_context.rs:305-333`).
- **Overlapping/adjacent windows MUST be merged in-file.** Two match lines whose `[start..end]` windows are adjacent (`start <= previous.end + 1`) MUST be merged into a single window with the union of their `match_lines` (`search_context.rs:277-296`).
- **Windows MUST be 1-indexed and bounded by file length.** `start_line = max(1, line - context_lines)`, `end_line = min(total_lines, line + context_lines)`. Matches at lines `0` or beyond `total_lines` are skipped (`search_context.rs:241-247`).
- **No panic on failure.** Every error path returns a `ToolCallOutcome`; the handler MUST NOT panic (`search_context.rs:87-131`).
- **`additionalProperties: false` and `deny_unknown_fields`.** The JSON schema sets `additionalProperties: false` (`search_context.rs:27`); the request struct sets `#[serde(deny_unknown_fields)]` (`search_context.rs:18`). Unknown fields produce a tool-level error with text `"invalid arguments: ..."`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT re-implement pattern matching independently from `Search`. All match discovery MUST flow through `handle_search` so backend selection (memory vs ugrep), fallback metadata, and security defenses are inherited.
- MUST NOT expand context for a path outside the canonicalized search root; such paths are silently dropped from `windows[]` so an attacker cannot use a regex that emits an absolute path to extract content outside the requested scope.
- MUST NOT collapse the structured `windows[]` array even when the rendered text view is empty; clients depend on the array's stability.
- MUST NOT propagate the underlying `Search` payload's diagnostic fields (`backend`, `plan_kind`, etc.) at the top level. Only `search_truncated`, `search_timed_out`, `search_backend`, and the match counters are re-surfaced (`search_context.rs:421-425`).
- MUST NOT return the search-tool path-list-injection rejection as a `search_context`-specific error class; failures propagate through `Search`'s existing error envelope unchanged.

## 5. Design Goals

- **One round trip, file-grouped output for code-reading agents.** A caller running on Search would otherwise need to issue a follow-up `Read` per match. `search_context` collapses that into a single response with merged windows and `>line` markers indicating matched lines, so an agent can quote a finding in one shot.
- **Trust the underlying `Search` invariants.** All backend selection, glob handling, fuzzy logic, regex DoS controls, path-list injection defenses, and timeout/cancellation behaviors live in `Search`. `search_context` adds *only* the per-file context expansion and re-checks the root containment of returned paths.
- **Bounded expansion cost.** Per-call expansion is bounded by `max_matches ≤ 200` and `context_lines ≤ 50`, so the worst case is 200 windows × ~101 lines each in addition to the per-file read cost.
- **Deterministic line markers.** Match lines are prefixed with `>`; surrounding context lines with a space. Line numbers are right-aligned to the width of the largest line number in the window (`search_context.rs:345-376`).

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `pattern` | string | Yes | — | Non-whitespace | Text or regex to search for. Forwarded to `Search`'s `pattern`. |
| `path` | string | No | `"."` | Non-whitespace when supplied | File or directory to search. Forwarded to `Search`'s `path`. |
| `case` | string | No | `"smart"` (per `Search`) | Forwarded to `Search` enum | Smart / sensitive / insensitive matching. See `docs/tools/search.md` §6.1. |
| `fixed_strings` | boolean | No | `false` | — | Treat pattern as literal. Forwarded to `Search`. |
| `word_regexp` | boolean | No | `false` | — | Whole-word search. Forwarded to `Search`. |
| `glob` | array of string | No | — | — | Glob filter list. Forwarded to `Search`. |
| `hidden` | boolean | No | `false` | — | Search hidden files. Forwarded to `Search`. |
| `follow` | boolean | No | `false` | — | Follow symlinks. Forwarded to `Search`. |
| `no_ignore` | boolean | No | `false` | — | Bypass `.gitignore`/`.ignore`. Forwarded to `Search`. |
| `context_lines` | integer | No | `3` | Clamped to `[0, 50]` | Lines of context on each side of every match in the expanded window. Local to this tool; NOT forwarded to `Search` (which always receives `context: 0`). |
| `max_matches` | integer | No | `20` | Clamped to `[1, 200]` | Maximum match lines to expand into context windows. Forwarded to `Search` as `max_results`. |
| `max_results` | integer | No | — | Schema range `[1, 200]`; clamped to `[1, 200]` when used | Alias for `max_matches`. Used only when `max_matches` is absent (`search_context.rs:108-109`). |
| `timeout_ms` | integer | No | `10000` (per `Search`) | `>= 100` (per `Search` clamp) | Forwarded to `Search` as the per-call deadline. |
| `fuzzy` | integer | No | _(none)_ | `[1, 4]` | Forwarded to `Search`. |

Schema source: `tools-mcp-local/src/tools/search_context.rs:8-29`.
Request struct: `tools-mcp-local/src/tools/handlers/search_context.rs:17-47` (`#[serde(deny_unknown_fields)]`).

### 6.2 Behavior

Ordered execution steps from `handle_search_context` (`tools-mcp-local/src/tools/handlers/search_context.rs:87-131`). Each step lists the file:line for verification.

1. **Parse arguments and validate non-empty fields.** Deserialize JSON into `SearchContextRequest` (rejects unknown fields). Call `validation::validate_non_empty(&req.pattern, "pattern", None)` (`search_context.rs:88-95`). If `path` was supplied, also call `validate_non_empty` on it (`search_context.rs:96-100`).
2. **Resolve `context_lines` and `max_matches`.** Clamp via `validation::clamp_limit` to `[0, 50]` (default `3`) and `[1, 200]` (default `20`). When both `max_matches` and `max_results` are absent, the default applies; when only `max_results` is present it is used; when both are present `max_matches` wins (`search_context.rs:102-113`).
3. **Build inner `Search` arguments.** Construct a JSON object with `pattern`, `context: 0`, `max_results: max_matches`, plus pass-through fields (`path`, `case`, `fixed_strings`, `word_regexp`, `glob`, `hidden`, `follow`, `no_ignore`, `timeout_ms`, `fuzzy`) when present (`search_context.rs:133-151`).
4. **Invoke `Search`.** Call `handle_search(None, search_args).await` (`search_context.rs:115-116`). If the result has `isError: true`, return it verbatim — including all upstream diagnostic fields (`search_context.rs:117-119`).
5. **Extract match locations and canonicalize against the search root.** Iterate `result["matches"]`, keeping only `type == "match"` entries (skipping context events). For each entry, extract `data.path.text` and `data.line_number`. Reject paths/locations that fail any of: empty/whitespace path text, missing `line_number`, `line_number` overflows `usize`, path is not canonicalizable, search root is not canonicalizable, the path does not match (file root) or does not lie under (directory root) the canonical root. Each surviving match yields a `MatchLocation { path, read_path, line_number }` where `read_path` is the canonical absolute path used for the disk read and `path` is the original display path from `Search` (`search_context.rs:163-204`).
6. **Read each matched file once and expand windows.** Group `MatchLocation`s by `read_path` while preserving first-seen order (`search_context.rs:206-222`). For each path, perform `tokio::fs::read(read_path).await`; on I/O error, return tool-level error `"failed to read search match path <path>: <err>"` (`search_context.rs:226-228`). Decode bytes with `String::from_utf8_lossy`; split on `\n` (a trailing `\r` is dropped from each segment); compute `total_lines`. Sort and dedup the match line numbers per file (`search_context.rs:239-240`); skip line numbers equal to `0` or greater than `total_lines` (`search_context.rs:242-244`). For each surviving match line, compute `start_line = max(1, line - context_lines)` and `end_line = min(total_lines, line + context_lines)`; pass to `push_merged_range`, which merges with the previous window when `start_line <= previous.end_line + 1` (`search_context.rs:245-247,277-296`).
7. **Render numbered text per window.** For each merged window, emit one line per source line in the form `<marker><line_number>\t<text>` where `marker` is `>` for match lines and a space for context (`search_context.rs:345-376`). Line numbers are right-aligned to the decimal width of the window's `end_line`. CRLF line terminators are normalized to LF in the rendered output because `\r` is stripped during splitting (`search_context.rs:319-326`).
8. **Render the combined text view.** Concatenate windows with `\n\n` separators; prefix each window with a `path:start-end\n` header (`search_context.rs:378-406`).
9. **Build the response payload.** Echo the caller's pattern, the search root (default `"."`), the clamped `context_lines`, propagate the underlying search payload's `match_count`, `event_count`, `truncated` (as `search_truncated`), `timed_out` (as `search_timed_out`), and `backend` (as `search_backend`, may be null if missing), plus the trimmed match locations and the expanded windows (`search_context.rs:408-442`). Return `ToolCallOutcome::ok(payload)`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "tools-mcp-local/src/tools/handlers/read_file.rs:1-3\n>1\t//! File reading handler implementation.\n 2\t\n 3\tuse memchr::memchr2;"}],
  "isError": false,
  "pattern": "File reading handler implementation.",
  "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
  "context_lines": 2,
  "match_count": 1,
  "event_count": 1,
  "search_truncated": false,
  "search_timed_out": false,
  "search_backend": "memory",
  "matches": [
    {"path": "tools-mcp-local/src/tools/handlers/read_file.rs", "line_number": 1}
  ],
  "windows": [
    {
      "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
      "start_line": 1,
      "end_line": 3,
      "match_lines": [1],
      "total_lines": 200,
      "text": ">1\t//! File reading handler implementation.\n 2\t\n 3\tuse memchr::memchr2;"
    }
  ]
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Combined rendering of all windows. Each window is prefixed with `<path>:<start>-<end>\n`, lines are tab-delimited `<marker><line_no>\t<text>`, and consecutive windows are separated by `\n\n`. Empty when no matches survive the root-containment filter. |
| `isError` | boolean | Yes | Always `false` on success. |
| `pattern` | string | Yes | Echoes the caller's pattern. |
| `path` | string | Yes | The caller's `path`, or `"."` when omitted. |
| `context_lines` | integer | Yes | The clamped `context_lines` (`[0, 50]`, default `3`). |
| `match_count` | integer | Yes | Copied from the inner `Search` payload's `match_count`. |
| `event_count` | integer | Yes | Copied from the inner `Search` payload's `event_count`. |
| `search_truncated` | boolean | Yes | Copied from the inner `Search` payload's `truncated`. |
| `search_timed_out` | boolean | Yes | Copied from the inner `Search` payload's `timed_out`. |
| `search_backend` | string / null | Yes | Copied from the inner `Search` payload's `backend` (memory or ugrep-fallback), or `null` when absent. |
| `matches[]` | array | Yes | One entry per surviving match: `{path: string, line_number: integer}`. |
| `windows[]` | array | Yes | One entry per merged window: `{path, start_line, end_line, match_lines: integer[], total_lines, text}`. |

`match_count` is taken from `Search`'s view (the full pre-filtering count) while the surviving `matches[]` and `windows[]` reflect post-canonicalization filtering. The two can differ if some matched paths failed canonicalization or fell outside the search root.

**Tool-level error (`isError: true`):**

Tool-level errors use `ToolCallOutcome::err` (`tools-mcp-core/src/tool_outcome.rs:35`). When the upstream `Search` fails, the exact upstream `ToolCallOutcome` is returned, which may include structured diagnostic fields beyond the basic envelope.

```json
{
  "content": [{"type": "text", "text": "pattern is required (non-empty string)"}],
  "isError": true
}
```

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Missing `pattern` | `true` | `"pattern is required (non-empty string)"` |
| Whitespace-only `pattern` | `true` | `"pattern is required (non-empty string)"` |
| Whitespace-only `path` | `true` | `"path is required (non-empty string)"` |
| Unknown field in `arguments` | `true` | `"invalid arguments: unknown field ..."` |
| Wrong type for a field | `true` | `"invalid arguments: invalid type ..."` |
| Underlying `Search` returns an error | `true` | Verbatim from `Search` (see `docs/tools/search.md` §6.4). |
| File read fails for a matched path (I/O error) | `true` | `"failed to read search match path <path>: <err>"` |
| Matched path is outside the canonicalized search root | `false` (silently dropped from `windows[]`) | Matched entry is simply not expanded; no error surfaced. |
| Match line number is `0` or beyond `total_lines` | `false` (silently dropped from `windows[]`) | No error surfaced. |

The handler does NOT define a `timed_out` field at its own top level; per-call timeout is surfaced via the underlying `search_timed_out` copy. The underlying `Search` is responsible for honoring `timeout_ms`.

## 7. Security Considerations

- **Path-policy enforcement boundary.** `search_context` does NOT call into `tools-mcp-local/src/path_policy.rs`. Instead it enforces a narrower property via `validate_match_path` (`search_context.rs:188-204`): every returned match path MUST canonicalize to a location under the caller's search root. This is a per-call containment check, not a workspace-root check. A caller can still point the search root anywhere the process can read; what they cannot do is induce `search_context` to expand a window for a file *outside the requested root* (which an attacker might attempt by registering a regex that ugrep emits as an absolute path).
- **Inherited Search defenses.** The path-list-injection defenses (`--from=-` LF/CR rejection, non-UTF-8 path rejection, defense-in-depth result filtering) are entirely in `Search`; `search_context` benefits from them transitively because it never spawns ugrep itself. See `docs/tools/search.md` §7 and `tools-mcp-local/src/tools/handlers/search_file_selection.rs:374-388,659-685`.
- **Inherited regex DoS controls.** The memory backend's `RegexBuilder::size_limit` and the per-call `timeout_ms` deadline are honored by `Search`; `search_context` simply propagates `timeout_ms`. The local expansion step has no regex evaluation.
- **File-read trust boundary.** Per-file reads use `tokio::fs::read` (`search_context.rs:226`); the bytes are decoded with `from_utf8_lossy` so invalid bytes cannot inject control characters but are replaced with `U+FFFD`. Returned `text`/`content[0].text` is external data; consuming systems MUST treat it as untrusted input and MUST NOT execute, eval, or interpret it as instructions.
- **Bounded output size.** `windows[].text` size is bounded by `max_matches × (2 × context_lines + 1)` lines per window, with `max_matches ≤ 200` and `context_lines ≤ 50`. Each line is the raw file line (after CRLF normalization); there is no per-line cap, but `tokio::fs::read` will fail before reading files larger than the OS allows. Adjacent windows are merged, further bounding worst-case repeated context.
- **Symlink handling.** When `follow=false` (default), `Search` skips symlinks during walking and `search_context` never sees them. When `follow=true`, the matched path is canonicalized via `std::fs::canonicalize` before the disk read (`search_context.rs:189`), so symlinks are resolved to their targets and the root-containment check uses the canonical target.

## 8. Configuration

This tool reads no environment variables of its own. All environment-driven behavior is inherited from the underlying `Search` (see `docs/tools/search.md` §8).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 39 |
| Tool name + schema | `tools-mcp-local/src/tools/search_context.rs` | 4-30 |
| Handler entry point | `tools-mcp-local/src/tools/handlers/search_context.rs` | 87-131 |
| Request struct (`deny_unknown_fields`) | `tools-mcp-local/src/tools/handlers/search_context.rs` | 17-47 |
| Argument validation (`pattern`/`path` non-empty) | `tools-mcp-local/src/tools/handlers/search_context.rs` | 93-100 |
| Clamp `context_lines`, `max_matches` | `tools-mcp-local/src/tools/handlers/search_context.rs` | 102-113 |
| `max_matches` wins over `max_results` | `tools-mcp-local/src/tools/handlers/search_context.rs` | 108-109 |
| Build inner `Search` args (always `context: 0`) | `tools-mcp-local/src/tools/handlers/search_context.rs` | 133-151 |
| Propagate inner `isError: true` verbatim | `tools-mcp-local/src/tools/handlers/search_context.rs` | 117-119 |
| Canonical root-containment check | `tools-mcp-local/src/tools/handlers/search_context.rs` | 188-204 |
| Per-file read via `tokio::fs::read` | `tools-mcp-local/src/tools/handlers/search_context.rs` | 226-228 |
| UTF-8-lossy line splitting with CRLF stripping | `tools-mcp-local/src/tools/handlers/search_context.rs` | 298-333 |
| Window expansion `start_line/end_line` math | `tools-mcp-local/src/tools/handlers/search_context.rs` | 241-247 |
| Adjacent-window merge rule | `tools-mcp-local/src/tools/handlers/search_context.rs` | 277-296 |
| Numbered window rendering with `>` marker | `tools-mcp-local/src/tools/handlers/search_context.rs` | 345-376 |
| Multi-window text view | `tools-mcp-local/src/tools/handlers/search_context.rs` | 378-406 |
| Response payload | `tools-mcp-local/src/tools/handlers/search_context.rs` | 408-442 |
| Schema bounds for `context_lines` (`[0, 50]`) and `max_matches`/`max_results` (`[1, 200]`) | `tools-mcp-local/src/tools/search_context.rs` | 20-22 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "search_context",
    "arguments": {
      "pattern": "File reading handler implementation.",
      "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
      "fixed_strings": true,
      "context_lines": 0,
      "max_matches": 1
    }
  }
}
```

### 10.2 Success response (single match, zero context)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "tools-mcp-local/src/tools/handlers/read_file.rs:1-1\n>1\t//! File reading handler implementation."}],
    "isError": false,
    "pattern": "File reading handler implementation.",
    "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
    "context_lines": 0,
    "match_count": 1,
    "event_count": 1,
    "search_truncated": false,
    "search_timed_out": false,
    "search_backend": "memory",
    "matches": [
      {"path": "tools-mcp-local/src/tools/handlers/read_file.rs", "line_number": 1}
    ],
    "windows": [
      {
        "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
        "start_line": 1,
        "end_line": 1,
        "match_lines": [1],
        "total_lines": 200,
        "text": ">1\t//! File reading handler implementation."
      }
    ]
  }
}
```

### 10.3 Merged window (two close matches)

When two matches in the same file lie within `context_lines + 1` of each other, their windows merge into one. For a file `src/example.rs` with matches at lines 3 and 6 and `context_lines: 2`:

```json
{
  "result": {
    "content": [{"type": "text", "text": "src/example.rs:1-8\n 1\tuse std::io;\n 2\t\n>3\tlet first = io::stdin();\n 4\t// ...\n 5\t// ...\n>6\tlet second = io::stdout();\n 7\t// ...\n 8\t// ..."}],
    "isError": false,
    "pattern": "io::std",
    "path": "src/example.rs",
    "context_lines": 2,
    "match_count": 2,
    "event_count": 2,
    "search_truncated": false,
    "search_timed_out": false,
    "search_backend": "memory",
    "matches": [
      {"path": "src/example.rs", "line_number": 3},
      {"path": "src/example.rs", "line_number": 6}
    ],
    "windows": [
      {
        "path": "src/example.rs",
        "start_line": 1,
        "end_line": 8,
        "match_lines": [3, 6],
        "total_lines": 12,
        "text": " 1\tuse std::io;\n 2\t\n>3\tlet first = io::stdin();\n 4\t// ...\n 5\t// ...\n>6\tlet second = io::stdout();\n 7\t// ...\n 8\t// ..."
      }
    ]
  }
}
```

### 10.4 Missing pattern (validation error)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "search_context",
    "arguments": {"path": "src"}
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [{"type": "text", "text": "invalid arguments: missing field `pattern`. Required fields are missing; provide all required arguments per the tool schema."}],
    "isError": true
  }
}
```

### 10.5 Underlying `Search` failure propagated

If the upstream `Search` cannot run ugrep (e.g., the memory backend declines and ugrep is not on PATH), the error is propagated unchanged:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "ugrep error: failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: program not found"}],
    "isError": true
  }
}
```

## 11. Testing

### 11.1 Integration tests (MCP envelope, spawned server binary)

| Test | File | What it covers |
|---|---|---|
| `test_search_context_returns_numbered_file_window` | `tools-mcp-server/tests/integration_test.rs:345-387` | End-to-end: a single literal match against a known file returns one window whose text contains the `path:start-end` header and the `>N\t<line>` match marker. Locks `context_lines: 0`, `windows.len() == 1`, and the rendered marker shape. |

### 11.2 In-module unit tests in `tools-mcp-local/src/tools/handlers/search_context.rs`

| Test | File | What it covers |
|---|---|---|
| `push_merged_range_merges_overlapping_match_windows` | `tools-mcp-local/src/tools/handlers/search_context.rs:458-481` | Adjacent windows whose ranges overlap or are within one line of each other MUST be merged with their `match_lines` unioned. |
| `render_numbered_window_marks_multiple_match_lines` | `tools-mcp-local/src/tools/handlers/search_context.rs:483-496` | Match lines get `>`; context lines get a space; line numbers right-aligned. |
| `collect_lines_lossy_matches_str_lines_edges` | `tools-mcp-local/src/tools/handlers/search_context.rs:498-503` | CRLF line splitting matches `str::lines` semantics for empty middle lines. |
| `collect_lines_lossy_replaces_invalid_utf8` | `tools-mcp-local/src/tools/handlers/search_context.rs:505-510` | Invalid UTF-8 bytes are replaced with `U+FFFD`. |
| `render_numbered_window_omits_crlf_terminators` | `tools-mcp-local/src/tools/handlers/search_context.rs:512-525` | `\r\n`-terminated lines render without trailing `\r`. |
| `render_context_text_assembles_window_headers_and_spacing` | `tools-mcp-local/src/tools/handlers/search_context.rs:527-552` | Multi-window assembly: `path:start-end\n<text>` joined by `\n\n`. |
| `validate_match_path_rejects_out_of_root_path` | `tools-mcp-local/src/tools/handlers/search_context.rs:554-578` | A match path outside the canonicalized root MUST be rejected; a path inside MUST be accepted. |

Coverage gaps (no targeted tests today): (a) propagation of `isError: true` from `Search` directly into the `search_context` response; (b) interaction of `max_matches` overriding `max_results` when both are supplied; (c) reading a file whose total_lines is below a reported `line_number`; (d) the `search_backend` field being `null` when the upstream payload lacks `backend`. These are not currently regression-locked.

## 12. Open Questions

1. The handler defaults the path to `"."` only when `path` is absent (`search_context.rs:121`). When `path` is supplied but the inner `Search` returns matches whose paths cannot be canonicalized (e.g., the file was removed between `Search` and the disk read), those matches are silently dropped from `windows[]` while still appearing in `match_count`. Whether this divergence between `match_count` (pre-filter) and `matches[]`/`windows[]` (post-filter) is intended or should be surfaced as a diagnostic (e.g., `dropped_matches_count`) is a product decision outside this SDD's scope. The current contract is described in §6.3.
2. The schema declares `max_results` as an `integer` with `minimum: 1, maximum: 200` (`search_context.rs:22`) while `max_matches` has `default: 20`. A caller supplying `max_results` without `max_matches` will be subject to the default-vs-alias logic in `search_context.rs:108-109` — the alias is honored, but no test currently locks this in. Whether `max_results` should be removed from the schema (so the only way to override is `max_matches`) or kept for cross-tool compatibility is unresolved.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `search_context` re-implement pattern matching? | No. It delegates entirely to `handle_search` (`search_context.rs:115-116`), then reads the matched files from disk to expand windows. All backend selection, fallback metadata, regex DoS controls, and path-list injection defenses live in `Search`. |
| 2 | Does `search_context` enforce a workspace path policy? | No. It enforces a *per-call* root-containment check via `validate_match_path` (`search_context.rs:188-204`): every matched path's canonicalization MUST equal (file root) or lie under (directory root) the canonicalized search root. This is narrower than the workspace-CWD enforcement done by `Read`/`Write`/`Edit`/`Delete`/`Move`/`Copy`/`Pwsh`. |
| 3 | What context-line value is forwarded to `Search`? | Always `0` (`search_context.rs:136`). Context expansion is performed locally so the upstream `max_results` budget is spent only on match events. |
| 4 | When both `max_matches` and `max_results` are present, which wins? | `max_matches` (`search_context.rs:108-109` evaluates `req.max_matches.or(req.max_results)`). The schema range for both is `[1, 200]` (`search_context.rs:21-22`). |
| 5 | Are matched paths outside the search root reported as errors? | No. They are silently filtered out by `validate_match_path` and excluded from `matches[]`/`windows[]` (`search_context.rs:178-186,188-204`). The upstream `match_count` is NOT decremented, so a caller can detect drops by comparing `match_count` to `matches.len()`. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok` / `err` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_limit` helpers (§4.2, §6.1, §6.2). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/tools/handlers/ripgrep.rs` | `handle_search` is the delegate target (§3.1, §6.2). |
| `tools-mcp-local/src/tools/handlers/search_contract.rs` | Source of the `matches[]` / `files[]` shape this tool parses (§6.2 step 5). |
| `docs/tools/search.md` | Authoritative SDD for the underlying `Search` tool (§3.2, §7, §8). |
| `docs/security.md` | Project-wide trust-boundary guidance for tool-returned content (§7). |
