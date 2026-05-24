# SDD: git_snapshot

**Date:** 2026-05-24
**Scope:** Design contract for the `git_snapshot` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/snapshot.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `git_snapshot` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`git_snapshot` is a read-only triage MCP tool that bundles `git status --porcelain=1 -b` with optional `git diff --stat` and `git diff --cached --stat` invocations to produce a single structured worktree snapshot. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_snapshot` (`tools-mcp-git/src/git/handlers/snapshot.rs:64`). It is registered via `GitSnapshotTool` (`tools-mcp-git/src/tools.rs:9-25`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- JSON-RPC framing and method routing (covered in `docs/protocol.md`).
- Tool-registry composition (covered in `docs/architecture.md`).
- The lower-level `git status` and `git diff` tools (see `docs/tools/git-status.md`, `docs/tools/git-diff.md`).
- Mutating git operations (see `docs/tools/git-add.md`, `docs/tools/git-commit.md`, etc.).
- Cross-cutting environment variables (full catalog in `docs/configuration.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `git_snapshot` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_snapshot` (`tools-mcp-git/src/git/handlers/snapshot.rs:64`) |
| Schema definition | `tools-mcp-git/src/tools.rs:8-25` |
| Registration call | `tools-mcp-git/src/tools.rs:259` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to the literal string `"true"`, `tools_mcp_git::register_tools` MUST return without registering this tool (`tools-mcp-git/src/lib.rs:7-10`). Locked in by `test_git_tools_disabled_by_default` (`tools-mcp-server/tests/integration_test.rs:1270`).
- **Working-directory authority** — When `working_dir` is provided, it MUST be canonicalized and confined under the server's startup working directory; resolution errors MUST surface as a `git error: ...` tool-level error (`tools-mcp-git/src/git/path_policy.rs:163-181`, surfaced via `run_git` at `tools-mcp-git/src/git/mod.rs:158-159`).
- **Argument-list invocation** — Git MUST be invoked through `tokio::process::Command::args` with `--no-pager -c color.ui=false -c diff.external= -c core.fsmonitor=` prepended; arguments MUST NOT be passed through any shell interpreter (`tools-mcp-git/src/git/mod.rs:82-99,181-198`).
- **Authority env scrubbed** — Authority and helper environment variables (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_PARAMETERS`, `GIT_SSH`, `GIT_SSH_COMMAND`, `GIT_ASKPASS`, `SSH_ASKPASS`, plus all `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs) MUST be removed from the spawned child's environment (`tools-mcp-git/src/git/mod.rs:68-80,294-320`).
- **No system/global config** — `GIT_CONFIG_NOSYSTEM=1` MUST be set and `GIT_CONFIG_GLOBAL` MUST be redirected to `NUL` (Windows) or `/dev/null` (Unix) (`tools-mcp-git/src/git/mod.rs:184-192`).
- **Bounded execution** — Each git invocation MUST honor `timeout_ms` clamped to `[100, 300_000]` ms and capture stdout up to `DEFAULT_GIT_STDOUT_BYTES = 200_000` bytes and stderr up to `DEFAULT_GIT_STDERR_BYTES = 100_000` bytes (`tools-mcp-git/src/git/mod.rs:164-166`; `tools-mcp-core/src/config.rs:4-13`).
- **Diff stats skipped when clean** — When `include_diff_stats=true`, the status output is clean, and stdout was not truncated, the diff invocations MUST be skipped and synthetic empty `GitExecResult` values MUST be substituted (`tools-mcp-git/src/git/handlers/snapshot.rs:120-143,263-280`).
- **Concurrent diff invocations** — When the diff invocations actually run, the unstaged and staged variants MUST be awaited concurrently via `tokio::join!` (`tools-mcp-git/src/git/handlers/snapshot.rs:127-142`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`; the handler MUST NOT panic (`tools-mcp-git/src/git/handlers/snapshot.rs:80-83,103,131-141`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register the tool when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through `/bin/sh`, `cmd.exe`, or any other shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables (they are scrubbed in `remove_git_authority_env`).
- MUST NOT mutate the worktree, index, refs, or stash stack; this tool runs only `git status` and `git diff --stat` (`tools-mcp-git/src/git/handlers/snapshot.rs:93,220-245`).
- MUST NOT enable color output. `color.ui=false` is prepended on every invocation (`tools-mcp-git/src/git/mod.rs:82-99`).
- MUST NOT accept `working_dir` paths that resolve outside the server working directory (`tools-mcp-git/src/git/path_policy.rs:40-55`).

## 5. Design Goals

- **One round-trip for triage.** Combining porcelain status with structured counts and optional diff stats reduces multiple tool calls to a single MCP request, which is the most common worktree-inspection pattern for agentic callers.
- **Opt-in expense.** `git diff --stat` can be slow on large worktrees, so `include_diff_stats` defaults to `false` (`tools-mcp-git/src/git/handlers/snapshot.rs:87`). When the worktree is clean, the diff invocations are elided entirely.
- **Structured + human-readable text.** Callers that prefer machine parsing read `clean`, `branch`, `counts`, and `entries`; callers that prefer prose read `content[0].text` rendered by `render_snapshot_text` (`tools-mcp-git/src/git/handlers/snapshot.rs:345-380`).
- **Read-only by construction.** Snapshot uses only `git status` and `git diff --stat`; it cannot mutate refs, the index, or the worktree.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git commands. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` in `run_git` (`mod.rs:164`) | Per-command timeout in milliseconds. |
| `untracked` | boolean | No | `true` | — | When `false`, `-uno` is appended to `git status` (`snapshot.rs:226-228`). |
| `include_diff_stats` | boolean | No | `false` | — | When `true`, also runs `git diff --stat` and `git diff --cached --stat` (`snapshot.rs:87,120-164`). |
| `paths` | string array | No | `[]` | After trimming, MUST contain at least one non-empty element when provided (`snapshot.rs:202-218`) | Pathspec list appended after `--` to every git invocation. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:22`); the deserializer sets `#[serde(deny_unknown_fields)]` (`tools-mcp-git/src/git/handlers/snapshot.rs:66`). Unknown fields produce a tool-level error (`isError: true`) with text starting `"invalid arguments: ..."` and the hint `" Unknown fields are not allowed; check argument names against the tool schema."` (`tools-mcp-core/src/tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:8-25`

### 6.2 Behavior

1. **Parse + validate arguments** — Deserialize into the local `GitSnapshotRequest` struct via `ToolCallOutcome::parse_args` (`snapshot.rs:80-83`). On failure, return `isError: true` with the `parse_args` error wording (`tools-mcp-core/src/tool_outcome.rs:61-75`).
2. **Normalize paths** — `requested_paths` filters whitespace-only entries; when `paths` is supplied but every element is whitespace, return the tool-level error `"paths must include at least one non-empty path"` (`snapshot.rs:88-91,202-218`).
3. **Build status command** — Always start with `["status", "--porcelain=1", "-b"]`; append `"-uno"` when `untracked=false`; append `["--", paths...]` when `paths` is non-empty (`snapshot.rs:220-231`).
4. **Run status** — Call `run_git` (`tools-mcp-git/src/git/mod.rs:151`). Working directory MUST be resolved via `path_policy::resolve_working_dir` before spawning (`mod.rs:158-159`). Spawn `git`/`git.exe` with the safety-prefixed args (`mod.rs:181-198`), capture stdout/stderr with `read_to_end_limited` (`tools-mcp-core/src/process.rs:49-76`), and enforce the timeout via `tokio::time::timeout` (`mod.rs:220-235`). On timeout, kill the child and give it a 2 s grace period.
5. **Handle status failure** — If `status_exec.success` is `false`, build the snapshot-specific error envelope: `content[0].text` is the first non-empty of stderr/stdout (`first_non_empty`, `snapshot.rs:409-416`); the result includes `isError: true`, `working_dir`, and a `status` command summary (`snapshot.rs:106-114`).
6. **Parse porcelain status** — `parse_porcelain_status` strips the leading `## ...` branch line into `parsed.branch` and pushes `StatusEntry` records for every remaining non-trivial line, splitting `path -> original_path` rename arrows (`snapshot.rs:290-324`).
7. **Compute counts + cleanliness** — `count_status_entries` derives `staged`, `unstaged`, `untracked`, and `conflicted` counts using XY status classification (`snapshot.rs:19-44,326-343`). `clean = parsed.entries.is_empty()` (`snapshot.rs:118`).
8. **Optional diff stats** — When `include_diff_stats=true`:
   - **Clean fast path** — If `clean` is `true` and `status_exec.truncated_stdout` is `false`, synthesize empty `GitExecResult` values via `empty_diff_stat_exec` (`snapshot.rs:121-126,263-280`); no further git invocations occur.
   - **Concurrent diff calls** — Otherwise, spawn `run_diff_stat(working_dir, false, paths, timeout_ms)` and `run_diff_stat(working_dir, true, paths, timeout_ms)` and await them with `tokio::join!` (`snapshot.rs:127-142,247-261`). Each invocation runs `git diff --no-ext-diff --no-textconv --stat [--cached] [-- paths...]` (`snapshot.rs:233-245`).
   - **Diff infrastructure failure** — If either `run_git` future returns `Err`, return `ToolCallOutcome::err(format!("git diff --stat error: {err:#}"))` or the cached variant (`snapshot.rs:131-141`).
   - **Diff non-zero exit** — If either diff exits non-zero, return an `isError: true` payload with text drawn from the failing variant's stderr/stdout and command summaries for `status`, `unstaged_diff`, and `staged_diff` (`snapshot.rs:144-159`).
9. **Render human text** — `render_snapshot_text` builds the `content[0].text`. It emits `branch: <name|<unknown>>`, `clean: true|false`, `status:` block (either the trimmed porcelain stdout or `  <clean>`), then optional `unstaged diff stat:` and `staged diff stat:` blocks falling back to `  <none>` when empty (`snapshot.rs:345-407`).
10. **Compose response** — Return `ToolCallOutcome::ok` wrapping `{ content, isError: false, working_dir, clean, branch, counts: {staged, unstaged, untracked, conflicted}, entries: [...], status, unstaged_diff?, staged_diff? }` (`snapshot.rs:174-199`). Each command summary contains `{ args, stdout, stderr, exit_code, success, timed_out, truncated_stdout, truncated_stderr }` (`snapshot.rs:418-429`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "branch: main\nclean: false\nstatus:\n M src/lib.rs\n?? scratch.md\n"}],
  "isError": false,
  "working_dir": "C:/Users/Daniel/tools-mcp",
  "clean": false,
  "branch": "main...origin/main",
  "counts": {"staged": 0, "unstaged": 1, "untracked": 1, "conflicted": 0},
  "entries": [
    {"index_status": " ", "worktree_status": "M", "path": "src/lib.rs", "original_path": null},
    {"index_status": "?", "worktree_status": "?", "path": "scratch.md", "original_path": null}
  ],
  "status": {"args": ["--no-pager", "-c", "color.ui=false", "-c", "diff.external=", "-c", "core.fsmonitor=", "status", "--porcelain=1", "-b"], "stdout": "...", "stderr": "", "exit_code": 0, "success": true, "timed_out": false, "truncated_stdout": false, "truncated_stderr": false},
  "unstaged_diff": null,
  "staged_diff": null
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Human-readable rendering from `render_snapshot_text` (`snapshot.rs:345-380`). |
| `isError` | boolean | Yes | `false` on success. |
| `working_dir` | string \| null | Yes | Canonicalized working directory or `null` when `working_dir` was not supplied (`mod.rs:160-162,275`). |
| `clean` | boolean | Yes | `true` iff `parsed.entries` is empty (`snapshot.rs:118`). |
| `branch` | string \| null | Yes | Text after the `## ` branch header, or `null` when no branch line was present (`snapshot.rs:293-295`). |
| `counts` | object | Yes | `{staged, unstaged, untracked, conflicted}` integers from `count_status_entries` (`snapshot.rs:326-343`). |
| `entries` | array | Yes | Per-entry records `{index_status, worktree_status, path, original_path}`. `index_status` and `worktree_status` are 1-character strings; `original_path` is `null` unless the entry was a rename (`snapshot.rs:186-194,318-323`). |
| `status` | object | Yes | Command summary for the `git status` invocation (`snapshot.rs:418-429`). |
| `unstaged_diff` | object \| null | Yes | Command summary for `git diff --stat`, or `null` when `include_diff_stats=false` (`snapshot.rs:196-197`). |
| `staged_diff` | object \| null | Yes | Command summary for `git diff --cached --stat`, or `null` when `include_diff_stats=false` (`snapshot.rs:196-197`). |

