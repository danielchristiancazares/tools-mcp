# SDD: GitDiff

**Date:** 2026-05-24
**Scope:** Design contract for the `GitDiff` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/diff.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `GitDiff` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`GitDiff` is an MCP tool that wraps `git diff` in two distinct modes:

1. **Worktree mode** (default) — diffs the working tree or staging area against `HEAD` (or against the index when `cached=true`), returning the unified-diff stdout in the response.
2. **Ref-export mode** — when `from_ref`, `to_ref`, and `output_dir` are all supplied, the handler enumerates changed files via `git diff --name-status -z` + `git diff --numstat -z`, writes one `*.patch` file per changed path under `output_dir`, and emits a `_summary.json` manifest. The MCP response contains a structured summary rather than the patch text.

The tool is owned by the `tools-mcp-git` crate; the handler is `handle_git_diff` (`tools-mcp-git/src/git/handlers/diff.rs:547`). It is registered via `GitDiffTool` (`tools-mcp-git/src/tools.rs:46-69`), and registration is gated by `MCP_ENABLE_GIT=true` (`tools-mcp-git/src/lib.rs:7-10`).

### 3.2 Explicitly Out of Scope

- Mutating index/worktree operations (see `docs/tools/git-add.md`, `docs/tools/git-restore.md`, `docs/tools/git-commit.md`).
- Read-only triage with counts (see `docs/tools/git-snapshot.md`).
- Commit-level inspection (see `docs/tools/git-show.md`, `docs/tools/git-log.md`, `docs/tools/git-blame.md`).
- Protocol routing and tool registry composition.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `GitDiff` |
| Aliases | None |
| Registration gate | `MCP_ENABLE_GIT=true` REQUIRED. Any other value (including unset) MUST skip registration (`tools-mcp-git/src/lib.rs:7-10`). |
| Owning crate | `tools-mcp-git` |
| Handler function | `handle_git_diff` (`tools-mcp-git/src/git/handlers/diff.rs:547`) |
| Schema definition | `tools-mcp-git/src/tools.rs:46-69` |
| Registration call | `tools-mcp-git/src/tools.rs:261` (via `tools_mcp_git::register_tools` in `tools-mcp-server/src/composition.rs:90`) |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Gate before register** — When `MCP_ENABLE_GIT` is unset or not equal to `"true"`, this tool MUST NOT register (`tools-mcp-git/src/lib.rs:7-10`).
- **Ref-export tuple is all-or-nothing** — `from_ref`, `to_ref`, and `output_dir` MUST appear together. Any other combination MUST return the tool-level error `"from_ref, to_ref, and output_dir are required together"` (`tools-mcp-git/src/git/handlers/diff.rs:586-633`).
- **Ref-export field non-empty validation** — In ref-export mode, each of `from_ref`, `to_ref`, `output_dir` MUST be non-whitespace; an empty/whitespace value MUST return `"<field> is required (non-empty string)"` (`diff.rs:588-596`; `tools-mcp-core/src/validation.rs:11-22`).
- **Ref injection defense** — Ref-export commands MUST insert `--end-of-options` before `{from_ref}..{to_ref}` so an option-like ref (e.g., `--output=...`) is treated as a positional argument and cannot redirect output (`diff.rs:107-126,128-149`). Locked in by `git_diff_ref_export_does_not_treat_refs_as_options` (`diff.rs:1149`).
- **Output-dir authority** — `output_dir` MUST be resolved by `path_policy::resolve_output_dir`, which canonicalizes and confines it under the server's startup cwd; uncreated tail segments are allowed but the deepest existing ancestor MUST be inside the authority (`path_policy.rs:108-141,183-185`).
- **Working-directory authority** — When provided, `working_dir` MUST be canonicalized and confined under the server cwd (`path_policy.rs:163-181`).
- **Argument-list invocation + safety prefix** — Git MUST be invoked through `Command::args` with the safety prefix `--no-pager -c color.ui=false -c diff.external= -c core.fsmonitor=` prepended; arguments MUST NOT pass through a shell (`mod.rs:82-99,181-198`).
- **External helpers disabled in diff** — Diff invocations MUST include `--no-ext-diff` and `--no-textconv` to prevent external helpers from being executed during diff generation (`diff.rs:107-149,151-191`).
- **Authority env scrubbed** — Authority and helper env vars plus `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs MUST be removed from the spawned child; `GIT_CONFIG_NOSYSTEM=1`; `GIT_CONFIG_GLOBAL` redirected to `NUL`/`/dev/null`; `GIT_EXTERNAL_DIFF=""` set (`mod.rs:68-99,184-198,294-320`).
- **Bounded execution** — `timeout_ms` MUST be clamped to `[100, 300_000]` ms by `run_git`. In worktree mode, stdout capture honors `max_bytes` clamped to `[1, MAX_OUTPUT_BYTES = 5_000_000]` (`tools-mcp-core/src/validation.rs:36-38`; `tools-mcp-core/src/config.rs:16`). In ref-export mode, manifest queries use `MAX_OUTPUT_BYTES` for stdout and the per-file `git diff --output=...` calls clamp stdout to `PATCH_EXPORT_STDOUT_BYTES = 1` byte to detect unexpected stdout (`diff.rs:17,396-403,479-501`).
- **Patch-export stdout guard** — Any non-empty stdout (or `truncated_stdout`) returned by a `git diff --output=...` invocation MUST surface as `"git diff wrote unexpected stdout while exporting <path> with --output"` (`diff.rs:496-501`).
- **Binary-file placeholder** — When `git diff --numstat` reports `-\t-` (binary), and the `--output=...` patch file is empty, the handler MUST write the placeholder `"Binary file: <path>\n"` to the patch file (`diff.rs:310-330,370-382,503-505`).
- **Unique patch filenames** — Sanitized path filenames that collide MUST be disambiguated with numeric suffixes (`{base}.2.patch`, `{base}.3.patch`, ...) (`diff.rs:28-48`). Locked in by `unique_patch_filename_disambiguates_sanitized_path_collisions` and `git_diff_ref_export_uses_distinct_patch_files_for_sanitized_path_collisions` (`diff.rs:891,1097`).
- **Never panics** — Every error path MUST return a `ToolCallOutcome`.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT register when `MCP_ENABLE_GIT` is absent or not the literal string `"true"`.
- MUST NOT execute git arguments through a shell.
- MUST NOT enable color output, external diff helpers, text-conv filters, or pagers.
- MUST NOT accept partial ref-export tuples (e.g., `output_dir` without both `from_ref` and `to_ref`).
- MUST NOT accept `working_dir` or `output_dir` paths that resolve outside the server cwd.
- MUST NOT trust caller-supplied `GIT_*` authority/config environment variables.
- MUST NOT pass refs without `--end-of-options` in ref-export mode.

## 5. Design Goals

- **Two-mode interface for one mental model.** Most agentic callers want either the unified patch text (worktree mode) or a per-file patch corpus they can read individually (ref-export mode). Combining both behind one tool keeps the catalog small.
- **Bounded stdout in worktree mode.** A 5 MB worst-case stdout cap with a 200 KB default lets agents diff large changes without flooding the model's context.
- **Out-of-band patch corpus.** Ref-export mode writes each patch to disk so even a many-file rename can be inspected file-by-file without exceeding response budgets.
- **Hardened against arg injection.** `--end-of-options` plus `--output=...` with no stdout fallback (the per-file calls clamp stdout to 1 byte and treat any captured byte as an error) ensures a malicious ref cannot trick git into emitting patches outside the chosen directory.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `working_dir` | string | No | server cwd | MUST resolve inside server cwd (`path_policy.rs:40-55,163-174`) | Working directory for the git command. |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped to `[100, 300_000]` (`mod.rs:164`) | Per-command timeout in milliseconds. |
| `cached` | boolean | No | `false` | Worktree mode only | Appends `--cached` to diff staged changes against HEAD (`diff.rs:171-173`). |
| `stat` | boolean | No | `false` | Worktree mode only | Appends `--stat`. |
| `name_only` | boolean | No | `false` | Worktree mode only | Appends `--name-only`. |
| `unified` | integer | No | git default | `>= 0` (rejected as negative by handler in addition to schema, `diff.rs:638-640`) | Appends `-U<N>`. |
| `paths` | string array | No | `[]` | Trimmed entries MUST contain at least one non-empty path when provided (`diff.rs:92-105`) | Pathspec list appended after `--`. |
| `max_bytes` | integer | No | `200000` | Schema `>= 1`, `<= 5_000_000`; handler clamps via `clamp_bytes` (`validation.rs:36-38`) | Worktree-mode stdout cap. |
| `from_ref` | string | Conditional | — | Required with `to_ref`+`output_dir`. Non-empty (`diff.rs:588-596`). | Starting ref for ref-export mode. |
| `to_ref` | string | Conditional | — | Required with `from_ref`+`output_dir`. Non-empty. | Ending ref for ref-export mode. |
| `output_dir` | string | Conditional | — | Required with `from_ref`+`to_ref`. Resolved by `path_policy::resolve_output_dir`. Non-empty. | Directory for per-file patches. Created if missing. |

The schema sets `"additionalProperties": false` (`tools-mcp-git/src/tools.rs:66`); the deserializer sets `#[serde(deny_unknown_fields)]` (`diff.rs:549`). Unknown fields produce a tool-level error `"invalid arguments: ..."` with the "Unknown fields are not allowed" hint (`tool_outcome.rs:61-75`).

