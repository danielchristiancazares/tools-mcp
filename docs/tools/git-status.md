# SDD: GitStatus

**Date:** 2026-05-24
**Scope:** Design contract for the `GitStatus` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/status.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitStatus` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitStatus` is an MCP tool that invokes `git status` and returns the captured output plus a derived `clean` boolean. By default it requests porcelain v1 format with the branch header so the result is locale-stable and machine-parseable; the caller MAY opt out of porcelain mode and receive the human-readable output, in which case the handler runs a secondary `git status --porcelain=1` probe to compute `clean`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_status` (`tools-mcp-git/src/git/handlers/status.rs:104`). It is registered via `GitStatusTool` (`tools-mcp-git/src/tools.rs:27-44`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- The bundled triage view that adds counts and diff stats (see `docs/tools/git-snapshot.md`).
- Mutating index/worktree operations (see `docs/tools/git-add.md`, `docs/tools/git-restore.md`, `docs/tools/git-commit.md`).
- JSON-RPC framing and protocol routing (covered in `docs/protocol.md`).
- Tool-registry composition (covered in `docs/architecture.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitStatus` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_status` (`tools-mcp-git/src/git/handlers/status.rs:104`) |
| Schema definition | `tools-mcp-git/src/tools.rs:27-44` |
| Registration call | `tools-mcp-git/src/tools.rs:260` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to the literal string `"true"`, this tool MUST NOT appear in the registry (`tools-mcp-git/src/lib.rs:7-10`; locked in by `test_git_tools_disabled_by_default`, `tools-mcp-server/tests/integration_test.rs:1270`).
- **Working-directory authority** — When `working_dir` is provided, it MUST be canonicalized and confined under the server's startup working directory; resolution errors MUST surface as a `git error: ...` tool-level error (`tools-mcp-git/src/git/path_policy.rs:163-181`; surfaced via `tools-mcp-git/src/git/mod.rs:158-159`).
- **Argument-list invocation** — Git MUST be invoked through `tokio::process::Command::args` with the safety prefix `--no-pager -c color.ui=false -c diff.external= -c core.fsmonitor=` prepended; arguments MUST NOT pass through a shell (`tools-mcp-git/src/git/mod.rs:82-99,181-198`).
- **Authority env scrubbed** — Authority and helper environment variables, including all `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs, MUST be removed from the spawned child's environment (`tools-mcp-git/src/git/mod.rs:68-80,294-320`). `GIT_CONFIG_NOSYSTEM=1` MUST be set; `GIT_CONFIG_GLOBAL` MUST be redirected to `NUL`/`/dev/null` (`tools-mcp-git/src/git/mod.rs:184-192`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms by `run_git`; stdout capture MUST cap at `DEFAULT_GIT_STDOUT_BYTES = 200_000` bytes; stderr at `DEFAULT_GIT_STDERR_BYTES = 100_000` bytes (`tools-mcp-git/src/git/mod.rs:164-166`; `tools-mcp-core/src/config.rs:4-13`).
- **`clean` derivation is locale-stable** — When porcelain mode is enabled (default), `clean` MUST be computed from the porcelain output of the primary command. When porcelain mode is disabled, the handler MUST run a separate `git status --porcelain=1` probe to compute `clean`; the human-readable primary output MUST NOT be parsed for cleanliness (`tools-mcp-git/src/git/handlers/status.rs:55-72,141-151`).
- **Probe-failure fallback** — When the porcelain probe fails (timeout, infrastructure error, or non-zero exit), `clean` MUST default to `false` (`tools-mcp-git/src/git/handlers/status.rs:66-72`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome` (`tools-mcp-git/src/git/handlers/status.rs:120-122,128-139`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT parse localized human-readable `git status` output to determine `clean` (`tools-mcp-git/src/git/handlers/status.rs:248-258`).
- MUST NOT enable color output (the safety prefix forces `color.ui=false`).
- MUST NOT mutate the worktree, index, refs, or stash stack.

## 5. Design Goals

- **Locale-stable cleanliness.** Porcelain v1 is the only format whose XY status codes do not depend on the user's locale. By preferring porcelain mode and falling back to a porcelain probe in human mode, `clean` is reliable for agentic callers regardless of the host environment.
- **Pass-through stdout.** The text shown to the user is the trimmed git output (`git_response_text`, `tools-mcp-git/src/git/types.rs:67-74`), so operators reading the response see exactly what `git status` produced.
- **Fail-closed cleanliness.** When the porcelain probe fails, `clean` defaults to `false` so callers conservatively treat the worktree as dirty rather than acting on a false-positive clean state.
- **Read-only.** `GitStatus` runs only `git status` invocations and cannot mutate state.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` in `run_git` (`mod.rs:164`) | Per-command timeout in milliseconds. |
| `porcelain` | boolean | No | `true` | — | When `true`, appends `--porcelain=1` (`status.rs:33-44`). |
| `branch` | boolean | No | `true` | — | When `true` in porcelain mode, appends `-b` for the branch header (`status.rs:37-39`). Ignored in non-porcelain mode (`status.rs:228-236`). |
| `untracked` | boolean | No | `true` | — | When `false` in porcelain mode, appends `-uno` (`status.rs:40-42`). Ignored in non-porcelain mode. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:41`); the deserializer sets `#[serde(deny_unknown_fields)]` (`tools-mcp-git/src/git/handlers/status.rs:106`). Unknown fields produce a tool-level error with text `"invalid arguments: ..."` and the "Unknown fields are not allowed" hint (`tools-mcp-core/src/tool_outcome.rs:61-75`). Locked in by `test_unknown_fields_are_rejected_for_tool_requests` (`tools-mcp-server/tests/integration_test.rs:146`).

