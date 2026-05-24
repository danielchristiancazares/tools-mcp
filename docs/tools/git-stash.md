# SDD: GitStash

**Date:** 2026-05-24
**Scope:** Design contract for the `GitStash` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitStash` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitStash` is an MCP tool that wraps `git stash` and dispatches on an `action` enum: `push` (default), `save` (alias for `push`), `pop`, `apply`, `drop`, `list`, `show`, `clear`. Index-based actions accept a numeric stash `index` rendered as `stash@{N}`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_stash` (`tools-mcp-git/src/git/handlers/mutating.rs:465`). It is registered via `GitStashTool` (`tools-mcp-git/src/tools.rs:196-214`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Worktree status / diff inspection (see `docs/tools/git-status.md`, `docs/tools/git-diff.md`).
- Branch switching (see `docs/tools/git-checkout.md`).
- Discarding changes via `git restore` (see `docs/tools/git-restore.md`).
- Commit creation (see `docs/tools/git-commit.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitStash` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_stash` (`tools-mcp-git/src/git/handlers/mutating.rs:465`) |
| Schema definition | `tools-mcp-git/src/tools.rs:196-214` |
| Registration call | `tools-mcp-git/src/tools.rs:268` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Action default and empty handling** — An absent OR empty-string `action` MUST default to `"push"` (`tools-mcp-git/src/git/handlers/mutating.rs:489-493`). Locked in by `git_stash_empty_string_action_defaults_to_push` (`mutating.rs:591`).
- **Action enum is closed** — Any value not in `{push, save, pop, apply, drop, list, show, clear}` MUST return the error `"unknown stash action '<action>'. Valid: push, pop, apply, drop, list, show, clear"` *before* spawning git (`mutating.rs:540-545`).
- **`save` is an alias for `push`** — `action="save"` MUST be treated identically to `action="push"` (`mutating.rs:499`).
- **Index argument shape** — `index=N` MUST be rendered as the literal string `stash@{N}` and appended as a single argv element (`mutating.rs:511-513,517-519,523-525,533-535`).
- **`show` always emits `-p`** — Show action MUST append `-p` to produce patch output rather than the default stat output (`mutating.rs:530-532`).
- **`u32` index** — `index` is deserialized as `u32`; negative values are rejected by serde, and the schema sets `"minimum": 0` (`tools-mcp-git/src/tools.rs:207`; `mutating.rs:478`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT spawn `git stash` for an unknown action (the dispatcher's unknown branch MUST run first).
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **Closed action enum.** Constraining `action` to a known set prevents the handler from forwarding arbitrary subcommands to `git stash`.
- **Predictable index encoding.** Rendering `stash@{N}` once at the handler keeps the wire format stable regardless of platform shell quoting rules.
- **`show -p` default.** Patch output is the most useful default for agents inspecting a stash; the stat-only view is implicit in `list`.
- **Locale-stable empty list.** Returning `"no stashes"` for a clean stash stack gives downstream consumers a deterministic empty-result signal.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `action` | string | No | `"push"` | enum: `push`, `save`, `pop`, `apply`, `drop`, `list`, `show`, `clear`. Empty / absent → `"push"` (`mutating.rs:489-493`). | Stash action; `save` is an alias for `push`. |
| `message` | string | No | unset | `push`/`save` only | Stash message (`-m <message>`) (`mutating.rs:504-507`). |
| `index` | integer | No | unset | Schema `>= 0`; rust type `u32`. Used by `pop`/`apply`/`drop`/`show`. | Stash index (rendered as `stash@{N}`) (`mutating.rs:511-535`). |
| `include_untracked` | boolean | No | `false` | `push`/`save` only | Appends `-u` (`mutating.rs:501-503`). |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:211`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:467`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:196-214`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitStashRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:483-486`). On failure return `isError: true`.
2. **Resolve action** — `action = req.action.as_deref().filter(|s| !s.is_empty()).unwrap_or("push")` (`mutating.rs:489-493`).
3. **Initialize args** — `cmd_args = vec!["stash".into()]` (`mutating.rs:495-496`).
4. **Dispatch on action** (`mutating.rs:498-545`):
   - **`push` | `save`** — Push `"push"`; append `-u` if `include_untracked=true`; append `-m <message>` if `message.is_some()`.
   - **`pop`** — Push `"pop"`; if `index.is_some()`, append `stash@{N}`.
   - **`apply`** — Push `"apply"`; if `index.is_some()`, append `stash@{N}`.
   - **`drop`** — Push `"drop"`; if `index.is_some()`, append `stash@{N}`.
   - **`list`** — Push `"list"`.
   - **`show`** — Push `"show"`, `"-p"`; if `index.is_some()`, append `stash@{N}`.
   - **`clear`** — Push `"clear"`.
   - **default** — Return `"unknown stash action '<action>'. Valid: push, pop, apply, drop, list, show, clear"`.
5. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:547-558`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:557`).
6. **Derive response text** — On success: if stdout is whitespace-only, return `"no stashes"` when `action="list"` else `"ok"`; otherwise trimmed stdout. On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:560-573`).
7. **Compose response** — `build_git_response` with the standard envelope plus extra field `action: <effective_action>` (`mutating.rs:575-579`; `types.rs:100-142`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "Saved working directory and index state WIP on main: abc1234 feat(core): add feature"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","stash","push","-u","-m","wip: refactor"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "Saved working directory and index state WIP on main: abc1234 feat(core): add feature\n",
  "stderr": "",
  "action": "push"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed git output, or `"no stashes"` / `"ok"` placeholder (`mutating.rs:560-573`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `action` | string | Yes | Effective action (`"push"` when omitted/empty; otherwise the supplied string for known actions) (`mutating.rs:576`). |

**Tool-level error (`isError: true`):**

- **Argument parse errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **Unknown `action`** uses `ToolCallOutcome::err` with the validator message (`mutating.rs:540-545`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:557`).
- **`git stash` non-zero exit** uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| Unknown `action` value | `true` | `"unknown stash action '<action>'. Valid: push, pop, apply, drop, list, show, clear"` (`mutating.rs:541`). |
| Negative `index` | `true` | `"invalid arguments: ..."` (deserialization rejection on `u32`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:557`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git stash` non-zero exit (e.g., `pop` with conflicts, `show` of out-of-range index) | `true` | Trimmed stderr (`mutating.rs:569-573`). |

## 7. Security Considerations

- **Registration gate.** `GitStash` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Destructive scope.** `clear` deletes every stash entry; `drop` removes the targeted entry; `pop` removes the entry after applying it. `apply`, `push`, `save`, `show`, `list` do not destroy refs but may mutate the worktree (`pop`, `apply`, `push`).
- **Closed action enum.** The dispatcher rejects unknown actions before spawning git, so a caller cannot smuggle subcommands like `git stash branch ...` through this surface (`mutating.rs:540-545`).
- **Index encoding.** `stash@{N}` is built from a `u32`, so no shell metacharacters can appear inside the rendered reference (`mutating.rs:511-535`).
- **Message field.** `message` is forwarded verbatim as `-m <message>`. Unlike `GitCommit`, `git stash push -m` accepts multi-line messages; the handler does not sanitize newlines because stash messages do not have Conventional Commit trailer semantics. Untrusted message content should still be treated as untrusted text in any downstream rendering.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **Bounded output.** 200 KB stdout cap, 100 KB stderr cap.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 196-214 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 268 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 465 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 466-481 |
| Action default (`""` and `None` → `"push"`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 489-493 |
| Action dispatch | `tools-mcp-git/src/git/handlers/mutating.rs` | 498-545 |
| Unknown-action rejection | `tools-mcp-git/src/git/handlers/mutating.rs` | 540-545 |
| `show -p` default | `tools-mcp-git/src/git/handlers/mutating.rs` | 530-532 |
| `stash@{N}` formatting | `tools-mcp-git/src/git/handlers/mutating.rs` | 511-535 |
| Response text builder | `tools-mcp-git/src/git/handlers/mutating.rs` | 560-573 |
| Standard envelope + `action` extra | `tools-mcp-git/src/git/handlers/mutating.rs` | 575-579 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Push (default action)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStash",
    "arguments": {"message": "wip: refactor", "include_untracked": true}
  }
}
```

### 10.2 List

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStash",
    "arguments": {"action": "list"}
  }
}
```