> Schema source: `tools-mcp-git/src/tools.rs:46-69`

### 6.2 Behavior

1. **Parse arguments** — Deserialize into `GitDiffRequest` via `ToolCallOutcome::parse_args` (`diff.rs:575-578`).
2. **Normalize timeout + paths** — `timeout_ms` defaults to `DEFAULT_GIT_TIMEOUT_MS = 30_000`. `requested_paths` rejects whitespace-only entries with `"paths must include at least one non-empty path"` (`diff.rs:580-584,92-105`).
3. **Dispatch on ref tuple** — Match `(from_ref, to_ref, output_dir)` (`diff.rs:586-633`):
   - `(Some, Some, Some)` → ref-export mode.
   - `(None, None, None)` → worktree mode.
   - Any other combination → return `"from_ref, to_ref, and output_dir are required together"`.
4. **Ref-export mode — non-empty validation** — For each of `from_ref`, `to_ref`, `output_dir`, call `validation::validate_non_empty(...)`; return the field-specific error string on whitespace-only values (`diff.rs:588-596`).
5. **Ref-export mode — output dir resolution + creation** — `path_policy::resolve_output_dir(output_dir)` canonicalizes the path and confines it (`path_policy.rs:183-185`); the canonical path is reported back as `effective_output_dir`. The directory is created with `tokio::fs::create_dir_all` (`diff.rs:443-448`).
6. **Ref-export mode — collect manifest** — `collect_ref_diff_manifest`:
   - Run `git diff --no-ext-diff --no-textconv --find-renames --name-status -z --end-of-options {from_ref}..{to_ref} [-- paths...]` with stdout capped at `MAX_OUTPUT_BYTES` (`diff.rs:395-410`). Parse with `parse_name_status_z`: handles `A|D|M|T` (added/deleted/modified), `R` (renamed), `C` (copied) statuses; unsupported codes return an error (`diff.rs:214-283`).
   - Run the same query with `--numstat -z` (`diff.rs:413-431`). Parse with `apply_numstat_z`; the parser preserves tabs inside paths, classifies `-\t-` as binary, and rejects path mismatches against the name-status entries (`diff.rs:285-368`).
