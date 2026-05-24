# SDD: GitShow

**Date:** 2026-05-24
**Scope:** Design contract for the `GitShow` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/inspect.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitShow` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitShow` is a read-only MCP tool that invokes `git show` to display commit contents and (by default) the unified diff. It supports optional `--stat`, `--name-only`, and custom `--format=<value>` flags, defaults the revision to `HEAD` when `commit` is unset, and clamps stdout via `max_bytes`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_show` (`tools-mcp-git/src/git/handlers/inspect.rs:136`). It is registered via `GitShowTool` (`tools-mcp-git/src/tools.rs:216-235`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Commit list browsing (see `docs/tools/git-log.md`).
- Per-line authorship (see `docs/tools/git-blame.md`).
- Working-tree or ref-to-ref diffs (see `docs/tools/git-diff.md`).
- Mutating operations.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitShow` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_show` (`tools-mcp-git/src/git/handlers/inspect.rs:136`) |
| Schema definition | `tools-mcp-git/src/tools.rs:216-235` |
| Registration call | `tools-mcp-git/src/tools.rs:269` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Option-injection defense on `commit`** — When `commit` is supplied, the handler MUST reject whitespace-only values and values starting with `-` *before* spawning git, via `validate_non_option_arg` (`tools-mcp-git/src/git/handlers/inspect.rs:13-21,176-179`). Locked in by `git_show_rejects_option_like_commit` (`inspect.rs:334`).
- **`--end-of-options` before the commit** — When `commit` is supplied, the args list MUST insert `--end-of-options` immediately before the rev so the rev is treated as a positional even if the caller bypasses the `-`-prefix check (`inspect.rs:180-181`).
- **`HEAD` default** — When `commit` is absent, `git show` runs without a positional rev and defaults to `HEAD` (the args list contains only `["show", ...flags]`) (`inspect.rs:165,176-182`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms by `run_git`; stdout capture MUST honor `max_bytes` clamped to `[1, MAX_OUTPUT_BYTES = 5_000_000]` via `clamp_bytes` (`tools-mcp-core/src/validation.rs:36-38`; `inspect.rs:162-163`).
- **No empty-success placeholder** — Unlike `GitLog`, the handler passes `None` for the empty-success text; an empty success stdout surfaces as the empty string (`inspect.rs:23-39,197`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT accept `commit` values starting with `-`.
- MUST NOT enable color output, pagers, or external diff helpers.
- MUST NOT mutate the worktree, index, refs, or stash stack.

## 5. Design Goals

- **Commit-anchored inspection.** A single tool to read commit metadata and changes, complementing `GitLog` (listing) and `GitBlame` (authorship per line).
- **Defense in depth on the rev.** Both the `-`-prefix rejection and `--end-of-options` defend the positional argument; if one is bypassed, the other still applies.
- **Predictable stdout cap.** The 200 KB default lets `show HEAD` succeed for typical commits; the 5 MB ceiling accommodates large refactors.
- **Customization without surface bloat.** `--format=<value>` covers pretty-print needs; the tool does not duplicate every git format flag.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `commit` | string | No | `HEAD` (git's default) | Non-whitespace, MUST NOT start with `-` (`inspect.rs:176-179`) | Commit / rev to show. |
| `stat` | boolean | No | `false` | — | Appends `--stat` (`inspect.rs:167-169`). |
| `name_only` | boolean | No | `false` | — | Appends `--name-only` (`inspect.rs:170-172`). |
| `format` | string | No | unset | Forwarded verbatim as `--format=<value>` (`inspect.rs:173-175`) | Pretty-print format string. |
| `max_bytes` | integer | No | `200000` | Schema `>= 1`, `<= 5_000_000`; clamped via `clamp_bytes` (`validation.rs:36-38`) | Stdout capture cap. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:232`); the deserializer sets `#[serde(deny_unknown_fields)]` (`inspect.rs:138`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:216-235`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitShowRequest` via `ToolCallOutcome::parse_args` (`inspect.rs:156-159`). On failure return `isError: true`.
2. **Resolve timeout + clamp `max_bytes`** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)`; `max_bytes = clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES)` (`inspect.rs:161-163`).
3. **Build args** — Start with `["show"]`. Conditionally append:
   - `--stat` if `stat=true` (`inspect.rs:167-169`).
   - `--name-only` if `name_only=true` (`inspect.rs:170-172`).
   - `--format=<value>` if `format.is_some()` (`inspect.rs:173-175`).
   - If `commit.is_some()`: validate via `validate_non_option_arg("commit")`; append `--end-of-options`, then the rev (`inspect.rs:176-182`).
4. **Run git** — Call `run_git` with the clamped `max_bytes` for stdout and `DEFAULT_GIT_STDERR_BYTES` for stderr (`inspect.rs:184-195`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`inspect.rs:194`).
5. **Derive response text** — `inspect_output_text(&exec, None)`: on success, return trimmed stdout (empty string when stdout is whitespace, because `None` was passed for the placeholder); on failure, prefer trimmed stderr, falling back to trimmed stdout (`inspect.rs:23-39,197`).
6. **Compose response** — `build_git_response` with the standard envelope plus the extra field `max_bytes` (`inspect.rs:41-45,198-199`; `types.rs:100-142`).

`validate_non_option_arg` (`inspect.rs:13-21`) calls `validation::validate_non_empty` (returning the `<field> is required (non-empty string)` error) and additionally rejects any value starting with `-` with `<field> must not start with '-'`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "commit abc1234...\nAuthor: Test User <test@example.com>\nDate:   ...\n\n    feat(core): add feature\n\ndiff --git a/src/lib.rs b/src/lib.rs\n..."}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","show"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "commit abc1234...\n...",
  "stderr": "",
  "max_bytes": 200000
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed git output, or stderr-preferred fallback on failure (`inspect.rs:23-39,197`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `max_bytes` | integer | Yes | Effective clamped stdout cap. |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`inspect.rs:194`).
- **`git show` non-zero exit** (e.g., unknown rev) uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`). |
| Whitespace-only `commit` | `true` | `"commit is required (non-empty string)"` (`validation.rs:11-22`). |
| Option-like `commit` (starts with `-`) | `true` | `"commit must not start with '-'"` (`inspect.rs:15-19`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `inspect.rs:194`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git show` non-zero exit (e.g., unknown rev) | `true` | Trimmed stderr, e.g. `"fatal: ambiguous argument 'abc': unknown revision or path not in the working tree."` (`inspect.rs:31-38`). |

## 7. Security Considerations

- **Registration gate.** `GitShow` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Read-only operation.** `git show` does not mutate refs, the index, or the worktree.
- **Option-injection defense on `commit`.** `validate_non_option_arg` rejects values starting with `-` before git sees them; `--end-of-options` further hardens against option misinterpretation when the rev is supplied (`inspect.rs:13-21,176-182`). Locked in by `git_show_rejects_option_like_commit` (`inspect.rs:334`).
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **External helpers disabled.** The `git` safety prefix sets `diff.external=`; combined with `GIT_EXTERNAL_DIFF=""`, an attacker-controlled config cannot route show's diff through an executable.
- **Bounded output.** Stdout capped at the smaller of `max_bytes` (max 5 MB) and the clamp range; stderr at 100 KB. Truncation surfaces in the response.
- **Untrusted commit content.** Commit subjects, bodies, author identities, and diff payloads surface user-controlled data. Consumers MUST treat `content[0].text` as untrusted input.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 216-235 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 269 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/inspect.rs` | 136 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/inspect.rs` | 137-154 |
| `validate_non_option_arg` | `tools-mcp-git/src/git/handlers/inspect.rs` | 13-21 |
| `--end-of-options` before commit | `tools-mcp-git/src/git/handlers/inspect.rs` | 176-182 |
| `inspect_output_text` (no empty placeholder) | `tools-mcp-git/src/git/handlers/inspect.rs` | 23-39, 197 |
| `max_bytes` extra field | `tools-mcp-git/src/git/handlers/inspect.rs` | 41-45, 198-199 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Show HEAD (default)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitShow",
    "arguments": {}
  }
}
```

### 10.2 Show a specific commit with stat only

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitShow",
    "arguments": {"commit": "abc1234", "stat": true}
  }
}
```

Effective argv (after the safety prefix): `["show", "--stat", "--end-of-options", "abc1234"]` (`inspect.rs:167-182`).

### 10.3 Custom format

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitShow",
    "arguments": {"format": "%H %s %an"}
  }
}
```

