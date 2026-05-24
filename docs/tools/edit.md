# SDD: Edit

**Date:** 2026-05-24
**Scope:** Design contract for the `Edit` MCP tool.
**Source:** `tools-mcp-local/src/tools/edit.rs`, `tools-mcp-local/src/smart_file_edit/*.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Edit` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Edit` performs a surgical, exact-substring replacement in a single text file. It normalizes both file and snippets to a canonical LF view for matching, then writes back the file with its **original dominant** line-ending style (`LF`, `CRLF`, or `CR`) applied to the new content. The tool enforces a workspace-root path policy, supports optional hash-based staleness detection, and supports an optional line-range `match_hint` to disambiguate between multiple occurrences of the same snippet. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_edit` in `tools-mcp-local/src/smart_file_edit/mod.rs:46`.

### 3.2 Explicitly Out of Scope

- Unified-diff (patch-style) input. The `Edit` tool today accepts only `old_snippet` / `new_snippet` pairs; there is no `diff` field in the input schema (`tools-mcp-local/src/tools/edit.rs:8-34`).
- Multi-file edits. One call mutates exactly one file.
- `replace_all` / multi-match replacement. The current matcher returns the **first** match within the optional hint window and replaces only that one occurrence (`smart_file_edit/matching.rs:60`). Bulk replacement is intentionally not supported.
- Creating new files. Use `Write`. `Edit` requires the path to be an existing file (`smart_file_edit/mod.rs:62`).
- Path policy for read-only access. `Edit` enforces the policy; `Read` does not (see §7).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Edit` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_edit` (`tools-mcp-local/src/smart_file_edit/mod.rs:46`) |
| Schema definition | `tools-mcp-local/src/tools/edit.rs:4-36` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:21`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome` (`mod.rs:46-85`). Internal `anyhow::Error`s are caught at the boundary and wrapped with `ToolCallOutcome::err` (`mod.rs:81-83`).
- **Path policy enforcement.** `handle_edit` calls `path_policy::resolve_existing_file(&req.path, "path")` before reading the file (`mod.rs:62`). Paths that escape the server working directory are rejected with the standard path-policy diagnostic (`path_policy.rs:26-36`). Symlinks resolving outside the workspace are rejected.
- **`old_snippet` non-empty.** Empty `old_snippet` is rejected with text `"old_snippet cannot be empty. Remediation: use Read to copy the exact snippet from the file (use LF newlines), then retry Edit."` (`mod.rs:56-60`). The internal `apply_snippet_edit_impl` also re-checks (`edit.rs:54-56`).
- **`deny_unknown_fields`.** The outer `SimpleEditRequest` and the inner `MatchHint` both reject unknown properties (`mod.rs:31`, `matching.rs:20`). Unknown fields produce `"invalid arguments: ..."`.
- **Newline-style preservation.** The replacement bytes MUST be written with the file's dominant newline style as detected by `NewlineStats::default_kind()` (`edit.rs:116-122`, `model.rs:213-232`). When the file has no detected newlines, default to `LF` (`model.rs:227-231`). Tie-breaking prefers `CRLF` > `LF` > `CR` (`model.rs:214-225`).
- **Snippet normalization for matching.** Both `old_snippet` and `new_snippet` are normalized to LF before matching (`edit.rs:57-58`, `edit.rs:155-179`). A caller may pass CRLF-terminated snippets and they will still match an LF file. Locked in by `apply_snippet_edit_accepts_crlf_snippets_from_clients` (`edit.rs:292`).
- **Strict `match_hint`.** When `match_hint` is provided, search is restricted to the canonical bytes inside the hint window; if no match exists inside the hint, the result is `status: "no_match"` and the file is NOT modified (`matching.rs:39-58`). There is NO fallback search outside the hint. Locked in by `match_hint_selects_correct_occurrence_and_is_strict` (`edit.rs:427`).
- **First-match replacement (no `replace_all`).** Even when `old_snippet` appears multiple times in the file, `compute_match_range` returns only the first match (`matching.rs:60`). The remaining occurrences are unchanged. There is no input flag to widen this behavior.
- **Stale-file refusal.** If `file_hash` is provided AND non-empty AND differs from the file's current SHA-256 hash, the handler returns `status: "stale_file"` and does NOT write the file (`edit.rs:63-80`). Locked in by `apply_snippet_edit_rejects_stale_file_hash_and_does_not_modify_file` (`edit.rs:395`).
- **Hash format.** Hashes are formatted `"sha256:<64-hex-chars>"` (`model.rs:368-372`). Callers that round-trip `file_hash_after` from one edit into `file_hash` of the next get exact equality.
- **`status` reflects success class.** The response payload `status` field MUST be exactly one of `"ok"`, `"no_match"`, `"stale_file"` (`edit.rs:36-40,53-152`). `"ok"` returns `isError: false`; the other two return `isError: true` (`mod.rs:77-79`).
- **UTF-8 byte-offset correctness.** Multi-byte UTF-8 characters in the file MUST map correctly between canonical and byte positions; the replacement MUST not cut a UTF-8 sequence (`model.rs:64-153`, `edit.rs:96-103`). Locked in by `apply_snippet_edit_preserves_utf8_byte_offsets` (`edit.rs:316`).
- **`region_id` round-trip.** When the caller supplies `region_id`, the success payload echoes it verbatim (`edit.rs:149`). When omitted, the field is `null` in the payload.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT write a file when the snippet does not match (`no_match`). The file is left exactly as it was.
- MUST NOT write a file when `file_hash` is stale (`stale_file`). The file is left exactly as it was.
- MUST NOT normalize the file's line endings. The dominant style is detected and the same style is reused for the replacement.
- MUST NOT silently fall back to a global search when a `match_hint` is supplied; if the hint window has no match, the result is `no_match`.
- MUST NOT accept paths outside the workspace root, including via traversal segments (`..`) or symlinks (`path_policy.rs:303-322`).
- MUST NOT replace more than one occurrence of `old_snippet`. There is no `replace_all` mode.
- MUST NOT create a new file. The target path must already exist as a regular file.

