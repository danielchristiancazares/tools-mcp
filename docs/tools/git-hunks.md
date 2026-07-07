# SDD: GitHunks

**Date:** 2026-07-05
**Scope:** Design contract for the `GitHunks` MCP tool.
**Source:** `tools-mcp-git/src/git/handlers/hunks.rs`

## 1. Normative Language

BCP 14 keywords apply only when written in all capitals.

## 2. Self-Containment

This document is the authoritative contract for `GitHunks`.

## 3. Scope

`GitHunks` is a gated read-only git tool that runs a pinned `git diff` command, parses raw stdout bytes as UTF-8 unified diff text, returns file/hunk records, and mints snapshot-scoped `diff_id` and hunk IDs for `GitStageHunks`.

Out of scope: durable hunk IDs, non-UTF-8 diff responses, splitting a hunk, and staging unsupported records.

## 4. Design Contract

| Property | Value |
|---|---|
| MCP tool name | `GitHunks` |
| Registration gate | `MCP_ENABLE_GIT=true` |
| Owning crate | `tools-mcp-git` |
| Handler | `handle_git_hunks` (`tools-mcp-git/src/git/handlers/hunks.rs`) |
| Schema | `tools-mcp-git/src/tools.rs` |

Invariants:

- MUST reject invalid literal path filters before spawning git.
- MUST reject literal paths targeting `.git` metadata, including detectable Windows 8.3 aliases such as `GIT~1`.
- MUST require `working_dir` to be the worktree root.
- MUST perform authority-bounded manual repository discovery and validate a real `<worktree>/.git` directory before the first git subprocess.
- MUST reject non-SHA-1 repositories with `unsupported_object_format` in v1.
- MUST reject sparse checkout, sparse-index, split-index metadata, and other required lowercase `.git/index` extensions with `unsupported_repository_metadata` in v1, including split-index `link` and sparse-index `sdir`.
- MUST reject authority-contained symlinked object-store metadata directories and fanout directories with `unsupported_repository_metadata` in v1, and authority-escaping metadata symlinks with `git_metadata_outside_authority`.
- MUST revalidate the resolved repository metadata identity before diff enumeration; same-path metadata replacement returns `repo_identity_changed`, while authority escapes retain `git_metadata_outside_authority` precedence.
- MUST reject truncated or non-UTF-8 diff output before returning IDs.
- MUST return unsupported records with machine-readable `unsupported_reason` where safely parsed.
- MUST mark malformed, all-zero, or non-regular-mode `index` extended headers unsupported in v1.
- MUST return `recommended_next_action_template` for `GitStageHunks`.

## 5. Design Goals

- Make hunk selection non-interactive and deterministic.
- Keep IDs scoped to one repository identity, staged flag, context, path list, and raw diff bytes.
- Surface unsupported changes explicitly.

## 6. Tool Specification

| Field | Type | Required | Default | Constraints |
|---|---|---|---|---|
| `staged` | boolean | No | `false` | Uses `git diff --cached` when true |
| `paths` | string array | No | `[]` | Literal repo-relative POSIX-style filters |
| `context` | integer | No | `3` | `>= 0` |
| `max_bytes` | integer | No | `200000` | Clamped to output max |
| `working_dir` | string | No | server cwd | Must be worktree root |
| `timeout_ms` | integer | No | `30000` | `>= 100`, clamped above max |
| `include_advanced_templates` | boolean | No | `false` | Adds `stage_only` template |

Response fields include `diff_id`, `staged`, `context`, `paths`, `diff_bytes`, `counts`, `recommended_next_action`, `recommended_next_action_template`, and `files[]` with `hunks[]`.

## 7. Security Considerations

`GitHunks` is read-only but reads repository content through Git. It assumes repo-local config/attributes and the Git executable path are trusted. It performs bounded manual repository discovery before the first git subprocess, disables selected diff behavior and environment influences, rejects linked, sparse, split-index, authority-escaping metadata, and authority-contained symlinked object-store metadata layouts in v1, revalidates stable repository metadata before diff enumeration, and does not inspect submodules.

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `MCP_ENABLE_GIT` | unset | Only literal `true` registers this tool. |

## 9. Code Anchors

| Claim | File |
|---|---|
| Tool schema and registration | `tools-mcp-git/src/tools.rs` |
| Handler, path validation, parser, IDs | `tools-mcp-git/src/git/handlers/hunks.rs` |
| Raw byte capture | `tools-mcp-git/src/git/mod.rs` |

## 10. Examples

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "GitHunks",
    "arguments": {"working_dir": "/repo", "context": 3}
  }
}
```

## 11. Testing

Parser, path validation, metadata validation, and repository identity revalidation unit tests live in `tools-mcp-git/src/git/handlers/hunks.rs`, including unknown extended-metadata rejection before a record can be treated as supported, leading pre-record data rejection, malformed/truncated hunk body rejection before hunk IDs are minted, LF/CRLF/bare-CR hunk byte preservation, no-final-newline marker handling, parser complexity caps, deterministic generated malformed-input fail-closed coverage, deterministic pseudo-random parser no-panic/error-code/ID-uniqueness coverage, generated ID determinism/uniqueness coverage, `.git` file indirection rejection, non-empty object alternates rejection, authority-contained metadata symlink rejection, same-path `.git` replacement detection, stable `HEAD`/`config`/`packed-refs`/`logs/HEAD` content-change detection, `refs`/object-store info/pack/existing fanout replacement detection, alternates anchor deletion detection, new fanout allowance, and authority-escape precedence. Integration coverage exercises enumeration through the MCP server, including invalid request-shape rejection, invalid literal paths, path-list caps, `max_bytes` truncation, binary and mode-only unsupported hunkless records, linked-worktree metadata rejection, authority-escaping object metadata rejection, explicit subdirectory `working_dir` rejection, omitted `working_dir` from a server subdirectory authority, the safe default `recommended_next_action_template`, and the opt-in-only `advanced_stage_only_template`.

## 12. Open Questions

None for the v1 contract. Additional parser property tests remain useful future hardening, but they do not expand the current guarantees beyond the tested UTF-8, bounded, fail-closed parser contract.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Are IDs durable? | No. Re-run `GitHunks` after any index/worktree change. |

## 14. References

| Source | Use |
|---|---|
| `docs/tools/git-stage-hunks.md` | Consumer of `diff_id` and hunk IDs |
