# SDD: Search

**Date:** 2026-05-24
**Scope:** Design contract for the `Search` MCP tool.
**Source:** `tools-mcp-local/src/tools/search.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Search` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`Search` is the MCP tool that searches file *contents* for fixed text, identifiers, error strings, or regular expressions across a repository tree. It is registered by the `tools-mcp-local` crate. Every call is dispatched first to the in-memory trigram backend (`tools-mcp-local/src/tools/handlers/search_memory.rs:1504`); when the in-memory backend is ineligible for the query but reports a tool-level fallback is allowed, the same request is rerun through an out-of-process `ugrep` invocation (`tools-mcp-local/src/tools/handlers/ripgrep.rs:309-322`). Both backends return the same structured response envelope. The handler entry point is `handle_search` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:309`).

### 3.2 Explicitly Out of Scope

- The `search_context` tool, which wraps `Search` and expands matches into numbered file windows. See `docs/tools/search-context.md`.
- The `SemanticIndex` / `SemanticSearch` tools, which use embedding-based retrieval; they share no code with `Search`.
- JSON-RPC framing, method routing, and tool registry composition. See `docs/protocol.md` and `docs/architecture.md`.
- The `Read`, `Outline`, `Glob`, and `ListDir` tools, which surface file paths and content via independent handlers.
- The shared scope/ignore-fingerprint cache implementation in `tools-mcp-local/src/tools/scope_cache.rs`; this SDD references it only at the call-site level.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Search` |
| Aliases | None |
| Registration gate | Always registered (no env gate). |
| Owning crate | `tools-mcp-local` |
| Handler function | `handle_search` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:309`) |
| Schema definition | `tools-mcp-local/src/tools/search.rs:4-29` |
| Registration call | `tools-mcp-local/src/tools/mod.rs:38` (invoked from `tools-mcp-server/src/composition.rs:88` via `tools_mcp_local::register_tools`) |
| Cache warmer call | `tools-mcp-server/src/main.rs:78` → `tools-mcp-local/src/lib.rs:11` → `tools-mcp-local/src/tools/mod.rs:42` → `tools-mcp-local/src/tools/handlers/search_memory.rs:1439` |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **Single tool entry point, two backends.** Every call enters `handle_search` (`ripgrep.rs:309`). The in-memory backend (`search_memory.rs:1504`) is attempted first. The ugrep backend (`ripgrep.rs:338`) runs only when the memory backend returned `MemoryError { fallback_allowed: true, .. }` (`ripgrep.rs:315-319`). Per-request failures (`query_timeout`, `cancelled`) MUST set `fallback_allowed=false` so they short-circuit to a tool-level error rather than incurring a second backend pass (`search_memory.rs:79-101`).
- **Pattern is required and non-whitespace.** Arguments without `pattern`, or with a `pattern` containing only whitespace, MUST be rejected with `"pattern is required (non-empty string)"` (`tools-mcp-core/src/validation.rs:11-22`, invoked from `search_contract.rs:202`).
- **Path is required and non-whitespace.** When `path` is supplied it MUST be non-whitespace; when omitted it defaults to `"."` (`search_contract.rs:167,203`). The default `.` is the MCP server's working directory.
- **Deterministic normalization and clamping.** `max_results` MUST be clamped to `[1, 10000]` with a default of `100`; `timeout_ms` MUST be clamped to `[100, 300000]` with a default of `10000`; `fuzzy` MUST be clamped to `[1, 4]` when supplied (`search_contract.rs:177-180`).
- **Globs normalized before backend dispatch.** Glob entries MUST be trimmed of surrounding whitespace, empty entries dropped, then sorted and deduped to form the cache key (`search_contract.rs:185-195`). The original (pre-normalization) list is preserved separately as `raw_globs` and passed to the ugrep backend via `-g` to maintain ugrep-native glob semantics (`ripgrep.rs:282-288`).
- **`additionalProperties: false` and `deny_unknown_fields`.** The JSON schema sets `additionalProperties: false` (`search.rs:26`); the request struct sets `#[serde(deny_unknown_fields)]` (`search_contract.rs:10`). Unknown fields produce `isError: true` with text `"invalid arguments: ..."` (`tool_outcome.rs:61-75`).
- **No panic on failure.** Every error path returns a `ToolCallOutcome` via `ok` / `err` / `err_with`. The handler MUST NOT panic (`ripgrep.rs:546-579`).
- **Memory backend fallback transparency.** When the memory backend is ineligible and the request is rerun via ugrep, the resulting payload MUST be tagged with `backend: "ugrep"`, `fallback_reason: <static reason>`, `fallback_source: "memory"`, `fallback_error_type: <error_type>`, `fallback_available: true`, `memory_eligibility: "fallback"`, and `plan_kind: "ugrep"` (`ripgrep.rs:25-42`).
- **ugrep is executed directly, not via a shell.** The handler spawns `ugrep` / `ugrep.exe` with `Command::new` and `kill_on_drop(true)` (`ripgrep.rs:381-391,45`). Argument quoting is performed by the OS; no shell expansion occurs.
- **End-of-options marker.** The ugrep command line MUST terminate options with `--` before the pattern so patterns starting with `-` are not interpreted as flags (`ripgrep.rs:292-294`).
- **Bounded stderr capture.** ugrep stderr MUST be captured up to `MAX_UGREP_STDERR_BYTES = 16 KiB` (`ripgrep.rs:21,75-117`); anything beyond is dropped and the response notes `[stderr truncated after 16384 bytes]`.
- **Bounded output line width.** ugrep is invoked with `--width=4096` so individual output lines are wrapped before exceeding `MAX_UGREP_OUTPUT_LINE_COLUMNS` columns (`ripgrep.rs:23,246`). Independently, the response renderer further truncates any single rendered snippet to `SEARCH_SNIPPET_MAX_LINE_BYTES = 200` bytes followed by `…` (`search_contract.rs:7,223-248`), preserving the original byte length in `line_length`.
- **Deadline-driven cancellation.** ugrep is killed when the per-call deadline expires or when the inflight cancellation token fires (`ripgrep.rs:445-490`). Timeouts that occur *after* the result limit has already been hit MUST NOT be reported as `timed_out: true` (`ripgrep.rs:498-510`).
- **Path-injection defense (`--from=-` only).** When the memory file selector produces a pre-resolved file list, ugrep MUST be invoked with `--from=-` and the list MUST be filtered to exclude any path whose OS string contains LF or CR bytes (`search_file_selection.rs:374-388,659-675`). The handler additionally rejects matched paths that would require lossy UTF-8 conversion before reaching ugrep's stdin (`search_file_selection.rs:382-388`). A defense-in-depth result filter discards any path ugrep emits that is not in the authorized set (`ripgrep.rs:56-73,367-372,460-462`).
- **Glob behavior depends on backend.** The memory backend rejects globs that contain `,`, `{`, `}`, `\`, leading `!`/`^`, or trailing `/` with `fallback_reason: "unsupported_glob_syntax"` so the ugrep fallback can preserve ugrep-native semantics (`search_file_selection.rs:516-527,618-626`). The ugrep path-list builder treats the same patterns as "no path list" and lets ugrep do native globbing instead (`search_file_selection.rs:524-526`).

### 4.3 Explicitly Forbidden Shapes

- MUST NOT route a search query through a shell; only `Command::new(ugrep_binary_name())` with explicit `args(...)` is permitted (`ripgrep.rs:381-391`).
- MUST NOT pass file lists to ugrep via `--from=-` without LF/CR/non-UTF-8 rejection (`search_file_selection.rs:374-388`).
- MUST NOT report a memory-backend `query_timeout` or `cancelled` as ugrep-fallback work; per-request failures MUST surface to the caller directly (`search_memory.rs:79-101,316-321`).
- MUST NOT emit a payload whose `content[0].text` exceeds the structured event budget. The renderer enforces per-event snippet truncation to keep response size bounded (`search_contract.rs:223-248`).
- MUST NOT return `isError: false` for an ugrep invocation that exited with code `2` (error) when there was no truncation reason. Success is defined as `(truncated || status.success() || exit_code == Some(1)) && !timed_out` (`ripgrep.rs:214-225`).
- MUST NOT panic on glob compile failure, walker failure, ugrep launch failure, or stderr-read failure; each path returns a `ToolCallOutcome` (`ripgrep.rs:392-394,546-578`).
- MUST NOT add or remove fields from the success payload without updating both the unit test that asserts the field set (`search_contract.rs:706-790`) and the README schema documentation.

## 5. Design Goals

- **Fast first call, no startup cost.** The default scope is `.` (the server's CWD), and `Search` is registered eagerly. The optional warm-cache thread (`search_memory.rs:1439-1502`) populates likely first-call cache keys at server start so the first user-issued query usually hits a warm index.
- **In-memory backend by default, ugrep as a verified fallback.** The in-memory trigram backend serves common queries (`fixed_strings`, seeded regex, fuzzy with required trigram seeds) without spawning a process, and falls back transparently to ugrep for queries it cannot prove safe (multiline regex, follow-symlinks, unsupported glob syntax, regex without a required trigram seed). Parity between the two backends is tested by 24 `public_*_match_forced_ugrep` tests in `tools-mcp-local/src/tools/handlers/search_parity.rs`.
- **Defense in depth against path-list injection.** The ugrep fallback is the only path-list-aware mode (`--from=-`). LF/CR-bearing filenames could otherwise inject a second path entry pointing outside the search root. The selector rejects such paths before they reach ugrep's stdin, and the result-parsing loop further filters ugrep output against the original authorized set so a future regression cannot silently leak content (`search_file_selection.rs:374-395`, `ripgrep.rs:460-462`).
- **Predictable response shape regardless of backend.** Both backends emit the same top-level payload: `pattern`, `path`, `exit_code`, `truncated`, `timed_out`, `match_count`, `event_count`, `count` (alias for `event_count`), `matches[]`, `files[]` (`search_contract.rs:511-533`). Memory backend adds diagnostics; ugrep fallback adds metadata about why memory was skipped. Callers that only need matches MUST work without parsing diagnostics.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `pattern` | string | Yes | — | Non-whitespace | Text or regex to search for. With `fixed_strings=true`, treated as literal text (`-F`). |
| `path` | string | No | `"."` | Non-whitespace | File or directory to search. A directory is walked recursively; a file is searched as a single target. |
| `case` | string | No | `"smart"` | `"smart"` / `"sensitive"` / `"insensitive"` (case-insensitive; aliases `case-sensitive`, `case_sensitive`, `ignore`, `ignore-case`, `ignore_case` also accepted; unknown values silently coerced to `"smart"` — `search_contract.rs:69-77`) | `smart`: lowercase pattern → case-insensitive; uppercase → case-sensitive. `sensitive`: exact casing. `insensitive`: ignore case. |
| `fixed_strings` | boolean | No | `false` | — | Treat `pattern` as literal text (ugrep `-F`). Required to engage memory `WordExact`/`ShortExact` plans. |
| `word_regexp` | boolean | No | `false` | — | Whole-word match (ugrep `-w`). Memory backend supports this only for ASCII fixed-string literals bounded by word bytes; otherwise falls back to ugrep with `fallback_reason: "unsupported_word_regexp"` (`search_memory.rs:2122-2135`). |
| `glob` | array of string | No | — | — | Filter files. Globs without `/` are matched against the basename; globs with `/` are matched against the search-root-relative path (`search_file_selection.rs:472-490,597-608`). Patterns containing `,`, `{`, `}`, `\`, leading `!`/`^`, or trailing `/` are unsupported by the memory backend and route to ugrep (`search_file_selection.rs:618-626`). |
| `hidden` | boolean | No | `false` | — | Search dotfiles/hidden directories (ugrep `--hidden`, ignore walker `hidden(false)`). |
| `follow` | boolean | No | `false` | — | Follow symlinks (ugrep `--dereference`). Memory backend MUST fall back when set (`search_memory.rs:2136-2142`). |
| `no_ignore` | boolean | No | `false` | — | Bypass `.gitignore` / `.ignore` filtering (ugrep `--no-ignore-files`). |
| `context` | integer | No | `0` | `>= 0` | Lines of surrounding context per match (ugrep `-C`). |
| `max_results` | integer | No | `100` | Clamped to `[1, 10000]` | Maximum match/context events to return. When reached the response is `truncated=true`. |
| `timeout_ms` | integer | No | `10000` | Clamped to `[100, 300000]` | Per-call wall-clock budget. Exceeding it sets `timed_out=true` unless the result cap was already hit. |
| `fuzzy` | integer | No | _(none)_ | Clamped to `[1, 4]` when supplied | Approximate-match edit distance. Memory backend supports fuzzy only when the pattern can be partitioned into trigram seeds (`search_memory.rs:2117-2118`); otherwise falls back to ugrep `-Z<dist>`. |

Schema source: `tools-mcp-local/src/tools/search.rs:8-27`.
Request struct: `tools-mcp-local/src/tools/handlers/search_contract.rs:9-50` (`#[serde(deny_unknown_fields)]`).
Normalization: `tools-mcp-local/src/tools/handlers/search_contract.rs:160-183`.

