# SDD: GitApply

**Date:** 2026-07-05
**Scope:** Design contract for the `GitApply` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/apply.rs`

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 when, and only when, they appear in all capitals.

## 2. Self-Containment

This document is the authoritative design contract for `GitApply`. If code and this document diverge, reconcile the divergence in favor of the correct behavior.

## 3. Scope

`GitApply` is a gated git mutation primitive that feeds an agent-supplied unified diff to `git apply` over stdin. It accepts only v1-supported tracked textual modified-file records. It does not replace `GitHunks` plus `GitStageHunks` for normal agent commit splitting.

Out of scope: added/deleted files, binary patches, renames/copies, type/mode changes, submodules, linked worktrees, `.git` file indirection, and arbitrary git-apply flags.

## 4. Design Contract

| Property | Value |
|---|---|
| MCP tool name | `GitApply` |
| Registration gate | `MCP_ENABLE_GIT=true` |
| Owning crate | `tools-mcp-git` |
| Handler | `handle_git_apply` (`tools-mcp-git/src/git/handlers/apply.rs`) |
| Schema | `tools-mcp-git/src/tools.rs` |

Invariants:

- MUST run only when the git tool family is registered.
- MUST feed patch bytes through stdin, not temp files.
- MUST reject empty patches, oversized patches, NUL bytes, unsupported diff records, unmerged indexes, and non-tracked or non-regular targets before mutation.
- MUST reject patch paths targeting `.git` metadata, including detectable Windows 8.3 aliases such as `GIT~1`.
- MUST reject malformed, all-zero, or non-regular-mode `index` extended headers in v1.
- MUST reject patch targets with `intent-to-add`, `skip-worktree`, or `assume-unchanged` index flags in v1.
- MUST require `working_dir` to be the repository worktree root for this v1 implementation.
- MUST perform authority-bounded manual repository discovery and validate a real `<worktree>/.git` directory before the first git subprocess.
- MUST reject non-SHA-1 repositories with `unsupported_object_format` in v1.
- MUST reject sparse checkout, sparse-index, split-index metadata, and other required lowercase `.git/index` extensions with `unsupported_repository_metadata` in v1, including split-index `link` and sparse-index `sdir`.
- MUST reject authority-contained symlinked object-store metadata directories and fanout directories with `unsupported_repository_metadata` in v1, and authority-escaping metadata symlinks with `git_metadata_outside_authority`.
- MUST revalidate the resolved repository metadata identity before later probes/mutations; same-path metadata replacement returns `repo_identity_changed`, while authority escapes retain `git_metadata_outside_authority` precedence.
- MUST set ordinary git subprocess stdin to null through the shared runner, and patch subprocess stdin to a bounded pipe.
- MUST NOT expose `--unsafe-paths`, arbitrary `-p`, `--reject`, include/exclude filters, or shell interpolation.

## 5. Design Goals

- Provide a low-level patch primitive for callers that already have a supported textual patch.
- Keep the common hunk commit workflow on `GitHunks` and `GitStageHunks`.
- Preserve existing git response envelope fields while adding semantic state fields.

## 6. Tool Specification

| Field | Type | Required | Default | Constraints |
|---|---|---|---|---|
| `patch` | string | Yes | none | Non-empty; byte length <= `MAX_GIT_STDIN_BYTES` |
| `target` | string | No | `cached` | `cached`, `index_worktree`, `worktree` |
| `check_only` | boolean | No | `false` | Adds `--check` |
| `reverse` | boolean | No | `false` | Adds `-R` |
| `three_way` | boolean | No | `false` | Rejected with `target=worktree` |
| `recount` | boolean | No | `true` | Adds `--recount` |
| `unidiff_zero` | boolean | No | `false` | Adds `--unidiff-zero` |
| `whitespace` | string | No | `nowarn` | `nowarn`, `warn`, `fix`, `error`, `error-all` |
| `working_dir` | string | No | server cwd | Must resolve to worktree root |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped above max |

Success states:

- `state="checked"` when `check_only=true` succeeds.
- `state="applied"` when a non-check apply succeeds.

Failure states include `failed` and `state_unknown`. Check-only git nonzero exits are determinate `failed` because `--check` is non-mutating. Index-lock contention for `target="cached"` and `target="index_worktree"` is a determinate `failed` response with top-level `error_type="index_locked"`; `target="worktree"` does not write the index and is not blocked by a fixture index lock. Other non-check git nonzero exits are `state_unknown` with `state_unknown_reason="unproved_git_nonzero"` unless they are part of a specifically verified failure class. Non-check `three_way` failures probe the index for unmerged entries and report `state_unknown_reason="three_way_conflict"` with `conflicted=true` when conflicts are observed, otherwise `three_way_indeterminate`.

Responses keep the standard git envelope and add `state`, `applied`, `checked`, `target`, `reverse`, `three_way`, optional `state_unknown_reason`, optional top-level `error_type`, optional `conflicted`, and stdin diagnostic fields.

## 7. Security Considerations

`GitApply` is a mutating tool. It assumes the repository, remaining repo-local config, hooks, attributes, and Git executable path are trusted by the operator. It performs bounded manual repository discovery before the first git subprocess, pins selected git config, scrubs selected `GIT_*` environment variables, pins `GIT_NO_REPLACE_OBJECTS=1`, rejects linked, sparse, split-index, common-dir indirection, per-worktree config, repo-config include, selected path-valued core metadata, shallow repository, grafts, and replace-ref metadata layouts in v1, revalidates stable repository metadata before later git invocations, and rejects worktree writes through symlinked, reparse-point, non-regular, or hardlinked paths. Direct-child timeout/drop cleanup is not a process-tree sandbox. MCP cancellation is not rollback; a cancelled apply can still mutate, so callers should inspect `GitStatus` and `GitDiff` before further mutation. If post-exec identity verification fails after a git success, the response is failure-shaped with `state="state_unknown"`.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset | Only literal `true` registers this tool. |

## 9. Code Anchors

| Claim | File |
|---|---|
| Tool schema and registration | `tools-mcp-git/src/tools.rs` |
| Handler and validation | `tools-mcp-git/src/git/handlers/apply.rs` |
| Shared stdin runner | `tools-mcp-git/src/git/mod.rs` |
| Constants | `tools-mcp-core/src/config.rs` |

## 10. Examples

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitApply",
    "arguments": {
      "working_dir": "/repo",
      "check_only": true,
      "patch": "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n"
    }
  }
}
```

