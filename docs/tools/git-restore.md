# SDD: GitRestore

**Date:** 2026-05-24
**Scope:** Design contract for the `GitRestore` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitRestore` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitRestore` is a **destructive** MCP tool that invokes `git restore` to discard uncommitted changes. It accepts an explicit non-empty list of paths and the operator's choice of `--staged` and/or `--worktree` targets. The default target is the working tree (`worktree=true`, `staged=false`), which matches `git restore <paths>`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_restore` (`tools-mcp-git/src/git/handlers/mutating.rs:37`). It is registered via `GitRestoreTool` (`tools-mcp-git/src/tools.rs:71-88`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

For reversing selected staged hunks without restoring whole files, use `GitHunks staged=true` followed by `GitStageHunks action="unstage"`.

### 3.2 Explicitly Out of Scope

- Staging changes (see `docs/tools/git-add.md`).
- Branch switching or path checkout from a different ref (see `docs/tools/git-checkout.md`).
- Stash-based recovery (see `docs/tools/git-stash.md`).
- Commit creation (see `docs/tools/git-commit.md`).
- Diff inspection (see `docs/tools/git-diff.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitRestore` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_restore` (`tools-mcp-git/src/git/handlers/mutating.rs:37`) |
| Schema definition | `tools-mcp-git/src/tools.rs:71-88` |
| Registration call | `tools-mcp-git/src/tools.rs:262` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Non-empty paths required** — After filtering whitespace-only entries, `paths` MUST contain at least one entry. An empty effective list MUST return `"paths must be non-empty"` *before* spawning git (`tools-mcp-git/src/git/handlers/mutating.rs:57-60,12-17`). Locked in by `git_restore_handler_rejects_whitespace_only_paths` (`mutating.rs:645`).
- **At least one target** — `staged=false` AND `worktree=false` MUST return `"at least one of staged/worktree must be true"` *before* spawning git (`mutating.rs:65-67`).
- **Default target is worktree** — When neither `staged` nor `worktree` is supplied, `staged=false` and `worktree=true` apply (`mutating.rs:62-63`). This matches `git restore <paths>`.
- **Pathspec separator** — The git invocation MUST include `--` immediately before the paths to prevent any path that begins with `-` from being parsed as an option (`mutating.rs:80-81`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`; `tools-mcp-core/src/config.rs:4-13`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome` (`mutating.rs:52-54,93,107-110`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT spawn `git restore` when the effective paths list is empty (the validation gate MUST run first).
- MUST NOT spawn `git restore` when both `staged` and `worktree` are `false`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths that resolve outside the server cwd.
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **Explicit and bounded destruction.** Requiring a non-empty `paths` array prevents accidentally restoring the entire worktree.
- **Fail-loud target selection.** The `staged=false, worktree=false` combination is rejected outright rather than silently doing nothing.
- **Stable default for agentic callers.** Default `worktree=true` matches the most common "throw away my uncommitted edits in these files" intent.
- **Read-modify pattern stays distinct.** `GitRestore` only invokes `git restore`; it does not perform stage selection (`GitAdd`), branch switching (`GitCheckout`), or stash operations (`GitStash`).

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `paths` | string array | Yes | — | After trimming, MUST contain at least one entry (`mutating.rs:57-60,12-17`) | Paths to restore. Passed after `--`. |
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `staged` | boolean | No | `false` | — | Appends `--staged` (`mutating.rs:73-75`). |
| `worktree` | boolean | No | `true` | — | Appends `--worktree` (`mutating.rs:76-78`). |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:85`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:39`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:71-88`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitRestoreRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:52-55`). On failure return `isError: true`.
2. **Normalize paths** — `non_empty_paths` filters whitespace-only entries (`mutating.rs:12-17,57`). If the filtered list is empty, return `"paths must be non-empty"` (`mutating.rs:58-60`).
3. **Validate target selection** — `staged = req.staged.unwrap_or(false)`; `worktree = req.worktree.unwrap_or(true)`. If both are `false`, return `"at least one of staged/worktree must be true"` (`mutating.rs:62-67`).
4. **Resolve timeout** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)` (`mutating.rs:69`).
5. **Build args** — Start with `["restore"]`; append `"--staged"` if `staged`; append `"--worktree"` if `worktree`; append `"--"`; append each filtered path (`mutating.rs:71-81`).
6. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:83-94`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:93`).
7. **Derive response text** — On success: `"ok"` when both stdout and stderr are whitespace-only; trimmed stderr when stdout is whitespace; trimmed stdout otherwise. On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:96-108`).
8. **Compose response** — `build_git_response(&exec, &text, None)` returns the standard git envelope: `content`, `isError = !exec.success`, plus `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` (`mutating.rs:110-111`; `types.rs:100-142`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "ok"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","restore","--worktree","--","src/lib.rs"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "",
  "stderr": ""
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"ok"` on silent success, otherwise the trimmed git output (`mutating.rs:96-108`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:93`).
- **`git restore` non-zero exit** uses the standard `build_git_response` envelope with `isError: true`; `content[0].text` prefers stderr (`types.rs:128-142`).

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Missing `paths` field | `true` | `"invalid arguments: ..."` with the "Required fields are missing" hint (`tool_outcome.rs:61-75`). |
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint. |
| `paths` provided but all entries whitespace | `true` | `"paths must be non-empty"` (`mutating.rs:59`). |
| `staged=false`, `worktree=false` | `true` | `"at least one of staged/worktree must be true"` (`mutating.rs:66`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:93`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git restore` non-zero exit (e.g., path not in repo) | `true` | Trimmed stderr, e.g. `"error: pathspec 'missing.txt' did not match any file(s) known to git"` (`mutating.rs:104-108`; `types.rs:67-74`). |

## 7. Security Considerations

- **Registration gate.** `GitRestore` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Destructive operation.** `git restore --worktree` overwrites uncommitted changes in the named files; `git restore --staged` un-stages without modifying the worktree. Callers MUST treat this tool as a write boundary. The handler enforces explicit paths and explicit target selection so a single empty/`true`-for-everything call cannot wipe the worktree.
- **Pathspec injection defense.** `--` is always inserted before the pathspecs (`mutating.rs:80-81`), so a path beginning with `-` is treated as a positional argument, not as an option.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory under the server cwd (`path_policy.rs:40-185`).
- **Command-injection resistance.** Git arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation occurs. The safety prefix forces `--no-pager`, `color.ui=false`, `diff.external=`, `core.fsmonitor=`.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to `NUL`/`/dev/null`, `GIT_EXTERNAL_DIFF=""`, and the authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub prevent caller-controlled config from steering the operation (`mod.rs:184-198,294-320`).
- **Bounded output.** Stdout capped at 200 KB and stderr at 100 KB; truncation surfaces in the response.
- **No commit message handling.** This tool does not take a message; there is no commit-message injection surface here. See `docs/tools/git-commit.md` for the commit handler that does.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 71-88 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 262 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 37 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 38-50 |
| Non-empty paths filter | `tools-mcp-git/src/git/handlers/mutating.rs` | 12-17, 57-60 |
| Target validation | `tools-mcp-git/src/git/handlers/mutating.rs` | 62-67 |
| Arg construction with `--` | `tools-mcp-git/src/git/handlers/mutating.rs` | 71-81 |
| Response text selection | `tools-mcp-git/src/git/handlers/mutating.rs` | 96-108 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitRestore",
    "arguments": {"paths": ["src/lib.rs"]}
  }
}
```

### 10.2 Success response (worktree restore)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "ok"}],
    "isError": false,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","restore","--worktree","--","src/lib.rs"],
    "working_dir": "/repo",
    "exit_code": 0,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": "",
    "stderr": ""
  }
}
```

### 10.3 Unstage only

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitRestore",
    "arguments": {
      "paths": ["src/lib.rs"],
      "staged": true,
      "worktree": false
    }
  }
}
```