7. **Ref-export mode — per-file patch export** — For each manifest entry:
   - Sanitize the path for filesystem use: replace `/` and `\` with `__` (`diff.rs:20-26`).
   - Pick a unique patch filename via `unique_patch_filename`; first attempt is `{base}.patch`, then `{base}.{n}.patch` starting at `n=2`.
   - Compute the absolute output path; on Windows strip `\\?\` / `\\?\UNC\` verbatim prefixes (`diff.rs:193-211`).
   - Run `git diff --no-ext-diff --no-textconv --find-renames --output={out_path} --end-of-options {from_ref}..{to_ref} -- [old_path] new_path` with stdout clamped to 1 byte (`diff.rs:128-149,471-501`). Stdout MUST be empty; any captured stdout (`!stdout.is_empty()` or `truncated_stdout`) returns `"git diff wrote unexpected stdout while exporting <path> with --output"` (`diff.rs:496-501`).
   - If the entry is binary, call `write_binary_placeholder_if_empty` to write `"Binary file: <path>\n"` when the produced patch file is empty (`diff.rs:370-382,503-505`).
   - Push a `FileDiffEntry` record `{path, status, old_path?, insertions, deletions, patch_file, binary}` (`diff.rs:50-62,507-516`).
8. **Ref-export mode — write summary** — Build `DiffSummary { from_ref, to_ref, generated_at, summary: {files_changed, insertions, deletions}, files }`, serialize with `serde_json::to_string_pretty`, and write to `<output_dir>/_summary.json` (`diff.rs:518-536`).
9. **Ref-export mode — response** — Build `{content: [{type:"text", text:"Diff between {from_ref} and {to_ref}: <n> files changed. Patches written to <output_dir>"}], isError: false, from_ref, to_ref, output_dir, summary, files}` (`diff.rs:608-625`).
10. **Worktree mode — clamp `max_bytes`** — `max_bytes = clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES)` (`diff.rs:635-636`).
11. **Worktree mode — `unified` non-negative** — `req.unified.is_some_and(|u| u < 0)` returns `"unified must be >= 0"` (`diff.rs:638-640`).
12. **Worktree mode — build args** — `build_worktree_diff_args` always starts with `["diff", "--no-ext-diff", "--no-textconv"]`, then appends `--cached`, `--stat`, `--name-only`, `-U<N>` conditionally, then `--` and paths if any (`diff.rs:151-191`).
13. **Worktree mode — execute** — Call `run_git` with the clamped `max_bytes` for stdout and `DEFAULT_GIT_STDERR_BYTES` for stderr (`diff.rs:649-660`).
14. **Worktree mode — derive response text** — On success: `"no diff"` when stdout is whitespace, else trimmed stdout. On failure: trimmed stderr if non-empty, else trimmed stdout (`diff.rs:662-672`).
15. **Worktree mode — response** — `build_git_response` with the standard envelope (`content`, `isError = !success`, `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr`) plus the extra field `max_bytes` (`diff.rs:674-678`; `types.rs:100-142`).

### 6.3 Response Schema

**Worktree-mode success:**

```json
{
  "content": [{"type": "text", "text": "diff --git a/src/lib.rs b/src/lib.rs\n..."}],
  "isError": false,
  "git_bin": "git",
  "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","diff","--no-ext-diff","--no-textconv"],
  "working_dir": "/repo",
  "exit_code": 0,
  "timed_out": false,
  "truncated_stdout": false,
  "truncated_stderr": false,
  "stdout": "diff --git ...\n",
  "stderr": "",
  "max_bytes": 200000
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Trimmed diff text or `"no diff"` (`diff.rs:662-672`). |
| `isError` | boolean | Yes | `!exec.success` (`types.rs:131`). |
| `git_bin`, `args`, `working_dir`, `exit_code`, `timed_out`, `truncated_stdout`, `truncated_stderr`, `stdout`, `stderr` | — | Yes | Standard git envelope (`types.rs:128-142`). |
| `max_bytes` | integer | Yes | Effective clamped stdout cap. |