Empty-stack response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "no stashes"}],
    "isError": false,
    "stdout": "",
    "stderr": "",
    "action": "list"
  }
}
```

### 10.3 Pop a specific stash

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStash",
    "arguments": {"action": "pop", "index": 0}
  }
}
```

Effective argv (after the safety prefix): `["stash", "pop", "stash@{0}"]` (`mutating.rs:509-513`).

### 10.4 Show with patch

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStash",
    "arguments": {"action": "show", "index": 1}
  }
}
```

Effective argv: `["stash", "show", "-p", "stash@{1}"]` (`mutating.rs:530-535`).

### 10.5 Unknown-action rejection

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "content": [{"type": "text", "text": "unknown stash action 'branch'. Valid: push, pop, apply, drop, list, show, clear"}],
    "isError": true
  }
}
```

### 10.6 Success response (push)

```json
{
  "result": {
    "content": [{"type": "text", "text": "Saved working directory and index state WIP on main: abc1234 feat(core): add feature"}],
    "isError": false,
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","stash","push","-u","-m","wip: refactor"],
    "exit_code": 0,
    "stdout": "Saved working directory and index state WIP on main: abc1234 feat(core): add feature\n",
    "stderr": "",
    "action": "push"
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_stash_empty_string_action_defaults_to_push` | `tools-mcp-git/src/git/handlers/mutating.rs:591` | Empty string and `None` both default to `"push"`. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitStash` absent without `MCP_ENABLE_GIT=true`. |

No targeted integration test exercises a stash round-trip end-to-end; coverage rests on the action-default unit test and the shared `run_git` envelope tests. This is a coverage gap recorded here for future work.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Why is `save` accepted alongside `push`? | `git stash save` is a legacy alias retained for muscle-memory compatibility; the handler treats them identically (`mutating.rs:499`). The forwarded git subcommand is always `push`. |
| 2 | Can the caller pass paths to `git stash push`? | No. The schema does not include a `paths` field, so the handler issues `git stash push` without pathspecs. Callers needing partial stashes MUST use a different surface or the worktree. |
| 3 | Why does `show` always emit `-p`? | Patch output is the most useful default for agentic inspection; the stat-only view is implicit when the caller wants a summary they can use `list` (`mutating.rs:530-532`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
