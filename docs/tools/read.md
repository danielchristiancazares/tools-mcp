# SDD: Read

**Date:** 2026-05-24
**Scope:** Design contract for the `Read` MCP tool.
**Source:** `tools-mcp-local/src/tools/read.rs`, `tools-mcp-local/src/tools/handlers/read_file.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Read` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Read` is the MCP tool that returns the textual contents of a single file, optionally restricted to a 1-indexed inclusive line range and optionally rendered with line-number prefixes. It preserves the file's original line endings inside any returned slice, performs lossy UTF-8 decoding, and streams large files through a bounded line scanner instead of loading them whole when a range is requested. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_read_file` in `tools-mcp-local/src/tools/handlers/read_file.rs:20`.

### 3.2 Explicitly Out of Scope

- Binary/image preview, PDF rendering, and notebook (`.ipynb`) cell parsing. `Read` returns text only; binary files are decoded with `String::from_utf8_lossy` and the U+FFFD replacement character is inserted for invalid sequences (`read_file.rs:357-366`). The `Read` tool exposed by the harness to the model accepts those file types, but this MCP-level `Read` does not — it is a text reader only.
- Search inside the file. Use the `Search` tool.
- Listing directories. Use `ListDir`.
- Path-policy mutation enforcement. `Read` does not call `path_policy::resolve_existing_file`; see §7 for the trust boundary.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Read` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_read_file` (`tools-mcp-local/src/tools/handlers/read_file.rs:20`) |
| Schema definition | `tools-mcp-local/src/tools/read.rs:4-32` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:20`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every failure path MUST return a `ToolCallOutcome::err` value (`read_file.rs:44,49,53,81,154-172`). The handler MUST NOT panic on invalid arguments, unreadable files, or invalid UTF-8.
- **Argument validation precedes I/O.** `start_line == 0`, `end_line == 0`, and `start_line > end_line` MUST be rejected before opening or reading the file (`read_file.rs:42-55`). Tests `read_file_start_line_zero_is_validated_before_file_read` and `read_file_end_line_zero_is_validated_before_file_read` lock this in (`read_file.rs:570-603`).
- **`deny_unknown_fields`.** The request deserializer rejects any property outside the documented schema (`read_file.rs:22`). Unknown fields produce `"invalid arguments: ..."`.
- **`show_line_numbers` defaults to `false`.** When the field is absent the handler returns raw bytes (no `N\t` prefix). Locked in by `read_file_show_line_numbers_defaults_to_false` (`read_file.rs:508`).
- **Empty file is success.** A zero-byte file MUST return `isError: false` with empty `content[0].text`, `start_line: 0`, `end_line: 0`, `total_lines: 0` (`read_file.rs:72-73`). Locked in by `read_file_empty_file_returns_empty_content` (`read_file.rs:531`).
- **Line-ending preservation.** A returned slice MUST contain the original line terminators (`LF`, `CRLF`, or `CR`) byte-for-byte as they appear in the file. The line scanner counts `\r\n` as one line and never duplicates the `\n` (`read_file.rs:394-407`). Locked in by `read_file_range_preserves_mixed_newline_endings` (`read_file.rs:628`).
- **Lossy UTF-8 decoding.** Invalid byte sequences MUST be replaced with U+FFFD (`\u{FFFD}`); the handler MUST NOT return an error solely because the file is not valid UTF-8 (`read_file.rs:357-366`). Locked in by `read_file_range_preserves_invalid_utf8_lossy_replacement` and `read_file_full_file_preserves_invalid_utf8_lossy_replacement` (`read_file.rs:651,785`).
- **Range vs full-file streaming threshold.** When a range is requested (`start_line != 1` OR `end_line` is present) AND the file is larger than `LARGE_FILE_STREAMING_THRESHOLD_BYTES = 64 KiB`, the handler MUST stream through a chunked scanner (`STREAM_READ_BUFFER_SIZE = 64 KiB`) rather than loading the entire file into memory (`read_file.rs:13-14,60-65,101-115`). Locked in by `read_file_large_range_returns_raw_selected_lines` (`read_file.rs:674`).
- **Out-of-range start is an error.** `start_line > total_lines` MUST return `isError: true` with text `"start_line {N} exceeds file line count {M}"` (`read_file.rs:80-84,133-137`).
- **End clamped to total lines.** When `end_line` is greater than the file's line count, the resolved end MUST be clamped to `total_lines` and reported in the `end_line` response field (`read_file.rs:85,139`).
- **Line-number column width.** When `show_line_numbers=true`, the prefix width MUST equal the number of decimal digits in the resolved end line number, right-aligned, followed by a single TAB (`read_file.rs:373-388`). Locked in by `read_file_numbered_range_uses_resolved_end_width` (`read_file.rs:806`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT accept `start_line < 1` or `end_line < 1`; both MUST be rejected with a deterministic error message.
- MUST NOT return an error for an empty file. An empty file is a valid input with an empty body.
- MUST NOT modify the file under any circumstance. `Read` is purely a read-only operation.
- MUST NOT rewrite, normalize, or collapse line endings in returned slices.
- MUST NOT load the entire file into memory when a range request targets a file larger than 64 KiB; streaming is required to keep memory bounded.
- MUST NOT prepend line numbers unless `show_line_numbers` is explicitly `true`.

## 5. Design Goals

- **Predictable line semantics.** 1-indexed inclusive ranges with strict validation matches user mental models of editor line numbers and avoids the off-by-one ambiguity of 0-indexed exclusive ranges.
- **Cheap for small files, bounded for large files.** Full-file reads use a single `tokio::fs::read`; ranged reads of large files use an O(range) streaming scanner so callers can target line 1,000,000 of a multi-GB log without OOM.
- **Round-trippable bytes.** Preserving original line endings byte-for-byte lets `Read` cooperate safely with `Edit`, which writes back with the file's dominant newline style; if `Read` normalized newlines, downstream edits could corrupt mixed-ending files.
- **Enables editing.** Every successful `Read` records an in-memory snapshot of the file's SHA-256 (keyed by canonical path, scoped to the server process). `Edit` requires this snapshot and refuses if the file was not read or has since changed, so the read-before-edit contract is enforced without the caller copying any hash between calls. The snapshot is computed from the same full-file bytes the read already scans, including on the large-file streaming path.
- **No silent failure on bad UTF-8.** Returning U+FFFD lets the caller see the file's textual structure even when it contains stray bytes, instead of forcing the caller to handle a hard error for every binary-looking file.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-empty (whitespace-only rejected) | Filesystem path to read. Resolved against the server's working directory when relative. |
| `start_line` | integer | No | `1` | `>= 1` | First line to return (1-indexed inclusive). |
| `end_line` | integer | No | _file's total line count_ | `>= 1` AND `>= start_line` | Last line to return (inclusive). When omitted, the read returns from `start_line` through end-of-file. |
| `show_line_numbers` | boolean | No | `false` | — | When `true`, prefix each returned line with `"<N>\t"` where `N` is right-aligned to the resolved end-line width. |

The schema sets `"additionalProperties": false` (`tools-mcp-local/src/tools/read.rs:29`); the request deserializer uses `#[serde(deny_unknown_fields)]` (`read_file.rs:22`). Unknown fields produce a tool-level error (`isError: true`) with text `"invalid arguments: unknown field ..." ` plus the registry hint `" Unknown fields are not allowed; check argument names against the tool schema."`.