### 6.2 Behavior

Ordered execution steps from `handle_search` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:309-322`). Each step lists the file:line for verification.

1. **Parse arguments and validate non-empty `pattern`/`path`.** Deserialize JSON into `SearchRequest`; reject unknown fields, then call `validation::validate_non_empty` on both `pattern` and the resolved `path` (defaulting to `"."`). Normalize globs, clamp `max_results`/`timeout_ms`/`fuzzy`. (`search_contract.rs:197-206`.)
2. **Attempt the memory backend** via `handle_memory_search(&req)` (`ripgrep.rs:315`, `search_memory.rs:1504-1728`):
   1. Build the per-query deadline from `req.timeout_ms` (`search_memory.rs:1508`).
   2. Build a `QueryPlan` from the request (`search_memory.rs:2106-2177`). The plan branches into `Exact` / `ShortExact` / `WordExact` / `Regex` / `Fuzzy`. Ineligible queries return a `MemoryError { fallback_allowed: true, .. }` whose `fallback_reason` is one of `unsupported_word_regexp`, `unsupported_follow`, `unsupported_multiline_literal`, `query_without_required_trigram`, `unsupported_non_ascii_short_literal_case`, `unsupported_glob_syntax`, `invalid_glob`, `unsupported_unicode_regex_case_insensitive`, `unsupported_unicode_regex_smart_case`, or one of the regex-dialect fallback reasons.
   3. Validate plan-specific limits (`search_memory.rs:2434-2446`).
   4. Acquire or build the index snapshot via `get_or_build_snapshot` (`search_memory.rs:1512,1730-1788`). Cache key is `IndexKey { root, hidden, follow, no_ignore, globs }`. Cache size is capped by `TOOLS_SEARCH_INDEX_CACHE_MAX_ENTRIES` (default `8`) and `TOOLS_SEARCH_INDEX_CACHE_MAX_BYTES` (default `0` = unbounded). File selection comes from the shared scope cache (`scope_cache.rs`); single-file roots are walked directly via `WalkBuilder` (`search_file_selection.rs:165,238-308`).
   5. Run phase one (candidate generation via trigram postings) and phase two (line verification), both bounded by the deadline (`search_memory.rs:1517-1566`).
   6. Validate the snapshot is still fresh (`search_memory.rs:1568-1572`); on staleness, the next call will rebuild.
   7. Build the success payload (`search_memory.rs:1581-1592`) and attach memory-backend diagnostics (`backend: "memory"`, `plan_kind`, `memory_eligibility: "eligible"`, `index_cache`, freshness fields, telemetry — `search_memory.rs:1594-1725`).
3. **Memory result → return.** When step 2 returns `Ok(outcome)`, the handler returns that outcome verbatim (`ripgrep.rs:316`).
4. **Memory failure with `fallback_allowed=true` → run ugrep, then merge fallback metadata.** Call `handle_search_ugrep(req)` (`ripgrep.rs:318,338-579`) and post-process with `add_fallback_metadata` to attach `backend: "ugrep"`, `fallback_reason`, `fallback_source: "memory"`, `fallback_error_type`, `fallback_available: true`, `memory_eligibility: "fallback"`, `plan_kind: "ugrep"` (`ripgrep.rs:25-42`).
5. **Memory failure with `fallback_allowed=false` → tool-level error.** For `query_timeout` and `cancelled` classes (`search_memory.rs:79-101`), call `MemoryError::into_tool_outcome` which returns `ToolCallOutcome::err_with` containing `backend: "memory"`, `error_type`, `fallback_reason`, `fallback_available: false`, `memory_eligibility: "error"`, a generic remediation string, plus the original `pattern`/`path`, `exit_code: null`, `truncated: false`, `timed_out`, `count: 0`, `matches: []` (`search_memory.rs:103-127`).
6. **ugrep backend pipeline** (`ripgrep.rs:338-579`):
   1. Compute deadline from `timeout_ms` (`ripgrep.rs:345`).
   2. Resolve the pre-filtered file list when the request's globs are ugrep-path-list-eligible; otherwise pass `None` (`ripgrep.rs:347,53`, `search_file_selection.rs:103-114,430-438`).
   3. If the resolved list is empty, short-circuit to a success payload with `exit_code: 1` and no events (`ripgrep.rs:348-357`).
   4. Build the authorized-path set from the resolved list, lowercased and slash-normalized on Windows (`ripgrep.rs:367-372`, `path_authorization_key` `ripgrep.rs:65-73`).
   5. Build the ugrep argv (`ripgrep.rs:233-301`). Base flags: `-r -n -H --color=never --no-group-separator --width=4096`. Conditional flags: `-Z<dist>` for fuzzy, `-F` for fixed strings, `-w` for word regexp, `-i` for case-insensitive, `-j` for smart case, `--hidden`, `--dereference`, `--no-ignore-files`, `-C <n>` for context. Either `--from=-` (when a path list is being piped) or one `-g <glob>` per raw glob (when not). Argv ends with `-- <pattern>` and, when not using `--from=-`, the search root.
   6. Spawn ugrep with `Command::new(ugrep_binary_name())` (`ripgrep.rs:381-391`). On spawn failure, return tool-level error `"ugrep error: failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: <e>"` (`ripgrep.rs:392-394,577`).
   7. If a path list is piped, spawn a stdin writer task that writes each authorized path followed by `\n` (`ripgrep.rs:396-422`).
   8. Stream stdout line-by-line; parse `path:line:text` (match) and `path-line-text` (context) using the parser in `parse_grep_line` (`ripgrep.rs:121-212`) which prefers `:`-separated matches from the start of the line and falls back to `-`-separated context from the end. The parser tolerates filenames containing `-<digits>-` and timestamps like `10:23:59`.
   9. Drop any parsed path not in the authorized set (`ripgrep.rs:460-462`).
   10. Stop streaming when `events.len() >= max_results`, when the deadline elapses, or when the inflight cancellation token fires (`ripgrep.rs:444-490`). On result-cap termination, kill the child and mark `truncated=true`; on timeout, mark `timed_out=true`; on cancellation, drain the child up to 500 ms and return tool-level error `"ugrep error: ugrep search cancelled"` (`ripgrep.rs:492-496`).
   11. Wait for the child up to 2000 ms (`ripgrep.rs:499-510`). Read bounded stderr (cap `16 KiB`, `ripgrep.rs:434,513-523`). Bound the stdin task to 2000 ms (`ripgrep.rs:525-531`).
   12. Classify success (`ripgrep.rs:214-225`): success iff `(truncated || exit success || exit 1) && !timed_out`. Exit code `1` (no matches) is success; exit code `2` (error) is failure.
   13. Render text view and structured events (`ripgrep.rs:547-576`); when stderr is non-empty, attach it as the top-level `stderr` field.
7. **Return ugrep payload.** The ugrep handler returns `ToolCallOutcome::ok(payload)` on success and `ToolCallOutcome::err(format!("ugrep error: {e:#}"))` on internal failure (`ripgrep.rs:575-578`).

#### 6.2.1 Cache warmer (optional, runs at server startup)

The server's `main.rs:78` calls `tools_mcp_local::start_search_cache_warmer` once at boot. The warmer (`search_memory.rs:1439-1502`) spawns a single background thread that:

1. Bails immediately if `TOOLS_SEARCH_INDEX_WARM_ENABLED` is set to `"false"`/`"0"`/`"no"`/`"off"` (default: enabled).
2. Sleeps `TOOLS_SEARCH_INDEX_WARM_START_DELAY_MS` ms (default 250, max 60000).
3. Resolves the current directory, then probes for a git worktree root using `git rev-parse --show-toplevel` with a hard timeout of `TOOLS_SEARCH_INDEX_WARM_GIT_TIMEOUT_MS` ms (default 2000, clamped to `[100, 30000]`); if the cwd is outside any git worktree, the warmer returns without populating.
4. Builds up to `TOOLS_SEARCH_INDEX_WARM_MAX_KEYS` warm keys (default 6, clamped to `[1, 16]`) consisting of the repo-default scope plus one per glob in `TOOLS_SEARCH_INDEX_WARM_GLOBS` (default `"*.rs,*.md"`; `"none"` disables; comma- and semicolon-separated; case-insensitive). When the cwd differs from the repo root, the same scopes are duplicated rooted at `.`.
5. Issues an internal warm-up search per key with `pattern: WARM_CACHE_PATTERN` and a budget of `TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS` ms (default 300000), sleeping `TOOLS_SEARCH_INDEX_WARM_KEY_DELAY_MS` ms (default 25) between keys.

The warmer is purely a performance optimization. Behavior of user-issued queries is identical with or without it; the only observable difference is `index_cache: "hit"` vs `index_cache: "miss"` in the memory backend response.

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "src/main.rs:7:let needle = true;\nsrc/main.rs-8-context line"}],
  "isError": false,
  "pattern": "needle",
  "path": "src",
  "exit_code": 0,
  "truncated": false,
  "timed_out": false,
  "match_count": 1,
  "event_count": 2,
  "count": 2,
  "matches": [
    {"type": "match", "data": {"path": {"text": "src/main.rs"}, "line_number": 7, "lines": {"text": "let needle = true;"}}},
    {"type": "context", "data": {"path": {"text": "src/main.rs"}, "line_number": 8, "lines": {"text": "context line"}}}
  ],
  "files": [
    {
      "path": "src/main.rs",
      "match_count": 1,
      "event_count": 2,
      "events": [
        {"type": "match", "data": {"line_number": 7, "lines": {"text": "let needle = true;"}}},
        {"type": "context", "data": {"line_number": 8, "lines": {"text": "context line"}}}
      ]
    }
  ],
  "backend": "memory"
}
```