> Schema source: `tools-mcp-git/src/tools.rs:27-44`

### 6.2 Behavior

1. **Parse + validate arguments** — Deserialize into `GitStatusRequest` via `ToolCallOutcome::parse_args` (`status.rs:120-123`). On failure return `isError: true`.
2. **Resolve options** — `GitStatusOptions::from_optional` applies defaults `porcelain=true, branch=true, untracked=true` when each field is omitted (`status.rs:19-31,126`).
3. **Build status args** — `build_status_args` always starts with `["status"]`; in porcelain mode, appends `"--porcelain=1"`, then `"-b"` if `branch=true`, then `"-uno"` if `untracked=false`. In non-porcelain mode, no extra args are appended (`status.rs:33-45`).
4. **Run status** — Call `run_git` (`tools-mcp-git/src/git/mod.rs:151`). `run_git` resolves `working_dir`, clamps `timeout_ms` and byte limits, spawns `git`/`git.exe` with the safety prefix and authority scrub, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:158-235`). On infrastructure error (spawn fails, capture fails, timeout grace expires), return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`status.rs:136-139`).
5. **Compute `clean`**:
   - **On non-success** — `clean = false` (`status.rs:141-142`).
   - **In porcelain mode** — Parse the captured stdout with `PorcelainStatusSummary::parse_v1`; `clean` is `true` iff no non-branch entry lines exist (`status.rs:143-145`; `tools-mcp-git/src/git/types.rs:15-31`; `tools-mcp-git/src/git/handlers/status.rs:51-53`).
   - **In non-porcelain (human) mode** — Run a second `git status --porcelain=1` invocation via `probe_porcelain_status_clean` against the same canonicalized working directory; on success, set `clean` from the porcelain summary. On infrastructure error or non-zero exit, the probe returns `Err` and `clean` defaults to `false` via `unwrap_or(false)` (`status.rs:55-98,146-151`).
6. **Build response text** — `git_response_text` returns `exec.stdout` trimmed of trailing `\r`/`\n` when `exec.success` is true or `stderr` is whitespace-only; otherwise it returns `exec.stderr` trimmed (`tools-mcp-git/src/git/types.rs:67-74`).
7. **Compose response** — `build_git_response_with_extra_fields` wraps the trimmed text in the standard git envelope (`content`, `isError = !exec.success`, plus `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr`) and inserts `clean: <bool>` (`tools-mcp-git/src/git/types.rs:114-126,128-142`; `status.rs:154-155`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "## main\n M src/lib.rs\n?? scratch.md"}],
  "isError": false,
  "git_bin": "git.exe",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","status","--porcelain=1","-b"],
  "working_dir": "C:/Users/Daniel/tools-mcp",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "## main\n M src/lib.rs\n?? scratch.md\n",
  "stderr": "",
  "clean": false
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed status output (stdout on success / stderr-preferred fallback on failure) (`types.rs:67-74`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin` | string | Yes | `"git"` on Unix, `"git.exe"` on Windows (`mod.rs:286-292`). |
| `args` | string array | Yes | Full prefixed argument vector (safety prefix + subcommand args). |
| `working_dir` | string \| null | Yes | Canonicalized working directory or `null`. |
| `exit_code` | integer \| null | Yes | Process exit code; `null` on signal/timeout. |
| `timed_out` | boolean | Yes | `true` iff killed by timeout enforcement (`mod.rs:219-235`). |
| `truncated_stdout` | boolean | Yes | `true` iff stdout exceeded 200 KB. |
| `truncated_stderr` | boolean | Yes | `true` iff stderr exceeded 100 KB. |
| `stdout` | string | Yes | Raw captured stdout (UTF-8 lossy). |
| `stderr` | string | Yes | Raw captured stderr (UTF-8 lossy). |
| `clean` | boolean | Yes | `true` iff porcelain output contains no non-branch entries (`status.rs:51-72,141-151`). |

