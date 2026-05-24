# SDD: GitBlame

**Date:** 2026-05-24
**Scope:** Design contract for the `GitBlame` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/inspect.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitBlame` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitBlame` is a read-only MCP tool that invokes `git blame` to annotate each line of a file with the commit, author, and timestamp that last modified it. It accepts a required `path`, optional `start_line`/`end_line` rendered as a `-L<start>,<end>` range, and an optional `commit` rev to blame at a specific revision. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_blame` (`tools-mcp-git/src/git/handlers/inspect.rs:205`). It is registered via `GitBlameTool` (`tools-mcp-git/src/tools.rs:237-256`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Commit history listing (see `docs/tools/git-log.md`).
- Showing commit contents (see `docs/tools/git-show.md`).
- Worktree-state inspection (see `docs/tools/git-status.md`, `docs/tools/git-diff.md`).
- Mutating operations.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitBlame` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_blame` (`tools-mcp-git/src/git/handlers/inspect.rs:205`) |
| Schema definition | `tools-mcp-git/src/tools.rs:237-256` |
| Registration call | `tools-mcp-git/src/tools.rs:270` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **`path` is required and non-empty** — The schema marks `path` as required (`tools-mcp-git/src/tools.rs:252`); the handler additionally calls `validation::validate_non_empty(&req.path, "path", None)` and returns `"path is required (non-empty string)"` on whitespace-only values *before* spawning git (`tools-mcp-git/src/git/handlers/inspect.rs:229-231`).
- **Option-injection defense on `commit`** — When `commit` is supplied, the handler MUST reject whitespace-only values and values starting with `-` *before* spawning git, via `validate_non_option_arg` (`inspect.rs:13-21,247-252`). Locked in by `git_blame_rejects_option_like_commit` (`inspect.rs:346`).
- **Pathspec separator** — `--` MUST precede the path so a path beginning with `-` is treated as a positional argument (`inspect.rs:254-255`).
- **`-L` rendering** — When both `start_line` and `end_line` are supplied, append `-L<start>,<end>`; when only `start_line` is set, append `-L<start>,`; when only `end_line` is set, append `-L1,<end>` (`inspect.rs:239-245`). All three values are `u32` (`inspect.rs:215-217`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms by `run_git`; stdout capture MUST honor `max_bytes` clamped to `[1, MAX_OUTPUT_BYTES = 5_000_000]` via `clamp_bytes` (`tools-mcp-core/src/validation.rs:36-38`; `inspect.rs:233-235`).
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

- **Line-anchored authorship in one call.** Pairing `path` with a line range and an optional rev covers the most common blame queries without exposing the full `git blame` surface.
- **Defense in depth on `commit`.** Both the `-`-prefix rejection and the pathspec `--` separator harden the positional arguments.
- **Predictable stdout cap.** The 200 KB default lets typical files blame cleanly; the 5 MB ceiling accommodates very large files when needed.
- **Echoed `path` for caller convenience.** Returning the requested `path` in the response avoids round-tripping it through the caller's bookkeeping (`inspect.rs:272-274`).

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `path` | string | Yes | — | Non-whitespace (`inspect.rs:229-231`; `validation.rs:11-22`) | File path to blame. Passed after `--`. |
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |
| `start_line` | integer | No | unset | Schema `>= 1`; rust type `u32` (`inspect.rs:215-216`) | Range start. Combined with `end_line` if present (`inspect.rs:239-245`). |
| `end_line` | integer | No | unset | Schema `>= 1`; rust type `u32` (`inspect.rs:217-218`) | Range end. When only `end_line` is set, the range is `-L1,<end>` (`inspect.rs:243-245`). |
| `commit` | string | No | unset | Non-whitespace, MUST NOT start with `-` (`inspect.rs:247-252`) | Blame at this commit instead of HEAD. |
| `max_bytes` | integer | No | `200000` | Schema `>= 1`, `<= 5_000_000`; clamped via `clamp_bytes` (`validation.rs:36-38`) | Stdout capture cap. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:253`); the deserializer sets `#[serde(deny_unknown_fields)]` (`inspect.rs:207`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:237-256`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitBlameRequest` via `ToolCallOutcome::parse_args` (`inspect.rs:224-227`). On failure return `isError: true`.
2. **Validate `path`** — `validation::validate_non_empty(&req.path, "path", None)` returns `"path is required (non-empty string)"` on whitespace-only values (`inspect.rs:229-231`).
3. **Resolve timeout + clamp `max_bytes`** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)`; `max_bytes = clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES)` (`inspect.rs:233-235`).
4. **Build args** — Start with `["blame"]`. Append range:
   - If both `start_line` and `end_line` are `Some`: append `-L<start>,<end>` (`inspect.rs:239-240`).
   - Else if only `start_line` is `Some`: append `-L<start>,` (`inspect.rs:241-242`).
   - Else if only `end_line` is `Some`: append `-L1,<end>` (`inspect.rs:243-245`).
   - Else: no `-L` (full file).
5. **Append commit (optional)** — If `commit.is_some()`: validate via `validate_non_option_arg("commit")`; append the rev (`inspect.rs:247-252`).
6. **Append pathspec separator + path** — Push `--` then `req.path` (`inspect.rs:254-255`).
7. **Run git** — Call `run_git` with the clamped `max_bytes` for stdout and `DEFAULT_GIT_STDERR_BYTES` for stderr (`inspect.rs:257-268`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`inspect.rs:267`).
8. **Derive response text** — `inspect_output_text(&exec, None)`: on success, return trimmed stdout (empty when whitespace, since `None` is passed for the placeholder); on failure, prefer trimmed stderr, falling back to trimmed stdout (`inspect.rs:23-39,270`).
9. **Compose response** — `build_git_response` with the standard envelope plus extra fields `path: <req.path>` and `max_bytes: <effective>` (`inspect.rs:272-277`; `types.rs:100-142`).

`validate_non_option_arg` (`inspect.rs:13-21`) calls `validation::validate_non_empty` (returning the `<field> is required (non-empty string)` error) and additionally rejects any value starting with `-` with `<field> must not start with '-'`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "abc1234 (Test User 2026-01-01 12:00:00 +0000   1) pub fn main() {"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","blame","-L1,1","--","src/lib.rs"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "abc1234 (Test User 2026-01-01 12:00:00 +0000   1) pub fn main() {\n",
  "stderr": "",
  "path": "src/lib.rs",
  "max_bytes": 200000
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed blame output (`inspect.rs:23-39,270`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `path` | string | Yes | Echo of the requested `path` (`inspect.rs:273`). |
| `max_bytes` | integer | Yes | Effective clamped stdout cap. |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`inspect.rs:267`).
- **`git blame` non-zero exit** (e.g., path not in repo, unknown rev) uses the standard envelope with `isError: true`; `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Missing `path` field | `true` | `"invalid arguments: ..."` with the "Required fields are missing" hint (`tool_outcome.rs:61-75`). |
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint. |
| Whitespace-only `path` | `true` | `"path is required (non-empty string)"` (`validation.rs:11-22`). |
| Whitespace-only `commit` | `true` | `"commit is required (non-empty string)"`. |
| Option-like `commit` (starts with `-`) | `true` | `"commit must not start with '-'"` (`inspect.rs:15-19`). |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `inspect.rs:267`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git blame` non-zero exit (e.g., path not in repo, line range out of bounds) | `true` | Trimmed stderr, e.g. `"fatal: no such path 'missing.txt' in HEAD"` (`inspect.rs:31-38`). |

## 7. Security Considerations

- **Registration gate.** `GitBlame` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Read-only operation.** `git blame` does not mutate refs, the index, or the worktree.
- **Pathspec injection defense.** `--` always precedes the path (`inspect.rs:254-255`), so a path starting with `-` is positional.
- **Option-injection defense on `commit`.** `validate_non_option_arg` rejects values starting with `-` before git sees them (`inspect.rs:247-252`). Locked in by `git_blame_rejects_option_like_commit` (`inspect.rs:346`).
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **Bounded output.** Stdout capped at the smaller of `max_bytes` (max 5 MB) and the clamp range; stderr at 100 KB. Truncation surfaces in the response.
- **Untrusted content.** Blame output embeds author names, dates, and source lines — all caller-controlled or repository-controlled data. Consumers MUST treat `content[0].text` as untrusted input.
- **No `--contents` exposure.** The schema does not expose `--contents <file>`, so callers cannot trick blame into reading arbitrary files on the host.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 237-256 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 270 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/inspect.rs` | 205 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/inspect.rs` | 206-222 |
| Path non-empty validation | `tools-mcp-git/src/git/handlers/inspect.rs` | 229-231 |
| `-L` range rendering | `tools-mcp-git/src/git/handlers/inspect.rs` | 239-245 |
| `validate_non_option_arg` | `tools-mcp-git/src/git/handlers/inspect.rs` | 13-21 |
| Pathspec separator | `tools-mcp-git/src/git/handlers/inspect.rs` | 254-255 |
| Response text + extras | `tools-mcp-git/src/git/handlers/inspect.rs` | 270-277 |
| Standard envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-directory authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Blame an entire file

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBlame",
    "arguments": {"path": "src/lib.rs"}
  }
}
```