> Schema source: `tools-mcp-local/src/tools/read.rs:8-30`

### 6.2 Behavior

Each step lists the file:line for verification.

1. **Parse arguments** — Call `ToolCallOutcome::parse_args::<ReadRequest>` (`read_file.rs:33`). On failure, return the parse error envelope.
2. **Validate `path`** — Call `validation::validate_non_empty(&req.path, "path", None)`; reject whitespace-only paths with `"path is required (non-empty string)"` (`read_file.rs:38`).
3. **Validate `start_line` and `end_line`** — Reject `start_line == 0` with `"start_line must be >= 1"`, reject `end_line == 0` with `"end_line must be >= 1"`, reject `start_line > end_line` with `"start_line cannot be greater than end_line"` (`read_file.rs:42-55`).
4. **Decide streaming vs in-memory path** — `should_stream_large_range` returns `true` only when the request is a range read (`start_line != 1` OR `end_line.is_some()`) AND the file metadata reports size > 64 KiB (`read_file.rs:101-116`).
5. **Streaming path** — Open the file with `tokio::fs::File::open` and feed 64 KiB chunks into `StreamLineRangeScanner` (`read_file.rs:239-258`). The scanner tracks a pending CR across buffer boundaries so a `\r\n` split across chunks counts as one line (`read_file.rs:280-313`). On success, return the selected bytes with `read_ok`; on `std::io::Error`, convert via `read_error` (`read_file.rs:60-64`).
6. **In-memory path** — `tokio::fs::read(path)` loads the file. If empty, short-circuit to `read_ok(path, "", 0, 0, 0)` (`read_file.rs:67-73`). Otherwise `scan_line_range` computes `total_lines`, `selected_start`, `selected_end` (`read_file.rs:76`).
7. **Range bounds check** — If `start > line_count`, return `"start_line {start} exceeds file line count {line_count}"` (`read_file.rs:80-83`). Clamp `resolved_end = end.min(line_count)` (`read_file.rs:85`).
8. **Build body** — If `show_line_numbers=true`, call `render_numbered_range` which writes `"{line_no:>width$}\t{line}"` for each line (`read_file.rs:90,373-388`). If the selected range is the entire buffer, return `bytes_to_string_lossy(data)`; otherwise return the lossy decode of the sliced range (`read_file.rs:92-96`).
9. **Build success payload** — `read_ok` returns a `ToolCallOutcome::ok` with the JSON object documented in §6.3 (`read_file.rs:174-191`).
10. **I/O error mapping** — `read_error` maps `NotFound`, `PermissionDenied`, and `IsADirectory` to specific human-readable messages with remediation hints; any other `io::Error` becomes `"failed to read {path}: {err}"` (`read_file.rs:154-172`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "<file body>"}],
  "isError": false,
  "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
  "start_line": 1,
  "end_line": 3,
  "total_lines": 832
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].type` | string | Yes | Always `"text"`. |
| `content[0].text` | string | Yes | The selected file bytes, lossily decoded as UTF-8 (invalid bytes become U+FFFD). When `show_line_numbers=true`, each line is prefixed with `"<right-aligned line number>\t"`. For an empty file the value is `""`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `path` | string | Yes | The path as supplied by the caller (echoed via `path.display()`). |
| `start_line` | integer | Yes | The requested first line, or `0` for an empty file. |
| `end_line` | integer | Yes | The resolved last line (clamped to total line count), or `0` for an empty file. |
| `total_lines` | integer | Yes | Total number of lines in the file, or `0` for an empty file. |

**Tool-level error (`isError: true`):**

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors are constructed with `ToolCallOutcome::err` (`tools-mcp-core/src/tool_outcome.rs:35`); they carry only `content` and `isError`. The handler MUST NOT panic; all failure paths MUST return a `ToolCallOutcome` value (`tool_outcome.rs:35`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Argument deserialization failure (unknown field, wrong type, missing required) | `true` | `"invalid arguments: ..."` plus the hint specific to the failure class (`tool_outcome.rs:62-74`) |
| Empty / whitespace-only `path` | `true` | `"path is required (non-empty string)"` (`validation.rs:17`) |
| `start_line == 0` | `true` | `"start_line must be >= 1"` (`read_file.rs:44`) |
| `end_line == 0` | `true` | `"end_line must be >= 1"` (`read_file.rs:49`) |
| `start_line > end_line` | `true` | `"start_line cannot be greater than end_line"` (`read_file.rs:53`) |
| `start_line > total_lines` | `true` | `"start_line {start} exceeds file line count {line_count}"` (`read_file.rs:81-83,134-136`) |
| File not found | `true` | `"file not found: {path}. Remediation: check the path (paths are resolved relative to the MCP server's working directory) or use Glob/ListDir to locate it."` (`read_file.rs:156-159`) |
| Permission denied | `true` | `"permission denied reading {path}. Remediation: check file permissions and whether another process is locking the file."` (`read_file.rs:160-163`) |
| Path is a directory | `true` | `"{path} is a directory. Remediation: use ListDir to inspect it, or pass a file path."` (`read_file.rs:164-167`) |
| Other `std::io::Error` | `true` | `"failed to read {path}: {err}"` (`read_file.rs:168`) |

## 7. Security Considerations

- **No path-policy enforcement on read.** `Read` does NOT route through `path_policy::resolve_existing_file`; it uses `Path::new(&req.path)` directly (`read_file.rs:57`). Operators MUST treat the server's process privileges as the disclosure boundary: the tool can read any file the process can open. Mutation tools (`Edit`, `Write`, `Delete`, `Move`, `Copy`) DO enforce a workspace-root policy; read operations intentionally do not, so callers can read repository-adjacent files (e.g., `~/.gitconfig`, system docs) without being blocked. Hosts that need read confinement MUST sandbox the server process itself.
- **Untrusted output.** File contents are external data. Consumers MUST treat `content[0].text` as untrusted input and MUST NOT execute it or interpret it as instructions. Lossy UTF-8 decoding can transform binary files into surprising glyph soup; downstream processing MUST tolerate U+FFFD characters.
- **Resource bounds.** Full-file reads are unbounded by the tool itself — a 10 GB log file requested without a range will allocate a 10 GB `Vec<u8>`. Ranged reads on files larger than 64 KiB stream and are O(file size) wall-clock but O(range bytes) memory. Callers SHOULD pass `start_line`/`end_line` when reading large files.
- **No symlink hardening.** Because path policy is not applied, a symlink pointing outside the workspace will be followed transparently. This is by design for read access (see first bullet); operators relying on filesystem confinement MUST use OS-level sandboxing.

## 8. Configuration

Not applicable. `Read` does not read any environment variables.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 20 |
| Tool name + schema | `tools-mcp-local/src/tools/read.rs` | 4-32 |
| Handler entry point | `tools-mcp-local/src/tools/handlers/read_file.rs` | 20 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/handlers/read_file.rs` | 21-31 |
| Argument validation order | `tools-mcp-local/src/tools/handlers/read_file.rs` | 33-55 |
| Streaming threshold (64 KiB) | `tools-mcp-local/src/tools/handlers/read_file.rs` | 13 |
| Stream buffer size (64 KiB) | `tools-mcp-local/src/tools/handlers/read_file.rs` | 14 |
| `should_stream_large_range` predicate | `tools-mcp-local/src/tools/handlers/read_file.rs` | 101-116 |
| `read_large_range` streaming path | `tools-mcp-local/src/tools/handlers/read_file.rs` | 118-152 |
| `StreamLineRangeScanner` chunk-boundary CR handling | `tools-mcp-local/src/tools/handlers/read_file.rs` | 280-313 |
| In-memory path + empty-file short-circuit | `tools-mcp-local/src/tools/handlers/read_file.rs` | 67-99 |
| `scan_line_range` (line counting) | `tools-mcp-local/src/tools/handlers/read_file.rs` | 221-237 |
| `for_each_line_with_endings` (CRLF / CR / LF awareness) | `tools-mcp-local/src/tools/handlers/read_file.rs` | 390-415 |
| Lossy UTF-8 decode | `tools-mcp-local/src/tools/handlers/read_file.rs` | 357-366 |
| `render_numbered_range` formatting | `tools-mcp-local/src/tools/handlers/read_file.rs` | 368-388 |
| Success payload assembly | `tools-mcp-local/src/tools/handlers/read_file.rs` | 174-191 |
| I/O error → user-facing message | `tools-mcp-local/src/tools/handlers/read_file.rs` | 154-172 |

