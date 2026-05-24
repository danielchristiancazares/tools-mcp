# SDD: GitCheckout

**Date:** 2026-05-24
**Scope:** Design contract for the `GitCheckout` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitCheckout` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitCheckout` is a state-mutating MCP tool that wraps `git checkout` to switch branches, create+switch in one step, detach onto a commit, or restore named paths from `HEAD` (or from a supplied commit/branch). The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_checkout` (`tools-mcp-git/src/git/handlers/mutating.rs:374`). It is registered via `GitCheckoutTool` (`tools-mcp-git/src/tools.rs:176-194`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Listing or deleting branches (see `docs/tools/git-branch.md`).
- Discarding uncommitted changes via `git restore` (see `docs/tools/git-restore.md`).
- Stash-based recovery (see `docs/tools/git-stash.md`).
- Commit creation (see `docs/tools/git-commit.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitCheckout` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_checkout` (`tools-mcp-git/src/git/handlers/mutating.rs:374`) |
| Schema definition | `tools-mcp-git/src/tools.rs:176-194` |
| Registration call | `tools-mcp-git/src/tools.rs:267` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Ref precedence is fixed** — When multiple ref-like fields are supplied, the handler MUST evaluate them in this order, taking the first present and ignoring the rest: `create_branch` (`-b <name>`), then `branch` (`<name>`), then `commit` (`<rev>`) (`tools-mcp-git/src/git/handlers/mutating.rs:402-418`).
- **Option-injection defense on refs** — `create_branch`, `branch`, and `commit` MUST be rejected when whitespace-only OR when they start with `-`, before spawning git (`mutating.rs:19-27,402-418`). Locked in by `git_checkout_rejects_option_like_branch` (`mutating.rs:681`).
- **Pathspec separator** — When `paths` is non-empty, `--` MUST precede the path list (`mutating.rs:420-423`).
- **At least one operand** — When neither ref nor non-empty paths are supplied, the handler MUST return `"at least one of branch, create_branch, commit, or paths is required"` *before* spawning git (`mutating.rs:425-429`). Locked in by `git_checkout_rejects_whitespace_only_paths_without_ref` (`mutating.rs:669`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT accept refs that begin with `-` (those would be parsed as options by git).
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **One tool, four intents.** Branch switch, create+switch, detach onto commit, and path restore are all `git checkout` variants in practice; a single tool keeps the catalog small.
- **Strict precedence over a state machine.** A fixed `create_branch > branch > commit` order plus path filtering keeps the dispatch readable without a separate "mode" enum.
- **Pre-spawn rejection of empty calls.** Refusing the no-op `git checkout` invocation prevents accidental success-without-effect.
- **Option-injection hardening.** Branches and commits are positional in `git checkout`, so rejecting `-`-prefixed values before spawn is the simplest defense.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `branch` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:408-412`) | Switch to this branch (`git checkout <branch>`). |
| `create_branch` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:402-407`) | Create and switch to a new branch (`git checkout -b <name>`). Takes precedence over `branch` and `commit`. |
| `commit` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:413-417`) | Checkout a specific commit (detached HEAD). Used only when both `create_branch` and `branch` are absent. |
| `paths` | string array | No | `[]` | Whitespace-only entries are dropped (`mutating.rs:12-17,400`) | Restore these paths from HEAD (or from the supplied ref). |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:191`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:376`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:176-194`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitCheckoutRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:392-395`). On failure return `isError: true`.
2. **Initialize args + paths** — `cmd_args = vec!["checkout".into()]`; `paths = non_empty_paths(req.paths.unwrap_or_default())` (`mutating.rs:397-400`).
3. **Apply ref precedence** (`mutating.rs:402-418`):
   - **`create_branch`** — Validate with `validate_non_option_arg("create_branch")`; push `-b` then the name.
   - **Else `branch`** — Validate with `validate_non_option_arg("branch")`; push the name.
   - **Else `commit`** — Validate with `validate_non_option_arg("commit")`; push the rev (detached HEAD).
4. **Append paths** — If `paths` is non-empty, push `--` then each path (`mutating.rs:420-423`).
5. **Empty-call guard** — If `cmd_args.len() == 1` (still just `["checkout"]`), return `"at least one of branch, create_branch, commit, or paths is required"` (`mutating.rs:425-429`).
6. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:431-442`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:441`).
7. **Derive response text** — On success: `"ok"` if both stdout and stderr are whitespace; otherwise trimmed stderr if non-empty (note: `git checkout` typically prints branch-switch progress to stderr) else trimmed stdout. On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:444-456`).
8. **Compose response** — `build_git_response(&exec, &text, None)` returns the standard git envelope (`mutating.rs:458-459`; `types.rs:100-142`).