**Ref-export-mode success:**

```json
{
  "content": [{"type": "text", "text": "Diff between HEAD~1 and HEAD: 2 files changed. Patches written to /repo/patches"}],
  "isError": false,
  "from_ref": "HEAD~1",
  "to_ref": "HEAD",
  "output_dir": "/repo/patches",
  "summary": {"files_changed": 2, "insertions": 5, "deletions": 3},
  "files": [
    {"path": "src/new.txt", "status": "renamed", "old_path": "src/old.txt", "insertions": 0, "deletions": 0, "patch_file": "src__new.txt.patch"},
    {"path": "README.md", "status": "modified", "insertions": 5, "deletions": 3, "patch_file": "README.md.patch"}
  ]
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Human-readable summary. |
| `isError` | boolean | Yes | `false` on success. |
| `from_ref`, `to_ref` | string | Yes | Echo of the requested refs. |
| `output_dir` | string | Yes | Canonicalized output directory. |
| `summary` | object | Yes | `{files_changed, insertions, deletions}` (`diff.rs:65-80`). |
| `files` | array | Yes | Per-file records `{path, status, old_path?, insertions, deletions, patch_file, binary?}` (`diff.rs:50-62`). `binary` is omitted when `false` (`#[serde(skip_serializing_if = ...)]`). |