**Tool-level error (`isError: true`):**

Two flavors appear:

- **Argument parse / infrastructure errors** use `ToolCallOutcome::err`: `{content: [{type:"text", text:"<message>"}], isError: true}` (`tools-mcp-core/src/tool_outcome.rs:35`).
- **`git status` non-zero exit** uses the same envelope as success but with `isError: true`; all the structured fields above remain present and the `text` is taken from stderr when available (`types.rs:128-142`). `clean` is `false` in this case (`status.rs:141-142`).

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the parse-args hint (`tool_outcome.rs:61-75`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` or `"git error: working_dir must reference an existing directory: ..."` (`path_policy.rs:46-55,163-174`; surfaced via `status.rs:136-139`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| `git status` exceeds `timeout_ms` and refuses to die | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git status` non-zero exit (e.g., not a git repo) | `true` | First non-empty of trimmed stderr / stdout, e.g. `"fatal: not a git repository (or any of the parent directories): .git"` (`types.rs:67-74`). |

## 7. Security Considerations

- **Registration gate.** `GitStatus` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes the supplied path, requires it to be an existing directory, and confines it under the process startup directory (`path_policy.rs:40-185`). Symlinks resolve to their canonical target inside the authority (`working_dir_resolution_returns_canonical_symlink_target`, `path_policy.rs:262`).
- **Command-injection resistance.** Git arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation occurs. The safety prefix forces `--no-pager`, `color.ui=false`, and disables `diff.external` / `core.fsmonitor` helpers (`mod.rs:82-99`).
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null|NUL`, `GIT_EXTERNAL_DIFF=""`, and scrubbing of authority / spoofing config env vars prevent caller-controlled config from steering this tool (`mod.rs:184-198,294-320`).
- **Read-only operation.** Only `git status` invocations are issued; no writes occur.
- **Bounded output.** 200 KB stdout / 100 KB stderr caps; truncation is reported in `truncated_stdout` / `truncated_stderr`.
- **Locale-stable cleanliness.** Even when the operator chooses human-readable output for display, `clean` is computed from porcelain v1, so translations of "nothing to commit" or "modified" cannot fool the cleanliness check (tests `human_status_output_is_not_used_for_clean_detection` and `git_status_non_porcelain_clean_detection_is_locale_stable`, `status.rs:248-258,402-441`).

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 27-44 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 260 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/status.rs` | 104 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/status.rs` | 105-118 |
| Status arg builder | `tools-mcp-git/src/git/handlers/status.rs` | 33-45 |
| Porcelain probe (human-mode `clean`) | `tools-mcp-git/src/git/handlers/status.rs` | 47-98 |
| `clean` derivation | `tools-mcp-git/src/git/handlers/status.rs` | 141-151 |
| Response composition | `tools-mcp-git/src/git/handlers/status.rs` | 152-155 |
| Response envelope builder | `tools-mcp-git/src/git/types.rs` | 114-142 |
| Response text selection | `tools-mcp-git/src/git/types.rs` | 67-74 |
| Porcelain v1 summary | `tools-mcp-git/src/git/types.rs` | 10-31 |
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
    "name": "GitStatus",
    "arguments": {}
  }
}
```

### 10.2 Success response (clean worktree, default porcelain mode)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "## main"}],
    "isError": false,
    "git_bin": "git.exe",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","status","--porcelain=1","-b"],
    "working_dir": "C:/Users/Daniel/tools-mcp",
    "exit_code": 0,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": "## main\n",
    "stderr": "",
    "clean": true
  }
}
```

### 10.3 Human-readable mode with porcelain probe

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStatus",
    "arguments": {"porcelain": false}
  }
}
```