### 10.2 Blame a specific line range

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBlame",
    "arguments": {"path": "src/lib.rs", "start_line": 10, "end_line": 25}
  }
}
```

Effective argv: `["blame", "-L10,25", "--", "src/lib.rs"]` (`inspect.rs:239-255`).

### 10.3 Blame from a specific commit

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBlame",
    "arguments": {"path": "src/lib.rs", "commit": "abc1234"}
  }
}
```

Effective argv: `["blame", "abc1234", "--", "src/lib.rs"]` (`inspect.rs:247-255`).

### 10.4 Option-injection rejection on commit

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitBlame",
    "arguments": {"path": "src/lib.rs", "commit": "--contents=Cargo.toml"}
  }
}
```

Result:

```json
{
  "result": {
    "content": [{"type": "text", "text": "commit must not start with '-'"}],
    "isError": true
  }
}
```

### 10.5 Success response (single-line range)

```json
{
  "result": {
    "content": [{"type": "text", "text": "abc1234 (Test User 2026-01-01 12:00:00 +0000   1) pub fn main() {"}],
    "isError": false,
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","blame","-L1,1","--","src/lib.rs"],
    "exit_code": 0,
    "stdout": "abc1234 (Test User 2026-01-01 12:00:00 +0000   1) pub fn main() {\n",
    "stderr": "",
    "path": "src/lib.rs",
    "max_bytes": 200000
  }
}
```

### 10.6 Path-not-in-repo error

```json
{
  "result": {
    "content": [{"type": "text", "text": "fatal: no such path 'missing.txt' in HEAD"}],
    "isError": true,
    "exit_code": 128,
    "stderr": "fatal: no such path 'missing.txt' in HEAD\n",
    "path": "missing.txt",
    "max_bytes": 200000
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_blame_rejects_option_like_commit` | `tools-mcp-git/src/git/handlers/inspect.rs:346` | Option-like `commit` value is refused before spawning git. |
| `inspect_output_text_preserves_success_stdout_trimming` | `tools-mcp-git/src/git/handlers/inspect.rs:302` | Trimmed stdout on success. |
| `inspect_output_text_uses_empty_success_text_when_configured` | `tools-mcp-git/src/git/handlers/inspect.rs:309` | Empty success stdout → empty string (since `None` placeholder is passed). |
| `inspect_output_text_prefers_failure_stderr_when_present` | `tools-mcp-git/src/git/handlers/inspect.rs:317` | Failure text prefers stderr. |
| `inspect_output_text_falls_back_to_failure_stdout` | `tools-mcp-git/src/git/handlers/inspect.rs:327` | Failure text falls back to stdout when stderr is empty. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitBlame` absent without `MCP_ENABLE_GIT=true`. |

No targeted integration test exercises a successful blame end-to-end; coverage rests on the validation tests and the shared `run_git` envelope tests.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | What happens when only `end_line` is supplied? | The handler emits `-L1,<end>` so the range starts at the first line (`inspect.rs:243-245`). |
| 2 | Does the handler emit `--end-of-options` before `commit` like `GitShow`? | No. `GitBlame` relies solely on `validate_non_option_arg` to reject `-`-prefixed values; `git blame` does not have the same option-vs-positional ambiguity for the rev because the pathspec `--` separator follows (`inspect.rs:247-255`). |
| 3 | Can the caller blame a directory? | Implementation-dependent on git's behavior. `git blame` typically fails for directories; the failure surfaces via the standard error envelope. The handler does not pre-check the path. |
| 4 | Why is `--contents` not exposed? | Exposing `--contents <file>` would let callers ask blame to read arbitrary host files. Keeping the surface narrow blocks that attack vector. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_bytes` (§6.2 step 2, 3, 5 and §6.4). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
