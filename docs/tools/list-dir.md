# SDD: ListDir

**Date:** 2026-05-24
**Scope:** Design contract for the `ListDir` MCP tool.
**Source:** `tools-mcp-local/src/tools/fileops.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `ListDir` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`ListDir` returns the (non-recursive) contents of a single directory. It returns the file/directory/symlink entries — name-sorted — both as a newline-joined text block in `content[0].text` and as a structured `entries` array. Hidden entries (basenames starting with `.`) are filtered by default. A `long` mode adds size and modification time. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_listdir` in `tools-mcp-local/src/tools/fileops.rs:881`.

### 3.2 Explicitly Out of Scope

- Recursive directory walks. For deep enumeration, use `Glob` with `**` (`tools-mcp-local/src/tools/glob.rs`).
- Pattern filtering. `ListDir` has no glob/include filter; combine with `Glob` for that.
- Path policy enforcement. `ListDir` does NOT route through `path_policy::resolve_existing_directory`; it uses `Path::new(&req.path)` directly (`fileops.rs:891`). It is a read-only inspection tool and intentionally not sandboxed (see §7).
- Following symlinks into other directories — `ListDir` reports symlink entries as type `"symlink"` but does not recurse.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `ListDir` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_listdir` (`tools-mcp-local/src/tools/fileops.rs:881`) |
| Schema definition | `tools-mcp-local/src/tools/fileops.rs:1004-1030` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:27`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err` (`fileops.rs:883,887-889,895-899,902-906,919-933`). The handler MUST NOT panic.
- **`deny_unknown_fields`.** `ListDirRequest` rejects any property outside `path` / `all` / `long` (`fileops.rs:872`).
- **`path` must exist and be a directory.** Non-existing paths return `"path not found: <path>. Remediation: check the path (relative to the server working directory) or use '.' for the current directory."` (`fileops.rs:895-899`). Files return `"not a directory: <path>. Remediation: pass a directory path (use Read for files)."` (`fileops.rs:902-906`).
- **Hidden-entry filter is the default.** When `all` is absent or `false`, entries whose basename starts with `.` are filtered out (`scope_cache.rs:729-731`). Locked in by `handle_listdir_lists_hidden_when_all_is_true` (`fileops.rs:1095`).
- **Name-sorted output.** Entries returned in the structured `entries` array are sorted by basename ascending (`scope_cache.rs:746`). The text body for the non-long mode mirrors this order. Locked in by `handle_listdir_returns_name_sorted_entries_and_text` (`fileops.rs:1146`).
- **Long-mode text is re-sorted on the formatted line.** Long-mode lines are formatted as `"<type-char> <size> <basename>"` and then sorted lexicographically on the formatted line (`fileops.rs:944-964,988-992`). This preserves a historical behavior different from the structured-array order.
- **Output suffixes for short mode.** Directory basenames get `"/"`, symlinks get `"@"`, regular files get no suffix (`fileops.rs:973-979`).
- **Symlink classification uses `symlink_metadata`.** Symlinks are reported as type `"symlink"`, not as their target type (`scope_cache.rs:733-744`).
- **No recursion.** Only the immediate children of `path` are returned.
- **Per-directory snapshot cache.** Repeat calls on the same `(path, all)` key reuse a cached `DirEntriesSnapshot` rather than re-running `read_dir` (`fileops.rs:912-934`). The cache invalidates when the directory's metadata changes. Locked in by `dir_entries_cache_returns_same_snapshot_for_repeat_list_dir_key` (`fileops.rs:1070`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT recurse. Subdirectories are reported as `"dir"` entries; their contents are not enumerated.
- MUST NOT follow symlinks. A symlink target may be a directory but is reported as type `"symlink"`; `ListDir` does not list the target's contents.
- MUST NOT silently hide existence: passing a file path returns an error, not an empty list.
- MUST NOT report hidden entries unless `all=true`.

## 5. Design Goals

- **Cheap and idempotent.** A directory-scoped snapshot cache lets repeated `ListDir` and `Glob` calls on the same path share filesystem work in the same MCP session.
- **Structured plus textual response.** The text body lets callers paste a quick `ls`-style summary into a chat; the `entries` array lets programmatic callers iterate without re-parsing.
- **Hidden by default.** Most agentic use cases want a directory's "logical" contents (source files), not `.DS_Store` / `.git` / `.cache` noise.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-empty; must exist and be a directory | Directory to list (non-recursive). Use `.` for the server working directory. |
| `all` | boolean | No | `false` | — | When `true`, include entries whose basename starts with `.`. |
| `long` | boolean | No | `false` | — | When `true`, the text body uses `<type> <size> <name>` (sorted lexicographically on the formatted line) and each entry in the array carries `size` and `modified` (Unix seconds). |

The schema sets `"additionalProperties": false` (`fileops.rs:1027`); the request type uses `#[serde(deny_unknown_fields)]` (`fileops.rs:872`).

