# SDD: Delete

**Date:** 2026-05-24
**Scope:** Design contract for the `Delete` MCP tool.
**Source:** `tools-mcp-local/src/tools/delete.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Delete` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Delete` removes a single regular file at the supplied path. It explicitly refuses symlinks, directories, paths outside the workspace root, and missing files. There is no recursive, force, or batch mode. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_delete` in `tools-mcp-local/src/tools/delete.rs:14`.

### 3.2 Explicitly Out of Scope

- Directory removal. The tool intentionally rejects directories; operators MUST remove contained files individually or use a shell utility (`delete.rs:47-52`).
- Recursive deletion. There is no `recursive` flag.
- Symlink removal. Symlinks are rejected; users MUST delete the target file directly (`delete.rs:40-45`).
- Trash / soft-delete semantics. The tool calls `tokio::fs::remove_file`, which is an unrecoverable unlink.
- Dry-run / preview. No `dry_run` flag exists.
- Path policy for read-only access. `Read` does not enforce the policy; `Delete` does (see §7).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Delete` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_delete` (`tools-mcp-local/src/tools/delete.rs:14`) |
| Schema definition | `tools-mcp-local/src/tools/delete.rs:65-81` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:23`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err` (`delete.rs:18,20,26,32,35,41,48,55`). The handler MUST NOT panic.
- **`deny_unknown_fields`.** `DeleteRequest` rejects any property outside `path` (`delete.rs:9`). Unknown fields produce `"invalid arguments: ..."`.
- **Path policy enforcement.** The tool MUST resolve through `path_policy::resolve_mutation_path(&req.path, "path")` before any filesystem mutation (`delete.rs:24`). Paths that escape the workspace MUST be rejected. Locked in by `delete_rejects_parent_traversal_outside_workspace` (`delete.rs:158`).
- **Symlink refusal — TOCTOU-safe.** The tool MUST use `tokio::fs::symlink_metadata` (NOT `metadata`) so symlinks themselves are detected rather than followed (`delete.rs:29`). If the entry is a symlink, return error `"cannot delete symlink: <path>. Remediation: delete the target file directly instead."` (`delete.rs:40-44`). Locked in by `delete_rejects_symlinks` (`delete.rs:86`).
- **Directory refusal.** If the entry is a directory, return error `"cannot delete directory: <path>. This tool only deletes files. Remediation: delete files within the directory first, or use a shell tool carefully if you intend to remove a directory."` (`delete.rs:47-52`). Locked in by `delete_rejects_directories_without_removing_them` (`delete.rs:137`).
- **Regular files only.** Successful deletion uses `tokio::fs::remove_file(&path)` (`delete.rs:54`). This unlinks a regular file in one syscall; it does NOT recurse and does NOT remove a non-empty directory.
- **NotFound mapping.** When `symlink_metadata` returns `NotFound`, the handler MUST return `"file not found: <path>"` (`delete.rs:31-33`).
- **Path resolution stripped of `..` segments.** `resolve_mutation_path` rejects relative paths that walk above the workspace root (`path_policy.rs:237-247`). The `delete.rs` handler does NOT independently check; it relies on path policy.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT delete a directory under any circumstance.
- MUST NOT delete a symlink, even when the target is a regular file.
- MUST NOT delete a file outside the workspace root, including via `..` traversal or symlink-resolved paths.
- MUST NOT follow a symlink and delete the target (the symlink itself is what is detected and refused).
- MUST NOT silently succeed when the target is missing; `NotFound` is an explicit error.
- MUST NOT batch-delete or accept a list of paths.

## 5. Design Goals

- **Refuse-by-default for risky shapes.** A symlink chain or a non-empty directory deletion is rarely the intent of an automated edit loop; failing closed forces the caller to make the intent explicit (e.g., via a shell-out).
- **TOCTOU-aware metadata inspection.** `symlink_metadata` reads attributes of the named entry itself, so even if a malicious party swaps a regular file for a symlink between the call and the unlink, the handler sees the swapped state and refuses.
- **Single-file granularity.** Combined with `Write`'s no-overwrite rule, "delete then write" is the explicit pattern for replacing a file; both halves are independently observable and reversible only as long as the caller checks the response.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-empty; must resolve inside the server working directory; must be an existing regular file | The file to delete. |

