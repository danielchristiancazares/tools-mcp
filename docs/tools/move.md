# SDD: Move

**Date:** 2026-05-24
**Scope:** Design contract for the `Move` MCP tool.
**Source:** `tools-mcp-local/src/tools/fileops.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Move` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Move` renames or relocates a single file or directory inside the workspace. When the destination is an existing directory, it places the source inside that directory using the source's filename (cp/mv-style). When the destination is a file path, it renames the source to that path. On cross-filesystem `rename` failures it falls back to `copy + remove` for regular files. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_move` in `tools-mcp-local/src/tools/fileops.rs:26`.

### 3.2 Explicitly Out of Scope

- Copying (use `Copy` — `tools-mcp-local/src/tools/fileops.rs:164`).
- Listing directories (use `ListDir` — `tools-mcp-local/src/tools/fileops.rs:881`).
- Reading or editing the moved file.
- Multi-source batch moves.
- Cross-filesystem directory moves; the `copy + remove` fallback applies only to regular files (`fileops.rs:96-110`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Move` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_move` (`tools-mcp-local/src/tools/fileops.rs:26`) |
| Schema definition | `tools-mcp-local/src/tools/fileops.rs:122-147` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:25`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err` (`fileops.rs:30,33,36,41,44,49,57,63,71,80,91,99,104,109`). The handler MUST NOT panic.
- **`deny_unknown_fields`.** `MoveRequest` rejects any property outside `source` / `destination` / `overwrite` (`fileops.rs:18`). Unknown fields produce `"invalid arguments: ..."`.
- **Path policy for BOTH endpoints.** Both `source` and `destination` MUST resolve through `path_policy::resolve_mutation_path` before any mutation (`fileops.rs:39,43`). The final destination (after directory-target expansion) is re-resolved through the policy a second time to validate any joined path component (`fileops.rs:62`). Locked in by `move_rejects_destination_outside_workspace` (`fileops.rs:520`).
- **Source must exist.** If `source` does not exist on disk, return `"source not found: <path>"` (`fileops.rs:48-50`).
- **Directory-target expansion.** When `destination` is an existing directory, the final destination becomes `destination.join(source.file_name())` (`fileops.rs:53-61`). A source with no `file_name()` (e.g., `/`) is rejected with `"source has no filename"`.
- **Refuse moving a directory inside itself.** When `source` is a directory and the destination canonicalizes inside the source tree, refuse with `"refusing move: destination <dst> is inside source <src>"` (`fileops.rs:67-77`). Locked in by `move_rejects_directory_into_own_descendant_without_creating_parent` (`fileops.rs:842`).
- **`overwrite=false` (default) rejects existing destination.** Return `"destination already exists: <dst>. Use overwrite: true to replace."` (`fileops.rs:79-84`).
- **Parent directory creation.** Missing parent directories of the final destination MUST be created with `create_dir_all` before the rename (`fileops.rs:87-92`).
- **Cross-filesystem fallback (regular files only).** If `tokio::fs::rename` fails AND `source.is_file()`, attempt `tokio::fs::copy + tokio::fs::remove_file`. If the copy fails, surface both errors. If the copy succeeds but the source remove fails, surface `"moved file but failed to remove source: <err>"` (`fileops.rs:94-110`). Cross-filesystem moves of directories are NOT supported (would fall into the else branch and surface the original rename error).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT move a file outside the workspace root, either as `source` or as `destination`.
- MUST NOT move a directory into a path nested inside itself (would either fail at the OS level or create a recursive structure).
- MUST NOT overwrite an existing destination unless `overwrite=true` is explicitly set.
- MUST NOT silently leave the destination in a partial state on cross-filesystem moves. If the copy succeeds but the source unlink fails, the response surfaces the situation explicitly.
- MUST NOT use empty `source` or empty `destination`. Both are validated with `validation::validate_non_empty`.

## 5. Design Goals

- **cp/mv-style ergonomics.** Treating an existing-directory destination as a container ("move into") matches user mental models from POSIX shell tools and avoids the surprise of accidentally renaming the directory itself.
- **Fail-closed on directory-inside-itself.** Moving a directory under one of its own descendants is rarely intentional and can create unbounded recursion or leave dangling subtrees. The explicit refusal turns the bug into a tool error.
- **Fallback only when safe.** The `copy + remove` fallback applies to regular files where the semantics are unambiguous; cross-fs directory moves are left to the caller because the right answer (atomicity? partial-state recovery?) is policy-dependent.
- **Per-endpoint policy validation.** Validating `source`, `destination`, and the final joined destination separately catches the case where the caller passes a workspace-internal `destination` directory but the joined path (with a malicious source filename) would escape.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `source` | string | Yes | — | Non-empty; must resolve inside the workspace; must exist | Path of the file or directory to move. |
| `destination` | string | Yes | — | Non-empty; must resolve inside the workspace | Destination path. If it is an existing directory, the source is moved into it with the same filename; otherwise it is the new path. |
| `overwrite` | boolean | No | `false` | — | When `true`, replace an existing destination. When `false` (default), an existing destination errors out. |

The schema sets `"additionalProperties": false` (`fileops.rs:144`); the request type uses `#[serde(deny_unknown_fields)]` (`fileops.rs:18`). Unknown fields produce a tool-level error with text `"invalid arguments: ..."`.

> Schema source: `tools-mcp-local/src/tools/fileops.rs:126-145`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<MoveRequest>` (`fileops.rs:27-30`).
2. **Validate `source` non-empty** — `validation::validate_non_empty(&req.source, "source", None)` (`fileops.rs:32-34`).
3. **Validate `destination` non-empty** — `validation::validate_non_empty(&req.destination, "destination", None)` (`fileops.rs:35-37`).
4. **Resolve `source` under workspace** — `path_policy::resolve_mutation_path(&req.source, "source")` (`fileops.rs:39-42`).
5. **Resolve `destination` under workspace** — `path_policy::resolve_mutation_path(&req.destination, "destination")` (`fileops.rs:43-46`).
6. **Source-existence check** — `source.exists()`; on miss, return `"source not found: <path>"` (`fileops.rs:48-50`).
7. **Directory-target expansion** — If `destination.is_dir()`, set `final_dest = destination.join(source.file_name()?)`. If `source.file_name()` is `None`, return `"source has no filename"` (`fileops.rs:53-61`).
8. **Re-resolve final destination** — `path_policy::resolve_mutation_path(&final_dest, "destination")` (`fileops.rs:62-65`).
9. **Refuse directory-inside-itself** — If `source.is_dir()`, compare canonicalized `source` and `final_dest`; if `final_dest` starts with `source`, refuse with `"refusing move: destination <dst> is inside source <src>"` (`fileops.rs:67-77`). Helper `canonicalize_existing_or_normalized` uses `canonicalize` when the path exists or a `..`-removed normalized path otherwise (`fileops.rs:325-332`).
10. **Existing-destination guard** — If `final_dest.exists()` and `overwrite=false`, refuse with `"destination already exists: <dst>. Use overwrite: true to replace."` (`fileops.rs:79-84`).
11. **Create parent dirs** — `create_dir_all(final_dest.parent())` when the parent does not exist (`fileops.rs:87-92`).
12. **Rename** — `tokio::fs::rename(&source, &final_dest)` (`fileops.rs:94`).
13. **Cross-fs fallback for files** — If `rename` fails AND `source.is_file()`: `tokio::fs::copy(&source, &final_dest)` followed by `tokio::fs::remove_file(&source)` (`fileops.rs:96-110`). On any failure, surface a composite error.
14. **Build success envelope** — `ToolCallOutcome::ok_text_with` with text `"Moved <source> to <final_dest>"` and extras `source` and `destination` (`fileops.rs:113-120`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Moved a.txt to b.txt"}],
  "isError": false,
  "source": "a.txt",
  "destination": "b.txt"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"Moved <source> to <destination>"`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `source` | string | Yes | Original `source` path (after workspace resolution, via `path.display()`). |
| `destination` | string | Yes | Final `destination` path (after directory-target expansion). |

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
| Empty / whitespace-only `source` | `true` | `"source is required (non-empty string)"` |
| Empty / whitespace-only `destination` | `true` | `"destination is required (non-empty string)"` |
| `source` path policy rejection | `true` | `"path rejected for 'source': ..."` (`path_policy.rs:26-36`) |
| `destination` path policy rejection (initial or after directory-target join) | `true` | `"path rejected for 'destination': ..."` (`fileops.rs:44,63`) |
| Source does not exist | `true` | `"source not found: <source>"` (`fileops.rs:49`) |
| Source has no filename (e.g., `/`) | `true` | `"source has no filename"` (`fileops.rs:57`) |
| Destination is inside the source directory | `true` | `"refusing move: destination <dst> is inside source <src>"` (`fileops.rs:71-75`) |
| Destination exists and `overwrite=false` | `true` | `"destination already exists: <dst>. Use overwrite: true to replace."` (`fileops.rs:80-83`) |
| Parent-directory create failure | `true` | `"failed to create parent directory: <err>"` (`fileops.rs:91`) |
| Rename failure (no fallback, directory source) | `true` | `"failed to move <source>: <err>"` (`fileops.rs:109`) |
| Cross-fs copy fallback failure | `true` | `"failed to move <source>: <rename err>, copy fallback failed: <copy err>"` (`fileops.rs:98-101`) |
| Cross-fs source unlink failure after successful copy | `true` | `"moved file but failed to remove source: <err>"` (`fileops.rs:104-106`) |

## 7. Security Considerations

- **Two-stage path-policy validation.** `source` and `destination` are independently policy-checked, and the joined `final_dest` is checked again (`fileops.rs:39-65`). This blocks an attack where the destination directory is workspace-valid but the join via a malicious source filename would escape.
- **Directory-inside-itself refusal.** Comparing `canonicalize_existing_or_normalized(source)` to `canonicalize_existing_or_normalized(final_dest)` prevents creating a recursive directory layout on the move (`fileops.rs:67-77`). Locked in by `move_rejects_directory_into_own_descendant_without_creating_parent` (`fileops.rs:842`).
- **`overwrite=true` semantics — caller responsibility.** When `overwrite=true`, `rename` will replace the destination at the OS level; in the cross-fs `copy + remove` fallback, `tokio::fs::copy` follows OS semantics (typically overwrite the destination file in place). Operators relying on atomicity should test their target filesystem.
- **No SUID / xattr / permissions guarantees.** This tool relies on `rename`/`copy` semantics from `tokio::fs`; it does not preserve extended attributes, ACLs, or file ownership across cross-filesystem fallbacks beyond what those primitives provide.
- **No symlink hardening.** `canonicalize` resolves symlinks; if the source is a symlink, the resolved target is used for the "inside source" check. Path policy still requires the resolved target stays within the workspace.

## 8. Configuration

Not applicable. `Move` reads no environment variables. The workspace root is `std::env::current_dir()` at the time of the call (`path_policy.rs:84`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 25 |
| Tool name + schema | `tools-mcp-local/src/tools/fileops.rs` | 122-147 |
| Handler entry point | `tools-mcp-local/src/tools/fileops.rs` | 26 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/fileops.rs` | 17-24 |
| Path policy: `source` + `destination` + final | `tools-mcp-local/src/tools/fileops.rs` | 39, 43, 62 |
| Source existence guard | `tools-mcp-local/src/tools/fileops.rs` | 48-50 |
| Directory-target join | `tools-mcp-local/src/tools/fileops.rs` | 53-61 |
| Directory-inside-itself refusal | `tools-mcp-local/src/tools/fileops.rs` | 67-77 |
| `overwrite=false` guard | `tools-mcp-local/src/tools/fileops.rs` | 79-84 |
| Parent directory creation | `tools-mcp-local/src/tools/fileops.rs` | 87-92 |
| Cross-fs `copy + remove` fallback | `tools-mcp-local/src/tools/fileops.rs` | 94-110 |
| Success envelope | `tools-mcp-local/src/tools/fileops.rs` | 113-120 |
| `canonicalize_existing_or_normalized` helper | `tools-mcp-local/src/tools/fileops.rs` | 325-332 |
| `normalize_path` helper | `tools-mcp-local/src/tools/fileops.rs` | 334-346 |

## 10. Examples

### 10.1 Rename a file

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Move",
    "arguments": {
      "source": "draft.md",
      "destination": "final.md"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "Moved draft.md to final.md"}],
    "isError": false,
    "source": "draft.md",
    "destination": "final.md"
  }
}
```

### 10.2 Move a file into an existing directory

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Move",
    "arguments": {
      "source": "scratch/notes.md",
      "destination": "docs/"
    }
  }
}
```

The final destination is `docs/notes.md` (the source filename joined onto the existing directory).

### 10.3 Existing-destination refusal

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "destination already exists: final.md. Use overwrite: true to replace."}],
    "isError": true
  }
}
```