> Schema source: `tools-mcp-local/src/tools/fileops.rs:1008-1028`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<ListDirRequest>` (`fileops.rs:882-885`).
2. **Validate `path` non-empty** — `validation::validate_non_empty(&req.path, "path", None)` (`fileops.rs:887-889`).
3. **Check existence + directoryness** — `path.exists()` / `path.is_dir()`. Distinct error messages for "not found" and "not a directory" (`fileops.rs:891,895-907`).
4. **Build snapshot via cache** — `dir_entries_cache().get_or_build(&DirEntriesKey {path, show_hidden})`. The async snapshot builder calls `tokio::fs::read_dir`, filters hidden entries unless `show_hidden=true`, captures `symlink_metadata` per entry, and sorts by basename (`fileops.rs:912-934`, `scope_cache.rs:721-758`).
5. **Map cache errors to user-facing messages** — I/O → `"failed to read directory <path>: <err>. Remediation: ..."`; Walk → `"list_dir: directory walk failed: ..."`; Timeout → `"list_dir: directory listing timed out. Remediation: ..."` (`fileops.rs:918-933`).
6. **Render text body** — Per entry:
   - **Short mode** (`long=false`): `"<basename><suffix>"` where suffix is `"/"` (dir), `"@"` (symlink), or `""` (file) (`fileops.rs:972-980`).
   - **Long mode** (`long=true`): `"<type-char> <size:>10> <basename>"` where type char is `'l'` (symlink), `'d'` (dir), or `'-'` (file) (`fileops.rs:944-964`).
7. **Build entries array** — Per entry: `{name, type}` (short mode) or `{name, type, size, modified}` (long mode). `modified` is Unix seconds (UNIX_EPOCH offset) or `null` (`fileops.rs:946-985`).
8. **Sort long-mode text** — `lines.sort()` after formatting so the text block is sorted by the formatted line, not by basename alone (`fileops.rs:988-992`). The structured `entries` array remains basename-sorted because the cache snapshot is already sorted (`scope_cache.rs:746`).
9. **Build success envelope** — `ToolCallOutcome::ok_text_with(lines.join("\n"), [("path", ...), ("count", ...), ("entries", ...)])` (`fileops.rs:994-1001`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "alpha.txt\nbeta/\nzeta.txt"}],
  "isError": false,
  "path": "src",
  "count": 3,
  "entries": [
    {"name": "alpha.txt", "type": "file"},
    {"name": "beta", "type": "dir"},
    {"name": "zeta.txt", "type": "file"}
  ]
}
```

In `long` mode, each entry also carries `size` (integer bytes) and `modified` (integer Unix seconds or `null`):