The schema sets `"additionalProperties": false` (`delete.rs:78`); the request type uses `#[serde(deny_unknown_fields)]` (`delete.rs:9`). Unknown fields produce a tool-level error with text `"invalid arguments: ..."`.

> Schema source: `tools-mcp-local/src/tools/delete.rs:69-79`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<DeleteRequest>` (`delete.rs:15-18`).
2. **Validate `path` non-empty** — `validation::validate_non_empty(&req.path, "path", None)`; whitespace-only paths produce `"path is required (non-empty string)"` (`delete.rs:20-22`).
3. **Resolve path under workspace** — `path_policy::resolve_mutation_path(&req.path, "path")`. Rejects paths outside the root (`delete.rs:24-27`).
4. **Inspect symlink metadata** — `tokio::fs::symlink_metadata(&path).await` (`delete.rs:29`). On `NotFound`, return `"file not found: <path>"`. On other I/O error, return `"failed to inspect <path>: <err>"`.
5. **Refuse symlinks** — If `file_type.is_symlink()`, return the symlink refusal message (`delete.rs:40-45`).
6. **Refuse directories** — If `file_type.is_dir()`, return the directory refusal message (`delete.rs:47-52`).
7. **Unlink** — `tokio::fs::remove_file(&path).await`. On failure, return `"failed to delete <path>: <err>"` (`delete.rs:54-56`).
8. **Build success envelope** — `ToolCallOutcome::ok_text_with("Deleted <path>", [("path", <path>)])` (`delete.rs:59-62`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Deleted notes/today.md"}],
  "isError": false,
  "path": "notes/today.md"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"Deleted <path>"`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `path` | string | Yes | The path as displayed by `path.display()` after workspace resolution. |

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
| Path policy rejection | `true` | `"path rejected for 'path': ...The resolved path must stay inside the server working directory <ws>. Remediation: ..."` (`path_policy.rs:26-36`) |
| Path does not exist | `true` | `"file not found: <path>"` (`delete.rs:32`) |
| Failed to inspect the entry | `true` | `"failed to inspect <path>: <err>"` (`delete.rs:35`) |
| Target is a symlink | `true` | `"cannot delete symlink: <path>. Remediation: delete the target file directly instead."` (`delete.rs:41-44`) |
| Target is a directory | `true` | `"cannot delete directory: <path>. This tool only deletes files. Remediation: delete files within the directory first, or use a shell tool carefully if you intend to remove a directory."` (`delete.rs:48-51`) |
| Unlink I/O error | `true` | `"failed to delete <path>: <err>"` (`delete.rs:55`) |

## 7. Security Considerations

- **Path policy enforcement.** Every `Delete` resolves through `path_policy::resolve_mutation_path` (`delete.rs:24`). The policy canonicalizes the workspace root, then either canonicalizes the existing path or walks the deepest existing ancestor and verifies that every step stays inside the workspace (`path_policy.rs:138-181`).
- **Symlink refusal closes a privilege-escalation surface.** Following a symlink and deleting its target would let a path inside the workspace cause the unlink of any file the process owns (including outside the workspace). The handler explicitly refuses symlinks via `symlink_metadata` followed by `file_type().is_symlink()` (`delete.rs:29,40`).
- **TOCTOU window between `symlink_metadata` and `remove_file`.** A racing actor could swap the file for a symlink after the check but before the unlink; in practice this requires the same write permissions, and `remove_file` on a symlink unlinks only the symlink itself (the target is preserved), so the worst case is removing a symlink the user did not intend to. The strong invariant is "no directory is ever recursively wiped".
- **Directory refusal closes the recursive-wipe surface.** This mirrors the same defense applied in `Copy` (see `tools-mcp-local/src/tools/fileops.rs:206-214,239-249`) where overwriting a directory with a non-directory source is also refused.
- **No undo.** `remove_file` is destructive. Callers needing recoverability MUST stage their own backups before calling.
- **No environment variables.** The tool reads only `std::env::current_dir()` indirectly through path policy.