## 5. Design Goals

- **Surgical, not patch-based.** Snippet replacement is easier for an LLM caller to reason about than a unified diff and avoids whitespace-fragility in hunk headers.
- **Preserve file format.** Editors and CI commonly enforce one line-ending convention per repo. Silently converting CRLF to LF (or vice versa) on every edit would produce noisy diffs and broken Windows tooling. Detecting and reusing the dominant style is the safest default.
- **Strict hint, not fuzzy hint.** A "soft" hint that falls back to global search invisibly defeats the disambiguation purpose. Strictness forces the caller to either trust the hint or omit it.
- **Optional staleness check.** Hash gating lets a chain of edits stay coherent across the read → edit → re-read cycle without race conditions, while remaining fully optional for one-shot edits.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-empty; must resolve to an existing file inside the server working directory | Target file. Relative paths resolved against CWD. |
| `old_snippet` | string | Yes | — | Non-empty | Exact text to find. Matched against the canonical (LF-normalized) view of the file. May include or omit a trailing newline; the match is byte-exact in the canonical view. |
| `new_snippet` | string | Yes | — | — | Replacement text. Use LF newlines; the handler converts them to the file's dominant style on write. |
| `file_hash` | string | No | — | `"sha256:<hex>"` when present | Expected current SHA-256 hash of the file. If present and not equal to the file's current hash, the edit is rejected with `status: "stale_file"`. Empty / whitespace-only values are ignored (treated as not provided). |
| `region_id` | string | No | — | — | Opaque caller-supplied identifier echoed in the success payload. Used for tracking which call produced which mutation; otherwise unused. |
| `match_hint` | object | No | — | `{start_line?: int >= 1, end_line?: int >= 1}` with `additionalProperties: false` | Line range hint (1-indexed inclusive) constraining where in the file to search. Strict: if the snippet does not appear inside the window, the result is `no_match`. |

The schema sets `"additionalProperties": false` (`edit.rs:33`); the request type uses `#[serde(deny_unknown_fields)]` (`mod.rs:31`). The inner `MatchHint` also uses `deny_unknown_fields` (`matching.rs:20`).