The 12 always-present top-level fields are: `content`, `isError`, `pattern`, `path`, `exit_code`, `truncated`, `timed_out`, `match_count`, `event_count`, `count`, `matches`, `files` (`search_contract.rs:511-533`; locked by the unit test `build_search_payload_preserves_response_shape_and_count` at `search_contract.rs:706-790`).

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Rendered grep-style text view, one rendered event per line. Format: `path:line:text` for matches, `path-line-text` for context. Each event's snippet is truncated at 200 bytes (UTF-8 char boundary) with `…` appended; original length preserved in structured `line_length`. (`search_contract.rs:223-294,478-490`.) |
| `isError` | boolean | Yes | `false` on success. |
| `pattern` | string | Yes | The pattern as supplied by the caller. |
| `path` | string | Yes | The search root: caller-provided value or `"."`. |
| `exit_code` | integer / null | Yes | ugrep exit code (`0`=matches, `1`=no matches, `2`=error). Null when memory backend served the response without spawning ugrep (memory backend assigns `0` or `1` based on whether any match event was produced, `search_memory.rs:1576-1580`). |
| `truncated` | boolean | Yes | `true` when `events.len() >= max_results` was hit. |
| `timed_out` | boolean | Yes | `true` when the wall-clock deadline elapsed *before* the result cap. |
| `match_count` | integer | Yes | Count of `events` with `is_match=true` (`search_contract.rs:411-415`). |
| `event_count` | integer | Yes | Total events (matches plus context). |
| `count` | integer | Yes | Alias of `event_count`. Documented as deprecation candidate (`search_contract.rs:516-517`). |
| `matches[]` | array | Yes | Flat list. Each entry: `{type: "match"\|"context", data: {path: {text: string}, line_number: integer, lines: {text: string}, snippet_truncated?: boolean, line_length?: integer}}`. |
| `files[]` | array | Yes | Per-file grouping by contiguous path runs. Each entry: `{path: string, match_count: integer, event_count: integer, events: [...]}` where each event drops the `path` field. |
| `stderr` | string | No (ugrep only, when non-empty) | ugrep stderr capped at 16 KiB; suffixed with `[stderr truncated after 16384 bytes]` if truncated (`ripgrep.rs:84-95,569-573`). |
| `backend` | string | Yes (memory or ugrep-fallback path) | `"memory"` or `"ugrep"`. |
| `plan_kind` | string | Yes (memory or ugrep-fallback path) | `"exact"`, `"regex"`, `"fuzzy"`, or `"ugrep"`. |
| `memory_eligibility` | string | Yes (memory or fallback) | `"eligible"`, `"fallback"`, or `"error"`. |
| `fallback_reason` | string | Yes (ugrep-fallback path) | Static reason from the memory error class (e.g., `unsupported_word_regexp`, `query_without_required_trigram`). |
| `fallback_source` | string | Yes (ugrep-fallback path) | Constant `"memory"`. |
| `fallback_error_type` | string | Yes (ugrep-fallback path) | The memory backend's `error_type`. |
| `fallback_available` | boolean | Yes (ugrep-fallback path) | Constant `true` on the fallback path. |
| `index_cache` | string | Yes (memory backend) | `"hit"` or `"miss"`. |
| `index_build_deduped` | boolean | Yes (memory backend) | `true` when the snapshot was reused after another concurrent caller built it. |
| `index_state` | string | Yes (memory backend) | Freshness state (`fresh`, `stale`, ...). |
| `index_generation` | integer | Yes (memory backend) | Monotonic counter per index rebuild. |
| `indexed_files`, `indexed_bytes` | integer | Yes (memory backend) | Snapshot statistics. |
| `cache_entries`, `cache_bytes`, `cache_evictions`, `cache_max_entries`, `cache_max_bytes` | integer / null | Yes (memory backend) | Index cache telemetry. |
| `index_lookup_ms`, `phase_one_ms`, `phase_two_ms`, `freshness_check_ms` | integer | Yes (memory backend) | Per-phase timings in ms. |
| `candidate_estimate`, `candidate_count`, `candidate_seed_count`, `candidate_limit` | integer | Yes (memory backend) | Candidate-set sizing. |
| `fuzzy_*` | varied | Yes when fuzzy (memory backend) | Fuzzy-seed planning diagnostics (`search_memory.rs:1651-1680`). |
| `freshness_*` | varied | Yes (memory backend) | Snapshot freshness diagnostics. |