Each command summary contains `args` (full prefixed argv), `stdout`, `stderr`, `exit_code` (nullable), `success`, `timed_out`, `truncated_stdout`, `truncated_stderr`.

**Tool-level error (`isError: true`):**

Three distinct error shapes can appear:

- Argument parse / validation failures use `ToolCallOutcome::err` (`tools-mcp-core/src/tool_outcome.rs:35`) — plain `{content, isError: true}`.
- Infrastructure failures from `run_git` use `ToolCallOutcome::err` with text `"git error: <chain>"` (`snapshot.rs:103`).
- `git status` or `git diff --stat` non-zero exits use `ToolCallOutcome::ok` wrapping a custom payload with `isError: true`, `working_dir`, and command summaries (`snapshot.rs:107-113,151-158`). Although built with `ToolCallOutcome::ok`, the embedded `isError: true` flag still marks the call as failed in the MCP envelope.

The handler MUST NOT panic; all failure paths return a `ToolCallOutcome` (`snapshot.rs:80-83,103,131-141`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with one of the hints from `parse_args` (`tool_outcome.rs:61-75`). |
| `paths` provided but all entries whitespace | `true` | `"paths must include at least one non-empty path"` (`snapshot.rs:96-99,210-213`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` or `"git error: working_dir must reference an existing directory: ..."` (`path_policy.rs:46-55,163-174`; surfaced via `snapshot.rs:103`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| `git status` non-zero exit | `true` | First non-empty of stderr / stdout (`first_non_empty`, `snapshot.rs:107,409`). |
| `git status` exceeds `timeout_ms` and refuses to die | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git diff --stat` infrastructure failure | `true` | `"git diff --stat error: ..."` (`snapshot.rs:131-133`). |
| `git diff --cached --stat` infrastructure failure | `true` | `"git diff --cached --stat error: ..."` (`snapshot.rs:134-141`). |
| `git diff --stat` or cached variant non-zero exit | `true` | First non-empty of failing variant's stderr / stdout (`snapshot.rs:145-159`). |

## 7. Security Considerations

- **Registration gate.** All git tools, including `git_snapshot`, are off by default and only register when `MCP_ENABLE_GIT=true` (`lib.rs:7-10`). This MUST NOT be weakened without operator consent.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes the supplied path, requires it exist as a directory, and confines it under the process startup directory; symlinks are followed and re-validated, parent-escapes (`..`) are rejected, and resolved paths outside the authority return a structured error (`path_policy.rs:40-185`). Locked in by `allows_current_working_dir`, `rejects_parent_working_dir`, and `working_dir_resolution_returns_canonical_symlink_target` (`path_policy.rs:206-277`).
- **Command-injection resistance.** Git arguments are passed as a `Vec<String>` to `tokio::process::Command::args` (`mod.rs:181-182`); no shell interpolation occurs. The safety prefix `--no-pager -c color.ui=false -c diff.external= -c core.fsmonitor=` runs ahead of every subcommand to disable pagers, color, and external diff/fsmonitor helpers (`mod.rs:82-99`).
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1` disables `/etc/gitconfig`; `GIT_CONFIG_GLOBAL` is redirected to the platform null sink to ignore the user's home `.gitconfig`; `GIT_EXTERNAL_DIFF=""` disables any inherited helper; the authority denylist and dynamic `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` scrub prevent caller-controlled config from steering this tool (`mod.rs:184-198,294-320`). Tests: `git_authority_env_denylist_includes_repository_and_helper_controls` and `git_config_spoofing_env_key_matches_indexed_key_value_patterns` (`mod.rs:329-369`).
- **Read-only operation.** Snapshot only invokes `git status` and `git diff --stat`; it cannot mutate refs, the index, or the worktree.
- **Bounded output.** Stdout is capped at 200 KB and stderr at 100 KB by default; truncation is reported via `truncated_stdout` / `truncated_stderr` per command summary, so callers can detect when the response is incomplete (`mod.rs:164-167,246-251`).
- **Locale-stable parsing.** Porcelain v1 output is locale-stable, so `clean` and `counts` do not depend on the user's locale or translated git messages.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate. Only the literal value `"true"` registers the git tool family, including `git_snapshot` (`tools-mcp-git/src/lib.rs:7-10`). |

`TOOLS_PRETTY_JSON` does not affect this tool's response because the handler builds the JSON payload directly via `ToolCallOutcome::ok`, not through `ok_json_content`.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 8-25 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 259 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/snapshot.rs` | 64 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/snapshot.rs` | 65-78 |
| Status arg construction | `tools-mcp-git/src/git/handlers/snapshot.rs` | 220-231 |
| Diff-stat arg construction | `tools-mcp-git/src/git/handlers/snapshot.rs` | 233-245 |
| Porcelain parser | `tools-mcp-git/src/git/handlers/snapshot.rs` | 290-324 |
| Counts derivation | `tools-mcp-git/src/git/handlers/snapshot.rs` | 326-343 |
| Clean fast-path skip of diffs | `tools-mcp-git/src/git/handlers/snapshot.rs` | 120-143 |
| Concurrent diff invocation | `tools-mcp-git/src/git/handlers/snapshot.rs` | 127-142 |
| Text renderer | `tools-mcp-git/src/git/handlers/snapshot.rs` | 345-380 |
| Response composition | `tools-mcp-git/src/git/handlers/snapshot.rs` | 174-199 |
| Command summary fields | `tools-mcp-git/src/git/handlers/snapshot.rs` | 418-429 |
| `run_git` core executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Safety prefix args | `tools-mcp-git/src/git/mod.rs` | 82-99 |
| Authority env scrub | `tools-mcp-git/src/git/mod.rs` | 68-80,294-320 |
| `GIT_CONFIG_NOSYSTEM` / `GIT_CONFIG_GLOBAL` | `tools-mcp-git/src/git/mod.rs` | 184-192 |
| Timeout + kill + grace | `tools-mcp-git/src/git/mod.rs` | 220-235 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |
| Config constants (timeouts, byte caps) | `tools-mcp-core/src/config.rs` | 4-16 |

## 10. Examples

### 10.1 Minimal request (clean worktree)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "git_snapshot",
    "arguments": {}
  }
}
```

### 10.2 Success response (clean worktree, default options)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "branch: main\nclean: true\nstatus:\n  <clean>\n"}],
    "isError": false,
    "working_dir": "C:/Users/Daniel/tools-mcp",
    "clean": true,
    "branch": "main",
    "counts": {"staged": 0, "unstaged": 0, "untracked": 0, "conflicted": 0},
    "entries": [],
    "status": {"args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","status","--porcelain=1","-b"], "stdout": "## main\n", "stderr": "", "exit_code": 0, "success": true, "timed_out": false, "truncated_stdout": false, "truncated_stderr": false},
    "unstaged_diff": null,
    "staged_diff": null
  }
}
```

### 10.3 With diff stats and pathspec filter

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "git_snapshot",
    "arguments": {
      "working_dir": "/repo",
      "include_diff_stats": true,
      "untracked": false,
      "paths": ["src/"]
    }
  }
}
```

