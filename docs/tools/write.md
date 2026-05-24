# SDD: Write

**Date:** 2026-05-24
**Scope:** Design contract for the `Write` MCP tool.
**Source:** `tools-mcp-local/src/tools/write.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Write` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Write` creates a single new file at the supplied path with the supplied UTF-8 string content, after creating any missing parent directories. It refuses to overwrite an existing file, refuses paths outside the workspace root, and writes bytes verbatim with no encoding transformation, BOM injection, or line-ending normalization. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_write` in `tools-mcp-local/src/tools/write.rs:16`.

### 3.2 Explicitly Out of Scope

- Modifying an existing file. Use `Edit` (`write.rs:50-55`).
- Writing arbitrary bytes (binary). `content` is a JSON string — binary content with embedded NUL or non-UTF-8 bytes cannot be represented at the schema level.
- Deleting a file. Use `Delete`.
- Creating a directory by itself. `Write` creates parent directories on demand but the leaf must be a file path; there is no "create empty directory" mode.
- Path policy for read-only access. `Read` does not enforce the policy; `Write` does (see §7).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Write` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_write` (`tools-mcp-local/src/tools/write.rs:16`) |
| Schema definition | `tools-mcp-local/src/tools/write.rs:149-169` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:22`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err` (`write.rs:28,35-39,51-58,62-63,66`). The handler MUST NOT panic.
- **`deny_unknown_fields`.** `WriteRequest` rejects any property outside `path` / `content` (`write.rs:10`). Unknown fields produce `"invalid arguments: ..."`.
- **No overwrite of an existing file.** The handler opens the destination with `OpenOptions::new().write(true).create_new(true).open(...)` (`write.rs:43-47`). `create_new(true)` MUST fail with `AlreadyExists` if the path exists; the handler MUST report `"file already exists: <path>. Use Edit to modify existing files."` and MUST leave the existing file content untouched. Locked in by `write_rejects_existing_files_without_overwriting` (`write.rs:123`).
- **Path policy enforcement.** Every write resolves through `path_policy::resolve_mutation_path(&req.path, "path")` before any filesystem mutation (`write.rs:26`). Paths that escape the server working directory MUST be rejected with the standard path-policy diagnostic (`path_policy.rs:26-36`). Locked in by `write_rejects_parent_traversal_outside_workspace` (`write.rs:83`).
- **Parent directories created.** When the resolved path's parent does not exist, the handler MUST call `tokio::fs::create_dir_all(parent).await` before opening the destination (`write.rs:31-39`). Failure to create the parent is reported as `"failed to create parent directories for <path>: <err>"`. Locked in by `write_preserves_in_workspace_create_behavior` (`write.rs:99`).
- **Bytes-verbatim semantics.** `content.as_bytes()` is the exact byte sequence written (`write.rs:42`). The handler MUST NOT inject a BOM, MUST NOT normalize newlines, and MUST NOT append a trailing newline. JSON-level UTF-8 is the only encoding constraint (caller cannot encode arbitrary bytes through a JSON string).
- **Flush before reporting success.** `file.flush().await` MUST run after `write_all` so the success response implies the bytes are at least handed to the OS (`write.rs:65-67`). Failure produces `"failed to flush <path>: <err>"`.
- **Success payload reports exact byte length.** The success message and the `bytes` field MUST equal `req.content.as_bytes().len()` (`write.rs:70-73`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT overwrite an existing file. Even when `overwrite` semantics would be convenient, the tool intentionally refuses to clobber existing files; callers must explicitly `Delete` then `Write` or use `Edit`.
- MUST NOT create a file outside the workspace root, including via traversal segments (`..`) or symlinks resolving outside the root.
- MUST NOT silently coerce or transform content (no UTF-16, no BOM, no line-ending normalization).
- MUST NOT write to a path with a missing parent without creating the parent first.
- MUST NOT report success when the OS-level write or flush failed.

## 5. Design Goals

- **No-overwrite default is a safety property, not an annoyance.** A multi-step agent loop that re-issues `Write` after a partial failure should not silently destroy in-progress work. The hard refusal forces the caller to acknowledge the situation (delete + retry, or switch to `Edit`).
- **Workspace confinement.** A creative agent should not be able to plant files in `~/.bashrc` or `/etc/cron.d/`; path policy keeps mutation inside the project.
- **Parent-dir convenience.** `mkdir -p`-style parent creation matches the most common intent — "drop a file at this path" — without forcing callers to issue a separate directory-creation tool.
- **Bytes-verbatim.** Format-preserving tools downstream (formatters, language servers) expect to see the exact bytes the caller intended. A line-ending normalizer would change diffs on every cross-platform write.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-empty; must resolve inside the server working directory; must not already exist | Destination file path. Parent directories are created on demand. |
| `content` | string | Yes | — | Any JSON string (UTF-8) | Bytes written verbatim. The JSON string is interpreted as UTF-8 with no BOM and no newline normalization. |

The schema sets `"additionalProperties": false` (`write.rs:166`); the request type uses `#[serde(deny_unknown_fields)]` (`write.rs:10`). Unknown fields produce a tool-level error with text `"invalid arguments: ..."`.

> Schema source: `tools-mcp-local/src/tools/write.rs:152-167`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<WriteRequest>` (`write.rs:17-20`). On failure return the parse error envelope.
2. **Validate `path` non-empty** — `validation::validate_non_empty(&req.path, "path", None)`; whitespace-only paths produce `"path is required (non-empty string)"` (`write.rs:22-24`).
3. **Resolve path under workspace** — `path_policy::resolve_mutation_path(&req.path, "path")` (`write.rs:26-29`). This step canonicalizes the deepest existing ancestor and validates that every component of the eventual path stays inside the workspace root (`path_policy.rs:38-181`).
4. **Create parent directories** — If `path.parent()` is `Some` and non-empty, call `tokio::fs::create_dir_all(parent).await`. Failure short-circuits with `"failed to create parent directories for <path>: <err>"` (`write.rs:31-39`).
5. **Open with `create_new(true)`** — `OpenOptions::new().write(true).create_new(true).open(&path).await` (`write.rs:43-47`). On `ErrorKind::AlreadyExists`, return `"file already exists: <path>. Use Edit to modify existing files."` (`write.rs:50-55`). On any other open error, return `"failed to create <path>: <err>"` (`write.rs:56-58`).
6. **Write all bytes** — `file.write_all(req.content.as_bytes()).await`. Failure produces `"failed to write <path>: <err>"` (`write.rs:61-63`).
7. **Flush** — `file.flush().await`. Failure produces `"failed to flush <path>: <err>"` (`write.rs:65-67`).
8. **Build success envelope** — `ToolCallOutcome::ok_text_with` with text `"Created <path> (<N> bytes)"` and extras `path` (string) and `bytes` (integer) (`write.rs:69-74`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Created src/new_file.rs (128 bytes)"}],
  "isError": false,
  "path": "src/new_file.rs",
  "bytes": 128
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"Created <path> (<N> bytes)"` where `<N>` is the UTF-8 byte length of `content`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `path` | string | Yes | The path as displayed by `path.display()` after workspace resolution. |
| `bytes` | integer | Yes | Number of bytes written. |

Constructed via `ToolCallOutcome::ok_text_with` (`tools-mcp-core/src/tool_outcome.rs:82-96`).

**Tool-level error (`isError: true`):**

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors use `ToolCallOutcome::err` (`tools-mcp-core/src/tool_outcome.rs:35`). The handler MUST NOT panic; every failure path returns a `ToolCallOutcome`.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Argument deserialization failure | `true` | `"invalid arguments: ..."` plus class hint (`tool_outcome.rs:62-74`) |
| Empty / whitespace-only `path` | `true` | `"path is required (non-empty string)"` (`validation.rs:17`) |
| Path policy rejection | `true` | `"path rejected for 'path': ...The resolved path must stay inside the server working directory <ws>. Remediation: ..."` (`path_policy.rs:26-36`) |
| `create_dir_all` failure on the parent | `true` | `"failed to create parent directories for <path>: <err>"` (`write.rs:35-38`) |
| Destination already exists | `true` | `"file already exists: <path>. Use Edit to modify existing files."` (`write.rs:51-54`) |
| Other open / create failure | `true` | `"failed to create <path>: <err>"` (`write.rs:57`) |
| `write_all` I/O error | `true` | `"failed to write <path>: <err>"` (`write.rs:62`) |
| `flush` I/O error | `true` | `"failed to flush <path>: <err>"` (`write.rs:66`) |

## 7. Security Considerations

- **Path policy enforcement.** Every write traverses `path_policy::resolve_mutation_path` (`write.rs:26`). The policy:
  - Canonicalizes the workspace root (`path_policy.rs:83-100`).
  - For non-existent paths, computes the deepest existing ancestor, canonicalizes it, and replays the remaining path components while re-checking containment after every `..` and every newly resolved symlink (`path_policy.rs:142-181,227-274`).
  - Rejects absolute paths outside the root, `..` traversal that escapes, and symlinks whose canonical target lies outside the root (`path_policy.rs:303-322`).
- **No overwrite — safety, not authorization.** The refusal is a "fail safely on a repeated call" property; it does not implement an authorization model. Operators that need stronger mutation control MUST sandbox the server process.
- **Parent directory creation is unbounded.** `create_dir_all` creates every missing parent up to the workspace root. This can create a deep nested path that the caller did not visually inspect. Path policy ensures every created directory stays inside the workspace, but operators should be aware that one `Write` call can create many directories.
- **No atomic-replace semantics.** `Write` always targets new files, so atomic replace is not at issue. If the OS write fails mid-stream, the caller sees an I/O error and the file may be a partial write — `Delete` it before retrying.
- **No symlink creation.** This tool only creates regular files (`OpenOptions::new().write(true).create_new(true)`). It never produces a symlink.
- **Bytes are caller-controlled.** Callers MUST treat `Write` as the user-trust boundary: nothing in the tool validates or sanitizes the content for the file type. Source files containing prompt-injection text are written verbatim.

## 8. Configuration

Not applicable. `Write` does not read any environment variables. The workspace root is `std::env::current_dir()` at the time of the call (`path_policy.rs:84`).

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 22 |
| Tool name + schema | `tools-mcp-local/src/tools/write.rs` | 149-169 |
| Handler entry point | `tools-mcp-local/src/tools/write.rs` | 16 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/write.rs` | 9-14 |
| Path policy resolution | `tools-mcp-local/src/tools/write.rs` | 26-29 |
| Parent directory creation | `tools-mcp-local/src/tools/write.rs` | 31-39 |
| `create_new(true)` no-overwrite | `tools-mcp-local/src/tools/write.rs` | 43-47 |
| AlreadyExists branch | `tools-mcp-local/src/tools/write.rs` | 50-55 |
| Write + flush + success envelope | `tools-mcp-local/src/tools/write.rs` | 61-73 |
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
    "name": "Write",
    "arguments": {
      "path": "notes/today.md",
      "content": "# Notes\n\n- one\n- two\n"
    }
  }
}
```

### 10.2 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "Created notes/today.md (24 bytes)"}],
    "isError": false,
    "path": "notes/today.md",
    "bytes": 24
  }
}
```

### 10.3 Existing-file refusal

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Write",
    "arguments": {
      "path": "Cargo.toml",
      "content": "this would clobber the manifest"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "file already exists: <ws>/Cargo.toml. Use Edit to modify existing files."}],
    "isError": true
  }
}
```