Memory-backend diagnostic fields are listed for completeness; callers that only need matches MUST be able to ignore every field outside the 12 always-present ones.

**Tool-level error (`isError: true`):**

Constructed via `ToolCallOutcome::err` or `ToolCallOutcome::err_with` (`tools-mcp-core/src/tool_outcome.rs:35,43`):

```json
{
  "content": [{"type": "text", "text": "pattern is required (non-empty string)"}],
  "isError": true
}
```

For memory per-request failures the payload also carries the diagnostic fields (`backend: "memory"`, `error_type`, `fallback_reason`, `fallback_available: false`, `memory_eligibility: "error"`, `remediation`, `pattern`, `path`, `exit_code: null`, `truncated: false`, `timed_out`, `count: 0`, `matches: []`) per `search_memory.rs:103-127`.

For ugrep internal failures (spawn, capture, or cancelled) the payload is the simple form: `ToolCallOutcome::err(format!("ugrep error: {msg}"))` (`ripgrep.rs:577`).

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Missing `pattern` | `true` | `"pattern is required (non-empty string)"` |
| Whitespace-only `pattern` | `true` | `"pattern is required (non-empty string)"` |
| Whitespace-only `path` | `true` | `"path is required (non-empty string)"` |
| Unknown field in `arguments` | `true` | `"invalid arguments: unknown field ..."` (with hint about not allowing unknown fields) |
| Wrong type for a field | `true` | `"invalid arguments: invalid type ..."` |
| Memory backend `query_timeout` (per-call deadline) | `true` | `"memory search timed out"` plus diagnostic fields |
| Memory backend cancelled (inflight cancellation token) | `true` | `"memory search cancelled"` plus diagnostic fields |
| Memory backend resource limit (e.g., `max_fuzzy_pattern_chars`) | `true` (when not fallback-allowed) | Class-specific message from `MemoryError::new`, plus diagnostic fields |
| ugrep binary not on PATH | `true` | `"ugrep error: failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: ..."` |
| ugrep search cancelled mid-stream | `true` | `"ugrep error: ugrep search cancelled"` |
| Path-list injection (LF/CR in matched path) | `true` | `"ugrep error: search aborted: matched path contains LF/CR bytes ..."` |
| Path-list injection (non-UTF-8 path on Unix) | `true` | `"ugrep error: search aborted: matched path contains non-UTF-8 bytes ..."` |
| ugrep exited with code `2` (genuine error) | `true` | `"Search error: <captured stderr>"` rendered into `content[0].text`; `exit_code: 2` |
| ugrep timed out before reaching result cap | `false` (still success) but `timed_out: true` | rendered events so far |
| `max_results` reached | `false` but `truncated: true` | rendered events |
| No matches found | `false`, `exit_code: 0` or `1`, empty `matches`/`files` | empty `content[0].text` |