## 10. Examples

### 10.1 Minimal request — read entire file

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Read",
    "arguments": {"path": "Cargo.toml"}
  }
}
```

### 10.2 Successful range read

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Read",
    "arguments": {
      "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
      "start_line": 1,
      "end_line": 3
    }
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "//! File reading handler implementation.\n\nuse memchr::memchr2;\n"
      }
    ],
    "isError": false,
    "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
    "start_line": 1,
    "end_line": 3,
    "total_lines": 832
  }
}
```

### 10.3 Numbered range output

With `show_line_numbers: true`, line numbers are right-aligned to the resolved end-line width:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Read",
    "arguments": {
      "path": "example.txt",
      "start_line": 9,
      "end_line": 10,
      "show_line_numbers": true
    }
  }
}
```

Response body (`content[0].text`):

```
 9\t9\n10\t10\n
```

### 10.4 Out-of-range error

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "start_line 5000 exceeds file line count 832"}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `read_file_show_line_numbers_defaults_to_false` | `tools-mcp-local/src/tools/handlers/read_file.rs:508` | Default behavior emits raw bytes with no tab prefix. |
| `read_file_empty_file_returns_empty_content` | `tools-mcp-local/src/tools/handlers/read_file.rs:531` | Empty file → success, empty body, zeroed line bounds. |
| `read_file_end_line_zero_returns_error` | `tools-mcp-local/src/tools/handlers/read_file.rs:551` | `end_line=0` rejected. |
| `read_file_start_line_zero_is_validated_before_file_read` | `tools-mcp-local/src/tools/handlers/read_file.rs:570` | `start_line=0` rejected before opening a missing file. |
| `read_file_end_line_zero_is_validated_before_file_read` | `tools-mcp-local/src/tools/handlers/read_file.rs:588` | `end_line=0` rejected before opening a missing file. |
| `read_file_start_greater_than_end_is_validated_before_file_read` | `tools-mcp-local/src/tools/handlers/read_file.rs:606` | Inverted range rejected before I/O. |
| `read_file_range_preserves_mixed_newline_endings` | `tools-mcp-local/src/tools/handlers/read_file.rs:628` | CRLF/LF/CR preserved in returned slice. |
| `read_file_range_preserves_invalid_utf8_lossy_replacement` | `tools-mcp-local/src/tools/handlers/read_file.rs:651` | Invalid UTF-8 returns U+FFFD, not an error. |
| `read_file_large_range_returns_raw_selected_lines` | `tools-mcp-local/src/tools/handlers/read_file.rs:674` | Streaming path returns correct slice for files > 64 KiB. |
| `read_file_large_numbered_range_preserves_formatting` | `tools-mcp-local/src/tools/handlers/read_file.rs:700` | Streaming path renders correct `"NNNN\t..."` prefix width. |
| `read_file_image_named_binary_preserves_lossy_text_behavior` | `tools-mcp-local/src/tools/handlers/read_file.rs:729` | PNG-like binary returns lossy text, not an image payload. |
| `read_file_large_image_named_range_preserves_lossy_text_behavior` | `tools-mcp-local/src/tools/handlers/read_file.rs:756` | Streaming path tolerates binary content. |
| `read_file_full_file_preserves_invalid_utf8_lossy_replacement` | `tools-mcp-local/src/tools/handlers/read_file.rs:785` | Whole-file path also returns U+FFFD for invalid bytes. |
| `read_file_numbered_range_uses_resolved_end_width` | `tools-mcp-local/src/tools/handlers/read_file.rs:806` | Numbered output width matches `resolved_end`, not `start_line`. |
| `line_scanner_handles_cr_only_files` | `tools-mcp-local/src/tools/handlers/read_file.rs:452` | Classic Mac CR-only files are line-counted correctly. |
| `line_scanner_handles_mixed_newlines` | `tools-mcp-local/src/tools/handlers/read_file.rs:458` | Mixed CRLF/LF/CR within one file. |
| `line_scanner_handles_crlf_without_counting_lf_twice` | `tools-mcp-local/src/tools/handlers/read_file.rs:464` | CRLF treated as a single line terminator. |
| `streaming_line_scanner_handles_crlf_split_across_chunks` | `tools-mcp-local/src/tools/handlers/read_file.rs:489` | `\r` at end of one buffer and `\n` at start of next is one line. |
| `test_read_file_no_line_numbers_by_default` | `tools-mcp-server/tests/integration_test.rs:194` | End-to-end: raw bytes, no `1\t` prefix by default. |
| `test_read_file_shows_line_numbers_when_enabled` | `tools-mcp-server/tests/integration_test.rs:232` | End-to-end: `show_line_numbers=true` adds the tab prefix. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `Read` apply the workspace-root path policy? | No. Unlike the mutation tools, `Read` opens the path directly via `Path::new(&req.path)` (`read_file.rs:57`). Read access is intentionally unsandboxed; mutation is sandboxed. See §7. |
| 2 | Does `Read` decode notebooks, PDFs, or images? | No. The MCP-level `Read` is a text reader that returns lossily decoded bytes. Binary file types return U+FFFD glyph soup rather than a structured render. |
| 3 | What happens to a `\r\n` split across the 64 KiB stream buffer? | The scanner stores a `pending_cr` flag, treats the following `\n` (if any) as part of the same CRLF terminator, and counts the pair as one line (`read_file.rs:280-313`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok`/`err` constructors and `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` is invoked at line 88 (§4.1). |
| `tools-mcp-local/src/tools/read.rs` | Schema and `define_mcp_tool!` invocation (§4.1, §6.1). |
| `tools-mcp-local/src/tools/handlers/read_file.rs` | Handler implementation (§6.2). |