Locked in by `write_rejects_existing_files_without_overwriting` (`write.rs:123`).

### 10.4 Path escape rejected

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Write",
    "arguments": {
      "path": "../outside-write-policy.txt",
      "content": "blocked"
    }
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "path rejected for 'path': ../outside-write-policy.txt resolves outside the server working directory. ..."}],
    "isError": true
  }
}
```

Locked in by `write_rejects_parent_traversal_outside_workspace` (`write.rs:83`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `write_rejects_parent_traversal_outside_workspace` | `tools-mcp-local/src/tools/write.rs:83` | `..` traversal rejected by path policy with `"outside the server working directory"`. |
| `write_preserves_in_workspace_create_behavior` | `tools-mcp-local/src/tools/write.rs:99` | Writes succeed inside the workspace; `create_dir_all` materializes missing parents. |
| `write_rejects_existing_files_without_overwriting` | `tools-mcp-local/src/tools/write.rs:123` | Existing file is not overwritten; original content preserved; `"file already exists"` returned. |
| `resolves_non_existing_path_inside_workspace` | `tools-mcp-local/src/path_policy.rs:342` | (Indirect) path policy supports new-file targets inside workspace. |
| `rejects_parent_traversal_outside_workspace` | `tools-mcp-local/src/path_policy.rs:357` | (Indirect) policy rejects `..` escape. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Can `Write` overwrite an existing file? | No. `create_new(true)` is used unconditionally (`write.rs:45`). Callers must `Delete` first or switch to `Edit`. |
| 2 | Does `Write` create parent directories? | Yes. `tokio::fs::create_dir_all(parent)` runs after path policy and before `create_new` (`write.rs:31-39`). |
| 3 | Does `Write` add a BOM, normalize newlines, or append a trailing newline? | No. `content.as_bytes()` is the exact byte stream sent to `write_all` (`write.rs:42`). |
| 4 | Are paths outside the workspace allowed? | No. `path_policy::resolve_mutation_path` rejects them (`write.rs:26-29`). |
| 5 | Is the response shape structured (`path`, `bytes`) or text-only? | Both. The text summary is in `content[0].text`; `path` and `bytes` appear at the result top level via `ok_text_with` (`write.rs:69-73`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok_text_with` and `err` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/path_policy.rs` | Workspace-root path resolution for new-file targets (§7). |
| `tools-mcp-local/src/tools/write.rs` | Handler and schema (§6.2). |