The `content[0].text` value for argument-validation errors is wrapped by the message-hint logic in `ToolCallOutcome::parse_args` (`tools-mcp-core/src/tool_outcome.rs:61-75`).

## 7. Security Considerations

- **No path-policy enforcement on the search root.** Unlike `Read`, `Write`, `Edit`, `Delete`, `Move`, `Copy`, and `Pwsh`, the `Search` tool does NOT call into `tools-mcp-local/src/path_policy.rs`. The walker (`ignore::WalkBuilder`) and ugrep accept any path the host filesystem grants the process. Callers MUST treat the tool as having the read scope of the server process. The `Search` tool MUST NOT be relied on as a workspace boundary; that boundary, when needed, is provided by the OS sandbox or by a wrapping tool (e.g., `search_context` filters returned paths through canonicalization, see `docs/tools/search-context.md` §7).
- **Path-list injection (`--from=-`).** A single in-root filename containing `\n` would otherwise inject an attacker-chosen second path entry into ugrep's stdin file list, causing reads outside the search root. The file selector rejects LF/CR-bearing paths with `fallback_reason: "unsupported_path_separator"` before ugrep is spawned (`search_file_selection.rs:374-388,659-675`). On Unix it also rejects matched paths whose byte form is not valid UTF-8 (`search_file_selection.rs:382-388,677-680`). A defense-in-depth result filter discards any path ugrep emits that is not in the authorized set (`ripgrep.rs:56-73,367-372,460-462`). End-to-end regression test: `search_ugrep_glob_newline_path_injection_is_blocked` (`ripgrep.rs:935-1015`).
- **Regex denial-of-service.** The memory backend's regex compiler is capped by `TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES` (default 10 MiB) with `RegexBuilder::size_limit` (`search_memory.rs:2331-2350`). Patterns that can match LF, that use look-around/inline-flag constructs, or that lack a required trigram seed are rejected by the planner and routed to ugrep (`search_memory.rs:2207-2329`). The per-call `timeout_ms` (clamped to `[100, 300000]` ms) bounds wall-clock cost in either backend; cancellation is honored via the inflight cancellation token.
- **File-read trust boundary.** ugrep reads file contents directly and returns them as `lines.text`. Returned content is external data; consuming systems MUST treat `matches[].data.lines.text` and `content[0].text` as untrusted input and MUST NOT execute, eval, or interpret it as instructions.
- **Output-size caps.** Per-line snippet truncated at 200 bytes (UTF-8-safe), suffixed with `…`; structured `line_length` preserves the original byte length. Total events bounded by `max_results`, clamped to `[1, 10000]`. ugrep output column width capped at `--width=4096`. Stderr bounded at 16 KiB. (`search_contract.rs:7,223-248`, `ripgrep.rs:21,23,246`.)
- **Process lifecycle.** ugrep is spawned with `kill_on_drop(true)`; cancellation, timeout, and result-cap termination all proactively `child.kill()` before draining (`ripgrep.rs:390,469-477,485-488`).
- **No shell.** ugrep is spawned with `Command::new(...)` and explicit `args(...)`; no shell metacharacters are expanded, and patterns containing `--` / `-foo` are guarded by the `--` end-of-options marker (`ripgrep.rs:292-294,381`).