> Schema source: `tools-mcp-local/src/tools/edit.rs:8-34`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `SimpleEditRequest`; on failure return the `parse_args` error envelope (`mod.rs:47-50`).
2. **Validate `path` non-empty** — `validation::validate_non_empty(&req.path, "path", None)`; whitespace-only paths produce `"path is required (non-empty string)"` (`mod.rs:52-54`).
3. **Validate `old_snippet` non-empty** — Reject empty `old_snippet` with the LF-remediation message (`mod.rs:56-60`).
4. **Resolve path under workspace** — `path_policy::resolve_existing_file(&req.path, "path")`. Rejects nonexistent files, directories, paths outside the workspace root, and symlinks that escape the root (`mod.rs:62-65`, `path_policy.rs:65-80`).
5. **Hand off to `apply_snippet_edit_impl`** — Wraps the request in `ApplySnippetEditRequest` (`mod.rs:67-74`).
6. **Snippet normalization** — `normalize_newlines_to_lf` converts CRLF and lone CR to LF in both snippets (`edit.rs:57-58,155-179`).
7. **Load file model** — `FileModel::from_path(&path)` reads bytes once, computes SHA-256 hash, splits into lines with newline detection, and builds the LF-canonical view with bidirectional offset boundaries (`edit.rs:61`, `model.rs:36-47`).
8. **Stale-file check** — If `file_hash` is provided and non-empty after trim, compare against `model.hash`; on mismatch return `SnippetStatusKind::StaleFile` with payload `{"action":"apply_snippet_edit","status":"stale_file","message":"file hash mismatch, refresh region before applying changes","expected_file_hash":<provided>,"current_file_hash":<actual>}` (`edit.rs:63-80`).
9. **Compute match range** — `compute_match_range(&model.canonical, hint, &old_snippet)` runs `memchr::memmem::Finder` on the canonical bytes, restricted to the hint window when provided (`matching.rs:28-61`).
10. **No-match handling** — If no match, return `SnippetStatusKind::NoMatch` with `no_match_payload`, which includes up to 3 near-miss candidates derived from the snippet's first non-blank logical line (`matching.rs:63-72,74-121`).
11. **Map canonical offsets to file bytes** — `model.canonical.byte_offset(canonical_start)` and `byte_offset(canonical_end)` (`edit.rs:96-103`, `model.rs:132-141`).
12. **Self-consistency check** — Confirm the canonical slice at `[canonical_start..canonical_end]` matches `old_snippet` exactly; on mismatch return an `anyhow!` error (`edit.rs:105-114`).
13. **Choose newline style for replacement** — `model.newline_stats.default_kind()` returns the dominant kind, or `LF` if there were no detected newlines (`edit.rs:116`, `model.rs:227-231`).
14. **Build updated bytes** — Concatenate `bytes[..byte_start]` + replacement bytes + `bytes[byte_end..]`. `append_replacement_bytes` re-encodes every LF in `new_snippet` to the target newline kind (`edit.rs:118-123,196-213`).
15. **Write file** — `fs::write(&path, &updated)` writes the new bytes (`edit.rs:125`). The file is replaced atomically at the OS level only insofar as `std::fs::write` provides; the tool does not stage to a temp file.
16. **Compute new hash + line numbers** — SHA-256 of the new bytes; convert `canonical_start` and `canonical_end - 1` back to line numbers via `line_index_for_offset` (`edit.rs:128-136`).
17. **Build success payload** — Return `SnippetStatusKind::Ok` with payload listed in §6.3 (`edit.rs:138-152`).
18. **Wrap in MCP envelope** — `handle_edit` converts the payload into `ToolCallOutcome::ok_json_content(&result.payload, is_error)` where `is_error` is `false` for `Ok` and `true` for `NoMatch` / `StaleFile` (`mod.rs:77-79`). `ok_json_content` honors `TOOLS_PRETTY_JSON` (`tool_outcome.rs:100-118`).

### 6.3 Response Schema

All response variants are returned as a JSON-serialized string in `content[0].text` via `ToolCallOutcome::ok_json_content` (`mod.rs:79`). Callers MUST parse `content[0].text` as JSON to recover the structured payload.

