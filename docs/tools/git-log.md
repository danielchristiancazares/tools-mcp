# SDD: GitLog

**Date:** 2026-05-24
**Scope:** Design contract for the `GitLog` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/inspect.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitLog` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitLog` is a read-only MCP tool that invokes `git log` and returns the captured output plus the standard git envelope. It exposes the most common filter knobs (`max_count`, `oneline`, `format`, `author`, `since`, `until`, `grep`, `path`) and clamps stdout via `max_bytes`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_log` (`tools-mcp-git/src/git/handlers/inspect.rs:50`). It is registered via `GitLogTool` (`tools-mcp-git/src/tools.rs:128-151`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Showing the contents of a single commit (see `docs/tools/git-show.md`).
- Per-line authorship (see `docs/tools/git-blame.md`).
- Diff text (see `docs/tools/git-diff.md`).
- Branch enumeration (see `docs/tools/git-branch.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitLog` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_log` (`tools-mcp-git/src/git/handlers/inspect.rs:50`) |
| Schema definition | `tools-mcp-git/src/tools.rs:128-151` |
| Registration call | `tools-mcp-git/src/tools.rs:265` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Path goes after `--`** — When `path` is supplied, the args list MUST insert `--` before the path so a value beginning with `-` is treated as a positional pathspec (`tools-mcp-git/src/git/handlers/inspect.rs:110-113`).
- **Filter values are prefixed with their option name** — `author`, `since`, `until`, and `grep` are emitted as `--author=<value>`, `--since=<value>`, `--until=<value>`, `--grep=<value>` (one argument each), so a value containing `=` or whitespace does not split into multiple args (`inspect.rs:98-109`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms by `run_git`; stdout capture MUST honor `max_bytes` clamped to `[1, MAX_OUTPUT_BYTES = 5_000_000]` via `clamp_bytes` (`tools-mcp-core/src/validation.rs:36-38`; `inspect.rs:84-85`).
- **Empty-success placeholder text** — When `git log` succeeds but stdout is whitespace-only (e.g., filters matched no commits), `content[0].text` MUST be `"no commits"` (`inspect.rs:23-39,128`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT enable color output, pagers, or external diff helpers.
- MUST NOT mutate the worktree, index, refs, or stash stack.

## 5. Design Goals

- **Common filters, structured cap.** The schema covers the queries agentic callers want (per-author, since/until, message grep, single-path history) without exposing the full `git log` surface, so behavior is predictable.
- **Bounded stdout.** History can be arbitrarily long; the default 200 KB cap with a 5 MB ceiling lets callers ask for "everything" without flooding the model context.
- **Locale-stable placeholder.** Returning `"no commits"` rather than empty stdout gives downstream consumers a deterministic empty-result signal.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `max_count` | integer | No | unset | Schema `>= 1`; rust type `u32` (`inspect.rs:59`) | Appends `-<n>` to limit commits shown (`inspect.rs:89-91`). |
| `oneline` | boolean | No | `false` | — | Appends `--oneline` (`inspect.rs:92-94`). |
| `format` | string | No | unset | Forwarded verbatim as `--format=<value>` (`inspect.rs:95-97`) | Pretty-print format string. |
| `author` | string | No | unset | Forwarded verbatim as `--author=<value>` (`inspect.rs:98-100`) | Author filter. |
| `since` | string | No | unset | Forwarded verbatim as `--since=<value>` (`inspect.rs:101-103`) | Date filter (`2024-01-01`, `2 weeks ago`, etc.). |
| `until` | string | No | unset | Forwarded verbatim as `--until=<value>` (`inspect.rs:104-106`) | Date filter (upper bound). |
| `grep` | string | No | unset | Forwarded verbatim as `--grep=<value>` (`inspect.rs:107-109`) | Commit message pattern filter. |
| `path` | string | No | unset | Inserted after `--` (`inspect.rs:110-113`) | Path filter (restrict log to commits touching this path). |
| `max_bytes` | integer | No | `200000` | Schema `>= 1`, `<= 5_000_000`; clamped via `clamp_bytes` (`validation.rs:36-38`) | Stdout capture cap. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:148`); the deserializer sets `#[serde(deny_unknown_fields)]` (`inspect.rs:52`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:128-151`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitLogRequest` via `ToolCallOutcome::parse_args` (`inspect.rs:78-81`). On failure return `isError: true`.
2. **Resolve timeout + clamp `max_bytes`** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)`; `max_bytes = clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES)` (`inspect.rs:83-85`).
3. **Build args** — Start with `["log"]`. Conditionally append in this order:
   - `-{max_count}` if `max_count` is `Some` (`inspect.rs:89-91`).
   - `--oneline` if `oneline=true` (`inspect.rs:92-94`).
   - `--format=<value>` if `format` is `Some` (`inspect.rs:95-97`).
   - `--author=<value>` if `author` is `Some` (`inspect.rs:98-100`).
   - `--since=<value>` if `since` is `Some` (`inspect.rs:101-103`).
   - `--until=<value>` if `until` is `Some` (`inspect.rs:104-106`).
   - `--grep=<value>` if `grep` is `Some` (`inspect.rs:107-109`).
   - `--`, then `<path>` if `path` is `Some` (`inspect.rs:110-113`).
4. **Run git** — Call `run_git` with the clamped `max_bytes` for stdout and `DEFAULT_GIT_STDERR_BYTES` for stderr (`inspect.rs:115-126`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`inspect.rs:125`).
5. **Derive response text** — `inspect_output_text(&exec, Some("no commits"))`: on success, return trimmed stdout, falling back to `"no commits"` when stdout is whitespace; on failure, prefer trimmed stderr, falling back to trimmed stdout (`inspect.rs:23-39,128`).
6. **Compose response** — `build_git_response` with the standard envelope plus the extra field `max_bytes` (`inspect.rs:41-45,129-130`; `types.rs:100-142`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "abc1234 feat(core): add feature\ndef5678 fix(api): handle null"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","log","-10","--oneline"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "abc1234 feat(core): add feature\ndef5678 fix(api): handle null\n",
  "stderr": "",
  "max_bytes": 200000
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed log output or `"no commits"` (`inspect.rs:128`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `max_bytes` | integer | Yes | Effective clamped stdout cap. |

**Tool-level error (`isError: true`):**

- **Argument parse errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`inspect.rs:125`).
- **`git log` non-zero exit** (e.g., not a git repo, unknown ref in path) uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `inspect.rs:125`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git log` non-zero exit (e.g., not a repo) | `true` | Trimmed stderr, e.g. `"fatal: not a git repository"` (`inspect.rs:31-38`). |