## 8. Configuration

Environment variables read by this tool at runtime. None are required; all have defaults.

| Variable | Default | Description |
|---|---|---|
| `TOOLS_SEARCH_INDEX_MAX_FILE_BYTES` | `1048576` (1 MiB) | Per-file byte cap for memory indexing (`search_memory.rs:162,29`). |
| `TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES` | `268435456` (256 MiB) | Aggregate byte cap per index key (`search_memory.rs:163-166,30`). |
| `TOOLS_SEARCH_INDEX_MAX_FILES` | `50000` | File-count cap per index (`search_memory.rs:167,31`). |
| `TOOLS_SEARCH_MAX_CANDIDATES` | `20000` | Candidate-document cap before phase-two verification (`search_memory.rs:168,32`). |
| `TOOLS_SEARCH_MAX_FUZZY_PATTERN_CHARS` | `512` | Maximum Unicode-scalar pattern length for fuzzy queries (`search_memory.rs:169-172,33`). |
| `TOOLS_SEARCH_MAX_FUZZY_VERIFIED_LINES` | `200000` | Phase-two verified-line cap for fuzzy queries (`search_memory.rs:173-176,34`). |
| `TOOLS_SEARCH_MAX_FUZZY_LINE_CHARS` | `16384` | Max line length considered for fuzzy verification (`search_memory.rs:177-180,35`). |
| `TOOLS_SEARCH_MAX_SHORT_LITERAL_SCAN_LINES` | `200000` | Scan budget for short-literal plans (`search_memory.rs:181-184,36`). |
| `TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES` | `10485760` (10 MiB) | `RegexBuilder::size_limit` for memory regex matcher compilation (`search_memory.rs:185-188,37`). |
| `TOOLS_SEARCH_INDEX_CACHE_MAX_ENTRIES` | `8` | Index snapshot cache capacity (`search_memory.rs:5559-5565,607`). |
| `TOOLS_SEARCH_INDEX_CACHE_MAX_BYTES` | `0` (unbounded) | Optional total-bytes cap on cached index snapshots (`search_memory.rs:5567-5575,608`). |
| `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE` | `false` | Internal safety valve; when truthy, forces full-scope freshness validation for `no_ignore=false` queries (`search_memory.rs:5536-5538`). |
| `TOOLS_SEARCH_INDEX_WARM_ENABLED` | `true` | Enables the startup warm-cache thread (`search_memory.rs:894,1441-1444`). |
| `TOOLS_SEARCH_INDEX_WARM_START_DELAY_MS` | `250` (clamped to ≤ 60000) | Delay before warm thread runs (`search_memory.rs:895-901,39`). |
| `TOOLS_SEARCH_INDEX_WARM_KEY_DELAY_MS` | `25` (clamped to ≤ 60000) | Sleep between warm keys (`search_memory.rs:902-908,40`). |
| `TOOLS_SEARCH_INDEX_WARM_MAX_KEYS` | `6` (clamped to `[1, 16]`) | Maximum keys probed during warm-up (`search_memory.rs:910-914,41`). |
| `TOOLS_SEARCH_INDEX_WARM_GLOBS` | `"*.rs,*.md"` (case-insensitive `"none"` disables) | Comma/semicolon-separated glob list probed during warm-up (`search_memory.rs:5540-5557,42`). |
| `TOOLS_SEARCH_INDEX_WARM_GIT_TIMEOUT_MS` | `2000` (clamped to `[100, 30000]`) | `git rev-parse` timeout used by the warmer (`search_memory.rs:916-921,43`). |
| `TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS` | `300000` | Per-key timeout for the warm-up probe call (`search_memory.rs:2087-2090,38`). |

Process-wide variables that affect responses indirectly: `TOOLS_PRETTY_JSON` does **not** affect this tool's response shape because the handler builds the response object directly with `ToolCallOutcome::ok` rather than `ok_json_content`.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-local/src/tools/mod.rs` | 38 |
| Tool name + schema | `tools-mcp-local/src/tools/search.rs` | 4-29 |
| Handler entry point | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 309-322 |
| Request struct (`deny_unknown_fields`) | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 9-50 |
| Argument validation (`pattern`/`path` non-empty) | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 197-206 |
| Clamp `max_results`, `timeout_ms`, `fuzzy` | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 177-180 |
| Glob normalization (trim / drop empty / sort / dedup) | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 185-195 |
| Memory backend entry | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 1504-1728 |
| Memory eligibility/plan selection | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 2106-2177 |
| Per-request failure (no fallback) flag | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 79-101 |
| Memory error → tool outcome (with diagnostics) | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 103-127 |
| Memory diagnostic fields appended to payload | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 1594-1725 |
| ugrep argv builder | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 233-301 |
| ugrep spawn / kill-on-drop | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 381-394 |
| ugrep streaming loop / cancellation | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 443-496 |
| ugrep success classifier | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 214-225 |
| Bounded stderr / output width | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 21, 23, 75-117, 246 |
| `parse_grep_line` (path/line parsing) | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 121-212 |
| Defense-in-depth result filter | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 56-73, 367-372, 460-462 |
| `add_fallback_metadata` | `tools-mcp-local/src/tools/handlers/ripgrep.rs` | 25-42 |
| Path-list LF/CR rejection | `tools-mcp-local/src/tools/handlers/search_file_selection.rs` | 374-388, 659-675 |
| Non-UTF-8 path rejection | `tools-mcp-local/src/tools/handlers/search_file_selection.rs` | 382-388, 677-685 |
| `FileSelector::for_ugrep_path_list` | `tools-mcp-local/src/tools/handlers/search_file_selection.rs` | 103-114 |
| Glob compile / unsupported syntax | `tools-mcp-local/src/tools/handlers/search_file_selection.rs` | 499-573, 618-626 |
| Response payload shape | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 511-533 |
| Snippet truncation (200 bytes / UTF-8 boundary) | `tools-mcp-local/src/tools/handlers/search_contract.rs` | 7, 223-248 |
| Warm-cache thread launcher | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 1439-1502 |
| `WarmCacheConfig::from_env` | `tools-mcp-local/src/tools/handlers/search_memory.rs` | 891-925 |
| Warm-cache call site | `tools-mcp-server/src/main.rs` | 78 |