The on-disk artifacts in `output_dir` are: one `<sanitized_path>.patch` per entry (binary entries receive a `"Binary file: <path>\n"` placeholder when the git output was empty) plus `_summary.json` with `from_ref`, `to_ref`, `generated_at` (RFC 3339 UTC), `summary`, and `files` mirroring the response (`diff.rs:518-538`).

**Tool-level error (`isError: true`):**

Error envelopes use `ToolCallOutcome::err` (`tool_outcome.rs:35`) and contain `{content: [{type:"text", text:"<message>"}], isError: true}`. Worktree-mode git failures use the standard `build_git_response` envelope with `isError: true` from `!exec.success` (`types.rs:128-142`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Unknown / wrong-typed argument | `true` | `"invalid arguments: ..."` with parse-args hint (`tool_outcome.rs:61-75`). |
| `paths` provided but all entries whitespace | `true` | `"paths must include at least one non-empty path"` (`diff.rs:97-99`). |
| Partial ref-export tuple | `true` | `"from_ref, to_ref, and output_dir are required together"` (`diff.rs:631`). |
| Empty/whitespace `from_ref`/`to_ref`/`output_dir` in ref-export mode | `true` | `"<field> is required (non-empty string)"` (`validation.rs:11-22`; `diff.rs:588-596`). |
| `working_dir` outside server cwd | `true` | `"git error: working_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55`; surfaced via `mod.rs:158-159`). |
| `output_dir` outside server cwd | `true` | `"output_dir must resolve inside the server working directory (...): ..."` (`path_policy.rs:46-55,183-185`; surfaced via `diff.rs:444`). |
| `output_dir` creation failure | `true` | `"Failed to create output directory: <io error>"` (`diff.rs:446-448`). |
| `--name-status` infrastructure error | `true` | `"git diff --name-status error: ..."` (`diff.rs:403-404`). |
| `--name-status` non-zero exit | `true` | `"git diff --name-status failed: <stderr>"` (`diff.rs:405-410`). |
| Unsupported `--name-status` token | `true` | `"git diff --name-status returned unsupported status token <token>"` (`diff.rs:274-278`). |
| `--numstat` infrastructure error | `true` | `"git diff --numstat error: ..."` (`diff.rs:422-423`). |
| `--numstat` non-zero exit | `true` | `"git diff --numstat failed: <stderr>"` (`diff.rs:424-429`). |
| Malformed `--numstat` record | `true` | `"git diff --numstat returned malformed record: <token>"` (`diff.rs:298-308`). |
| `--numstat` path not in name-status | `true` | `"git diff --numstat returned a path not present in --name-status: <path>"` (`diff.rs:344-354`). |
| Per-file patch infrastructure error | `true` | `"git diff error for <path>: ..."` (`diff.rs:486-487`). |
| Per-file patch non-zero exit | `true` | `"git diff failed for <path>: <stderr>"` (`diff.rs:488-494`). |
| Per-file patch produced stdout (option-injection guard fires) | `true` | `"git diff wrote unexpected stdout while exporting <path> with --output"` (`diff.rs:496-501`). |
| Binary placeholder write failure | `true` | `"Failed to inspect <path>: ..."` / `"Failed to write <path>: ..."` (`diff.rs:372-379`). |
| Summary write failure | `true` | `"Failed to serialize summary: ..."` / `"Failed to write summary: ..."` (`diff.rs:530-535`). |
| Worktree-mode `unified < 0` | `true` | `"unified must be >= 0"` (`diff.rs:639`). |
| Worktree-mode git spawn / capture failure | `true` | `"git error: failed to spawn git[.exe]. Is Git installed and on PATH? error: ..."` etc. (`mod.rs:201-212`; surfaced via `diff.rs:659`). |
| Worktree-mode timeout grace expires | `true` | `"git error: git command timed out after <N> ms and did not terminate"` (`mod.rs:229-233`). |
| Worktree-mode non-zero exit | `true` | Trimmed stderr if non-empty, else trimmed stdout (`diff.rs:662-672`). |

