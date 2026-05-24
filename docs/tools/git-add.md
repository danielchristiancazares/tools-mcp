# SDD: GitAdd

**Date:** 2026-05-24
**Scope:** Design contract for the `GitAdd` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitAdd` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitAdd` is a state-mutating MCP tool that invokes `git add` to stage paths for the next commit. It supports stage-by-path (`paths`), stage-all (`all → -A`), and stage-modified-only (`update → -u`) modes. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_add` (`tools-mcp-git/src/git/handlers/mutating.rs:118`). It is registered via `GitAddTool` (`tools-mcp-git/src/tools.rs:90-107`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Discarding changes (see `docs/tools/git-restore.md`).
- Committing staged changes (see `docs/tools/git-commit.md`).
- Inspection of what is staged (see `docs/tools/git-status.md`, `docs/tools/git-diff.md`).
- Branch switching or checkout (see `docs/tools/git-checkout.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitAdd` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_add` (`tools-mcp-git/src/git/handlers/mutating.rs:118`) |
| Schema definition | `tools-mcp-git/src/tools.rs:90-107` |
| Registration call | `tools-mcp-git/src/tools.rs:263` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **At least one operand** — When `all=false` AND `update=false` AND `paths` is empty (after whitespace filtering), the handler MUST return `"paths required unless 'all' or 'update' is true"` *before* spawning git (`tools-mcp-git/src/git/handlers/mutating.rs:143-145`). Locked in by `git_add_rejects_whitespace_only_paths` (`mutating.rs:657`).
- **`all` overrides `update`** — When both `all=true` and `update=true` are supplied, the handler MUST emit `-A` and not `-u` (`mutating.rs:152-156`). The flags are mutually exclusive in effect.
- **Pathspec separator** — When `paths` is non-empty, `--` MUST precede the path list to ensure paths beginning with `-` are treated as positional arguments (`mutating.rs:158-161`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome` (`mutating.rs:134-137,143-145,173`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT spawn `git add` when no operand has been resolved (no paths, no `-A`, no `-u`).
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **Three orthogonal stage selectors.** Map directly to git's `<paths>`, `-A`, and `-u` so callers can express any common staging intent.
- **No silent no-op.** Forcing at least one operand prevents callers from issuing `git add` with no effect.
- **Pre-spawn validation.** Cheap validation runs before git is invoked, surfacing operator errors without process overhead.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `paths` | string array | No | `[]` | After trimming, MUST contain at least one entry unless `all=true` or `update=true` (`mutating.rs:141-145`) | Files to stage. Passed after `--`. |
| `all` | boolean | No | `false` | — | When `true`, appends `-A` (`mutating.rs:152-153`). |
| `update` | boolean | No | `false` | Ignored when `all=true` | When `true`, appends `-u` (`mutating.rs:154-156`). |
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:104`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:120`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:90-107`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitAddRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:134-137`). On failure return `isError: true`.
2. **Normalize operands** — `use_all = req.all.unwrap_or(false)`; `use_update = req.update.unwrap_or(false)`; `paths = non_empty_paths(req.paths.unwrap_or_default())` filters whitespace-only entries (`mutating.rs:12-17,139-141`).
3. **Validate operand presence** — When `!use_all && !use_update && paths.is_empty()`, return `"paths required unless 'all' or 'update' is true"` (`mutating.rs:143-145`).
4. **Resolve timeout** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)` (`mutating.rs:147`).
5. **Build args** — Start with `["add"]`; append `"-A"` if `use_all` (precedence over `update`); else `"-u"` if `use_update`; if `paths` is non-empty, append `"--"` then each path (`mutating.rs:149-161`).
6. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:163-174`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:173`).
7. **Derive response text** — On success: `"ok"` (note: success text is fixed regardless of git output). On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:176-182`).
8. **Compose response** — `build_git_response(&exec, &text, None)` returns the standard git envelope (`mutating.rs:184-185`; `types.rs:100-142`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "ok"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","add","--","src/lib.rs"],
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
| `content[0].text` | string | Yes | `"ok"` on success (fixed); trimmed stderr/stdout on failure (`mutating.rs:176-182`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:173`).
- **`git add` non-zero exit** uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| No operand resolved (`all=false`, `update=false`, paths empty/whitespace) | `true` | `"paths required unless 'all' or 'update' is true"` (`mutating.rs:144`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:173`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git add` non-zero exit (e.g., pathspec didn't match) | `true` | Trimmed stderr, e.g. `"fatal: pathspec 'missing.txt' did not match any files"` (`mutating.rs:178-182`). |

## 7. Security Considerations

- **Registration gate.** `GitAdd` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Destructive scope.** `git add` mutates the index; it does not modify the worktree or refs. Combined with `GitCommit`, it can persist new commits, so the gate is significant.
- **Pathspec injection defense.** `--` always precedes the path list when paths are present, so a path starting with `-` is positional (`mutating.rs:158-161`).
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Git arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation occurs. The safety prefix forces `--no-pager`, `color.ui=false`, `diff.external=`, `core.fsmonitor=`.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrubbing (`mod.rs:184-198,294-320`).
- **Bounded output.** 200 KB stdout cap, 100 KB stderr cap.
- **`.gitignore` boundary.** `git add` respects the repository's `.gitignore` rules (without `-f`). This tool does not expose `-f`, so callers cannot bypass repository ignore policy through `GitAdd`.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 90-107 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 263 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 118 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 119-132 |
| Non-empty paths filter | `tools-mcp-git/src/git/handlers/mutating.rs` | 12-17, 141 |
| Operand validation | `tools-mcp-git/src/git/handlers/mutating.rs` | 143-145 |
| `-A` over `-u` precedence | `tools-mcp-git/src/git/handlers/mutating.rs` | 152-156 |
| Pathspec separator | `tools-mcp-git/src/git/handlers/mutating.rs` | 158-161 |
| Response text (`"ok"` on success) | `tools-mcp-git/src/git/handlers/mutating.rs` | 176-182 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Minimal request (stage one file)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitAdd",
    "arguments": {"paths": ["src/lib.rs"]}
  }
}
```

### 10.2 Stage all (`-A`)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitAdd",
    "arguments": {"all": true}
  }
}
```

### 10.3 Stage only updates to tracked files (`-u`)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitAdd",
    "arguments": {"update": true}
  }
}
```

### 10.4 Validation rejection (no operands)

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "paths required unless 'all' or 'update' is true"}],
    "isError": true
  }
}
```

### 10.5 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "ok"}],
    "isError": false,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","add","--","src/lib.rs"],
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

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_add_rejects_whitespace_only_paths` | `tools-mcp-git/src/git/handlers/mutating.rs:657` | Whitespace-only paths trigger the "paths required" error without spawning git. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitAdd` absent without `MCP_ENABLE_GIT=true`. |
| `test_tools_list` | `tools-mcp-server/tests/integration_test.rs:114` | `GitAdd` present when registered. |

No dedicated integration test exercises a successful add end-to-end; coverage relies on the pre-spawn validation gates and the shared `run_git` envelope tests.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | What happens when both `all=true` and `update=true` are set? | `-A` wins; `-u` is not emitted (`mutating.rs:152-156`). |
| 2 | Does the handler expose `--force` (`-f`) to override `.gitignore`? | No. The schema does not include such a field; callers cannot bypass ignore policy through `GitAdd`. |
| 3 | Does the success text reflect git's actual stdout? | No. Success always returns `"ok"`; the raw `stdout` and `stderr` are still available on the structured response envelope for callers that want them (`mutating.rs:176-182`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
