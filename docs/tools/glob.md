# SDD: Glob

**Date:** 2026-05-24
**Scope:** Design contract for the `Glob` MCP tool.
**Source:** `tools-mcp-local/src/tools/glob.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Glob` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Glob` returns workspace-relative file paths matching one or more shell-style glob patterns (with brace expansion). Pattern matching uses the `glob` crate (`Pattern::matches_path_with`); directory traversal uses `ignore::WalkBuilder` via a shared per-scope snapshot cache so subsequent calls on the same root reuse the walk. Hidden files and ignore-file (`.gitignore` etc.) entries are skipped by default. The tool is owned by the `tools-mcp-local` crate; the entry point is `handle_glob` in `tools-mcp-local/src/tools/glob.rs:171`.

### 3.2 Explicitly Out of Scope

- Searching file contents. Use `Search` for regex content search.
- Listing the immediate contents of one directory. Use `ListDir`.
- Following symlinks during the walk (the underlying scope-cache walker is `WalkBuilder::follow_links(false)` — `scope_cache.rs:642`).
- Path policy enforcement on the `path` argument. `Glob` does NOT enforce the workspace policy; it walks from the given base path (see §7).
- Matching directories. The walker emits directories but the matcher skips them (`glob.rs:262-266`); only files (and through-symlinks at file granularity, when the walker surfaces them) are returned.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Glob` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_glob` (`tools-mcp-local/src/tools/glob.rs:171`) |
| Schema definition | `tools-mcp-local/src/tools/glob.rs:308-336` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:24`, invoked from `tools-mcp-server/src/composition.rs:88` |

### 4.2 Invariants

Behavioral guarantees that MUST hold on every invocation:

- **No panic.** Every error path returns a `ToolCallOutcome::err`. The handler MUST NOT panic.
- **`deny_unknown_fields`.** `GlobRequest` rejects any property outside `pattern` / `path` / `hidden` / `limit` (`glob.rs:159`).
- **`pattern` non-empty.** `validation::validate_non_empty(&req.pattern, "pattern", None)` (`glob.rs:177-179`).
- **Brace expansion is bounded.** Brace alternatives MUST be expanded innermost-first; each brace group MUST contain at most `MAX_BRACE_ALTERNATIVES = 64` parts and total expansion MUST stay below `MAX_EXPANDED_PATTERNS = 1024` patterns (`glob.rs:16-17,22-91`). Single-item braces are preserved as literals (`glob.rs:79,87`). Locked in by `expands_common_brace_patterns`, `preserves_single_item_braces_as_literals`, `expands_outer_group_when_inner_single_item_is_literal`, and `rejects_excessive_expansion_growth` (`glob.rs:381-409`).
- **Glob compilation errors are user-visible.** A pattern that the `glob` crate cannot parse returns `"invalid glob pattern: <err>. Remediation: use patterns like '**/*.rs' or 'src/*.{ts,tsx}'."` (`glob.rs:213-218`).
- **Base path validation.** `path` (default `"."`) MUST exist (`glob.rs:186-191`) and MUST be a directory (`glob.rs:192-197`). Distinct error texts for "does not exist" and "is not a directory".
- **Case-sensitive matching, literal path separators.** `MatchOptions` is `{case_sensitive: true, require_literal_separator: true, require_literal_leading_dot: !include_hidden}` (`glob.rs:221-225`). A leading `.` in a basename MUST NOT match `*` unless `hidden=true`.
- **Ignore rules honored.** The underlying walker (`ignore::WalkBuilder`) respects `.gitignore`, global gitignore, `.git/info/exclude`, and any `.ignore` files unless `no_ignore=true`. The `RepoScopeKey` passed by `Glob` sets `no_ignore: false` (`glob.rs:236`) so ignore rules always apply for this tool's calls.
- **Files only, no directories.** The matcher skips any walker entry whose `file_type` is `Dir` (`glob.rs:262-266`).
- **Sorted output, deterministic across calls.** The cache snapshot orders entries by `rendered_path` then by full path (`scope_cache.rs:687-691`), so successive calls on an unchanged tree return identical output. Locked in by `handle_glob_returns_sorted_limited_files` (`glob.rs:490`).
- **`limit` defaults and clamps.** `limit` defaults to `DEFAULT_GLOB_LIMIT = 1000` and is clamped to `[1, MAX_GLOB_LIMIT = 10_000]` (`glob.rs:183`, `tools-mcp-core/src/config.rs:19,22`).
- **Truncation flag when limit reached.** When the matched-file count reaches `limit`, the response MUST include `"truncated": true` (`glob.rs:280-283,301-303`). Locked in by `handle_glob_returns_sorted_limited_files` (`glob.rs:490`).
- **Walk deadline.** The scope cache build is bounded by `GLOB_SCOPE_CACHE_DEADLINE = 10s` (`glob.rs:14,238-239`). Timeouts produce `"glob: scope walk timed out. Remediation: narrow 'path' or reduce the search scope."` (`glob.rs:250-254`).
- **Cache reuse.** Successive `Glob` calls with identical `RepoScopeKey` reuse the cached snapshot via `Arc::ptr_eq` equality (`glob.rs:423-447`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT match directories. Directory entries returned by the walker are filtered out (`glob.rs:262-266`).
- MUST NOT follow symlinks during traversal (`follow_links(false)`).
- MUST NOT match `.dotfiles` unless `hidden=true`.
- MUST NOT exceed `limit` matches; the search MUST stop at `limit` and report `truncated: true`.
- MUST NOT expand a brace group with more than 64 alternatives or grow expansion beyond 1024 patterns.

## 5. Design Goals

- **Path patterns the way developers type them.** `**/*.rs`, `src/*.{ts,tsx}`, `**/*.{cpp,h}` — the brace expander handles the common shorthand without forcing callers to enumerate alternatives.
- **Honor repo ignore conventions.** Walking through `node_modules/`, `target/`, or `.venv/` is rarely the user's intent; the `ignore` crate enforces gitignore semantics by default.
- **Bounded scope walks.** The scope-cache snapshot is per-`(root, hidden, follow, no_ignore, max_depth)` key, so a session of `Glob` calls walks the tree once, not once per pattern.
- **Deterministic, sorted output.** Stable ordering makes diffs of `Glob` output meaningful and lets agents reason about "first N matches" consistently.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `pattern` | string | Yes | — | Non-empty; valid `glob` crate pattern after brace expansion | Glob with optional brace expansion: `*`, `?`, character classes `[abc]`, recursive `**`, and `{a,b}` alternatives. |
| `path` | string | No | `"."` | Must exist and be a directory | Base directory for the search (does not need to be inside the workspace). |
| `hidden` | boolean | No | `false` | — | When `true`, include files whose basename starts with `.` AND set the walker's hidden mode to traverse hidden directories. |
| `limit` | integer | No | `1000` (`DEFAULT_GLOB_LIMIT`) | `[1, 10000]` (`MAX_GLOB_LIMIT`) | Maximum number of matches. Search stops at this count and returns `truncated: true`. |

The schema sets `"additionalProperties": false` (`glob.rs:333`); the request type uses `#[serde(deny_unknown_fields)]` (`glob.rs:159`).

> Schema source: `tools-mcp-local/src/tools/glob.rs:312-334`

### 6.2 Behavior

1. **Parse arguments** — `ToolCallOutcome::parse_args::<GlobRequest>` (`glob.rs:172-175`).
2. **Validate `pattern` non-empty** — `validation::validate_non_empty(&req.pattern, "pattern", None)` (`glob.rs:177-179`).
3. **Resolve defaults** — `base_path = req.path.as_deref().unwrap_or(".")`; `include_hidden = req.hidden.unwrap_or(false)`; `limit = clamp_limit(req.limit, DEFAULT_GLOB_LIMIT, 1, MAX_GLOB_LIMIT)` (`glob.rs:181-183`).
4. **Validate base path** — Existence + directoryness checks; produce distinct errors (`glob.rs:185-197`).
5. **Brace expansion** — `expand_braces(&req.pattern)`. Returns one or more concrete patterns. Errors when bounds exceeded (`glob.rs:200-207`).
6. **Compile patterns** — `expanded.iter().map(|p| Pattern::new(p))`. On error: `"invalid glob pattern: <err>. Remediation: ..."` (`glob.rs:208-219`).
7. **Build match options** — `MatchOptions {case_sensitive: true, require_literal_separator: true, require_literal_leading_dot: !include_hidden}` (`glob.rs:221-225`).
8. **Build scope-cache key** — `RepoScopeKey {root: base.to_path_buf(), hidden: include_hidden, follow: false, no_ignore: false, max_depth: glob_traversal_max_depth(&expanded)}` (`glob.rs:231-237`). `glob_traversal_max_depth` walks the patterns and returns a finite depth bound iff no pattern contains `**`; otherwise `None` for unbounded depth (`glob.rs:94-152`, locked in by `derives_bounded_traversal_depth_for_finite_patterns` at `glob.rs:411-420`).
9. **Get or build snapshot** — `repo_scope_cache().get_or_build(&key, deadline)` with `deadline = now + 10s` (`glob.rs:238-256`). Cache errors map to:
   - `Walk(msg)` → `"glob walk error: <msg>. Remediation: check directory permissions or try a narrower 'path'."`
   - `Io(err)` → `"glob: I/O error: <err>. Remediation: ..."`
   - `Timeout` → `"glob: scope walk timed out. Remediation: ..."`
10. **Match** — Iterate snapshot entries. Skip `ScopeFileType::Dir`. For each remaining entry, parse `rendered_path` to a `Path` and test `patterns.iter().any(|p| p.matches_path_with(rel_path, match_options))` (`glob.rs:261-276`).
11. **Accumulate** — Push `entry.path.display().to_string()` into the result list; break when length reaches `limit` and set `truncated = true` (`glob.rs:277-284`).
12. **Build text body** — If `files` is empty, `"No files match pattern: <pattern>"`; else `files.join("\n")` (`glob.rs:286-290`).
13. **Build success envelope** — JSON payload with `content`, `isError: false`, `pattern`, `base_path`, `count`, `files`, and (when truncated) `truncated: true` (`glob.rs:292-303`). Returned via `ToolCallOutcome::ok(payload)`.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "src/a.rs\nsrc/b.rs"}],
  "isError": false,
  "pattern": "**/*.rs",
  "base_path": ".",
  "count": 2,
  "files": ["src/a.rs", "src/b.rs"]
}
```

When truncated, the payload also includes `"truncated": true`.

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Either the newline-joined match list or `"No files match pattern: <pattern>"`. |
| `isError` | boolean | Yes | Always `false` on success. |
| `pattern` | string | Yes | Echo of the caller's pattern (pre-expansion). |
| `base_path` | string | Yes | The resolved base path string (defaults to `"."`). |
| `count` | integer | Yes | Number of matches in `files`. |
| `files` | array of string | Yes | Display paths of matched files (entries from the scope-cache snapshot). |
| `truncated` | boolean | Only when `count == limit` | Present and `true` when the limit cap was hit during accumulation. |

Constructed via `ToolCallOutcome::ok(payload)` (`tools-mcp-core/src/tool_outcome.rs:30-32`).

**Tool-level error (`isError: true`):**

```json
{
  "content": [{"type": "text", "text": "<error message>"}],
  "isError": true
}
```

Errors use `ToolCallOutcome::err` (`tool_outcome.rs:35`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Argument deserialization failure | `true` | `"invalid arguments: ..."` plus class hint (`tool_outcome.rs:62-74`) |
| Empty / whitespace-only `pattern` | `true` | `"pattern is required (non-empty string)"` (`validation.rs:17`) |
| Base path does not exist | `true` | `"base path does not exist: <path>. Remediation: set 'path' to an existing directory (or omit it to use '.')."` (`glob.rs:188-190`) |
| Base path is not a directory | `true` | `"base path is not a directory: <path>. Remediation: pass a directory path to 'path'."` (`glob.rs:193-196`) |
| Brace alternative cap exceeded (`> 64`) | `true` | `"invalid glob pattern: brace group exceeded maximum of 64 alternatives. Remediation: reduce brace groups/options or use a simpler pattern."` (`glob.rs:73-77`) |
| Total expansion cap exceeded (`> 1024`) | `true` | `"invalid glob pattern: brace expansion exceeded maximum of 1024 patterns. Remediation: reduce brace groups/options or use a simpler pattern."` (`glob.rs:37-41`) |
| Invalid glob syntax (`glob::PatternError`) | `true` | `"invalid glob pattern: <err>. Remediation: use patterns like '**/*.rs' or 'src/*.{ts,tsx}'."` (`glob.rs:213-218`) |
| Scope-cache walk error | `true` | `"glob walk error: <message>. Remediation: check directory permissions or try a narrower 'path'."` (`glob.rs:241-245`) |
| Scope-cache I/O error | `true` | `"glob: I/O error: <err>. Remediation: ..."` (`glob.rs:246-249`) |
| Scope-cache timeout | `true` | `"glob: scope walk timed out. Remediation: narrow 'path' or reduce the search scope."` (`glob.rs:250-254`) |

## 7. Security Considerations

- **No path-policy enforcement.** `Glob` walks from the caller-supplied `path` (default `"."`) without `path_policy::resolve_existing_directory`. Read-only enumeration is intentionally unsandboxed, matching the design of `Read` and `ListDir`. Mutation tools enforce the workspace policy; enumeration does not.
- **Symlinks are not followed.** `WalkBuilder::follow_links(false)` (`scope_cache.rs:642`) prevents symlink-based loops and out-of-tree disclosure during recursive walks. Symlinks within the walked tree are still visible as entries; whether they match a pattern is up to the caller's pattern.
- **Ignore-file confidentiality.** The walker honors `.gitignore`, `.git/info/exclude`, and global gitignore; files explicitly listed there are not enumerated unless `no_ignore=true` is passed in the underlying key (this tool always passes `no_ignore: false`, `glob.rs:236`).
- **Brace expansion DoS bound.** Pattern compilation is bounded at `MAX_BRACE_ALTERNATIVES = 64` and `MAX_EXPANDED_PATTERNS = 1024` (`glob.rs:16-17`), keeping the expansion stage O(1024) regardless of input.
- **Walk-time bound.** The 10-second deadline (`GLOB_SCOPE_CACHE_DEADLINE = 10s`, `glob.rs:14`) prevents pathological trees from blocking the server indefinitely.
- **Result cap.** `limit ≤ 10_000` enforces a hard upper bound on payload size; even an unfiltered `**/*` returns at most this many entries.
- **Untrusted output.** Returned paths are external data; callers MUST NOT execute them or interpret them as instructions. Pathological filenames (control chars, ANSI) are returned verbatim.

## 8. Configuration

Not applicable. `Glob` reads no environment variables. The walk deadline and limit caps are compile-time constants.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 24 |
| Tool name + schema | `tools-mcp-local/src/tools/glob.rs` | 308-336 |
| Handler entry point | `tools-mcp-local/src/tools/glob.rs` | 171 |
| Request type (`deny_unknown_fields`) | `tools-mcp-local/src/tools/glob.rs` | 158-168 |
| Brace expansion bounds | `tools-mcp-local/src/tools/glob.rs` | 16-17 |
| Brace expansion algorithm | `tools-mcp-local/src/tools/glob.rs` | 22-91 |
| Traversal-depth derivation (`**` → unbounded) | `tools-mcp-local/src/tools/glob.rs` | 94-152 |
| `limit` clamp + defaults | `tools-mcp-local/src/tools/glob.rs` | 183 |
| Base path checks | `tools-mcp-local/src/tools/glob.rs` | 185-197 |
| Match options (case-sensitive, literal separator) | `tools-mcp-local/src/tools/glob.rs` | 221-225 |
| Scope-cache key | `tools-mcp-local/src/tools/glob.rs` | 231-237 |
| Walk deadline (10s) | `tools-mcp-local/src/tools/glob.rs` | 14, 238-239 |
| Directory entries skipped | `tools-mcp-local/src/tools/glob.rs` | 262-266 |
| Truncation flag | `tools-mcp-local/src/tools/glob.rs` | 280-283, 301-303 |
| Success payload | `tools-mcp-local/src/tools/glob.rs` | 292-305 |
| Underlying `WalkBuilder` configuration | `tools-mcp-local/src/tools/scope_cache.rs` | 638-647 |
| Snapshot sort order | `tools-mcp-local/src/tools/scope_cache.rs` | 687-691 |
| `DEFAULT_GLOB_LIMIT` / `MAX_GLOB_LIMIT` | `tools-mcp-core/src/config.rs` | 19, 22 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Glob",
    "arguments": {"pattern": "**/*.rs"}
  }
}
```

### 10.2 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "src/a.rs\nsrc/b.rs"}],
    "isError": false,
    "pattern": "**/*.rs",
    "base_path": ".",
    "count": 2,
    "files": ["src/a.rs", "src/b.rs"]
  }
}
```