```json
{
  "content": [{"type": "text", "text": "- 1234 alpha.txt\nd          0 beta\n- 4096 zeta.txt"}],
  "isError": false,
  "path": "src",
  "count": 3,
  "entries": [
    {"name": "alpha.txt", "type": "file", "size": 1234, "modified": 1748102400},
    {"name": "beta", "type": "dir", "size": 0, "modified": 1748100000},
    {"name": "zeta.txt", "type": "file", "size": 4096, "modified": 1748103000}
  ]
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Newline-joined listing. Empty string when the directory has no visible entries. |
| `isError` | boolean | Yes | Always `false` on success. |
| `path` | string | Yes | Echo of the input `path` via `path.display()`. |
| `count` | integer | Yes | Number of entries in `entries`. |
| `entries` | array of object | Yes | Per-entry objects (see above). |

Constructed via `ToolCallOutcome::ok_text_with` (`tools-mcp-core/src/tool_outcome.rs:82-96`).

**Tool-level error (`isError: true`):**

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors use `ToolCallOutcome::err` (`tool_outcome.rs:35`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Argument deserialization failure | `true` | `"invalid arguments: ..."` plus class hint (`tool_outcome.rs:62-74`) |
| Empty / whitespace-only `path` | `true` | `"path is required (non-empty string)"` (`validation.rs:17`) |
| Path does not exist | `true` | `"path not found: <path>. Remediation: check the path (relative to the server working directory) or use '.' for the current directory."` (`fileops.rs:896-899`) |
| Path is not a directory | `true` | `"not a directory: <path>. Remediation: pass a directory path (use Read for files)."` (`fileops.rs:903-906`) |
| Underlying `read_dir` I/O error | `true` | `"failed to read directory <path>: <err>. Remediation: check permissions and that the path is a directory."` (`fileops.rs:919-922`) |
| Directory walk error from snapshot builder | `true` | `"list_dir: directory walk failed: <message>. Remediation: check permissions and that the path is a directory."` (`fileops.rs:925-928`) |
| Snapshot timeout | `true` | `"list_dir: directory listing timed out. Remediation: retry or list a smaller directory."` (`fileops.rs:930-933`) |

## 7. Security Considerations

- **No path-policy enforcement.** `ListDir` deliberately uses `Path::new(&req.path)` (`fileops.rs:891`) and does NOT call `path_policy::resolve_existing_directory`. Read access is intentionally unsandboxed so callers can inspect repository-adjacent paths (e.g., for diagnostic purposes). Mutation tools (`Write`, `Edit`, `Delete`, `Move`, `Copy`) DO enforce the workspace policy. Hosts that need read confinement MUST sandbox the server process itself.
- **Untrusted output.** Filenames are external data. Callers MUST treat `name` strings as untrusted and MUST NOT execute or interpret them as commands. Pathological filenames (control characters, ANSI sequences, very long basenames) are returned verbatim.
- **Symlinks classified, not resolved.** A symlink basename returns type `"symlink"`. The handler does not follow it and so cannot inadvertently expose the target's contents.
- **Resource bounds.** Directories with hundreds of thousands of entries produce hundreds of thousands of JSON objects in `entries`. The snapshot cache caps the number of distinct directories cached (`DEFAULT_DIR_CACHE_MAX_ENTRIES = 64`, `scope_cache.rs:17`) but does not cap the entries per snapshot.

## 8. Configuration

Not applicable. `ListDir` reads no environment variables. The per-directory snapshot cache is process-internal.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 27 |
| Tool name + schema | `tools-mcp-local/src/tools/fileops.rs` | 1004-1030 |
| Handler entry point | `tools-mcp-local/src/tools/fileops.rs` | 881 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/fileops.rs` | 871-879 |
| Existence + directory checks | `tools-mcp-local/src/tools/fileops.rs` | 895-907 |
| Cache key construction | `tools-mcp-local/src/tools/fileops.rs` | 912-915 |
| Cache lookup + error mapping | `tools-mcp-local/src/tools/fileops.rs` | 916-934 |
| Long-mode formatting + size column | `tools-mcp-local/src/tools/fileops.rs` | 944-964 |
| Short-mode suffixes | `tools-mcp-local/src/tools/fileops.rs` | 972-980 |
| Long-mode text re-sort | `tools-mcp-local/src/tools/fileops.rs` | 988-992 |
| Success envelope | `tools-mcp-local/src/tools/fileops.rs` | 994-1001 |
| Async snapshot builder | `tools-mcp-local/src/tools/scope_cache.rs` | 721-758 |
| Hidden-entry filter | `tools-mcp-local/src/tools/scope_cache.rs` | 729-731 |
| Basename sort | `tools-mcp-local/src/tools/scope_cache.rs` | 746 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "ListDir",
    "arguments": {"path": "src"}
  }
}
```

### 10.2 Success response (short mode)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "alpha.txt\nbeta/\nzeta.txt"}],
    "isError": false,
    "path": "src",
    "count": 3,
    "entries": [
      {"name": "alpha.txt", "type": "file"},
      {"name": "beta", "type": "dir"},
      {"name": "zeta.txt", "type": "file"}
    ]
  }
}
```

