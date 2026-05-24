# SDD: Copy

**Date:** 2026-05-24
**Scope:** Design contract for the `Copy` MCP tool.
**Source:** `tools-mcp-local/src/tools/fileops.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Copy` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Copy` copies a regular file or — when `recursive=true` — a directory tree from `source` to `destination`, with workspace-root path policy enforcement on both endpoints. It implements cp-style "copy into existing directory" semantics, refuses unsafe shapes (overwriting a directory with a non-directory; recursing through symlinked directories; recursing into a destination nested inside the source), and uses staged temp paths to bound rollback when overwriting. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_copy` in `tools-mcp-local/src/tools/fileops.rs:164`.

### 3.2 Explicitly Out of Scope

- Moving / renaming (use `Move` — `tools-mcp-local/src/tools/fileops.rs:26`).
- Listing directories (use `ListDir` — `tools-mcp-local/src/tools/fileops.rs:881`).
- Reading or editing the copied file.
- Preserving extended attributes, ACLs, ownership, or extended metadata beyond what `tokio::fs::copy` provides.
- Multi-source batch copies.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Copy` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_copy` (`tools-mcp-local/src/tools/fileops.rs:164`) |
| Schema definition | `tools-mcp-local/src/tools/fileops.rs:478-508` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:26`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err`. The handler MUST NOT panic.
- **`deny_unknown_fields`.** `CopyRequest` rejects any property outside `source` / `destination` / `overwrite` / `recursive` (`fileops.rs:154`). Unknown fields produce `"invalid arguments: ..."`.
- **Path policy for BOTH endpoints.** Both `source` and `destination` MUST resolve through `path_policy::resolve_mutation_path` before any mutation (`fileops.rs:177,181`). The final destination (after directory-target join) is re-resolved through the policy a second time (`fileops.rs:228-231`). Locked in by `copy_rejects_source_outside_workspace` (`fileops.rs:544`).
- **Source must exist.** If `source` does not exist, return `"source not found: <source>"` (`fileops.rs:186-188`).
- **Refuse overwriting a directory with a non-directory.** When `overwrite=true` and `source` is a regular file and `destination` is a real (non-symlink) directory, return `"refusing to overwrite directory <dst> with non-directory source <src>: type-mismatch replacement would recursively delete the directory; remove or rename the directory first if replacement is intended"` (`fileops.rs:206-214`). The check is repeated on the joined `final_dest` for defense in depth (`fileops.rs:237-249`) and again inside the `copy_file_with_overwrite` primitive in case of a TOCTOU race (`fileops.rs:418-428`). Locked in by `copy_refuses_overwriting_directory_with_file` (`fileops.rs:707`) and `copy_file_with_overwrite_refuses_directory_destination` (`fileops.rs:765`).
- **Refuse recursive copy of a symlinked directory.** When `source` is a symlink whose target is a directory and `recursive=true`, return `"refusing recursive copy from symlinked directory: <source>"` (`fileops.rs:251-257`). Locked in by `copy_rejects_symlinked_directory_as_recursive_source` (Unix-only) (`fileops.rs:619`).
- **Refuse recursive copy into the source's own subtree.** When `source.is_dir()` and `recursive=true` and `final_dest` is inside `source`, refuse with `"refusing recursive copy: destination <dst> is inside source <src> (would recurse indefinitely)"` (`fileops.rs:259-269`). Locked in by `copy_rejects_recursive_copy_into_own_subdirectory` (`fileops.rs:562`).
- **Refuse recursing through symlinks during directory walk.** Within `copy_dir_recursive`, encountering a symlink at any depth returns `std::io::Error` with kind `InvalidInput` and message `"refusing to recurse through symlink while copying directory: <path>"` (`fileops.rs:361-369`). Locked in by `copy_rejects_symlink_inside_recursive_directory_copy` (`fileops.rs:587`).
- **Default refuses existing destination.** When `final_dest.exists()` and `overwrite=false`, refuse with `"destination already exists: <dst>. Use overwrite: true to replace."` (`fileops.rs:271-276`).
- **Parent directory creation.** Missing parent directories of the final destination MUST be created via `create_dir_all` before the copy (`fileops.rs:278-283`).
- **Directory source requires `recursive=true`.** If `source.is_dir()` and `recursive=false`, refuse with `"<source> is a directory. Use recursive: true to copy directories."` (`fileops.rs:290-295`).
- **Staged overwrite for files and directories.** Overwrite operations stage the new content at `replacement_stage_path(dst)` (a sibling temp path with a unique nanosecond and PID suffix), then remove the original and rename the staged copy into place (`fileops.rs:380-393,404-446,448-476`). Errors leave a best-effort cleanup attempt on the temp.
- **Container semantics for existing-directory destinations.** When `destination` is a real (non-symlink) directory, `Copy` puts the source INSIDE it using `source.file_name()` (`fileops.rs:200-227`). Symlinked-directory destinations are NOT treated as containers — they remain replaceable paths so `overwrite=true` only unlinks the symlink (`fileops.rs:216-219`). Locked in by `copy_overwrite_replaces_symlink_to_directory_without_destroying_target` (Unix-only) (`fileops.rs:797`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT overwrite a real directory with a non-directory source. (Multi-layer defense: handler entry, joined-path check, and primitive re-check.)
- MUST NOT recurse through symlinked directories during a recursive copy (would risk symlink loops, escape, or unintended data movement).
- MUST NOT recurse into a destination nested inside the source (would recurse indefinitely).
- MUST NOT copy a directory without `recursive=true`.
- MUST NOT copy outside the workspace root (either source or destination).
- MUST NOT overwrite an existing destination unless `overwrite=true` is explicitly set.

## 5. Design Goals

- **Fail-closed on every shape that risks recursive deletion.** The "overwrite directory with a file" defense exists at three layers (entry, joined path, primitive) so future callers cannot bypass it (`fileops.rs:206-214,237-249,418-428`).
- **Symlink-aware container semantics.** Real directories act as `cp`-style containers; symlinked directories are replaceable paths so an `overwrite=true` does not mutate the target the symlink pointed to.
- **Staged temp + rename for overwrite.** Bounds the time window in which the destination is in an inconsistent state and keeps a best-effort cleanup path if the rename fails.
- **Symlink refusal during recursion.** Symbolic links inside a copied tree can loop or reach outside the workspace; refusing closes both concerns.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `source` | string | Yes | — | Non-empty; resolves inside workspace; exists | Path to copy. |
| `destination` | string | Yes | — | Non-empty; resolves inside workspace | Destination path. When it is an existing real directory, the source is copied into it using the source's filename. |
| `overwrite` | boolean | No | `false` | — | When `true`, replace an existing destination (subject to the directory-with-file refusal). |
| `recursive` | boolean | No | `false` | — | When `true`, allow copying directories. |

The schema sets `"additionalProperties": false` (`fileops.rs:505`); the request type uses `#[serde(deny_unknown_fields)]` (`fileops.rs:154`).

> Schema source: `tools-mcp-local/src/tools/fileops.rs:482-506`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<CopyRequest>` (`fileops.rs:165-168`).
2. **Validate `source` non-empty** — `validation::validate_non_empty(&req.source, "source", None)` (`fileops.rs:170-172`).
3. **Validate `destination` non-empty** — `validation::validate_non_empty(&req.destination, "destination", None)` (`fileops.rs:173-175`).
4. **Resolve `source` under workspace** — (`fileops.rs:177-180`).
5. **Resolve `destination` under workspace** — (`fileops.rs:181-184`).
6. **Source-existence check** — `source.exists()`; on miss, `"source not found: <source>"` (`fileops.rs:186-188`).
7. **Inspect source metadata** — `tokio::fs::symlink_metadata(&source)` (`fileops.rs:190-198`).
8. **Determine if destination is a real directory** — `tokio::fs::symlink_metadata(&destination)` and `file_type().is_dir()` (the symlink check distinguishes real directories from symlinked ones) (`fileops.rs:201-204`).
9. **First-layer directory-with-file refusal** — When `overwrite=true` AND source is a file AND destination is a real directory, refuse (`fileops.rs:206-214`).
10. **Directory-target join** — Real-directory destination → `final_dest = destination.join(source.file_name())`; symlinked-directory destination → use `destination` as-is. Source without filename → refuse (`fileops.rs:219-227`).
11. **Re-resolve `final_dest`** — Through path policy (`fileops.rs:228-231`).
12. **Second-layer directory-with-file refusal (on joined path)** — Repeated against `final_dest` to catch the case where the join produced a path whose metadata then says "directory" (`fileops.rs:237-249`).
13. **Refuse recursive copy of a symlinked directory source** — When source is a symlink AND its resolved type is a directory AND `recursive=true` (`fileops.rs:251-257`).
14. **Refuse recursive copy into source's own subtree** — When source is a directory AND `recursive=true` AND `final_dest` starts with `source` after canonicalization (`fileops.rs:259-269`).
15. **Existing-destination guard** — When `final_dest.exists()` AND `overwrite=false`, refuse (`fileops.rs:271-276`).
16. **Create parent dirs** — `create_dir_all(final_dest.parent())` (`fileops.rs:278-283`).
17. **File branch** — `source.is_file()` → call `copy_file_with_overwrite(&source, &final_dest, overwrite)` (`fileops.rs:286-289`). This primitive uses staged temp + rename when overwriting, and re-checks the directory-destination defense (`fileops.rs:404-446`).
18. **Directory branch** — `source.is_dir()` AND `recursive=true` → `copy_directory_with_overwrite(&source, &final_dest, overwrite)` (`fileops.rs:290-303`). Internally, `copy_dir_recursive` walks the source and refuses any symlink encountered (`fileops.rs:349-378`).
19. **Build success envelope** — `ToolCallOutcome::ok_text_with` with text `"Copied <source> to <final_dest>"` and extras `source` and `destination` (`fileops.rs:305-311`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Copied a.txt to b.txt"}],
  "isError": false,
  "source": "a.txt",
  "destination": "b.txt"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"Copied <source> to <destination>"`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `source` | string | Yes | Original source path (post-policy, via `path.display()`). |
| `destination` | string | Yes | Final destination path (post directory-target join). |

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
| Empty `source` / `destination` | `true` | `"source is required (non-empty string)"` / `"destination is required (non-empty string)"` |
| Path policy rejection on either endpoint | `true` | `"path rejected for 'source': ..."` / `"path rejected for 'destination': ..."` |
| Source does not exist | `true` | `"source not found: <source>"` (`fileops.rs:187`) |
| Source metadata inspection failed | `true` | `"failed to inspect source <source>: <err>"` (`fileops.rs:193-196`) |
| Overwriting a real directory with a non-directory source (entry layer) | `true` | `"refusing to overwrite directory <dst> with non-directory source <src>: type-mismatch replacement would recursively delete the directory; remove or rename the directory first if replacement is intended"` (`fileops.rs:207-213`) |
| Overwriting a real directory with a non-directory source (joined path) | `true` | Same text (`fileops.rs:242-248`) |
| Overwriting a real directory in the primitive (TOCTOU defense) | `true` | `"refusing to overwrite directory <dst> with a non-directory source"` returned as `io::Error InvalidInput`; surfaces as `"failed to copy <source>: <err>"` (`fileops.rs:288-289,420-428`) |
| Source has no filename | `true` | `"source has no filename"` (`fileops.rs:223`) |
| Recursive copy from a symlinked directory | `true` | `"refusing recursive copy from symlinked directory: <source>"` (`fileops.rs:252-256`) |
| Recursive copy into source's own subtree | `true` | `"refusing recursive copy: destination <dst> is inside source <src> (would recurse indefinitely)"` (`fileops.rs:263-267`) |
| Symlink encountered during recursive directory walk | `true` | `"failed to copy directory <source>: refusing to recurse through symlink while copying directory: <path>"` (`fileops.rs:297-302,361-369`) |
| Destination exists and `overwrite=false` | `true` | `"destination already exists: <dst>. Use overwrite: true to replace."` (`fileops.rs:272-275`) |
| Parent directory creation failed | `true` | `"failed to create parent directory: <err>"` (`fileops.rs:283`) |
| Directory source without `recursive=true` | `true` | `"<source> is a directory. Use recursive: true to copy directories."` (`fileops.rs:292-294`) |
| File copy I/O failure | `true` | `"failed to copy <source>: <err>"` (`fileops.rs:288`) |
| Directory copy I/O failure | `true` | `"failed to copy directory <source>: <err>"` (`fileops.rs:297-302`) |

## 7. Security Considerations

- **Three-layer defense against directory recursive-wipe.** Overwriting a real directory with a non-directory source is refused at the handler entry, at the joined-path check, and inside the primitive. Tests `copy_refuses_overwriting_directory_with_file` (`fileops.rs:707`) and `copy_file_with_overwrite_refuses_directory_destination` (`fileops.rs:765`) lock these in. This closes the original exploit where `Copy {source: file, destination: dir, overwrite: true}` could be coaxed into recursively deleting a directory subtree.
- **Symlink refusal during recursion.** `copy_dir_recursive` refuses any symlink it encounters at any depth (`fileops.rs:361-369`). This blocks loop-based attacks (a symlink pointing back to an ancestor) and out-of-tree data movement (a symlink pointing outside the workspace).
- **Symlinked-directory source refusal.** A symlinked-directory source as the recursive starting point is also refused (`fileops.rs:251-257`); this would otherwise let a workspace symlink act as a launchpad for copying out-of-workspace data into the workspace.
- **Symlinked-directory destination — explicit safe behavior.** When `destination` is a symlink-to-directory and `overwrite=true`, the OS-level rename (via the staged temp) unlinks ONLY the symlink, leaving the target directory intact. Locked in by `copy_overwrite_replaces_symlink_to_directory_without_destroying_target` (`fileops.rs:797`).
- **Path-policy on both endpoints + joined path.** Identical to `Move`, but additionally checks `final_dest` after the directory-target join (`fileops.rs:228-231`).
- **No atomic semantics across all paths.** The staged-temp `rename` is atomic at the OS level on a single filesystem, but cross-fs copies are not atomic. The destination may be in a partial state if the second copy or rename fails; the temp is best-effort cleaned but may persist as `.<basename>.codex-tmp-<pid>-<nanos>`.
- **No xattrs / ACLs preserved beyond `tokio::fs::copy` defaults.** Operators relying on extended metadata must use shell tools.

## 8. Configuration

Not applicable. `Copy` reads no environment variables. The workspace root is `std::env::current_dir()` at the time of the call (`path_policy.rs:84`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 26 |
| Tool name + schema | `tools-mcp-local/src/tools/fileops.rs` | 478-508 |
| Handler entry point | `tools-mcp-local/src/tools/fileops.rs` | 164 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/fileops.rs` | 153-162 |
| Path policy: `source`, `destination`, `final_dest` | `tools-mcp-local/src/tools/fileops.rs` | 177, 181, 228 |
| Real-directory vs symlinked-directory detection | `tools-mcp-local/src/tools/fileops.rs` | 201-204 |
| Directory-with-file refusal — entry layer | `tools-mcp-local/src/tools/fileops.rs` | 206-214 |
| Directory-with-file refusal — joined-path layer | `tools-mcp-local/src/tools/fileops.rs` | 237-249 |
| Directory-with-file refusal — primitive layer | `tools-mcp-local/src/tools/fileops.rs` | 418-428 |
| Symlinked-directory recursive-source refusal | `tools-mcp-local/src/tools/fileops.rs` | 251-257 |
| Recursive-into-own-subtree refusal | `tools-mcp-local/src/tools/fileops.rs` | 259-269 |
| Symlink refusal in recursive walk | `tools-mcp-local/src/tools/fileops.rs` | 361-369 |
| `overwrite=false` existing-destination guard | `tools-mcp-local/src/tools/fileops.rs` | 271-276 |
| Parent directory creation | `tools-mcp-local/src/tools/fileops.rs` | 278-283 |
| Recursive directory requirement | `tools-mcp-local/src/tools/fileops.rs` | 290-295 |
| Staged temp path for overwrite | `tools-mcp-local/src/tools/fileops.rs` | 380-393 |
| File overwrite primitive | `tools-mcp-local/src/tools/fileops.rs` | 404-446 |
| Directory overwrite primitive | `tools-mcp-local/src/tools/fileops.rs` | 448-476 |

## 10. Examples

### 10.1 Copy a file

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Copy",
    "arguments": {
      "source": "src/main.rs",
      "destination": "backups/main.rs"
    }
  }
}
```

### 10.2 Copy a file into an existing directory

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Copy",
    "arguments": {
      "source": "notes/today.md",
      "destination": "archive/"
    }
  }
}
```

The final destination is `archive/today.md`.

### 10.3 Recursive directory copy with overwrite

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Copy",
    "arguments": {
      "source": "src",
      "destination": "dst",
      "recursive": true,
      "overwrite": true
    }
  }
}
```