### 10.3 Brace expansion

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Glob",
    "arguments": {"pattern": "src/*.{ts,tsx}"}
  }
}
```

Expands internally to `["src/*.ts", "src/*.tsx"]` and matches either (`glob.rs:382`).

### 10.4 Limit + truncation

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Glob",
    "arguments": {"pattern": "*.rs", "limit": 2}
  }
}
```

Response:

```json
{
  "result": {
    "content": [{"type": "text", "text": "alpha.rs\nbeta.rs"}],
    "isError": false,
    "pattern": "*.rs",
    "base_path": ".",
    "count": 2,
    "files": ["alpha.rs", "beta.rs"],
    "truncated": true
  }
}
```

Locked in by `handle_glob_returns_sorted_limited_files` (`glob.rs:490`).

### 10.5 No matches

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [{"type": "text", "text": "No files match pattern: **/*.xyz"}],
    "isError": false,
    "pattern": "**/*.xyz",
    "base_path": ".",
    "count": 0,
    "files": []
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `expands_common_brace_patterns` | `tools-mcp-local/src/tools/glob.rs:382` | `src/*.{ts,tsx}` → `["src/*.ts", "src/*.tsx"]`. |
| `preserves_single_item_braces_as_literals` | `tools-mcp-local/src/tools/glob.rs:388` | `{literal}` stays a literal, not an expansion. |
| `expands_outer_group_when_inner_single_item_is_literal` | `tools-mcp-local/src/tools/glob.rs:394` | Inner single-item brace preserved; outer brace expanded. |
| `rejects_excessive_expansion_growth` | `tools-mcp-local/src/tools/glob.rs:400` | More than `MAX_EXPANDED_PATTERNS` patterns rejected. |
| `derives_bounded_traversal_depth_for_finite_patterns` | `tools-mcp-local/src/tools/glob.rs:411` | Non-`**` patterns produce a finite max-depth; `**` produces `None`. |
| `scope_cache_returns_same_snapshot_for_repeat_glob_key` | `tools-mcp-local/src/tools/glob.rs:423` | Repeat key reuses the same `Arc<RecursiveScopeSnapshot>`. |
| `handle_glob_filters_files_in_tempdir` | `tools-mcp-local/src/tools/glob.rs:451` | `*.rs` matches only `.rs` files in the chosen dir. |
| `handle_glob_returns_sorted_limited_files` | `tools-mcp-local/src/tools/glob.rs:490` | Sorted results; `truncated: true` when `limit` reached. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Which glob crate is used? | The `glob` crate (`Pattern::matches_path_with`). Brace expansion is implemented locally (`glob.rs:22-91`) because the `glob` crate does not support brace alternatives. |
| 2 | Are ignore files (`.gitignore`) honored? | Yes. The underlying `ignore::WalkBuilder` is configured with `git_ignore(true)`, `git_global(true)`, `git_exclude(true)`, and `ignore(true)` for this tool's calls (`glob.rs:236`, `scope_cache.rs:644-647`). |
| 3 | Are hidden files matched? | No by default. Set `hidden=true` to include them. Match options also enforce `require_literal_leading_dot` when hidden is false (`glob.rs:221-225`). |
| 4 | Are directories returned? | No. Walker entries with `file_type: Dir` are skipped (`glob.rs:262-266`). |
| 5 | What's the max result count? | Defaults to 1000, capped at 10000 (`tools-mcp-core/src/config.rs:19,22`). Hitting the cap surfaces `truncated: true`. |
| 6 | How long can a walk take? | At most 10s (`GLOB_SCOPE_CACHE_DEADLINE`, `glob.rs:14`). Past that, the cache build returns `Timeout` and the tool errors with `"glob: scope walk timed out"`. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok` / `err` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty` error wording (§6.4). |
| `tools-mcp-core/src/config.rs` | `DEFAULT_GLOB_LIMIT`, `MAX_GLOB_LIMIT` (§4.2). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` invoked at line 88 (§4.1). |
| `tools-mcp-local/src/tools/glob.rs` | Handler and schema (§6.2). |
| `tools-mcp-local/src/tools/scope_cache.rs` | Scope-cache snapshot and walker configuration (§6.2). |