## 11. Testing

Current targeted coverage lives in `tools-mcp-git/src/git/handlers/apply.rs`, `tools-mcp-git/src/git/handlers/hunks.rs`, and `tools-mcp-git/src/git/mod.rs`, including stdin delivery, shared stdout/stderr cap boundaries, patch byte-length boundaries, multibyte byte-length validation, patch complexity cap boundaries, closed support-matrix pre-apply rejection, apply success/failure/timeout/stdin-delivery classification precedence, three-way nonzero conflict classification when the index has unmerged entries, cached and `index_worktree` three-way conflict response fields, and repository identity revalidation for same-path `.git` replacement, stable `HEAD`/`config` content changes, and authority-escaping metadata. Server integration coverage exercises registration, invalid request-shape rejection, successful cached apply, reverse cached apply, non-check `worktree`/`index_worktree` target semantics, `index_locked` classification for index-writing targets plus the `worktree` negative control, explicit subdirectory `working_dir` rejection, omitted `working_dir` from a server subdirectory authority, pre-existing unmerged-index rejection, unproved non-check failure classification, and hunk staging workflow.

## 12. Open Questions

None for the v1 contract. Additional broad stress, TOCTOU, and fault-injection matrices remain useful future hardening, but they do not expand the current guarantees beyond the tested support matrix and documented trusted-repository boundaries.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Should agents prefer this over hunk tools? | No. Prefer `GitHunks` plus default `GitStageHunks` for commit splitting. |

## 14. References

| Source | Use |
|---|---|
| `docs/tools/git-hunks.md` | Enumeration workflow |
| `docs/tools/git-stage-hunks.md` | Commit-preparation workflow |