## 7. Security Considerations

- **Registration gate.** `GitDiff` is off unless `MCP_ENABLE_GIT=true` (`lib.rs:7-10`).
- **Working-directory + output-directory authority.** Both `working_dir` and `output_dir` are confined under the server's startup directory by `path_policy` (`path_policy.rs:40-185`). `resolve_output_dir` allows uncreated tail components, but every ancestor and the resolved path itself MUST stay within the authority. Tests: `allows_nonexistent_output_dir_under_current_working_dir`, `rejects_nonexistent_output_dir_under_parent`, `rejects_output_dir_that_escapes_after_uncreated_tail` (`path_policy.rs:222,233,245`).
- **Ref-as-option injection defense.** Ref-export mode always inserts `--end-of-options` before the `{from_ref}..{to_ref}` positional, so an option-like ref (e.g., `--output=../side-effect`) cannot redirect output (`diff.rs:107-126,128-149`). The integration test `git_diff_ref_export_does_not_treat_refs_as_options` (`diff.rs:1149`) verifies that such inputs surface as `isError: true` with no side-effect file written.
- **Stdout sentinel for patch export.** The per-file `git diff --output=...` calls clamp stdout to 1 byte and treat any captured byte as a violation (`diff.rs:17,496-501`). This catches misuse such as a malformed manifest entry that would otherwise cause git to emit patch content to stdout.
- **External helpers disabled.** Diff invocations include `--no-ext-diff` and `--no-textconv`; the `git` safety prefix also sets `diff.external=`. Any caller-controlled config helper is scrubbed before spawn (`mod.rs:82-99,184-198,294-320`).
- **Command-injection resistance.** Arguments are passed as a `Vec<String>` to `Command::args`; no shell interpolation occurs (`mod.rs:181-182`).
- **Bounded output.** Worktree-mode stdout is capped at the smaller of `max_bytes` (max 5 MB) and the clamp range. Manifest queries use the 5 MB cap. Stderr is capped at 100 KB by default. Truncation flags surface in the response.
- **Patch-file UTF-8 safety.** Patches are written with `git diff --output=...` so git owns the file encoding. Binary entries are explicitly replaced with the ASCII placeholder when git emits no payload.
- **Read-only operation.** `GitDiff` does not modify refs, the index, or the worktree; the only writes are to the caller-chosen `output_dir`.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset (git tools disabled) | Hard registration gate; only the literal string `"true"` registers the git tool family. |

