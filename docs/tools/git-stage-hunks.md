# SDD: GitStageHunks

**Date:** 2026-07-05
**Scope:** Design contract for the `GitStageHunks` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/apply.rs`

## 1. Normative Language

BCP 14 keywords apply only when written in all capitals.

## 2. Self-Containment

This document is the authoritative contract for `GitStageHunks`.

## 3. Scope

`GitStageHunks` is the gated mutating hunk workflow. It recomputes a `GitHunks` diff, validates the caller's `diff_id` and selected `hunk_ids`, reconstructs a supported patch, runs `git apply --cached`, verifies the result, and by default prepares a commit-ready staged group.

Out of scope: splitting or editing a hunk, same-path staged+unstaged mixed-direction workflows, unsupported change records, and bypassing commit hooks.

## 4. Design Contract

| Property | Value |
|---|---|
| MCP tool name | `GitStageHunks` |
| Registration gate | `MCP_ENABLE_GIT=true` |
| Owning crate | `tools-mcp-git` |
| Handler | `handle_git_stage_hunks` (`tools-mcp-git/src/git/handlers/apply.rs`) |
| Schema | `tools-mcp-git/src/tools.rs` |

Invariants:

- MUST reject malformed, duplicate, unknown, unsupported, stale, or wrong-direction hunk IDs before applying.
- MUST reject literal paths targeting `.git` metadata, including detectable Windows 8.3 aliases such as `GIT~1`.
- MUST reject hunks from malformed, all-zero, or non-regular-mode `index` extended headers in v1.
- MUST reject selected paths with `intent-to-add`, `skip-worktree`, or `assume-unchanged` index flags in v1.
- MUST reject selected paths that also have opposite-direction changes with `mixed_direction_file`.
- MUST return `direction_check_unavailable` when the opposite staged/unstaged diff cannot be enumerated after the source diff matches; stale source mismatches MAY include `direction_check_unavailable=true` and `cause_error_type`.
- MUST default to `action="prepare_commit"`.
- MUST require a clean full index before default `prepare_commit`.
- MUST perform authority-bounded manual repository discovery and validate a real `<worktree>/.git` directory before the first git subprocess through the shared resolver.
- MUST reject non-SHA-1 repositories with `unsupported_object_format` in v1 through the shared repository resolver.
- MUST reject sparse checkout, sparse-index, split-index metadata, and other required lowercase `.git/index` extensions with `unsupported_repository_metadata` in v1 through the shared repository resolver, including split-index `link` and sparse-index `sdir`.
- MUST reject authority-contained symlinked object-store metadata directories and fanout directories with `unsupported_repository_metadata` in v1 through the shared repository resolver, and authority-escaping metadata symlinks with `git_metadata_outside_authority`.
- MUST revalidate the resolved repository metadata identity before recompute, preflight, apply, and verification git invocations; same-path metadata replacement returns `repo_identity_changed`, while authority escapes retain `git_metadata_outside_authority` precedence.
- MUST return `commit_ready=true` only after post-apply verification succeeds.
- MUST return `commit_ready=false` for `stage_only`, `unstage`, and every error path.

## 5. Design Goals

- Give agents one safe default path for atomic staged groups.
- Avoid interactive `git add -p`.
- Return a ready-to-fill `GitCommit` template only when the staged group is verified.

## 6. Tool Specification

| Field | Type | Required | Default | Constraints |
|---|---|---|---|---|
| `diff_id` | string | Yes | none | `sha256:<64 lowercase hex>` |
| `hunk_ids` | string array | Yes | none | Non-empty, unique, max 10000 |
| `action` | string | No | `prepare_commit` | `prepare_commit`, `stage_only`, `unstage` |
| `context` | integer | No | `3` | Must match enumeration |
| `paths` | string array | No | `[]` | Must match enumeration scope |
| `max_bytes` | integer | No | `200000` | Recompute capture cap |
| `working_dir` | string | No | server cwd | Must be worktree root |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped above max |
| `commit_type`, `commit_scope`, `commit_message` | string | No | placeholders | Used only in returned commit template |

Success fields include `state`, `action`, `source_diff_id`, `pre_apply_diff_id`, `requested_hunk_ids`, `applied_hunk_ids`, `post_apply_source_diff_id`, `post_apply_target_diff_id`, `post_apply_staged_diff_id`, `post_apply_unstaged_diff_id`, `verification_state`, `commit_ready`, and optionally `commit_call_template`. Default `prepare_commit` success also includes `pre_commit_verification` plus full-index verification fields such as `full_index_clean_before`, `full_index_verified_after`, `post_apply_full_staged_diff_id`, `post_apply_full_unstaged_diff_id`, and `next_actions`.

Post-apply verification compares the selected hunk-body multiset across the scoped source and target diffs, requires unrequested scoped hunk-body counts to remain unchanged, and performs blob-level expected-result verification for selected index blobs. For `prepare_commit`, the full cached diff must match exactly the selected group and the full unstaged diff must match the pre-apply full-unstaged baseline minus the selected group before `commit_ready=true` is returned. Unselected files in the full unstaged baseline must keep the same parsed diff inventory, so same-body relocations in unrelated files are treated as commit-group mismatches. Post-apply verification failures use `verification_unavailable`, `verification_mismatch`, `commit_group_verification_unavailable`, or `commit_group_verification_mismatch` and return `commit_ready=false`.

## 7. Security Considerations

`GitStageHunks` mutates the index and object store through `git apply --cached`. It assumes trusted remaining repo-local config/hooks/attributes and a trusted Git executable, performs bounded manual repository discovery before the first git subprocess, rejects linked, sparse, split-index, common-dir indirection, per-worktree config, repo-config include, selected path-valued core metadata, shallow repository, grafts, replace-ref metadata, authority-escaping metadata, and authority-contained symlinked object-store metadata layouts in v1, and revalidates stable repository metadata around recompute/apply/verification git invocations. Index writes can trigger hooks such as `post-index-change`. MCP cancellation is not rollback; a cancelled stage can still mutate, so callers should inspect `GitStatus` and `GitDiff` before further staging or committing. `commit_ready=true` proves only the pre-hook staged state; `GitCommit` hooks can still execute and mutate state.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset | Only literal `true` registers this tool. |

## 9. Code Anchors

| Claim | File |
|---|---|
| Tool schema and registration | `tools-mcp-git/src/tools.rs` |
| Handler and reconstruction | `tools-mcp-git/src/git/handlers/apply.rs` |
| Diff parsing and IDs | `tools-mcp-git/src/git/handlers/hunks.rs` |

## 10. Examples

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitStageHunks",
    "arguments": {
      "working_dir": "/repo",
      "diff_id": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "hunk_ids": ["0.0.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]
    }
  }
}
```