When `dst` is an existing real directory, the result is `dst/src/...`, leaving any sibling content of `dst` intact (locked in by `copy_overwrite_into_existing_directory_keeps_container`, `fileops.rs:649`).

### 10.4 Refuses overwriting a directory with a file

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "Copy",
    "arguments": {
      "source": "attacker.txt",
      "destination": "victim_dir",
      "overwrite": true
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "refusing to overwrite directory victim_dir with non-directory source attacker.txt: type-mismatch replacement would recursively delete the directory; remove or rename the directory first if replacement is intended"}],
    "isError": true
  }
}
```

Locked in by `copy_refuses_overwriting_directory_with_file` (`fileops.rs:707`).

### 10.5 Refuses recursive copy into own subtree

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "Copy",
    "arguments": {
      "source": "src",
      "destination": "src/nested",
      "recursive": true
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "refusing recursive copy: destination src/nested is inside source src (would recurse indefinitely)"}],
    "isError": true
  }
}
```

Locked in by `copy_rejects_recursive_copy_into_own_subdirectory` (`fileops.rs:562`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `copy_rejects_source_outside_workspace` | `tools-mcp-local/src/tools/fileops.rs:544` | `..` source blocked by path policy. |
| `copy_rejects_recursive_copy_into_own_subdirectory` | `tools-mcp-local/src/tools/fileops.rs:562` | Recursive copy into a nested destination refused. |
| `copy_rejects_symlink_inside_recursive_directory_copy` (Unix) | `tools-mcp-local/src/tools/fileops.rs:587` | Symlink loop inside a recursive copy refused with `"symlink"` in the message. |
| `copy_rejects_symlinked_directory_as_recursive_source` (Unix) | `tools-mcp-local/src/tools/fileops.rs:619` | Symlink-to-directory as the recursive root refused; destination not created. |
| `copy_overwrite_into_existing_directory_keeps_container` | `tools-mcp-local/src/tools/fileops.rs:649` | Real-directory destination acts as container; sibling files preserved. |
| `copy_overwrite_replaces_destination_type_mismatch` | `tools-mcp-local/src/tools/fileops.rs:677` | Copying a directory recursively onto an existing file (with `overwrite=true`) replaces the file with the directory. |
| `copy_refuses_overwriting_directory_with_file` | `tools-mcp-local/src/tools/fileops.rs:707` | Original exploit closure: directory not wiped by `overwrite=true` with a file source. |
| `copy_file_with_overwrite_refuses_directory_destination` | `tools-mcp-local/src/tools/fileops.rs:765` | Defense-in-depth at the primitive layer. |
| `copy_overwrite_replaces_symlink_to_directory_without_destroying_target` (Unix) | `tools-mcp-local/src/tools/fileops.rs:797` | Symlinked-directory destination: only the symlink is unlinked; target directory and its contents survive. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Can `Copy` overwrite a directory with a file when `overwrite=true`? | No. Refused at three layers (entry, joined path, primitive). |
| 2 | Does recursive copy follow symlinks? | No. Both symlinked-directory source and any symlink discovered during recursion are refused (`fileops.rs:251-257,361-369`). |
| 3 | What happens when the destination is an existing real directory? | The source is copied INTO it using `source.file_name()` (cp-style) (`fileops.rs:200-227`). |
| 4 | What happens when the destination is a symlink to a directory and `overwrite=true`? | The symlink itself is unlinked and replaced; the target directory and its contents are preserved (`fileops.rs:216-219,797`). |
| 5 | Are overwrite operations atomic? | Single-filesystem rename of the staged temp is atomic; cross-fs is not. Best-effort temp cleanup is performed on errors (`fileops.rs:404-476`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_text_with` and `err` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/path_policy.rs` | Workspace-root path resolution (§7). |
| `tools-mcp-local/src/tools/fileops.rs` | Handler and schema (§6.2). |