### 10.4 Option-injection rejection

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "commit must not start with '-'"}],
    "isError": true
  }
}
```

### 10.5 Success response

```json
{
  "result": {
    "content": [{"type": "text", "text": "commit abc1234...\nAuthor: Test User <test@example.com>\nDate:   ...\n\n    feat(core): add feature\n\ndiff --git a/src/lib.rs b/src/lib.rs\n..."}],
    "isError": false,
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","show","--end-of-options","HEAD"],
    "exit_code": 0,
    "stdout": "commit abc1234...\n",
    "stderr": "",
    "max_bytes": 200000
  }
}
```

### 10.6 Unknown-rev error

```json
{
  "result": {
    "content": [{"type": "text", "text": "fatal: ambiguous argument 'abc': unknown revision or path not in the working tree."}],
    "isError": true,
    "exit_code": 128,
    "stderr": "fatal: ambiguous argument 'abc'...\n",
    "max_bytes": 200000
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_show_rejects_option_like_commit` | `tools-mcp-git/src/git/handlers/inspect.rs:334` | Option-like `commit` value is refused before spawning git. |
| `inspect_output_text_preserves_success_stdout_trimming` | `tools-mcp-git/src/git/handlers/inspect.rs:302` | Trimmed stdout on success. |
| `inspect_output_text_uses_empty_success_text_when_configured` | `tools-mcp-git/src/git/handlers/inspect.rs:309` | Empty success stdout → empty string (since `None` placeholder is passed). |
| `inspect_output_text_prefers_failure_stderr_when_present` | `tools-mcp-git/src/git/handlers/inspect.rs:317` | Failure text prefers stderr. |
| `inspect_output_text_falls_back_to_failure_stdout` | `tools-mcp-git/src/git/handlers/inspect.rs:327` | Failure text falls back to stdout when stderr is empty. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitShow` absent without `MCP_ENABLE_GIT=true`. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Why is `--end-of-options` emitted *after* the format/stat flags but *before* the rev? | The `--end-of-options` separator terminates option parsing, so anything after it is positional. Placing it after the flag block lets git see the flags as options but the rev as a positional — even if the caller manages to slip a `-`-prefixed rev past the validator, git would not interpret it as an option (`inspect.rs:176-182`). |
| 2 | Does the handler emit `--end-of-options` when `commit` is absent? | No. The separator is only emitted alongside the rev (`inspect.rs:176-182`). Without a rev, git's own default (HEAD) applies. |
| 3 | Does an empty success stdout return `"no commits"` like `GitLog`? | No. `handle_git_show` passes `None` to `inspect_output_text` (`inspect.rs:197`); empty stdout becomes the empty string. The behaviour intentionally differs from `GitLog`, which passes `Some("no commits")`. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_bytes` (§6.2 step 2 and §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