## 11. Testing

Unit coverage for argument construction, ID validation, forward and reverse scoped hunk-count verification, selected-count underflow rejection, full-index selected-group verification that rejects hunkless metadata and unsupported hunked records, full-unstaged baseline verification for unrelated hunks, hunkless metadata, and unselected-file same-body relocations, blob-level expected-result verification, failure-shaped stage response contracts, index-lock path matching, three-way conflict classification, and shared repository identity revalidation, including stable metadata-content and directory-anchor changes, lives in `tools-mcp-git/src/git/handlers/apply.rs` and `tools-mcp-git/src/git/handlers/hunks.rs`. Integration coverage exercises the default `GitHunks` to `GitStageHunks` loop, invalid request-shape rejection, commit-ready response/template fields, explicit `stage_only`, explicit unstaging, malformed/overflowing/duplicate/unknown/unsupported hunk IDs, ambiguous same-body subsets, invalid literal paths, path-list caps, `max_bytes` truncation, wrong-direction and stale IDs, unavailable opposite-direction checks, context/path scope mismatch, explicit subdirectory `working_dir` rejection, omitted `working_dir` from a server subdirectory authority, pre-existing unmerged-index rejection, same-path mixed-direction rejection, dirty-index rejection, resolved `index.lock` contention, and post-index hook mutation or same-body relocation of an unrelated unstaged file during path-scoped `prepare_commit`.

## 12. Open Questions

None for the v1 contract. Additional broad stress, concurrency, parser-property, and fault-injection matrices remain useful future hardening, but they do not expand the current guarantees beyond the tested verification loop and documented cancellation, hook, and trusted-repository boundaries.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | What is the default action? | `prepare_commit`. |
| 2 | Does this commit? | No. It returns a `GitCommit` template when commit-ready. |

## 14. References

| Source | Use |
|---|---|
| `docs/tools/git-hunks.md` | Required enumeration step |
| `docs/tools/git-commit.md` | Next commit step |