## 7. Security Considerations

- **Registration gate.** `GitLog` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Read-only operation.** `git log` does not mutate refs, the index, or the worktree.
- **Pathspec injection defense.** `path` is always preceded by `--` (`inspect.rs:110-113`), so a value starting with `-` is treated as a positional pathspec.
- **Filter-value injection defense.** Each filter (`author`, `since`, `until`, `grep`) is emitted as a single `--option=value` argument, so embedded `=` or whitespace cannot split into multiple options.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** Authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""` (`mod.rs:184-198,294-320`).
- **Bounded output.** Stdout capped at the smaller of `max_bytes` (max 5 MB) and the clamp range; stderr at 100 KB. Truncation surfaces in `truncated_stdout` / `truncated_stderr`.
- **Untrusted commit content.** Commit subject lines, author names, and `--grep` matches surface user-controlled data. Consumers MUST treat `content[0].text` as untrusted input.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 128-151 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 265 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/inspect.rs` | 50 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/inspect.rs` | 51-76 |
| `inspect_output_text` (empty-success placeholder) | `tools-mcp-git/src/git/handlers/inspect.rs` | 23-39, 128 |
| Filter arg construction | `tools-mcp-git/src/git/handlers/inspect.rs` | 89-113 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Minimal request (full default log)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitLog",
    "arguments": {}
  }
}
```

### 10.2 Filtered call

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitLog",
    "arguments": {
      "working_dir": "/repo",
      "max_count": 5,
      "oneline": true,
      "author": "alice@example.com",
      "since": "2 weeks ago",
      "grep": "fix(",
      "path": "src/"
    }
  }
}
```

### 10.3 Success response (oneline)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "abc1234 feat(core): add feature\ndef5678 fix(api): handle null"}],
    "isError": false,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","log","-10","--oneline"],
    "working_dir": "/repo",
    "exit_code": 0,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": "abc1234 feat(core): add feature\ndef5678 fix(api): handle null\n",
    "stderr": "",
    "max_bytes": 200000
  }
}
```

### 10.4 No-match empty result

When filters match nothing, `content[0].text` is `"no commits"`:

```json
{
  "result": {
    "content": [{"type": "text", "text": "no commits"}],
    "isError": false,
    "stdout": "",
    "stderr": "",
    "max_bytes": 200000
  }
}
```

### 10.5 Not-a-repo error

```json
{
  "result": {
    "content": [{"type": "text", "text": "fatal: not a git repository (or any of the parent directories): .git"}],
    "isError": true,
    "exit_code": 128,
    "stderr": "fatal: not a git repository (or any of the parent directories): .git\n",
    "max_bytes": 200000
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `inspect_output_text_preserves_success_stdout_trimming` | `tools-mcp-git/src/git/handlers/inspect.rs:302` | Trimmed stdout on success. |
| `inspect_output_text_uses_empty_success_text_when_configured` | `tools-mcp-git/src/git/handlers/inspect.rs:309` | `"no commits"` placeholder when stdout is whitespace. |
| `inspect_output_text_prefers_failure_stderr_when_present` | `tools-mcp-git/src/git/handlers/inspect.rs:317` | Failure text prefers stderr. |
| `inspect_output_text_falls_back_to_failure_stdout` | `tools-mcp-git/src/git/handlers/inspect.rs:327` | Failure text falls back to stdout when stderr is empty. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitLog` absent without `MCP_ENABLE_GIT=true`. |

`GitLog` is not asserted in `test_tools_list` by name, but `test_git_tools_disabled_by_default` proves it is among the gated `Git*` tools by verifying that *all* `Git*`-prefixed names disappear when the env var is removed (`tools-mcp-server/tests/integration_test.rs:1294-1300`).

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | What happens if the caller supplies a `format` that conflicts with `oneline`? | git itself decides precedence; the last-effective option wins per git's rules. The handler emits both if both are set; no override is performed at the handler layer. |
| 2 | Can the caller pass multiple paths? | No. The schema accepts a single `path` string (`tools-mcp-git/src/tools.rs:144`). Callers needing multi-path log history MUST issue separate calls or use `GitDiff` for change inspection. |
| 3 | Does the tool support `--all`, `--graph`, `--decorate`, or `--reverse`? | No. Only the schema-listed filters are exposed (`tools-mcp-git/src/tools.rs:134-145`). Callers needing those flags MUST use a different surface. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `clamp_bytes` (§6.2 step 2). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