**Success — `status: "ok"` (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "{\"action\":\"apply_snippet_edit\",\"status\":\"ok\",\"replaced_byte_range\":[7,15],\"lines\":{\"start\":2,\"end\":2},\"bytes_written\":3,\"file_hash_before\":\"sha256:...\",\"file_hash_after\":\"sha256:...\",\"newline_kind\":\"LF\",\"region_id\":null}"}],
  "isError": false
}
```

| Payload field | Type | Always present | Description |
|---|---|---|---|
| `action` | string | Yes | Always `"apply_snippet_edit"`. |
| `status` | string | Yes | `"ok"`. |
| `replaced_byte_range` | `[int, int]` | Yes | `[byte_start, byte_end)` of the replaced region in the **original** file bytes. |
| `lines.start` / `lines.end` | integer | Yes | 1-indexed inclusive line range covered by `old_snippet` in the canonical view. |
| `bytes_written` | integer | Yes | Byte length of the replacement, already adjusted for the target newline style (e.g., `"a\n"` written with `CRLF` reports `3`). |
| `file_hash_before` | string | Yes | `"sha256:<hex>"` of the file before the write. |
| `file_hash_after` | string | Yes | `"sha256:<hex>"` of the file after the write. |
| `newline_kind` | string | Yes | One of `"LF"`, `"CRLF"`, `"CR"`, `"None"` — the style used for replacement encoding. |
| `region_id` | string \| null | Yes | Echo of the caller's `region_id` (or `null`). |

**No-match — `status: "no_match"` (`isError: true`):**

```json
{
  "action": "apply_snippet_edit",
  "status": "no_match",
  "reason": "old_snippet not found in canonical view",
  "match_hint": {"start_line": 4, "end_line": 4},
  "candidates": [
    {"start_line": 12, "end_line": 12, "similarity": 0.5}
  ]
}
```

| Payload field | Type | Always present | Description |
|---|---|---|---|
| `action` | string | Yes | `"apply_snippet_edit"`. |
| `status` | string | Yes | `"no_match"`. |
| `reason` | string | Yes | `"old_snippet not found in canonical view"`. |
| `match_hint` | object \| null | Yes | Echo of the caller's `match_hint`, or `null` if absent. |
| `candidates` | array | Yes | Up to 3 near-miss locations, each `{start_line, end_line, similarity}` where `similarity` is the fraction of subsequent lines that match the snippet's logical lines (`matching.rs:123-139`). |

**Stale-file — `status: "stale_file"` (`isError: true`):**

```json
{
  "action": "apply_snippet_edit",
  "status": "stale_file",
  "message": "file hash mismatch, refresh region before applying changes",
  "expected_file_hash": "sha256:abc...",
  "current_file_hash": "sha256:def..."
}
```

**Tool-level error (`isError: true`):**

For everything else (argument parse failure, path-policy rejection, empty `old_snippet`, internal invariant violation, write I/O error), the handler returns `ToolCallOutcome::err` with a plain text message:

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors use `ToolCallOutcome::err` (`tools-mcp-core/src/tool_outcome.rs:35`). The handler MUST NOT panic; every failure path returns a `ToolCallOutcome`.

### 6.4 Error Catalog

| Condition | Envelope | Text content |
|---|---|---|
| Argument deserialization failure | `isError: true`, plain text | `"invalid arguments: ..."` plus class hint (`tool_outcome.rs:62-74`) |
| Empty / whitespace-only `path` | `isError: true`, plain text | `"path is required (non-empty string)"` (`validation.rs:17`) |
| Empty `old_snippet` | `isError: true`, plain text | `"old_snippet cannot be empty. Remediation: use Read to copy the exact snippet from the file (use LF newlines), then retry Edit."` (`mod.rs:57-59`) |
| Path policy rejection (escape, missing, not a file, symlink outside) | `isError: true`, plain text | `"path rejected for 'path': ...The resolved path must stay inside the server working directory <ws>. Remediation: ..."` (`path_policy.rs:26-36`) |
| Snippet not found (no `match_hint`) | `isError: true`, JSON payload with `status: "no_match"` | See §6.3 no-match shape (`mod.rs:78`) |
| Snippet not found inside `match_hint` window | `isError: true`, JSON `status: "no_match"` | Same shape; `match_hint` echoed in payload |
| `file_hash` mismatch | `isError: true`, JSON `status: "stale_file"` | See §6.3 stale-file shape (`edit.rs:63-80`) |
| `match_hint` references lines outside the file | `isError: true`, plain text | `"edit error: match_hint lines are invalid for current file. Remediation: ensure 'path' exists and 'old_snippet' matches exactly; if there are multiple matches, provide match_hint."` (`matching.rs:48`, `mod.rs:81-83`) |
| Internal canonical-slice invariant violation | `isError: true`, plain text | `"edit error: internal invariant violated: canonical slice mismatch. Remediation: ..."` (`edit.rs:111-113`) |
| File write I/O error | `isError: true`, plain text | `"edit error: write patched bytes to <path>: <io error>. Remediation: ..."` (`edit.rs:125-126`) |

## 7. Security Considerations

- **Path policy enforcement.** Every edit MUST traverse `path_policy::resolve_existing_file` (`mod.rs:62`). The policy canonicalizes the workspace root and the candidate path, then rejects:
  - Absolute paths outside the workspace root (`path_policy.rs:309-322`).
  - Relative paths containing `..` segments that escape the root (`path_policy.rs:237-247`).
  - Symlinks whose canonical target lies outside the root (`path_policy.rs:226-247`).
  - Non-existing files, with a clear "does not exist" error so the caller does not accidentally treat `Edit` as a creator (`path_policy.rs:184-200`).
- **Symlinks inside the workspace.** Symlinks resolving inside the workspace ARE followed; the resolved canonical path is used for both reads and writes. Locked in by `public_edit_canonicalizes_symlinked_file_before_writing` (`mod.rs:171`).
- **No code execution.** The tool performs only file I/O. It does NOT exec, shell out, or invoke a formatter on the result.
- **Hash-based concurrent-edit detection.** When two callers chain edits, supplying `file_hash` defeats lost-update races: the second writer with a stale hash gets `stale_file` and must re-read.
- **Untrusted input.** The `new_snippet` content is written verbatim (after newline re-encoding). Callers MUST NOT pass user-controlled content into source files without their own review; the tool does not sanitize, lint, or validate the replacement.
- **No backup, no journaling.** The file is overwritten with `std::fs::write`; there is no `.bak` file or rename-into-place staging. A crash mid-write may leave a truncated file. Callers needing durability MUST snapshot externally.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `TOOLS_PRETTY_JSON` | unset (compact) | When `1`, `true`, `yes`, or `on` (case-insensitive), the JSON payload inside `content[0].text` is pretty-printed. Read once per process (`tool_outcome.rs:101-106`). |

The path-policy enforcement uses `std::env::current_dir()` (`path_policy.rs:84`) to determine the workspace root; this is the server process's CWD at startup and is not configurable through an env var.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 21 |
| Tool name + schema | `tools-mcp-local/src/tools/edit.rs` | 4-36 |
| Handler entry point | `tools-mcp-local/src/smart_file_edit/mod.rs` | 46 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/smart_file_edit/mod.rs` | 30-42 |
| `old_snippet` non-empty guard | `tools-mcp-local/src/smart_file_edit/mod.rs` | 56-60 |
| Path policy resolution | `tools-mcp-local/src/smart_file_edit/mod.rs` | 62-65 |
| Status mapping → `is_error` | `tools-mcp-local/src/smart_file_edit/mod.rs` | 77-79 |
| Snippet normalization (CRLF/CR → LF) | `tools-mcp-local/src/smart_file_edit/edit.rs` | 155-179 |
| Stale-file branch | `tools-mcp-local/src/smart_file_edit/edit.rs` | 63-80 |
| First-match-only finder | `tools-mcp-local/src/smart_file_edit/matching.rs` | 60 |
| Strict-hint window | `tools-mcp-local/src/smart_file_edit/matching.rs` | 39-58 |
| Replacement newline re-encode | `tools-mcp-local/src/smart_file_edit/edit.rs` | 196-213 |
| Newline dominance with `CRLF > LF > CR` tie-break | `tools-mcp-local/src/smart_file_edit/model.rs` | 213-232 |
| SHA-256 hash format | `tools-mcp-local/src/smart_file_edit/model.rs` | 368-372 |
| File model: bytes, hash, canonical, stats | `tools-mcp-local/src/smart_file_edit/model.rs` | 24-47 |
| UTF-8 boundary mapping | `tools-mcp-local/src/smart_file_edit/model.rs` | 132-153 |
| `MatchHint` schema (`deny_unknown_fields`) | `tools-mcp-local/src/smart_file_edit/matching.rs` | 19-26 |
| No-match candidate suggestion | `tools-mcp-local/src/smart_file_edit/matching.rs` | 63-72 |
| Path policy: workspace canonicalization | `tools-mcp-local/src/path_policy.rs` | 83-100 |
| Path policy: workspace-containment check | `tools-mcp-local/src/path_policy.rs` | 303-322 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Edit",
    "arguments": {
      "path": "src/main.rs",
      "old_snippet": "fn old_name(",
      "new_snippet": "fn new_name("
    }
  }
}
```

### 10.2 Successful edit on a CRLF file

The file on disk is `one\r\ntwo\r\nthree\r\n`. The caller sends LF-only snippets; the handler writes back CRLF:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Edit",
    "arguments": {
      "path": "crlf.txt",
      "old_snippet": "two\nthree",
      "new_snippet": "TWO\nTHREE"
    }
  }
}
```

