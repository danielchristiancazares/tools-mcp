# SDD: GitCommit

**Date:** 2026-05-24
**Scope:** Design contract for the `GitCommit` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/mutating.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitCommit` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitCommit` is a state-mutating MCP tool that creates a single Conventional Commit. The caller supplies `type`, optional `scope`, and `message`; the handler sanitizes each fragment, assembles the subject line `type(scope): message` (or `type: message` when scope is omitted/whitespace), and runs `git commit -m <subject>`. The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_commit` (`tools-mcp-git/src/git/handlers/mutating.rs:192`). It is registered via `GitCommitTool` (`tools-mcp-git/src/tools.rs:109-126`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Staging changes (see `docs/tools/git-add.md`).
- Multi-line commit bodies, trailers, signoff, or amend — none of these are exposed in the schema.
- Branch switching (see `docs/tools/git-checkout.md`).
- Discarding uncommitted changes (see `docs/tools/git-restore.md`).

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitCommit` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_commit` (`tools-mcp-git/src/git/handlers/mutating.rs:192`) |
| Schema definition | `tools-mcp-git/src/tools.rs:109-126` |
| Registration call | `tools-mcp-git/src/tools.rs:264` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **`type` and `message` are required and non-empty** — Both fields MUST be supplied (schema `"required": ["type","message"]`, `tools-mcp-git/src/tools.rs:122`) and MUST contain at least one non-whitespace character; whitespace-only values return the field-specific "is required (non-empty string)" error (`mutating.rs:212-217`; `tools-mcp-core/src/validation.rs:11-22`).
- **Single-line commit subject** — Each of `type`, `scope`, and `message` MUST be sanitized by `sanitize_commit_fragment`, which trims surrounding whitespace, replaces `\n` with a single space, and strips `\r` entirely (`mutating.rs:29-31,219-226`). The constructed `commit_msg` MUST NOT contain `\n` or `\r`, preventing injection of trailers like `Co-authored-by:` (locked in by `git_commit_message_sanitizes_newlines`, `mutating.rs:620`).
- **Scope assembly rule** — `scope` is included in the form `type(scope): message` ONLY when, after sanitization, it is non-empty after trimming; otherwise the subject is `type: message` (`mutating.rs:221-226`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server's startup cwd (`tools-mcp-git/src/git/path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix + env scrubbing** — Git MUST be spawned through `Command::args` with the standard safety prefix and authority env scrub (`tools-mcp-git/src/git/mod.rs:82-99,181-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms; stdout/stderr capped at the configured byte limits (`tools-mcp-git/src/git/mod.rs:164-167`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome` (`mutating.rs:207-217,241`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT spawn `git commit` without sanitizing the subject (the sanitizer enforces single-line content).
- MUST NOT execute git arguments through a shell.
- MUST NOT honor caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT accept `working_dir` paths outside the server cwd.
- MUST NOT expose `--amend`, `--allow-empty`, `--no-verify`, `--signoff`, `--author`, multi-line body, or trailers through this schema. Operators who need those MUST use a different surface.
- MUST NOT enable color output, pagers, or external diff helpers.

## 5. Design Goals

- **One canonical commit shape.** Conventional Commit subjects are easy to scan, easy to grep, and integrate with downstream tooling. Forcing the format eliminates per-call style debates.
- **Defense against trailer injection.** Sanitizing each fragment to strip newlines prevents callers from smuggling extra commit trailers (e.g., `Signed-off-by:` or `Co-authored-by:`) through the `message` field.
- **Narrow API surface.** Omitting `--amend`, `--no-verify`, and similar destructive options keeps the tool's blast radius small and predictable.
- **Structured commit-hash echo.** Returning the commit hash in `commit_hash` lets agentic callers chain follow-up operations without parsing prose.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `type` | string | Yes | — | Non-whitespace after trim (`mutating.rs:212-214`; `validation.rs:11-22`) | Conventional Commit type (e.g., `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`). |
| `scope` | string | No | none | Sanitized; empty-after-trim is treated as no scope (`mutating.rs:221-226`) | Optional scope inside the parentheses. |
| `message` | string | Yes | — | Non-whitespace after trim (`mutating.rs:215-217`) | Commit subject description (single line after sanitization). |
| `working_dir` | string | No | server cwd | MUST resolve inside server's startup cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:123`); the deserializer sets `#[serde(deny_unknown_fields)]` (`mutating.rs:194`). The argument struct renames the field `commit_type` ↔ `"type"` (`mutating.rs:196-197`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:109-126`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitCommitRequest` via `ToolCallOutcome::parse_args` (`mutating.rs:207-210`). On failure return `isError: true`.
2. **Non-empty validation** — `validation::validate_non_empty(&req.commit_type, "type", None)` and the same for `message`; on whitespace-only values return `"type is required (non-empty string)"` or `"message is required (non-empty string)"` (`mutating.rs:212-217`).
3. **Sanitize fragments** — `sanitize_commit_fragment` trims surrounding whitespace, replaces all `\n` with a single space, and strips all `\r` (`mutating.rs:29-31`). Applied to `type`, `message`, and (when supplied) `scope` (`mutating.rs:219-226`).
4. **Assemble subject** — When sanitized `scope` is `Some(s)` AND `s.trim().is_empty()` is `false`, build `"{type}({scope}): {message}"`; otherwise `"{type}: {message}"` (`mutating.rs:221-226`).
5. **Resolve timeout + args** — `timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS)`; args are `["commit", "-m", commit_msg]` (`mutating.rs:228-229`).
6. **Run git** — Call `run_git` with `DEFAULT_GIT_STDOUT_BYTES` / `DEFAULT_GIT_STDERR_BYTES` caps (`mutating.rs:231-242`). `run_git` resolves `working_dir`, clamps `timeout_ms`, applies the safety prefix and authority env scrub, spawns `git`/`git.exe`, captures stdout/stderr with bounded readers, and enforces the timeout (`mod.rs:151-284`). On infrastructure error, return `ToolCallOutcome::err(format!("git error: {e:#}"))` (`mutating.rs:241`).
7. **Extract commit hash** — Scan stdout whitespace tokens for the first that is at least 7 characters and consists entirely of ASCII hex digits (optionally followed by `]`); trim trailing `]` (`mutating.rs:244-248`). `git commit` outputs lines like `[main abc1234] subject`, so this captures the abbreviated hash.
8. **Derive response text** — On success: trimmed stdout. On failure: trimmed stderr if non-empty, else trimmed stdout (`mutating.rs:250-256`).
9. **Compose response** — `build_git_response` with the standard envelope (`content`, `isError = !exec.success`, `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_*`, `stdout`, `stderr`) plus extra fields `commit_message: <subject>` and `commit_hash: <abbreviated hash or null>` (`mutating.rs:258-263`; `types.rs:100-142`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "[main abc1234] feat(core): add feature"}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","commit","-m","feat(core): add feature"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "[main abc1234] feat(core): add feature\n 1 file changed, 1 insertion(+)\n",
  "stderr": "",
  "commit_message": "feat(core): add feature",
  "commit_hash": "abc1234"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed git output or stderr-preferred fallback (`mutating.rs:250-256`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_*`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `commit_message` | string | Yes | The exact subject passed to `git commit -m` (`mutating.rs:259`). |
| `commit_hash` | string \| null | Yes | Abbreviated commit hash if extracted from stdout, else `null` (`mutating.rs:244-248`). |

**Tool-level error (`isError: true`):**

- **Argument parse / validation errors** use `ToolCallOutcome::err`: `{content, isError: true}` (`tool_outcome.rs:35`).
- **`run_git` infrastructure errors** use `ToolCallOutcome::err` with text `"git error: <chain>"` (`mutating.rs:241`).
- **`git commit` non-zero exit** (e.g., nothing staged, hook failure) uses the standard envelope with `isError: true`; `commit_hash` may be `null` and `content[0].text` prefers stderr.

The handler MUST NOT panic.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Missing `type` or `message` field | `true` | `"invalid arguments: ..."` with the "Required fields are missing" hint (`tool_outcome.rs:61-75`). |
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint. |
| Whitespace-only `type` | `true` | `"type is required (non-empty string)"` (`validation.rs:11-22`). |
| Whitespace-only `message` | `true` | `"message is required (non-empty string)"`. |
| `working_dir` outside server cwd or not a directory | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,163-174`; via `mutating.rs:241`). |
| `git`/`git.exe` not found on PATH | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` (`mod.rs:201-203`). |
| Stdout/stderr capture setup failure | `true` | `"git error: failed to capture git stdout"` / `"failed to capture git stderr"` (`mod.rs:205-212`). |
| Timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| `git commit` non-zero exit (e.g., nothing to commit) | `true` | Trimmed stderr, e.g. `"On branch main\nnothing to commit, working tree clean"` (`mutating.rs:252-256`). |
| Pre-commit hook failure | `true` | Trimmed stderr from the hook (`mutating.rs:252-256`). |

## 7. Security Considerations

- **Registration gate.** `GitCommit` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Commit-trailer injection defense.** `sanitize_commit_fragment` replaces `\n` with a single space and strips `\r` before assembling the subject (`mutating.rs:29-31`), so a `message` like `"add feature\n\nSigned-off-by: attacker <evil@evil.com>"` is collapsed to one line and cannot inject trailers. Locked in by `git_commit_message_sanitizes_newlines` (`mutating.rs:620`).
- **Subject-only API.** Only `-m <subject>` is passed; the schema does not expose `-F <file>`, `--amend`, `--allow-empty`, `--no-verify`, `--signoff`, or `--author`, so the surface for adversarial commits is narrow.
- **Untrusted commit-message data.** The `message` field is operator-controlled but may originate from external content (e.g., a webfetched issue title). Downstream readers should treat `commit_message` and `content[0].text` as untrusted and avoid acting on them as instructions. See `docs/security.md` for the project-wide trust-boundary guidance.
- **Pre-commit hooks still run.** Repository-local hooks (e.g., `.git/hooks/pre-commit`) execute under this tool. Operators who do not trust the repo MUST audit hooks before invoking `GitCommit`.
- **Working-directory authority.** `path_policy::resolve_working_dir` canonicalizes and confines the working directory (`path_policy.rs:40-185`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args` (`mod.rs:181-182`); no shell interpolation.
- **Hostile-environment hardening.** `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` redirected to platform null sink, `GIT_EXTERNAL_DIFF=""`, authority + `GIT_CONFIG_KEY_*`/`GIT_CONFIG_VALUE_*` scrub (`mod.rs:184-198,294-320`).
- **Bounded output.** 200 KB stdout cap, 100 KB stderr cap.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`/`GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL` are inherited from the parent process (they are not in the authority scrub list at `mod.rs:68-80`). If the repository has no `user.name` / `user.email` configured AND the parent process did not export those env vars, `git commit` will fail; the failure surfaces through the standard error envelope.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 109-126 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 264 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/mutating.rs` | 192 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 193-205 |
| `type`/`message` non-empty validation | `tools-mcp-git/src/git/handlers/mutating.rs` | 212-217 |
| `sanitize_commit_fragment` | `tools-mcp-git/src/git/handlers/mutating.rs` | 29-31 |
| Subject assembly | `tools-mcp-git/src/git/handlers/mutating.rs` | 219-226 |
| Commit args (`-m`) | `tools-mcp-git/src/git/handlers/mutating.rs` | 228-229 |
| Commit-hash extraction | `tools-mcp-git/src/git/handlers/mutating.rs` | 244-248 |
| Response text + extras | `tools-mcp-git/src/git/handlers/mutating.rs` | 250-263 |
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
    "name": "GitCommit",
    "arguments": {
      "type": "feat",
      "scope": "core",
      "message": "add feature"
    }
  }
}
```

### 10.2 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "[main abc1234] feat(core): add feature\n 1 file changed, 1 insertion(+)"}],
    "isError": false,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","commit","-m","feat(core): add feature"],
    "working_dir": "/repo",
    "exit_code": 0,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": "[main abc1234] feat(core): add feature\n 1 file changed, 1 insertion(+)\n",
    "stderr": "",
    "commit_message": "feat(core): add feature",
    "commit_hash": "abc1234"
  }
}
```