Behaviour: the primary call runs `git status` and returns its human-readable output in `content[0].text`; the handler internally runs `git status --porcelain=1` against the same working directory to derive `clean`. If the probe fails for any reason, `clean` falls back to `false` (`status.rs:55-98,146-151`).

### 10.4 Unknown-field error

```json
{
  "jsonrpc": "2.0",
  "id": 9,
  "result": {
    "content": [{"type": "text", "text": "invalid arguments: unknown field `bogus`, expected one of `working_dir`, `timeout_ms`, `porcelain`, `branch`, `untracked` at line ... Unknown fields are not allowed; check argument names against the tool schema."}],
    "isError": true
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `status_args_default_to_porcelain_with_branch_header` | `tools-mcp-git/src/git/handlers/status.rs:211` | Default arg construction. |
| `status_args_preserve_untracked_suppression_in_porcelain_mode` | `tools-mcp-git/src/git/handlers/status.rs:218` | `-uno` is appended when `untracked=false`. |
| `status_args_ignore_porcelain_only_flags_in_human_mode` | `tools-mcp-git/src/git/handlers/status.rs:229` | `branch`/`untracked` are no-ops when `porcelain=false`. |
| `porcelain_clean_ignores_branch_header` | `tools-mcp-git/src/git/handlers/status.rs:240` | Branch-only porcelain output = clean. |
| `human_status_output_is_not_used_for_clean_detection` | `tools-mcp-git/src/git/handlers/status.rs:248` | Human-mode `clean` requires the porcelain probe. |
| `non_porcelain_clean_probe_failure_falls_back_to_dirty` | `tools-mcp-git/src/git/handlers/status.rs:260` | Probe error → `clean = false`. |
| `git_status_default_branch_header_still_reports_clean` | `tools-mcp-git/src/git/handlers/status.rs:281` | End-to-end clean detection with default options. |
| `git_status_non_porcelain_reports_clean` | `tools-mcp-git/src/git/handlers/status.rs:321` | Human-mode clean via probe. |
| `git_status_non_porcelain_reports_dirty_from_porcelain_probe` | `tools-mcp-git/src/git/handlers/status.rs:362` | Human-mode dirty via probe. |
| `git_status_non_porcelain_clean_detection_is_locale_stable` | `tools-mcp-git/src/git/handlers/status.rs:402` | Primary output stays human-readable; probe is the truth source. |
| `git_status_reports_canonical_working_dir_for_symlinked_directory` | `tools-mcp-git/src/git/handlers/status.rs:445` | Symlink working dirs canonicalize. |
| `porcelain_status_summary_*` | `tools-mcp-git/src/git/types.rs:180,186` | Summary parser behaviour. |
| `test_unknown_fields_are_rejected_for_tool_requests` | `tools-mcp-server/tests/integration_test.rs:146` | Unknown-field rejection over the MCP wire. |
| `test_git_status_tool_call_if_git_installed` | `tools-mcp-server/tests/integration_test.rs:750` | End-to-end status invocation against an initialized repo. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | Confirms `GitStatus` is absent when `MCP_ENABLE_GIT` is removed. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Why run a second `git status --porcelain=1` invocation in human-readable mode? | Human-readable output is localized and unstable across git versions, so it cannot reliably yield a `clean` boolean. The probe gives callers locale-stable cleanliness without forcing them to drop human-readable presentation (`status.rs:55-98,248-258`). |
| 2 | Does an empty stderr override a successful stdout in `content[0].text`? | No. `git_response_text` returns stdout when `exec.success` is `true` or stderr is whitespace; stderr only wins on failure with non-empty content (`types.rs:67-74`). |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default, unknown-field, and end-to-end assertions (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