`validate_non_option_arg` (`mutating.rs:19-27`) calls `validation::validate_non_empty` (returning the `<field> is required (non-empty string)` error) and additionally rejects any value starting with `-` with `<field> must not start with '-'`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Switched to branch 'main'"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","checkout","main"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "",
  "stderr": "Switched to branch 'main'\n"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | `"ok"` on silent success; otherwise stderr-preferred fallback (because git checkout emits status on stderr) (`mutating.rs:444-456`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:441`).
- **`git checkout` non-zero exit** uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| Whitespace-only ref (`branch`/`create_branch`/`commit`) | `true` | `"<field> is required (non-empty string)"` (`validation.rs:11-22`). |
| Option-like ref (starts with `-`) | `true` | `"<field> must not start with '-'"` (`mutating.rs:22-25`). |
| No ref and no non-empty paths | `true` | `"at least one of branch, create_branch, commit, or paths is required"` (`mutating.rs:427`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:441`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git checkout` non-zero exit (e.g., dirty worktree blocking switch, unknown ref) | `true` | Trimmed stderr (`mutating.rs:452-456`). |

## 7. Security Considerations

- **Registration gate.** `GitCheckout` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Destructive scope.** `git checkout <branch>` moves `HEAD` and updates the worktree; `git checkout -b <name>` additionally creates a branch; `git checkout <commit>` enters detached HEAD; `git checkout -- <paths>` overwrites worktree paths from HEAD (similar to `git restore --worktree`). Treat this tool as a write boundary.
- **Option-injection defense.** Every ref (`create_branch`, `branch`, `commit`) is rejected when starting with `-` (`mutating.rs:19-27,402-418`). Locked in by `git_checkout_rejects_option_like_branch` (`mutating.rs:681`), which proves a value like `--detach` is refused before git sees it.
- **Pathspec injection defense.** `--` always precedes the path list when paths are present (`mutating.rs:420-423`).
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **Bounded output.** 200 KB stdout cap, 100 KB stderr cap.
- **Detached HEAD risk.** `commit` mode leaves the worktree in detached HEAD; subsequent commits would not advance any branch. The handler does not warn about this; callers should follow up with `GitStatus` to confirm the new HEAD state.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 176-194 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 267 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 374 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 375-390 |
| `validate_non_option_arg` | `tools-mcp-git/src/git/handlers/mutating.rs` | 19-27 |
| Ref precedence dispatch | `tools-mcp-git/src/git/handlers/mutating.rs` | 402-418 |
| Pathspec separator | `tools-mcp-git/src/git/handlers/mutating.rs` | 420-423 |
| Empty-call guard | `tools-mcp-git/src/git/handlers/mutating.rs` | 425-429 |
| Response text builder | `tools-mcp-git/src/git/handlers/mutating.rs` | 444-456 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Switch to an existing branch

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCheckout",
    "arguments": {"branch": "feature/foo"}
  }
}
```

### 10.2 Create and switch

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCheckout",
    "arguments": {"create_branch": "feature/bar"}
  }
}
```

### 10.3 Detach onto a commit

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCheckout",
    "arguments": {"commit": "abc1234"}
  }
}
```

### 10.4 Restore paths from HEAD

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCheckout",
    "arguments": {"paths": ["src/lib.rs"]}
  }
}
```

### 10.5 Restore paths from a specific branch

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCheckout",
    "arguments": {"branch": "main", "paths": ["src/lib.rs"]}
  }
}
```

### 10.6 Empty-call rejection

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "result": {
    "content": [{"type": "text", "text": "at least one of branch, create_branch, commit, or paths is required"}],
    "isError": true
  }
}
```

### 10.7 Option-injection rejection

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "result": {
    "content": [{"type": "text", "text": "branch must not start with '-'"}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_checkout_rejects_whitespace_only_paths_without_ref` | `tools-mcp-git/src/git/handlers/mutating.rs:669` | Empty-call guard (`"at least one ..."`). |
| `git_checkout_rejects_option_like_branch` | `tools-mcp-git/src/git/handlers/mutating.rs:681` | Option-like `branch` value is refused. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitCheckout` absent without `MCP_ENABLE_GIT=true`. |

No targeted integration test exercises a successful branch switch end-to-end; coverage rests on the validation tests and the shared `run_git` envelope tests.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | If both `create_branch` and `branch` are set, what wins? | `create_branch` (`-b`). The handler evaluates the precedence chain in order and ignores later fields once one is consumed (`mutating.rs:402-418`). |
| 2 | What if `create_branch` and `paths` are both set? | The handler emits `-b <name>` and then `-- <paths>`. git's behavior is to first create the branch and then restore the listed paths from the source ref. Whether that combination is desirable is a caller policy decision; the handler does not block it. |
| 3 | Does the tool refuse a checkout that would clobber uncommitted changes? | No. The handler does not pre-check the worktree; git's own safety refuses with a clear error that surfaces via the standard envelope (e.g., `"error: Your local changes to the following files would be overwritten by checkout"`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` (§6.2 step 3 and §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