## 8. Configuration

Not applicable. `Delete` reads no environment variables. The workspace root is `std::env::current_dir()` at the time of the call (`path_policy.rs:84`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 23 |
| Tool name + schema | `tools-mcp-local/src/tools/delete.rs` | 65-81 |
| Handler entry point | `tools-mcp-local/src/tools/delete.rs` | 14 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/delete.rs` | 8-12 |
| Path policy resolution | `tools-mcp-local/src/tools/delete.rs` | 24-27 |
| `symlink_metadata` (TOCTOU-safe inspection) | `tools-mcp-local/src/tools/delete.rs` | 29 |
| Symlink refusal | `tools-mcp-local/src/tools/delete.rs` | 40-45 |
| Directory refusal | `tools-mcp-local/src/tools/delete.rs` | 47-52 |
| `remove_file` unlink | `tools-mcp-local/src/tools/delete.rs` | 54-56 |
| Success payload | `tools-mcp-local/src/tools/delete.rs` | 58-62 |
| Path policy: workspace canonicalization | `tools-mcp-local/src/path_policy.rs` | 83-100 |
| Path policy: workspace containment check | `tools-mcp-local/src/path_policy.rs` | 303-322 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Delete",
    "arguments": {"path": "scratch/tmp.txt"}
  }
}
```

### 10.2 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "Deleted scratch/tmp.txt"}],
    "isError": false,
    "path": "scratch/tmp.txt"
  }
}
```

### 10.3 Directory refusal

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Delete",
    "arguments": {"path": "src/"}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "cannot delete directory: src/. This tool only deletes files. Remediation: delete files within the directory first, or use a shell tool carefully if you intend to remove a directory."}],
    "isError": true
  }
}
```

Locked in by `delete_rejects_directories_without_removing_them` (`delete.rs:137`).

### 10.4 Symlink refusal

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Delete",
    "arguments": {"path": "link-to-target"}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "cannot delete symlink: link-to-target. Remediation: delete the target file directly instead."}],
    "isError": true
  }
}
```

Locked in by `delete_rejects_symlinks` (`delete.rs:86`).

### 10.5 Path escape rejected

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "Delete",
    "arguments": {"path": "../outside-delete-policy.txt"}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "path rejected for 'path': ../outside-delete-policy.txt resolves outside the server working directory. ..."}],
    "isError": true
  }
}
```

Locked in by `delete_rejects_parent_traversal_outside_workspace` (`delete.rs:158`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `delete_rejects_symlinks` (Unix only when symlink creation succeeds) | `tools-mcp-local/src/tools/delete.rs:86` | Symlink is rejected; the target file is preserved. |
| `delete_still_deletes_regular_files` | `tools-mcp-local/src/tools/delete.rs:117` | Regular file unlinks successfully; `isError: false`. |
| `delete_rejects_directories_without_removing_them` | `tools-mcp-local/src/tools/delete.rs:137` | Directory rejected; directory still exists. |
| `delete_rejects_parent_traversal_outside_workspace` | `tools-mcp-local/src/tools/delete.rs:158` | `..` rejected by path policy with `"outside the server working directory"`. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `Delete` support a recursive flag? | No. The schema has only `path`; the handler refuses directories outright (`delete.rs:47-52`). |
| 2 | Does `Delete` follow symlinks? | No. `symlink_metadata` inspects the entry itself, and any symlink is refused (`delete.rs:29,40-45`). |
| 3 | Is missing-target a success or an error? | An error: `"file not found: <path>"` (`delete.rs:32`). |
| 4 | Are deletions reversible / journaled? | No. `tokio::fs::remove_file` is an unconditional unlink. Callers must stage external backups if needed. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_text_with` and `err` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/path_policy.rs` | Workspace-root path resolution (§7). |
| `tools-mcp-local/src/tools/delete.rs` | Handler and schema (§6.2). |
| `tools-mcp-local/src/tools/fileops.rs` | Reference for the same anti-recursive-wipe defense in `Copy` (§7). |
