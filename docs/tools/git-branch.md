# SDD: GitBranch

**Date:** 2026-05-24
**Scope:** Design contract for the `GitBranch` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitBranch` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitBranch` is an MCP tool that wraps `git branch` in five mutually exclusive modes selected by which argument is present:

- **list** (default) — `git branch -v`, optionally with `-a` (all) or `-r` (remote only).
- **create** — `git branch <name>` for a new branch (does not switch).
- **delete** — `git branch -d <name>` (refuses if not merged).
- **force_delete** — `git branch -D <name>`.
- **rename** — `git branch -m <old_name> <new_name>` (requires `new_name`).

The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_branch` (`tools-mcp-git/src/git/handlers/mutating.rs:269`). It is registered via `GitBranchTool` (`tools-mcp-git/src/tools.rs:153-174`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Switching branches or restoring paths (see `docs/tools/git-checkout.md`).
- Branch enumeration through `git status` headers (see `docs/tools/git-status.md`).
- Remote-tracking branch management beyond listing (the tool does not expose `--track`, `--set-upstream-to`, push/pull).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitBranch` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_branch` (`tools-mcp-git/src/git/handlers/mutating.rs:269`) |
| Schema definition | `tools-mcp-git/src/tools.rs:153-174` |
| Registration call | `tools-mcp-git/src/tools.rs:266` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Mode precedence is fixed** — The handler MUST evaluate modes in this order, taking the first that is present and ignoring the rest: `create`, `delete`, `force_delete`, `rename`, then list (`mutating.rs:302-340`).
- **Branch-name option-injection defense** — Every value forwarded as a positional branch identifier (`create`, `delete`, `force_delete`, `rename`, `new_name`) MUST be rejected when whitespace-only OR when it starts with `-`; the rejection MUST happen before spawning git (`mutating.rs:19-27,302-329`).
- **`rename` requires `new_name`** — Setting `rename` without `new_name` MUST return `"new_name required when renaming a branch"` (`mutating.rs:330-332`).
- **`list_all` overrides `list_remote`** — When both are `true` in list mode, the handler emits `-a` and not `-r` (`mutating.rs:333-339`).
- **List mode always passes `-v`** — The list-mode argv MUST include `-v` so the response carries the per-branch commit summary (`mutating.rs:339`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT accept branch identifiers that begin with `-` (those would be parsed as options by git).
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **One tool, five intents.** Collapsing the common branch verbs into a single tool keeps the catalog small while making each verb explicit.
- **Pre-spawn validation.** Branch-name shape checks run before git is invoked so the most common malformed inputs surface immediately and without process overhead.
- **Distinct delete and force_delete.** Splitting `-d` (refuse unmerged) and `-D` (force) into separate fields forces the caller to opt into destructiveness explicitly.
- **Verbose list by default.** `-v` adds per-branch commit summaries that help downstream tooling and humans alike.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `list_all` | boolean | No | `false` | List mode only | Appends `-a` (`mutating.rs:333-335`). Ignored when a mutating field is set. |
| `list_remote` | boolean | No | `false` | List mode only; superseded by `list_all` | Appends `-r` (`mutating.rs:335-338`). |
| `create` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:302-306`) | Create a new branch with this name (no switch). |
| `delete` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:307-312`) | Delete branch (refuses if not merged); emits `-d <name>`. |
| `force_delete` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:313-318`) | Force-delete branch; emits `-D <name>`. |
| `rename` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:319-323`) | Old name when renaming. Requires `new_name`. |
| `new_name` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`mutating.rs:325-329`). Required when `rename` is set. | New name when renaming. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:171`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:271`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:153-174`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitBranchRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:293-296`). On failure return `isError: true`.
2. **Resolve timeout + initialize args** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)`; `cmd_args = vec!["branch".into()]` (`mutating.rs:298-300`).
3. **Dispatch on mode** (`mutating.rs:302-340`):
   - **Create** — `req.create` is `Some(name)`: validate via `validate_non_option_arg("create")`; push `name`.
   - **Delete** — Else `req.delete` is `Some(name)`: validate; push `-d`, `name`.
   - **Force delete** — Else `req.force_delete` is `Some(name)`: validate; push `-D`, `name`.
   - **Rename** — Else `req.rename` is `Some(old)`: validate `old` (field name `"rename"`); push `-m`, `old`. Then require `req.new_name = Some(new)`; validate `new` (field name `"new_name"`); push `new`. If `new_name` is absent, return `"new_name required when renaming a branch"`.
   - **List (else branch)** — Append `-a` if `list_all=true`, else `-r` if `list_remote=true`. Always append `-v`.
4. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:342-353`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:352`).
5. **Derive response text** — On success: trimmed stdout, or `"ok"` if stdout is whitespace-only. On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:355-365`).
6. **Compose response** — `build_git_response(&exec, &text, None)` returns the standard git envelope (`mutating.rs:367-368`; `types.rs:100-142`).

`validate_non_option_arg` (`mutating.rs:19-27`) is the shared guard: it calls `validation::validate_non_empty` (returning the `<field> is required (non-empty string)` error) and additionally rejects any value starting with `-` with `<field> must not start with '-'`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "* main                abc1234 feat(core): add feature\n  feature/foo def5678 work in progress"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","branch","-v"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "* main                abc1234 feat(core): add feature\n  feature/foo def5678 work in progress\n",
  "stderr": ""
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed branch listing, or `"ok"` when stdout was whitespace (e.g., successful create/delete/rename) (`mutating.rs:355-365`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:352`).
- **`git branch` non-zero exit** (e.g., branch already exists, branch not fully merged for `-d`) uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| Whitespace-only branch identifier (`create`/`delete`/`force_delete`/`rename`/`new_name`) | `true` | `"<field> is required (non-empty string)"` (`validation.rs:11-22`; `mutating.rs:19-21`). |
| Option-like branch identifier (starts with `-`) | `true` | `"<field> must not start with '-'"` (`mutating.rs:22-25`). |
| `rename` set without `new_name` | `true` | `"new_name required when renaming a branch"` (`mutating.rs:331`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:352`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git branch` non-zero exit (e.g., `-d` refusing unmerged branch, name collision) | `true` | Trimmed stderr (`mutating.rs:361-365`). |

## 7. Security Considerations

- **Registration gate.** `GitBranch` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Destructive scope.** `delete` and `force_delete` modify refs; `rename` modifies refs and HEAD when the current branch is renamed. `create` and the list-mode invocations do not mutate refs.
- **Option-injection defense.** All caller-supplied branch identifiers pass through `validate_non_option_arg`, which rejects values starting with `-` *before* git sees them (`mutating.rs:19-27,302-329`). This prevents `--detach`, `--track=evil`, etc., from being interpreted as options. (See `tests::git_checkout_rejects_option_like_branch` for the analogous test on `GitCheckout`; the same `validate_non_option_arg` helper is used here.)
- **Force-delete is opt-in by field name.** Splitting `-d` and `-D` into distinct fields means a caller cannot accidentally escalate to a destructive force-delete through a flag toggle.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **Bounded output.** 200 KB stdout cap, 100 KB stderr cap.
- **Remote operations not exposed.** This tool does not invoke `git push`, `git fetch`, or `git pull`, so it cannot publish branch changes to a remote. Operators wanting remote sync MUST use a different surface.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 153-174 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 266 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 269 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 270-291 |
| Mode dispatch | `tools-mcp-git/src/git/handlers/mutating.rs` | 302-340 |
| `validate_non_option_arg` | `tools-mcp-git/src/git/handlers/mutating.rs` | 19-27 |
| Rename `new_name` enforcement | `tools-mcp-git/src/git/handlers/mutating.rs` | 325-332 |
| List-mode `-v` always emitted | `tools-mcp-git/src/git/handlers/mutating.rs` | 339 |
| Response text builder | `tools-mcp-git/src/git/handlers/mutating.rs` | 355-365 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 List all branches

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBranch",
    "arguments": {"list_all": true}
  }
}
```

### 10.2 Create a branch

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBranch",
    "arguments": {"create": "feature/new-thing"}
  }
}
```

### 10.3 Delete a branch (must be merged)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBranch",
    "arguments": {"delete": "feature/done"}
  }
}
```

### 10.4 Force-delete a branch

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBranch",
    "arguments": {"force_delete": "feature/abandoned"}
  }
}
```

### 10.5 Rename a branch

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBranch",
    "arguments": {"rename": "old-name", "new_name": "new-name"}
  }
}
```

### 10.6 Option-injection rejected

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "result": {
    "content": [{"type": "text", "text": "delete must not start with '-'"}],
    "isError": true
  }
}
```

### 10.7 List success

```json
{
  "result": {
    "content": [{"type": "text", "text": "* main         abc1234 feat(core): add feature\n  feature/foo def5678 work in progress"}],
    "isError": false,
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","branch","-v"],
    "exit_code": 0,
    "stdout": "* main         abc1234 feat(core): add feature\n  feature/foo def5678 work in progress\n",
    "stderr": ""
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_checkout_rejects_option_like_branch` | `tools-mcp-git/src/git/handlers/mutating.rs:681` | Exercises the shared `validate_non_option_arg` helper used by `GitBranch` for `create`/`delete`/`force_delete`/`rename`/`new_name`. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitBranch` absent without `MCP_ENABLE_GIT=true`. |

No targeted unit/integration test covers `GitBranch`'s mode-dispatch directly; coverage rests on the shared `validate_non_option_arg`, the `run_git` envelope tests, and the registration-gate integration test. This is a coverage gap recorded here for future work.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | What happens when multiple mutating fields are set (e.g., both `create` and `delete`)? | The first present field in the order `create > delete > force_delete > rename` wins; the others are ignored (`mutating.rs:302-340`). Future schemas MAY restrict this with a `oneOf`. |
| 2 | Does the tool ever fall through to list mode when a mutating field is set? | No. List mode only runs in the `else` branch of the dispatch (`mutating.rs:333-340`). |
| 3 | Why is `-v` always emitted in list mode? | Verbose output includes the per-branch commit summary that downstream tooling typically needs; the trade-off is a small bytes overhead per branch (`mutating.rs:339`). |

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