Response (`content[0].text` parsed):

```json
{
  "action": "apply_snippet_edit",
  "status": "ok",
  "replaced_byte_range": [5, 16],
  "lines": {"start": 2, "end": 3},
  "bytes_written": 12,
  "file_hash_before": "sha256:...",
  "file_hash_after": "sha256:...",
  "newline_kind": "CRLF",
  "region_id": null
}
```

File on disk after: `one\r\nTWO\r\nTHREE\r\n` (locked in by `apply_snippet_edit_preserves_crlf_newlines_in_replacement_bytes`, `edit.rs:266`).

### 10.3 `match_hint` disambiguates between duplicate snippets

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Edit",
    "arguments": {
      "path": "hint.txt",
      "old_snippet": "target",
      "new_snippet": "TARGET2",
      "match_hint": {"start_line": 4, "end_line": 4}
    }
  }
}
```

Replaces the line-4 occurrence only; line-2 `target` is untouched (`edit.rs:427`).

### 10.4 No match (with `match_hint`)

When the snippet does not appear inside the hint window, `status: "no_match"` and the file is unchanged:

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "{\"action\":\"apply_snippet_edit\",\"status\":\"no_match\",\"reason\":\"old_snippet not found in canonical view\",\"match_hint\":{\"start_line\":1,\"end_line\":1},\"candidates\":[{\"start_line\":2,\"end_line\":2,\"similarity\":1.0},{\"start_line\":4,\"end_line\":4,\"similarity\":1.0}]}"}],
    "isError": true
  }
}
```