### 10.3 No scope variant

Subject becomes `"feat: add feature"`:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCommit",
    "arguments": {"type": "feat", "message": "add feature"}
  }
}
```

### 10.4 Newline injection collapsed

A `message` containing `\n` is collapsed to a single line; the assembled subject contains neither `\n` nor `\r`:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitCommit",
    "arguments": {
      "type": "feat",
      "message": "add feature\n\nSigned-off-by: attacker <evil@evil.com>"
    }
  }
}
```

Resulting commit subject (`commit_message` field): `"feat: add feature  Signed-off-by: attacker <evil@evil.com>"` — single line, no trailer interpreted by git.

### 10.5 Nothing-to-commit error

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "On branch main\nnothing to commit, working tree clean"}],
    "isError": true,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","commit","-m","feat: add feature"],
    "working_dir": "/repo",
    "exit_code": 1,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": "On branch main\nnothing to commit, working tree clean\n",
    "stderr": "",
    "commit_message": "feat: add feature",
    "commit_hash": null
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `git_commit_message_sanitizes_newlines` | `tools-mcp-git/src/git/handlers/mutating.rs:620` | Collapses `\n`/`\r` in `type`, `scope`, `message` so the assembled subject is single-line and cannot inject trailers. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | `GitCommit` absent without `MCP_ENABLE_GIT=true`. |
| `test_tools_list` | `tools-mcp-server/tests/integration_test.rs:115` | `GitCommit` present when registered. |

No dedicated integration test exercises a successful commit end-to-end; coverage relies on the sanitization unit test and the shared `run_git` envelope tests.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Can a multi-line commit body be provided? | No. The schema accepts only `type`, `scope`, `message`. Each fragment is sanitized to single-line content. Operators who need a body or trailers MUST use a different surface (`mutating.rs:29-31,219-226`). |
| 2 | Does the handler call `git add` before `git commit`? | No. `GitCommit` only commits what is already staged. The caller is responsible for staging via `GitAdd` first. |
| 3 | What does `commit_hash` contain on a merge commit or non-standard `git commit` output? | The extractor takes the first whitespace-separated token of length ≥ 7 consisting of ASCII hex (optionally followed by `]`). For typical `[branch hash] subject` output this is the abbreviated hash. For unrecognized output formats, `commit_hash` is `null` (`mutating.rs:244-248`). |
| 4 | Why is `--no-verify` not exposed? | Bypassing pre-commit hooks weakens repository safety and is rarely the right default for an agent. Operators who must bypass hooks should do so explicitly outside this tool. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` (§6.2 step 2). |
| `tools-mcp-core/src/config.rs` | Default timeout and byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default assertion (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