### 10.4 Empty-paths rejection (no git call)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "paths must be non-empty"}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_restore_rejects_whitespace_only_paths` | `tools-mcp-git/src/git/handlers/mutating.rs:611` | Whitespace-only paths are filtered out. |
| `git_restore_handler_rejects_whitespace_only_paths` | `tools-mcp-git/src/git/handlers/mutating.rs:645` | Handler returns `paths must be non-empty` without spawning git. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitRestore` absent without `MCP_ENABLE_GIT=true`. |
| `test_tools_list` | `tools-mcp-server/tests/integration_test.rs:113` | `GitRestore` present when registered. |

No dedicated integration test exercises a successful restore end-to-end; coverage relies on the destructive default's pre-spawn validation gates and the shared `run_git` envelope tests (`types.rs:179-222`).

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Why default `worktree=true` rather than match git's CLI default (which is also worktree)? | Both match. The explicit default keeps the schema self-documenting and avoids relying on git's CLI defaults across versions. |
| 2 | Could a malicious caller pass `paths=["-rf", "/"]`? | No. The handler always inserts `--` before the paths (`mutating.rs:80-81`), so values starting with `-` are positional pathspecs to git, not options. |
| 3 | Does the tool refuse `paths=[".", "*"]`? | No. The handler does not interpret pathspecs; git itself decides whether the path matches. Operators who want to forbid wildcard restores should restrict at the harness or policy layer. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