`TOOLS_PRETTY_JSON` does not affect this tool's response shape.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Registration gate | `tools-mcp-git/src/lib.rs` | 7-10 |
| Tool macro / schema | `tools-mcp-git/src/tools.rs` | 46-69 |
| Tool registration call | `tools-mcp-git/src/tools.rs` | 261 |
| Composition wiring | `tools-mcp-server/src/composition.rs` | 90 |
| Handler entry point | `tools-mcp-git/src/git/handlers/diff.rs` | 547 |
| Argument struct (`deny_unknown_fields`) | `tools-mcp-git/src/git/handlers/diff.rs` | 548-573 |
| Ref-tuple dispatch | `tools-mcp-git/src/git/handlers/diff.rs` | 586-633 |
| Ref-export arg builder (with `--end-of-options`) | `tools-mcp-git/src/git/handlers/diff.rs` | 107-126 |
| Per-file patch arg builder | `tools-mcp-git/src/git/handlers/diff.rs` | 128-149 |
| Worktree arg builder | `tools-mcp-git/src/git/handlers/diff.rs` | 151-191 |
| `--name-status -z` parser | `tools-mcp-git/src/git/handlers/diff.rs` | 214-283 |
| `--numstat -z` parser | `tools-mcp-git/src/git/handlers/diff.rs` | 285-368 |
| Patch-export stdout guard | `tools-mcp-git/src/git/handlers/diff.rs` | 17, 496-501 |
| Binary placeholder writer | `tools-mcp-git/src/git/handlers/diff.rs` | 370-382 |
| Unique patch filename | `tools-mcp-git/src/git/handlers/diff.rs` | 28-48 |
| Sanitized path filename | `tools-mcp-git/src/git/handlers/diff.rs` | 20-26 |
| Summary build + write | `tools-mcp-git/src/git/handlers/diff.rs` | 518-538 |
| Worktree response builder | `tools-mcp-git/src/git/handlers/diff.rs` | 662-678 |
| Standard git envelope builder | `tools-mcp-git/src/git/types.rs` | 100-142 |
| `run_git` executor | `tools-mcp-git/src/git/mod.rs` | 151-284 |
| Working-dir + output-dir authority | `tools-mcp-git/src/git/path_policy.rs` | 40-185 |

## 10. Examples

### 10.1 Minimal worktree diff

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitDiff",
    "arguments": {"cached": true, "stat": true}
  }
}
```

### 10.2 Worktree-mode success

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": " src/lib.rs | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)"}],
    "isError": false,
    "git_bin": "git",
    "args": ["--no-pager","-c","color.ui=false","-c","diff.external=","-c","core.fsmonitor=","diff","--no-ext-diff","--no-textconv","--cached","--stat"],
    "working_dir": "/repo",
    "exit_code": 0,
    "timed_out": false,
    "truncated_stdout": false,
    "truncated_stderr": false,
    "stdout": " src/lib.rs | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)\n",
    "stderr": "",
    "max_bytes": 200000
  }
}
```

### 10.3 Ref-export call

```json
{
  "jsonrpc": "2.0",
  "id": 420,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitDiff",
    "arguments": {
      "working_dir": "/repo",
      "from_ref": "HEAD~1",
      "to_ref": "HEAD",
      "output_dir": "/repo/patches"
    }
  }
}
```

### 10.4 Ref-export success

```json
{
  "jsonrpc": "2.0",
  "id": 420,
  "result": {
    "content": [{"type": "text", "text": "Diff between HEAD~1 and HEAD: 1 files changed. Patches written to /repo/patches"}],
    "isError": false,
    "from_ref": "HEAD~1",
    "to_ref": "HEAD",
    "output_dir": "/repo/patches",
    "summary": {"files_changed": 1, "insertions": 0, "deletions": 0},
    "files": [{"path": "src/new.txt", "status": "renamed", "old_path": "src/old.txt", "insertions": 0, "deletions": 0, "patch_file": "src__new.txt.patch"}]
  }
}
```