### 10.4 Directory-inside-itself refusal

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "Move",
    "arguments": {
      "source": "src",
      "destination": "src/nested/moved"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "refusing move: destination src/nested/moved is inside source src"}],
    "isError": true
  }
}
```

Locked in by `move_rejects_directory_into_own_descendant_without_creating_parent` (`fileops.rs:842`).

### 10.5 Destination outside workspace rejected

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "Move",
    "arguments": {
      "source": "move-source.txt",
      "destination": "../outside-move-policy.txt"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "path rejected for 'destination': ../outside-move-policy.txt resolves outside the server working directory. ..."}],
    "isError": true
  }
}
```

Locked in by `move_rejects_destination_outside_workspace` (`fileops.rs:520`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `move_rejects_destination_outside_workspace` | `tools-mcp-local/src/tools/fileops.rs:520` | `..` destination blocked by path policy; source untouched. |
| `move_rejects_directory_into_own_descendant_without_creating_parent` | `tools-mcp-local/src/tools/fileops.rs:842` | Directory-inside-itself refusal; no parent dirs created, original files preserved. |

## 12. Open Questions

1. The current test set does not lock in the cross-filesystem `copy + remove` fallback path explicitly; behavior is exercised only when the rename returns `ErrorKind::CrossesDevices` at runtime. Adding a fault-injection test would be valuable but is out of scope for this SDD.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `Move` overwrite an existing destination? | Only when `overwrite=true`. The default refuses (`fileops.rs:79-84`). |
| 2 | What happens when the destination is an existing directory? | The source is moved into the directory using `source.file_name()` as the new basename (`fileops.rs:53-61`). |
| 3 | Are cross-filesystem moves supported? | For regular files yes, via a `copy + remove_file` fallback (`fileops.rs:96-110`). For directories, no — the original rename error surfaces (`fileops.rs:108-109`). |
| 4 | Does `Move` validate the joined directory-target path again through path policy? | Yes (`fileops.rs:62-65`), so a workspace-internal `destination` directory plus a `..`-laden `source.file_name()` cannot escape. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_text_with` and `err` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/path_policy.rs` | Workspace-root path resolution (§7). |
| `tools-mcp-local/src/tools/fileops.rs` | Handler and schema (§6.2). |