## 10. Examples

### 10.1 Minimal request (memory-eligible fixed string)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Search",
    "arguments": {
      "pattern": "handle_read_file",
      "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
      "fixed_strings": true,
      "max_results": 20
    }
  }
}
```

### 10.2 Success response (memory backend)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "tools-mcp-local/src/tools/handlers/read_file.rs:20:pub async fn handle_read_file(_id: Option<Value>, args: Value) -> ToolCallOutcome {"}],
    "isError": false,
    "pattern": "handle_read_file",
    "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
    "exit_code": 0,
    "truncated": false,
    "timed_out": false,
    "match_count": 1,
    "event_count": 1,
    "count": 1,
    "matches": [
      {"type": "match", "data": {"path": {"text": "tools-mcp-local/src/tools/handlers/read_file.rs"}, "line_number": 20, "lines": {"text": "pub async fn handle_read_file(_id: Option<Value>, args: Value) -> ToolCallOutcome {"}}}
    ],
    "files": [
      {
        "path": "tools-mcp-local/src/tools/handlers/read_file.rs",
        "match_count": 1,
        "event_count": 1,
        "events": [
          {"type": "match", "data": {"line_number": 20, "lines": {"text": "pub async fn handle_read_file(_id: Option<Value>, args: Value) -> ToolCallOutcome {"}}}
        ]
      }
    ],
    "backend": "memory",
    "plan_kind": "exact",
    "memory_eligibility": "eligible",
    "index_cache": "hit",
    "index_state": "fresh",
    "index_generation": 12,
    "indexed_files": 1,
    "indexed_bytes": 5876
  }
}
```

(Diagnostic fields elided for brevity; the response includes the full set listed in §6.3.)

### 10.3 ugrep fallback (regex without required trigram seed)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "Search",
    "arguments": {
      "pattern": "^[0-9]+$",
      "path": "fixtures/",
      "fixed_strings": false,
      "no_ignore": true
    }
  }
}
```

Response (truncated):

```json
{
  "result": {
    "content": [{"type": "text", "text": "fixtures/numbers.txt:1:12345"}],
    "isError": false,
    "pattern": "^[0-9]+$",
    "path": "fixtures/",
    "exit_code": 0,
    "truncated": false,
    "timed_out": false,
    "match_count": 1,
    "event_count": 1,
    "count": 1,
    "backend": "ugrep",
    "fallback_reason": "query_without_required_trigram",
    "fallback_source": "memory",
    "fallback_error_type": "unsupported_regex_dialect",
    "fallback_available": true,
    "memory_eligibility": "fallback",
    "plan_kind": "ugrep"
  }
}
```

### 10.4 Argument validation failure (missing `pattern`)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Search",
    "arguments": {"path": "src"}
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "invalid arguments: missing field `pattern`. Required fields are missing; provide all required arguments per the tool schema."}],
    "isError": true
  }
}
```

### 10.5 LF-in-path injection blocked

A request whose glob would select an in-root pathname containing a literal `\n` byte is aborted before ugrep is spawned. End-to-end coverage: `search_ugrep_glob_newline_path_injection_is_blocked` (`ripgrep.rs:935-1015`). The response is `isError: true`, `backend: "ugrep"`, and `content[0].text` starts with `"ugrep error: search aborted"` and contains the substring `"LF/CR"`.

## 11. Testing

### 11.1 Integration tests (MCP envelope, spawned server binary)

| Test | File | What it covers |
|---|---|---|
| `test_search_fixed_string_default_smart_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:270-302` | Default `Search` call against a known file uses memory backend (`backend: "memory"`) and returns at least one match. |
| `test_search_literal_default_options_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:304-343` | Smart-case literal hits the memory backend, surfaces the uppercased match. |
| `test_search_seeded_regex_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:389-439` | Regex with a required trigram seed runs in memory and rejects cross-line false positives. |
| `test_search_common_seeded_regex_escape_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:441-488` | Regex with escape (`\d+`) runs in memory and rejects unmatched candidates. |
| `test_search_unseeded_regex_falls_back_to_ugrep` | `tools-mcp-server/tests/integration_test.rs:490-531` | `^[0-9]+$` triggers `backend: "ugrep"` + `fallback_reason: "query_without_required_trigram"`. |
| `test_search_fuzzy_fixed_string_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:533-580` | Sensitive fuzzy fixed-string runs in memory. |
| `test_search_unsupported_fuzzy_mode_falls_back_to_ugrep` | `tools-mcp-server/tests/integration_test.rs:582-630` | Insensitive fuzzy fixed-string falls back to ugrep with a fuzzy-specific reason. |
| `test_search_glob_filtered_fixed_string_uses_memory_backend` | `tools-mcp-server/tests/integration_test.rs:632-678` | Glob-filtered literal stays in memory; non-matching files are excluded. |
| `test_search_ugrep_fallback_preserves_slash_glob_or_semantics` | `tools-mcp-server/tests/integration_test.rs:680-747` | `word_regexp=true` with regex-meta pattern routes to ugrep; slash globs match root-relative; non-glob files are excluded. |

### 11.2 In-module unit tests in `tools-mcp-local/src/tools/handlers/`

