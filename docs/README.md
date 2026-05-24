# tools-mcp Documentation

This directory holds the design contracts for every MCP tool the server registers, plus cross-cutting documentation for the server itself.

Each tool has its own Spec-Driven Design (SDD) document under `tools/`. SDDs are the authoritative behavioral contract: when implementation diverges from an SDD, the SDD is corrected if the implementation was the right behavior, or the implementation is corrected if the SDD was the right contract.

The SDD template is at [`TEMPLATE_TOOL_SDD.md`](./TEMPLATE_TOOL_SDD.md).

## Tool SDDs

### Web

| Tool | Doc | Status |
|---|---|---|
| `WebFetch` | [tools/webfetch.md](./tools/webfetch.md) | Written |

### Search

| Tool | Doc | Status |
|---|---|---|
| `Search` | [tools/search.md](./tools/search.md) | Written |
| `search_context` | [tools/search-context.md](./tools/search-context.md) | Written |
| `SemanticIndex` | [tools/semantic-index.md](./tools/semantic-index.md) | Written |
| `SemanticSearch` | [tools/semantic-search.md](./tools/semantic-search.md) | Written |

### Files

| Tool | Doc | Status |
|---|---|---|
| `Read` | [tools/read.md](./tools/read.md) | Written |
| `Edit` | [tools/edit.md](./tools/edit.md) | Written |
| `Write` | [tools/write.md](./tools/write.md) | Written |
| `Delete` | [tools/delete.md](./tools/delete.md) | Written |
| `Move` | [tools/move.md](./tools/move.md) | Written |
| `Copy` | [tools/copy.md](./tools/copy.md) | Written |
| `ListDir` | [tools/list-dir.md](./tools/list-dir.md) | Written |
| `Glob` | [tools/glob.md](./tools/glob.md) | Written |
| `Outline` | [tools/outline.md](./tools/outline.md) | Written |

### Shell

| Tool | Doc | Status |
|---|---|---|
| `Pwsh` | [tools/pwsh.md](./tools/pwsh.md) | Written |

### Git

| Tool | Doc | Status |
|---|---|---|
| `git_snapshot` | [tools/git-snapshot.md](./tools/git-snapshot.md) | Written |
| `GitStatus` | [tools/git-status.md](./tools/git-status.md) | Written |
| `GitDiff` | [tools/git-diff.md](./tools/git-diff.md) | Written |
| `GitRestore` | [tools/git-restore.md](./tools/git-restore.md) | Written |
| `GitAdd` | [tools/git-add.md](./tools/git-add.md) | Written |
| `GitCommit` | [tools/git-commit.md](./tools/git-commit.md) | Written |
| `GitLog` | [tools/git-log.md](./tools/git-log.md) | Written |
| `GitBranch` | [tools/git-branch.md](./tools/git-branch.md) | Written |
| `GitCheckout` | [tools/git-checkout.md](./tools/git-checkout.md) | Written |
| `GitStash` | [tools/git-stash.md](./tools/git-stash.md) | Written |
| `GitShow` | [tools/git-show.md](./tools/git-show.md) | Written |
| `GitBlame` | [tools/git-blame.md](./tools/git-blame.md) | Written |

### Health

| Tool | Doc | Status |
|---|---|---|
| `Ping` | [tools/ping.md](./tools/ping.md) | Written |

## Cross-Cutting

| Topic | Doc | Status |
|---|---|---|
| Architecture | [architecture.md](./architecture.md) | Stub |
| MCP protocol | [protocol.md](./protocol.md) | Stub |
| Configuration | [configuration.md](./configuration.md) | Stub |
| Security | [security.md](./security.md) | Stub |
| Dependencies | [dependencies.md](./dependencies.md) | Stub |

## Pre-existing Documents

The following documents predate the SDD restructure and are retained as-is:

- [`hauberk-in-memory-search-srd.md`](./hauberk-in-memory-search-srd.md) — Search backend design record
- [`tools-mcp-threat-model.md`](./tools-mcp-threat-model.md) — Security threat model
- [`plans/PLAN_IN_MEMORY_FUZZY_SEARCH.md`](./plans/PLAN_IN_MEMORY_FUZZY_SEARCH.md) — Implementation plan
- [`plans/PLAN_SEARCH_BACKEND_PERFORMANCE.md`](./plans/PLAN_SEARCH_BACKEND_PERFORMANCE.md) — Performance plan

## Document Status Legend

- **Written** — Adopted contract; reflects current code.
- **Stub** — Document does not yet exist; the row is a placeholder for the planned SDD.
- **Draft** — Document exists but is marked as a work-in-progress proposal.