Locked in by `handle_listdir_returns_name_sorted_entries_and_text` (`fileops.rs:1146`).

### 10.3 Including hidden entries

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "ListDir",
    "arguments": {"path": ".", "all": true}
  }
}
```

Response includes `.gitignore`, `.git/`, `.env`, etc. when present. Locked in by `handle_listdir_lists_hidden_when_all_is_true` (`fileops.rs:1095`).

### 10.4 Long-mode output

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "ListDir",
    "arguments": {"path": "src", "long": true}
  }
}
```

Response shape:

```json
{
  "result": {
    "content": [{"type": "text", "text": "-       1234 alpha.txt\nd          0 beta\n-       4096 zeta.txt"}],
    "isError": false,
    "path": "src",
    "count": 3,
    "entries": [
      {"name": "alpha.txt", "type": "file", "size": 1234, "modified": 1748102400},
      {"name": "beta", "type": "dir", "size": 0, "modified": 1748100000},
      {"name": "zeta.txt", "type": "file", "size": 4096, "modified": 1748103000}
    ]
  }
}
```

### 10.5 Not a directory

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "ListDir",
    "arguments": {"path": "Cargo.toml"}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "not a directory: Cargo.toml. Remediation: pass a directory path (use Read for files)."}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `dir_entries_cache_returns_same_snapshot_for_repeat_list_dir_key` | `tools-mcp-local/src/tools/fileops.rs:1070` | Repeat `ListDir` reuses the same `Arc<DirEntriesSnapshot>`. |
| `handle_listdir_lists_hidden_when_all_is_true` | `tools-mcp-local/src/tools/fileops.rs:1095` | `all=true` includes `.secret`; default filters it. |
| `handle_listdir_returns_name_sorted_entries_and_text` | `tools-mcp-local/src/tools/fileops.rs:1146` | Entries sorted by basename; suffix `"/"` for directories. |
| `dir_entries_cache_rebuilds_after_directory_change` | `tools-mcp-local/src/tools/scope_cache.rs:1271` | Cache invalidates when the directory's metadata changes. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `ListDir` enforce path policy? | No. It is a read-only inspection tool and uses the path verbatim (`fileops.rs:891`). Mutation tools enforce the policy. |
| 2 | Does `ListDir` recurse? | No. Only direct children are returned. Use `Glob` with `**` for deep enumeration. |
| 3 | Are hidden files included by default? | No. Set `all=true` to include them (`scope_cache.rs:729-731`). |
| 4 | Why is long-mode text sorted on the formatted line while the array stays basename-sorted? | Preserves a historical text-format sort (size/type-driven appearance after formatting) while keeping the structured array deterministic. The handler explicitly does `lines.sort()` after formatting (`fileops.rs:988-992`). |
| 5 | Does `ListDir` follow symlinks? | No. Symlinks are classified via `symlink_metadata` and reported as type `"symlink"` (`scope_cache.rs:733-744`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_text_with` and `err` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/tools/fileops.rs` | Handler and schema (§6.2). |
| `tools-mcp-local/src/tools/scope_cache.rs` | Snapshot cache + async `read_dir` builder (§6.2). |