| Test | File | What it covers |
|---|---|---|
| `parse_grep_line_*` (5 tests) | `ripgrep.rs:618-693` | Path/line/text parsing: matches preferred over context, Windows drive paths, paths with `:N:`, paths with `-N-`. |
| `bounded_stderr_caps_output_and_reports_truncation` | `ripgrep.rs:648-659` | Stderr capped at `MAX_UGREP_STDERR_BYTES`. |
| `truncated_*_classify_success_*` (3 tests) | `ripgrep.rs:597-616` | Success classification across truncation/timeout/exit-status combinations. |
| `ugrep_binary_name_matches_platform` | `ripgrep.rs:830-836` | Platform-specific binary selection. |
| `build_ugrep_args_preserves_direct_search_contract` | `ripgrep.rs:838-889` | argv shape for direct (non-`--from=-`) invocation. |
| `build_ugrep_args_preserves_path_list_contract` | `ripgrep.rs:891-919` | argv shape for `--from=-` invocation. |
| `is_path_authorized_*` (3 tests) | `ripgrep.rs:799-828` | Result-filter behavior. |
| `path_has_line_separator_detects_lf_and_cr_via_raw_bytes` | `ripgrep.rs:696-711` | LF/CR detection on raw OS bytes (Unix). |
| `resolve_globbed_files_rejects_lf_in_matched_path` / `resolve_globbed_files_rejects_cr_in_matched_path` | `ripgrep.rs:714-771` | Path-list injection vector blocked before ugrep spawn. |
| `resolve_globbed_files_accepts_clean_paths` | `ripgrep.rs:774-797` | Happy path for path-list resolution. |
| `search_ugrep_glob_newline_path_injection_is_blocked` | `ripgrep.rs:935-1015` | End-to-end exploit-closure regression for the path-list injection PoC. |
| 24 `public_*_match_forced_ugrep` tests | `tools-mcp-local/src/tools/handlers/search_parity.rs:349-1517` | Behavioral parity between the public `handle_search` (memory-first) and `handle_search_ugrep_for_test` (forced ugrep) across exact / fuzzy / regex / glob / hidden / ignore / follow / context / truncation / file-vs-directory-root / fallback-boundary scenarios. |
| `build_search_payload_preserves_response_shape_and_count` | `tools-mcp-local/src/tools/handlers/search_contract.rs:706-790` | Locks the 12-field success payload. |
| `build_search_payload_truncates_long_lines_in_text_and_structured_events` | `tools-mcp-local/src/tools/handlers/search_contract.rs:792-831` | Snippet truncation rendering and `line_length` preservation. |
| `render_search_text_truncates_multibyte_lines_on_char_boundary` | `tools-mcp-local/src/tools/handlers/search_contract.rs:857-883` | UTF-8 safety on truncation. |
| `unix_memory_discovery_rejects_lf_cr_paths_before_rendering` | `tools-mcp-local/src/tools/handlers/search_file_selection.rs:871-893` | Memory backend file selector rejects LF/CR paths with `unsafe_path_separator`. |
| `unix_ugrep_path_list_rejects_non_utf8_matched_paths` | `tools-mcp-local/src/tools/handlers/search_file_selection.rs:895-916` | Path-list builder rejects non-UTF-8 matched paths. |
| `scope_cache_returns_same_arc_for_repeated_key` | `tools-mcp-local/src/tools/handlers/search_file_selection.rs:1010-1036` | Scope-cache memoization across repeated discovery calls. |

The in-module `tests` module of `search_memory.rs` (`tools-mcp-local/src/tools/handlers/search_memory.rs:5584-8575`) contains additional unit and integration tests covering index building, eligibility classification, freshness validation, and warm-cache scheduling; these lock in implementation details that are not surfaced through the response contract.

## 12. Open Questions

1. The `count` field is documented as an alias for `event_count` and flagged for possible removal in a future release (`search_contract.rs:516-517`). No deprecation timeline exists in code; resolving the removal date requires a product decision outside the scope of this SDD.
2. The shipped `case` enum accepts undocumented aliases (`case-sensitive`, `case_sensitive`, `ignore`, `ignore-case`, `ignore_case`) that are not listed in the JSON schema (`search_contract.rs:69-77`). The schema's `enum: ["smart", "sensitive", "insensitive"]` will reject those aliases at MCP-client schema-validation time even though the Rust normalizer would accept them. This SDD documents both surfaces; whether the schema should be widened or the aliases narrowed is a future correctness call.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does `Search` enforce a workspace-root path policy? | No. Unlike `Read`/`Write`/`Edit`/`Delete`/`Move`/`Copy`/`Pwsh`, the handler does not call into `tools-mcp-local/src/path_policy.rs`. The walker (`ignore::WalkBuilder`) and ugrep accept any path the process can read. See §7. |
| 2 | Does the memory backend ever return partial results on timeout? | No. The memory backend returns partial success *only* for `max_results` truncation (`truncated=true`). Per-call timeout and cancellation both surface as tool-level errors with `fallback_allowed=false`, so they cannot silently fall back to ugrep and double-charge the deadline. (`search_memory.rs:79-101`.) |
| 3 | Is the ugrep `Search` invocation routed through a shell? | No. `Command::new(ugrep_binary_name())` plus explicit `args(...)` is used; no shell interpretation occurs. The `--` end-of-options marker is appended before the pattern to defend against patterns starting with `-`. (`ripgrep.rs:292-294,381`.) |
| 4 | What happens when ugrep is missing from the host? | The handler returns `ToolCallOutcome::err("ugrep error: failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: <e>")` (`ripgrep.rs:392-394`). Memory-only queries continue to work even without ugrep installed; ugrep is invoked only when the memory backend reports a fallback-eligible failure. |
| 5 | Does the cache warmer affect query behavior? | No. The warmer only pre-populates the index snapshot cache (`search_memory.rs:1439-1502`). Behavior of user-issued queries is identical with or without it; the only observable difference is `index_cache: "hit"` vs `"miss"` in the memory backend response. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::ok` / `err` / `err_with` / `parse_args` shapes (§6.3, §6.4). |
| `tools-mcp-core/src/validation.rs` | `validate_non_empty`, `clamp_limit`, `clamp_timeout` helpers (§4.2, §6.1). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_local::register_tools` is invoked at line 88 (§4.1). |
| `tools-mcp-server/src/main.rs` | Warm-cache thread is launched at line 78 (§4.1, §6.2.1). |
| `docs/hauberk-in-memory-search-srd.md` | Pre-existing design record for the memory backend; informational only. |
| `docs/tools/search-context.md` | Sibling SDD for the wrapper tool that consumes `Search` output. |
| `docs/security.md` | Project-wide trust-boundary guidance for tool-returned content (§7). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