### 10.5 Ref-as-option injection rejected

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitDiff",
    "arguments": {
      "from_ref": "--output=../side-effect",
      "to_ref": "HEAD",
      "output_dir": "/repo/patches"
    }
  }
}
```

The call returns `isError: true` (the underlying `git diff --name-status` fails because the rev is invalid), and the integration test `git_diff_ref_export_does_not_treat_refs_as_options` verifies no `side-effect..HEAD` file is written (`diff.rs:1149`).

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `parse_name_status_z_handles_rename_entries` | `tools-mcp-git/src/git/handlers/diff.rs:750` | Rename parsing. |
| `parse_name_status_z_handles_copy_entries` | `tools-mcp-git/src/git/handlers/diff.rs:772` | Copy parsing. |
| `apply_numstat_z_populates_rename_counts` | `tools-mcp-git/src/git/handlers/diff.rs:760` | Numstat for renames. |
| `apply_numstat_z_populates_copy_counts` | `tools-mcp-git/src/git/handlers/diff.rs:782` | Numstat for copies. |
| `apply_numstat_z_preserves_tabs_in_paths` | `tools-mcp-git/src/git/handlers/diff.rs:794` | Tab-in-path safety. |
| `ref_diff_args_place_modes_before_end_of_options` | `tools-mcp-git/src/git/handlers/diff.rs:805` | `--end-of-options` ordering. |
| `ref_patch_args_keep_output_before_refs_and_paths_after_separator` | `tools-mcp-git/src/git/handlers/diff.rs:831` | Per-file patch arg ordering. |
| `worktree_diff_args_keep_options_before_path_separator` | `tools-mcp-git/src/git/handlers/diff.rs:857` | Worktree arg ordering. |
| `git_output_path_args_strip_windows_verbatim_prefixes` (cfg windows) | `tools-mcp-git/src/git/handlers/diff.rs:879` | Strip `\\?\` / `\\?\UNC\`. |
| `unique_patch_filename_disambiguates_sanitized_path_collisions` | `tools-mcp-git/src/git/handlers/diff.rs:891` | Filename disambiguation. |
| `git_diff_rejects_whitespace_only_paths` | `tools-mcp-git/src/git/handlers/diff.rs:913` | Path validation. |
| `git_diff_rejects_negative_unified_context` | `tools-mcp-git/src/git/handlers/diff.rs:925` | `unified` range. |
| `git_diff_ref_export_requires_complete_non_empty_ref_tuple` | `tools-mcp-git/src/git/handlers/diff.rs:937` | All-or-nothing tuple. |
| `git_diff_ref_export_honors_paths_filter` | `tools-mcp-git/src/git/handlers/diff.rs:962` | Pathspec filter in ref-export mode. |
| `git_diff_ref_export_preserves_patch_output` | `tools-mcp-git/src/git/handlers/diff.rs:995` | Per-file patch matches raw `git diff` output. |
| `git_diff_ref_export_reports_canonical_output_dir_for_symlinked_parent` (cfg unix) | `tools-mcp-git/src/git/handlers/diff.rs:1053` | Output-dir symlink resolves canonically. |
| `git_diff_ref_export_uses_distinct_patch_files_for_sanitized_path_collisions` | `tools-mcp-git/src/git/handlers/diff.rs:1097` | Disambiguation in real repo. |
| `git_diff_ref_export_does_not_treat_refs_as_options` | `tools-mcp-git/src/git/handlers/diff.rs:1149` | Ref-as-option injection rejected. |
| `test_git_diff_ref_export_preserves_rename_metadata` | `tools-mcp-server/tests/integration_test.rs:868` | End-to-end ref-export including rename status + patch contents. |
| `test_git_tools_disabled_by_default` | `tools-mcp-server/tests/integration_test.rs:1270` | Confirms `GitDiff` absent without `MCP_ENABLE_GIT=true`. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Why does the per-file patch invocation cap stdout at 1 byte? | The `--output=...` flag should send all patch content to disk. Any byte that appears on stdout signals git accepted the file as an option-like ref or behaved unexpectedly. Treating that as a hard error preserves the option-injection defense (`diff.rs:17,496-501`). |
| 2 | Does the worktree-mode response include the `from_ref`, `to_ref`, `output_dir`, `summary`, `files` fields? | No. Those fields are exclusive to ref-export mode. Worktree mode returns the standard git envelope plus `max_bytes` (`diff.rs:608-678`). |
| 3 | What happens if `output_dir` already exists and contains files? | The directory is preserved; `create_dir_all` is a no-op when the path exists. The handler writes new `*.patch` files and `_summary.json`, potentially overwriting prior runs. Filename collisions inside a single run are disambiguated by suffix. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shapes, `parse_args` error wording (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_bytes` (§6.2 step 4, 10). |
| `tools-mcp-core/src/config.rs` | Default timeout, byte caps (§4.2). |
| `tools-mcp-server/src/composition.rs` | Composition root invocation (§4.1). |
| `tools-mcp-server/tests/integration_test.rs` | Disabled-by-default and end-to-end ref-export assertions (§11). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/security.md` | Project-wide trust-boundary guidance (§7). |