### 10.4 Working-directory rejection

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "git error: working_dir must resolve inside the server working directory (C:\\Users\\Daniel\\tools-mcp): C:\\ resolves outside the permitted authority"}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `parse_porcelain_status_extracts_branch_and_entries` | `tools-mcp-git/src/git/handlers/snapshot.rs:441` | Branch header strip, rename arrow split, staged/unstaged/untracked counts. |
| `render_snapshot_text_includes_empty_diff_sections` | `tools-mcp-git/src/git/handlers/snapshot.rs:463` | Empty diff blocks render `  <none>`. |
| `build_diff_stat_args_preserves_pathspec_and_cached_order` | `tools-mcp-git/src/git/handlers/snapshot.rs:473` | Diff arg ordering, pathspec separator. |
| `empty_diff_stat_exec_matches_diff_command_summary_contract` | `tools-mcp-git/src/git/handlers/snapshot.rs:492` | Synthesized clean-path `GitExecResult` matches command-summary shape. |
| `porcelain_status_summary_ignores_branch_headers` | `tools-mcp-git/src/git/types.rs:180` | Branch-only output is considered clean. |
| `porcelain_status_summary_counts_status_entries` | `tools-mcp-git/src/git/types.rs:186` | Non-empty porcelain → not clean. |
| `git_authority_env_denylist_includes_repository_and_helper_controls` | `tools-mcp-git/src/git/mod.rs:330` | Authority env denylist completeness. |
| `git_config_spoofing_env_key_matches_indexed_key_value_patterns` | `tools-mcp-git/src/git/mod.rs:349` | `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` scrubbing. |
| `build_git_args_preserves_standard_safety_prefix` | `tools-mcp-git/src/git/mod.rs:372` | Safety prefix is prepended on every invocation. |
| `allows_current_working_dir` | `tools-mcp-git/src/git/path_policy.rs:207` | Server cwd is in scope. |
| `rejects_parent_working_dir` | `tools-mcp-git/src/git/path_policy.rs:214` | Parent of server cwd is rejected. |
| `working_dir_resolution_returns_canonical_symlink_target` | `tools-mcp-git/src/git/path_policy.rs:262` | Symlink working dir resolves to canonical target inside authority. |
| `test_tools_list` (registers when env set) | `tools-mcp-server/tests/integration_test.rs:77` | Confirms `git_snapshot` appears in the tool list when the harness sets `MCP_ENABLE_GIT=true`. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | Confirms `git_snapshot` and all `Git*` tools are absent when `MCP_ENABLE_GIT` is removed. |
| `test_git_snapshot_tool_call_if_git_installed` | `tools-mcp-server/tests/integration_test.rs:809` | End-to-end snapshot invocation against an initialized repo. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `MCP_ENABLE_GIT` accept truthy synonyms like `1` or `yes`? | No. The gate compares against the literal string `"true"`; any other value (including `"1"`, `"yes"`, `"True"`) leaves the tool unregistered (`tools-mcp-git/src/lib.rs:7-10`). |
| 2 | Why is the diff-stat path skipped on a clean worktree? | `git diff --stat` against an empty changeset still incurs index walks. The clean fast-path elides both diff invocations when the porcelain output proves the worktree is clean and not truncated (`snapshot.rs:120-126`). |
| 3 | Are the per-command summaries equivalent to the response shape of `GitStatus` / `GitDiff`? | No. `git_snapshot` returns a bespoke summary with a fixed key set (`snapshot.rs:418-429`) that omits `git_bin` and includes only what the snapshot view needs. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` constructors, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2, §8). |
| `tools-mcp-core/src/process.rs` | `read_to_end_limited` shape (§6.2 step 4). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default and tools/list assertions (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