### 10.5 Stale-file rejection

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "Edit",
    "arguments": {
      "path": "stale.txt",
      "old_snippet": "beta",
      "new_snippet": "BETA",
      "file_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    }
  }
}
```

Returns `status: "stale_file"`; the file is not modified. Locked in by `public_edit_rejects_stale_file_hash_without_modifying_file` (`mod.rs:101`).

### 10.6 Path escape rejected

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "mcp/tools/call",
  "params": {
    "name": "Edit",
    "arguments": {
      "path": "../outside.txt",
      "old_snippet": "x",
      "new_snippet": "y"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "path rejected for 'path': ../outside.txt resolves outside the server working directory. The resolved path must stay inside the server working directory <ws>. Remediation: use a relative path under the server working directory, or an absolute path within that directory."}],
    "isError": true
  }
}
```

Locked in by `public_edit_rejects_parent_traversal_outside_workspace` (`mod.rs:153`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `public_edit_rejects_stale_file_hash_without_modifying_file` | `tools-mcp-local/src/smart_file_edit/mod.rs:101` | `file_hash` mismatch produces `status: "stale_file"` and file content is preserved. |
| `public_edit_forwards_region_id_to_success_payload` | `tools-mcp-local/src/smart_file_edit/mod.rs:127` | `region_id` echoes verbatim on success. |
| `public_edit_rejects_parent_traversal_outside_workspace` | `tools-mcp-local/src/smart_file_edit/mod.rs:153` | `..` traversal blocked by path policy. |
| `public_edit_canonicalizes_symlinked_file_before_writing` (Unix) | `tools-mcp-local/src/smart_file_edit/mod.rs:171` | Symlink inside workspace resolves to its target file before writing. |
| `apply_snippet_edit_preserves_crlf_newlines_in_replacement_bytes` | `tools-mcp-local/src/smart_file_edit/edit.rs:266` | CRLF file gets CRLF replacement; reports `newline_kind: "CRLF"`. |
| `apply_snippet_edit_accepts_crlf_snippets_from_clients` | `tools-mcp-local/src/smart_file_edit/edit.rs:292` | CRLF snippet from caller still matches an LF-canonical view. |
| `apply_snippet_edit_preserves_utf8_byte_offsets` | `tools-mcp-local/src/smart_file_edit/edit.rs:316` | Multi-byte chars (é, ï, ñ) mapped correctly. |
| `apply_snippet_edit_preserves_cr_newlines_in_replacement_bytes` | `tools-mcp-local/src/smart_file_edit/edit.rs:340` | Classic-Mac CR-only files keep CR; reports `newline_kind: "CR"`. |
| `apply_snippet_edit_uses_dominant_newline_for_mixed_file` | `tools-mcp-local/src/smart_file_edit/edit.rs:369` | Tie-break prefers CRLF over LF in mixed files. |
| `apply_snippet_edit_rejects_stale_file_hash_and_does_not_modify_file` | `tools-mcp-local/src/smart_file_edit/edit.rs:395` | Internal-layer stale-file refusal. |
| `match_hint_selects_correct_occurrence_and_is_strict` | `tools-mcp-local/src/smart_file_edit/edit.rs:427` | Hint targets one of multiple matches; out-of-window hint returns `no_match` (no fallback). |
| `normalize_newlines_to_lf_handles_crlf_and_cr` | `tools-mcp-local/src/smart_file_edit/edit.rs:251` | Snippet normalization correctness. |
| `test_build_replacement_bytes_tracks_trailing_newline` | `tools-mcp-local/src/smart_file_edit/edit.rs:239` | Trailing LF in `new_snippet` becomes CRLF when needed. |
| `test_split_lines_handles_mixed_newlines` | `tools-mcp-local/src/smart_file_edit/model.rs:379` | NewlineStats counts CRLF/LF/CR correctly. |
| `test_canonical_byte_offsets_cover_line_boundaries` | `tools-mcp-local/src/smart_file_edit/model.rs:389` | Canonical-to-byte mapping handles newline-byte differences. |
| `line_index_lookup_uses_canonical_full_line_boundaries` | `tools-mcp-local/src/smart_file_edit/model.rs:402` | `line_index_for_offset` returns correct 0-indexed line per canonical offset. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `Edit` support unified-diff input? | No. The schema accepts only `old_snippet` and `new_snippet` (`tools-mcp-local/src/tools/edit.rs:8-34`). The crate doc comment in `smart_file_edit/mod.rs:1-15` describes the design intent. |
| 2 | Does `Edit` support `replace_all`? | No. `compute_match_range` returns only the first match (`matching.rs:60`). |
| 3 | Does `Edit` normalize the file's line endings? | No. The file's dominant style is detected once via `NewlineStats::default_kind()` and re-applied to the replacement; the rest of the file is untouched (`edit.rs:116-124`, `model.rs:213-232`). |
| 4 | What happens if `match_hint` is provided but the snippet appears only outside the hint window? | `status: "no_match"`; the file is not modified. There is no fallback to a global search. Locked in by `match_hint_selects_correct_occurrence_and_is_strict` (`edit.rs:427`). |
| 5 | Is `Edit` required to be preceded by a `Read`? | No protocol-level read-before-edit requirement exists. Callers MAY use `file_hash` (returned by previous edits or computed externally) to enforce read-coherence; it is optional. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_json_content` shape, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/path_policy.rs` | Workspace-root path resolution (§7). |
| `tools-mcp-local/src/smart_file_edit/mod.rs` | Public handler entry (§6.2). |
| `tools-mcp-local/src/smart_file_edit/edit.rs` | Snippet-replacement implementation (§6.2). |
| `tools-mcp-local/src/smart_file_edit/matching.rs` | Match finder and hint semantics (§4.2, §6.2). |
| `tools-mcp-local/src/smart_file_edit/model.rs` | File model, hash, newline detection (§4.2, §6.2). |
