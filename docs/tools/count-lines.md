# SDD: CountLines

**Date:** 2026-07-11
**Scope:** Design contract for the `CountLines` MCP tool.
**Source:** `tools-mcp-local/src/tools/count_lines.rs`

## 1. Scope

`CountLines` summarizes files with one extension under each immediate child directory of a root. It recursively counts matching files and their non-empty lines, matching PowerShell's `Get-Content | Measure-Object -Line` behavior, then returns both an aligned text table and structured totals.

The default request reproduces the common Rust-workspace report:

```json
{
  "extension": "rs"
}
```

It scans the server working directory, excludes directories named `target`, `.git`, and `.claude`, and sorts child directories by line count descending.

## 2. Registration

| Property | Value |
|---|---|
| MCP tool name | `CountLines` |
| Registration gate | None; always registered |
| Owning crate | `tools-mcp-local` |
| Handler | `handle_count_lines` |
| Registration | `tools-mcp-local/src/tools/mod.rs` |

## 3. Input Schema

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `extension` | string | Yes | - | Extension to count. `rs`, `.rs`, and `*.rs` are equivalent. Compound suffixes such as `tar.gz` are supported. |
| `path` | string | No | `.` | Root whose immediate child directories are summarized. |
| `exclude` | array of strings | No | `["target", ".git", ".claude"]` | Directory basenames excluded at the root and at every recursive depth. Pass `[]` to disable exclusions. Maximum 128 entries. |

Unknown fields are rejected. Exclusion entries must be individual directory names, not paths.

## 4. Behavior

1. Validate and normalize the extension by removing one leading `*.` or `.`.
2. Validate that `path` exists and is a directory.
3. Enumerate the root's immediate real directories. Files and directory symlinks are not report rows.
4. Exclude configured directory names using ASCII case-insensitive comparison.
5. Recursively walk every remaining child directory without following symlinks. Hidden files are included, and `.ignore`/Git ignore files are not applied.
6. Match filename suffixes using ASCII case-insensitive comparison. For example, `lib.rs` and `LIB.RS` both match `rs`.
7. Count non-empty lines directly from bytes:
   - LF, CRLF, and CR are recognized as line endings.
   - CRLF counts as one line ending.
   - Empty lines do not count; whitespace-only lines do count.
   - A non-empty unterminated final line counts.
   - Empty files have zero lines.
   - Input does not need to be UTF-8.
8. Include child directories with zero matching files.
9. Sort rows by `lines` descending, then `files` descending, then `directory` ascending.

Any directory-walk or matching-file read failure fails the request with `isError: true`; counts are never silently partial.

## 5. Response

Successful responses contain:

| Field | Type | Description |
|---|---|---|
| `content[0].text` | string | Aligned `Directory`, `Files`, and `Lines` table. Empty when there are no included child directories. |
| `isError` | boolean | `false` on success. |
| `path` | string | Root path used for the scan. |
| `extension` | string | Normalized extension without a leading dot. |
| `excluded_directories` | array of strings | Normalized exclusion names used for the scan. |
| `directory_count` | integer | Number of rows in `directories`. |
| `total_files` | integer | Sum of matching files across all rows. |
| `total_lines` | integer | Sum of non-empty lines across all rows. |
| `directories` | array of objects | Sorted `{directory, path, files, lines}` rows. |

Example:

```json
{
  "content": [
    {
      "type": "text",
      "text": "Directory        Files  Lines\n---------------  -----  -----\ntools-mcp-local     31   8200\ntools-mcp-core      14   3100"
    }
  ],
  "isError": false,
  "path": ".",
  "extension": "rs",
  "excluded_directories": ["target", ".git", ".claude"],
  "directory_count": 2,
  "total_files": 45,
  "total_lines": 11300,
  "directories": [
    {
      "directory": "tools-mcp-local",
      "path": "./tools-mcp-local",
      "files": 31,
      "lines": 8200
    },
    {
      "directory": "tools-mcp-core",
      "path": "./tools-mcp-core",
      "files": 14,
      "lines": 3100
    }
  ]
}
```

## 6. Errors

Tool-level errors use the standard MCP content shape with `isError: true`. Error conditions include:

- Missing, empty, or invalid `extension`.
- A `path` value that is empty, inaccessible, nonexistent, or not a directory.
- Invalid `exclude` entries or more than 128 entries.
- Failure to enumerate, walk, open, or read a relevant filesystem object.
- Counter overflow or blocking-worker failure.

## 7. Security and Resource Use

- `CountLines` is read-only and follows the same unrestricted read-path model as `ListDir` and `Glob`; it does not apply mutation path policy.
- Directory and file symlinks are not followed.
- The scan runs on Tokio's blocking pool so filesystem traversal does not block the async MCP loop.
- Scans are intentionally unbounded to provide exact totals. Callers should narrow `path` or add exclusions for very large trees.
- Filenames and paths are untrusted output and must not be interpreted as commands.

## 8. Examples

Count Rust files and lines by workspace directory:

```json
{
  "name": "CountLines",
  "arguments": {
    "extension": "rs"
  }
}
```

Count TypeScript files under `packages`, excluding generated and dependency directories:

```json
{
  "name": "CountLines",
  "arguments": {
    "path": "packages",
    "extension": "ts",
    "exclude": ["node_modules", "dist", "generated"]
  }
}
```

## 9. Tests

Unit tests in `tools-mcp-local/src/tools/count_lines.rs` cover extension normalization, exclusion validation, line-ending semantics, sorting, default exclusions, empty exclusions, and path errors.

Server tests cover registration, schema defaults and constraints, README inventory parity, and an end-to-end `mcp/tools/call` request.
