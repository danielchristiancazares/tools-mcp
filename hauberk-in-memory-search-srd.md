# System Requirements Document: tools-mcp In-Memory Search POC for Hauberk

As requested for technical specifications, the key words **MUST**, **MUST NOT**,
**REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**,
**RECOMMENDED**, **MAY**, and **OPTIONAL** in this document are to be
interpreted as described in BCP 14.

## 1. Status, Scope, and Repository Target

This document specifies a proof of concept (POC) for an in-memory local code
search backend implemented in the `tools-mcp` repository first. The POC is meant
to validate the architecture before any Hauberk integration. It MUST NOT be
treated as final Hauberk design authority until the corresponding Hauberk IFA,
security, approval, queueing, and harness invariants are cited and reviewed.

The POC target is the existing `tools-mcp` MCP `Search` tool:

1. The public tool name remains `Search`.
2. The tool automatically attempts the in-memory backend for eligible queries.
3. The existing `ugrep` subprocess backend remains the fallback for unsupported
   or ambiguous cases.
4. Unsupported or ambiguous in-memory cases MUST delegate to the current `ugrep`
   backend rather than silently changing search semantics.

The POC is successful only if it proves that indexed search can preserve the
current `Search` contract for an explicitly defined subset of queries, while
falling back for all other cases.

## 2. Existing tools-mcp Baseline

The current public `Search` tool is implemented in `tools-mcp-local` and is
registered by the local tool registry. It shells out to `ugrep` directly, not
through a shell.

The existing request schema is the compatibility baseline:

1. `pattern` is required.
2. `path` defaults to the server current working directory.
3. `case` accepts `smart`, `sensitive`, or `insensitive`, defaulting to `smart`.
4. `fixed_strings` maps to literal matching.
5. `word_regexp` maps to word-boundary matching.
6. `glob` is an array of include filters.
7. `hidden` controls hidden file and directory traversal.
8. `follow` controls symlink following.
9. `no_ignore` disables ignore-file behavior.
10. `context` controls lines of context.
11. `max_results` limits emitted match or context events.
12. `timeout_ms` controls overall backend timeout.
13. `fuzzy` enables ugrep fuzzy matching.

The existing response shape is also the compatibility baseline:

1. `content[0].text` contains readable grep-like text output.
2. `isError` distinguishes successful tool execution from tool-level failure.
3. `pattern` echoes the request pattern.
4. `path` echoes the resolved root argument.
5. `exit_code` reports the backend exit code when applicable.
6. `truncated` reports output truncation from `max_results`.
7. `timed_out` reports timeout.
8. `count` reports the number of emitted match or context events.
9. `matches` contains structured match and context events.
10. `stderr` is present when the backend produced stderr.

The POC MUST preserve this default public behavior unless a later document
explicitly approves a behavior change.

## 3. Goals

The POC SHALL provide:

1. An automatic in-memory backend for eligible uses of the existing `Search`
   tool.
2. Conservative trigram candidate filtering with no false negatives for the
   explicitly supported query subset.
3. Exact or bounded fuzzy Phase Two verification for every candidate returned by
   Phase One.
4. Compatibility-preserving fallback to `ugrep` for unsupported or ambiguous
   cases.
5. Query-visible freshness checks so deleted, modified, and added files are not
   answered from stale index state.
6. Bounded query execution and bounded index memory usage.
7. Tests comparing the in-memory backend with the existing `ugrep` backend on
   representative fixtures.
8. A clear handoff path for a later Hauberk-native architecture.

## 4. Non-Goals

The POC SHALL NOT provide:

1. Semantic search, ranking, embeddings, or natural-language code understanding.
2. A new public MCP tool name.
3. A persistent on-disk index.
4. File-system watchers as a correctness requirement.
5. Full replacement of all `ugrep` behavior.
6. Hauberk queueing, approval, continuation, resume, or proof-carrying state
   integration.
7. Read acceleration in the initial POC.

Read acceleration MAY be specified in a later document after the search POC
proves correctness and compatibility.

## 5. Backend Selection

The backend selector MUST be internal to tools-mcp. The MCP request schema MUST
NOT change for the initial POC, and there MUST NOT be an environment flag or
public request parameter for choosing the backend.

Backend selection SHALL be automatic:

1. Eligible exact or fuzzy fixed-string requests use the in-memory backend.
2. Unsupported, ambiguous, or incomplete in-memory cases delegate to `ugrep`.
3. Memory errors that cannot safely fall back, such as memory-backend timeout,
   return structured tool errors.

The POC MUST include response metadata when the in-memory backend handles a
query:

```json
{
  "backend": "memory"
}
```

When the selector delegates to `ugrep`, it MAY include:

```json
{
  "backend": "ugrep",
  "fallback_reason": "unsupported_regex_dialect"
}
```

The additional fields are additive and MUST NOT remove existing fields.

## 6. Definitions

- **Document:** A file eligible for search after path, ignore, hidden, symlink,
  glob, binary, and size filtering are applied.
- **DocId:** A stable internal identifier assigned to a document within one
  published index snapshot.
- **Index Snapshot:** An immutable in-memory structure containing document
  metadata, trigram postings, line offsets, and generation metadata.
- **Generation:** A monotonically increasing identifier for a published snapshot.
- **Phase One:** Conservative trigram candidate filtering.
- **Phase Two:** Exact literal verification or bounded fuzzy verification over
  candidate content.
- **Fallback:** Delegation to the existing `ugrep` backend without returning
  partial in-memory results.
- **Eligible Query:** A query whose semantics the POC can preserve exactly.
- **Freshness Check:** Metadata validation performed before a query result is
  returned.
- **Fuzzy Distance:** The integer `N` from `fuzzy=N`, interpreted as ugrep
  `-Z<N>` bounded edit distance over insertion, deletion, and substitution.
- **Fuzzy Literal Plan:** A memory query plan for `fixed_strings=true`,
  `case=sensitive`, `word_regexp=false`, and `fuzzy=N` that uses exact seeds for
  candidate generation and bounded fuzzy verification for Phase Two.
- **Seed Segment:** One of `N + 1` non-overlapping contiguous Unicode-scalar
  segments of a fuzzy pattern. Each eligible seed segment MUST encode to at
  least three UTF-8 bytes.
- **Bounded Fuzzy Verification:** Authoritative Phase Two matching that accepts
  a candidate line only when some line substring is within the requested fuzzy
  distance.

## 7. Public API Compatibility

The initial POC MUST preserve the existing `Search` input schema. It MUST NOT add
a public `backend` request parameter in the first implementation.

For memory-backed searches, the POC MUST preserve:

1. `pattern`, `path`, `truncated`, `timed_out`, `count`, and `matches`.
2. The `matches[]` event shape:
   - `type` is `match` or `context`.
   - `data.path.text` is the rendered path.
   - `data.line_number` is 1-indexed.
   - `data.lines.text` is the rendered line text.
3. `content[0].text` line rendering in the same `path:line:text` and
   `path-line-text` style used by the current tool.
4. `max_results` behavior: reaching `max_results` is a successful truncated
   result, not a tool error.
5. Timeout behavior: a timeout is a tool-level error with `timed_out: true`.

The POC MUST NOT return success-shaped responses for internal index
inconsistency, stale verification, memory-backend timeout, or resource-limit
failures other than `max_results` truncation.

## 8. Behavior Changes and Approvals

The initial POC intentionally changes behavior by introducing an automatic
in-memory fast path for eligible exact literal, seeded regex, plain-regex-
literal, and fuzzy fixed-string queries.

The following changes are approved for POC mode:

1. Additive response metadata fields: `backend` and `fallback_reason`.
2. Different process model: in-memory verification instead of subprocess
   execution for eligible queries.
3. Automatic ugrep fallback when the in-memory backend cannot preserve exact
   semantics.
4. Narrow fuzzy memory support for eligible fixed-string, case-sensitive,
   non-word, single-line UTF-8 queries with partitionable exact seeds.
5. Common exact literal support for plain regex patterns without metacharacters,
   conservative case-sensitive seeded regex support for regex patterns with
   metacharacters, plus ugrep-compatible ASCII case folding for fixed-string
   smart and insensitive searches.

The following changes are not approved by this document:

1. Removing or renaming existing request fields.
2. Returning partial or success-shaped memory results when fallback is required.
3. Adding fuzzy ranking, semantic similarity, score output, best-match sorting,
   or fuzzy behavior beyond integer ugrep-compatible `-Z<N>` threshold
   semantics.
4. Changing `max_results` truncation from success to error.
5. Changing hidden, ignore, symlink, or glob defaults.
6. Adding a public MCP schema field for backend selection.

## 9. Query Eligibility

The in-memory backend MUST handle only queries whose semantics it can preserve.
All other queries MUST fall back to `ugrep`.

The initial implemented eligible query subset is:

1. Exact literal memory queries:
   1. `word_regexp=false` and `fuzzy` absent.
   2. The pattern is either `fixed_strings=true` or a plain regex literal with
      no regex metacharacters.
   3. The literal contains at least three bytes.
   4. The literal does not contain `\n` or `\r`.
   5. `case=sensitive` is byte-exact.
   6. Fixed-string `case=insensitive` and lowercase `case=smart` use
      ugrep-compatible ASCII case folding.
   7. Plain regex literals use memory for ASCII smart or insensitive matching
      and fall back when Unicode regex case folding would be required.
2. Seeded regex memory queries:
   1. `fixed_strings=false`, `case=sensitive`, `word_regexp=false`, and `fuzzy`
      absent.
   2. The pattern compiles with the configured Rust byte-regex verifier.
   3. The pattern is line-oriented and does not require matching `\n` or `\r`.
   4. The selected scope is proven valid UTF-8 text and non-binary.
   5. The query planner can prove at least one required literal byte substring
      of length at least three for every possible match path.
   6. Concatenation seeds are intersected, alternation seeds are unioned only
      when every branch has a required seed, and Phase Two regex verification is
      authoritative for all candidate lines.
3. Fuzzy literal memory queries:
   1. `fixed_strings=true`, `case=sensitive`, `word_regexp=false`, and `fuzzy`
      present.
   2. `fuzzy=N` is the existing ugrep-compatible integer `-Z<N>` bounded edit
      distance over insertion, deletion, and substitution.
   3. The pattern is valid UTF-8 and contains no `\n` or `\r`.
   4. The selected fuzzy search scope is proven valid UTF-8 text and non-binary.
   5. The pattern is seed-partitionable into `N + 1` contiguous Unicode-scalar
      seed segments whose UTF-8 encodings are each at least three bytes.

The in-memory backend MUST fall back for:

1. Regex fuzzy queries.
2. `word_regexp=true`, including word-regexp fuzzy queries.
3. Unsupported `case=insensitive` and `case=smart` variants, including all
   fuzzy case-insensitive and fuzzy smart-case queries.
4. Regex queries with metacharacters when the planner cannot prove required
   seeds, the verifier cannot compile the pattern, or dialect parity is
   unsupported.
5. Plain regex literals requiring Unicode smart-case or insensitive regex
   folding.
6. Queries with no proven required literal byte substring of length at least
   three.
7. Multiline fixed-string or multiline fuzzy queries.
8. Invalid UTF-8 or binary fuzzy or seeded-regex scope.
9. Fuzzy patterns that are too short or unseedable under the `N + 1` seed rule.
10. Requests whose file-selection semantics cannot be matched exactly.
11. Any fuzzy query whose parity with ugrep is unproven.

The POC MAY later expand eligibility, but every expansion MUST include parity
tests against the existing `ugrep` backend.

## 10. Regex Dialect, Exact Verification, and Fuzzy Verification

The initial in-memory backend SHALL use byte-level literal verification for
exact literal Phase Two. Eligible seeded regex queries SHALL use a bounded byte
regex verifier over candidate lines. Regex queries with metacharacters MUST
delegate to `ugrep` unless the planner proves required seeds and the configured
verifier can preserve the supported subset's semantics. Plain regex literals MAY
use the same exact-literal verifier when case semantics can be preserved.

Seeded regex memory support MUST use Rust `regex::bytes` or another explicitly
specified verifier and configure regex compilation with:

1. A size limit.
2. Unicode mode enabled only for selected scopes proven to be valid UTF-8 text.
3. Case-insensitive Unicode regex matching disabled until parity tests prove
   behavior matches the delegated backend for the eligible subset.
4. Multi-line behavior matching the existing line-oriented search contract for
   the eligible subset.

If a non-literal regex query is received without a proven required literal seed
or supported verifier plan, the backend MUST delegate to `ugrep`.

Phase Two is authoritative. A result MUST NOT be returned unless Phase Two
verifies the requested match against content observed by the query snapshot.
Exact literal Phase Two verifies either an exact byte match or the explicitly
eligible ASCII case-insensitive literal match. Fuzzy Phase Two verifies bounded
Unicode-scalar edit distance against candidate lines and is the authority for
whether a fuzzy match exists.

## 11. Required Literal and Trigram Extraction

Phase One MUST be conservative. Every possible matching document for an eligible
query MUST remain in the candidate set passed to Phase Two.

The initial extractor SHALL operate over exact byte literals, plain regex
literals, seeded regex HIR literals, ASCII-folded eligible literals, and fuzzy
fixed-string seed segments:

1. For exact `fixed_strings=true` queries and eligible plain regex literals, the
   required literal is the pattern bytes.
2. For fuzzy `fixed_strings=true` queries, the planner SHALL split the pattern
   into `distance + 1` non-overlapping contiguous Unicode-scalar seed segments.
3. Every valid fuzzy match within `distance` edits has at least one untouched
   seed segment. Therefore Phase One SHALL intersect trigrams within each seed
   and union the candidate sets from all seeds.
4. Each fuzzy seed segment MUST have a UTF-8 encoding of at least three bytes.
   If any seed is shorter, the query is ineligible and MUST fall back.
5. Regex queries with metacharacters are eligible only when the planner can
   prove required literal seeds for every possible match path.
6. Case-insensitive fixed-string queries use ASCII-folded trigrams and
   ASCII-insensitive Phase Two verification.
7. Regex seed extraction MUST parse with `regex-syntax` or an equivalent parser
   that exposes a syntax tree.
8. Regex seed extraction MUST NOT treat literal byte substrings under optional
   repetition (`?`, `*`, `{0,n}`) as required.
9. Regex seed extraction MUST make alternation ineligible unless it can prove at
   least one required byte substring for every branch and union those branch
   candidate sets.
10. Character classes, anchors, boundaries, groups without required literals, and
   wildcard constructs MUST NOT contribute required trigrams.
11. Unicode regex case-insensitive literals MUST make the query ineligible until
    exact case-folding parity is specified and tested.

For each required literal of length at least three, the engine SHALL generate
all overlapping byte trigrams. The candidate set for that literal is the
intersection of postings for those trigrams. When multiple required literals are
available, the query planner SHOULD evaluate the most selective literal first
using document frequency metadata.

If no required literal of length at least three is available, the query is
ineligible and MUST fall back or error according to backend mode.

## 12. File Selection Semantics

File selection MUST preserve the existing `Search` request semantics for the
eligible subset.

The POC SHALL implement file discovery with the `ignore` crate plus explicit
filters:

1. `path` selects the file or directory root.
2. `hidden=false` excludes hidden files and directories.
3. `hidden=true` includes hidden files and directories.
4. `follow=false` does not follow symlinks.
5. `follow=true` follows symlinks only if the implementation can avoid cycles
   and preserve path rendering.
6. `no_ignore=false` respects `.gitignore`, repository excludes, and global git
   ignore behavior.
7. `no_ignore=true` disables ignore-file filtering to match current Search
   behavior as closely as possible.
8. `glob` filters MUST be applied before indexing or verification.
9. Binary files MUST be handled the same way as the current backend for the
   eligible subset, or the query MUST fall back.
10. Files larger than the configured POC maximum size MUST cause fallback unless
    the implementation includes them in direct Phase Two verification.

The POC MUST NOT silently omit an eligible file from both indexing and fallback.
If the in-memory backend cannot prove complete coverage for the selected scope,
it MUST delegate to `ugrep`.

## 13. Index Ownership and State Machine

The POC index SHALL live inside the `tools-mcp-local` crate. A recommended file
layout is:

```text
tools-mcp-local/src/tools/search.rs
tools-mcp-local/src/tools/handlers/ripgrep.rs
tools-mcp-local/src/tools/handlers/search_memory/
tools-mcp-local/src/tools/handlers/search_memory/mod.rs
tools-mcp-local/src/tools/handlers/search_memory/index.rs
tools-mcp-local/src/tools/handlers/search_memory/query.rs
tools-mcp-local/src/tools/handlers/search_memory/render.rs
```

The implementation MAY choose different private file names, but it MUST keep the
public tool registration unchanged.

The index manager SHALL expose this state machine:

1. `Disabled`: memory backend is not selected.
2. `Cold`: no snapshot exists for the requested root and file-selection shape.
3. `Building`: a snapshot is being built.
4. `Ready`: a snapshot exists and can be freshness-checked.
5. `Refreshing`: a query observed stale metadata and is rebuilding or patching
   the snapshot.
6. `Unavailable`: the index cannot be built safely; delegate or error according
   to backend mode.

The tools-mcp server SHALL start a best-effort background warm-cache thread when
the process starts and the current working directory is inside a Git worktree.
The warm-cache thread SHOULD build the default repository-root file-selection key
(`hidden=false`, `follow=false`, `no_ignore=false`, and no globs) without
blocking stdin/stdout startup. It MUST NOT write diagnostics to stdout.

The POC MAY still build synchronously on the first eligible query for cold
roots, non-default file-selection keys, or warm-cache failures. It SHOULD avoid
blocking unrelated roots. A query MUST obtain an immutable `Arc<IndexSnapshot>`
before Phase One starts. The snapshot MUST NOT be mutated after publication.

## 14. Snapshot Contents

Each `IndexSnapshot` MUST contain:

1. Generation identifier.
2. Root path and file-selection key.
3. Path-to-DocId map.
4. DocId-to-metadata table.
5. Trigram-to-DocId postings.
6. Document frequency metadata for trigrams.
7. Per-file line offset tables sufficient to render line and context output.
8. Total indexed file count and indexed byte count.

Document metadata MUST include:

1. Rendered path.
2. Canonical filesystem path where available.
3. File size.
4. Modification timestamp.
5. File type classification needed by the filters.
6. Optional content hash when timestamp and size are insufficient to detect a
   modification race.

The POC MAY store postings as `RoaringBitmap` or another compressed bitset. If a
plain `Vec<DocId>` or `HashSet<DocId>` is used for the first POC, the acceptance
benchmarks MUST record memory usage and query latency so the representation can
be revisited before Hauberk integration.

## 15. Freshness and Mutation Visibility

Because the POC does not require file-system watchers, mutation visibility is
defined at query boundaries.

Before returning a memory-backed result, the implementation MUST validate that:

1. Every result file still exists.
2. Every result file still matches the metadata used for verification, or a
   content hash proves the bytes are unchanged.
3. Deleted files are not returned.
4. Modified files are either re-read and re-verified against current bytes,
   handled by a refreshed snapshot, or cause fallback/error.
5. Added files under the selected scope are visible after the next freshness
   scan, or the query falls back rather than returning incomplete success.

The POC MUST NOT claim immediate asynchronous mutation visibility. The POC
visibility guarantee is:

> New queries either use a freshness-checked complete snapshot for the selected
> scope or delegate/error; they do not return knowingly stale memory-backed
> success.

This is weaker than the intended future Hauberk main/delta/tombstone model and
MUST be revisited before Hauberk integration.

## 16. Query Execution

For an eligible memory-backed query, execution SHALL proceed as follows:

1. Validate and normalize request arguments using the existing tool argument
   parsing behavior.
2. Build a file-selection key from `path`, `hidden`, `follow`, `no_ignore`, and
   `glob`.
3. Acquire or build a freshness-checked `IndexSnapshot`.
4. Compile either the exact literal verifier or the bounded fuzzy verifier.
5. Extract exact required literals or fuzzy seed segments and their trigrams.
6. For exact plans, intersect postings in ascending document-frequency order.
7. For fuzzy plans, intersect postings per seed and union the per-seed candidate
   sets.
8. Run the selected Phase Two verifier over every candidate file or line.
9. Render matches and requested context lines.
10. Stop rendering when `max_results` is reached and set `truncated=true`.
11. Return the compatibility response shape with `backend: "memory"`.

If any step cannot preserve exact semantics, the implementation MUST fall back
or return a structured error according to backend mode.

## 17. Phase Two Verification and Rendering

Phase Two MUST read bytes from the filesystem or from a verified snapshot source
whose metadata still matches the filesystem.

Exact Phase Two emits a match only when the exact literal occurs in the verified
line bytes. Fuzzy Phase Two emits a match line once when one or more bounded
fuzzy matches occur on that line. Fuzzy results MUST NOT include scores,
rankings, best-match order, or per-match edit distances.

The POC MUST produce match and context events with deterministic memory ordering
for eligible cases:

1. Sort by rendered path in deterministic order.
2. Within each file, sort exact matches by line number and match offset; sort
   fuzzy match lines by line number.
3. Emit context events according to the requested `context` value.
4. Do not duplicate context events when multiple matches share context lines.
5. Count both match and context events toward `max_results`, matching current
   behavior.
6. Apply `max_results` to that emitted event order. If truncation occurs, the
   response is success-shaped with `truncated=true`; it is not a ranked
   best-match prefix.

The renderer MUST preserve valid UTF-8 text. For non-UTF-8 bytes, it MAY use
lossy UTF-8 conversion if parity tests show that matches the current structured
output behavior for eligible cases. Otherwise it MUST fall back.

## 18. Error Behavior

All memory backend tool errors MUST use MCP tool-level errors:

```json
{
  "content": [{"type": "text", "text": "..."}],
  "isError": true
}
```

Memory backend errors SHOULD include:

1. `backend: "memory"`.
2. `error_type`.
3. `fallback_available`.
4. `remediation`.

Required `error_type` values:

1. `unsupported_regex_dialect`.
2. `unsupported_search_option`.
3. `search_index_incomplete`.
4. `search_index_unavailable`.
5. `file_changed_during_verification`.
6. `query_timeout`.
7. `resource_limit_exceeded`.
8. `internal_index_inconsistency`.

Unsupported dialects/options and incomplete index coverage SHOULD delegate to
`ugrep` instead of returning these errors. Errors are reserved for cases where
fallback would be unsafe or where fallback execution itself fails.

Fuzzy fallback diagnostics SHOULD use specific `fallback_reason` values when
available:

1. `unsupported_fuzzy_mode`.
2. `fuzzy_pattern_too_short`.
3. `fuzzy_pattern_unseedable`.
4. `fuzzy_scope_not_utf8`.
5. `fuzzy_parity_unproven`.
6. `unsupported_regex_fuzzy`.
7. `unsupported_case_fuzzy`.
8. `unsupported_word_fuzzy`.
9. `unsupported_multiline_fuzzy`.

## 19. Resource Limits

The POC MUST enforce configurable limits. Initial defaults:

1. `TOOLS_SEARCH_INDEX_MAX_FILE_BYTES`: 1 MiB.
2. `TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES`: 256 MiB per file-selection key.
3. `TOOLS_SEARCH_INDEX_MAX_FILES`: 50,000.
4. `TOOLS_SEARCH_MAX_CANDIDATES`: 20,000.
5. `TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS`: 300,000 ms for startup warm-cache
   build.
6. `TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES`: 10 MiB per compiled seeded-regex
   verifier.
7. Existing `timeout_ms`: per-query wall-clock deadline.
8. Existing `max_results`: output event cap.

If index build limits are exceeded, the query MUST fall back to `ugrep`.

Fuzzy memory execution MUST also enforce bounded verifier limits:

1. Maximum fuzzy pattern Unicode-scalar length.
2. Maximum candidate documents and candidate lines.
3. Maximum verified line length.
4. Per-query deadline checks during seed planning, candidate generation, and
   bounded edit-distance verification.

If fuzzy planning, candidate generation, or verification exceeds a resource
limit or deadline, the memory backend MUST NOT return partial success. It MUST
fall back before producing memory results when safe, or return a structured
tool-level error. The only resource limit that MAY return partial success is
`max_results` truncation.

If `max_results` is reached, the query MUST return success with
`truncated=true`, matching the existing tool behavior.

## 20. Diagnostics

Memory-backed successful responses SHOULD include lightweight diagnostics:

```json
{
  "backend": "memory",
  "index_cache": "hit",
  "index_generation": 3,
  "indexed_files": 1200,
  "indexed_bytes": 8300000,
  "candidate_count": 17,
  "fuzzy_seed_count": 0,
  "fuzzy_verified_lines": 0,
  "phase_one_ms": 2,
  "phase_two_ms": 4
}
```

Diagnostics MUST NOT expose file contents beyond normal match output.

Fallback responses MAY include:

```json
{
  "backend": "ugrep",
  "fallback_reason": "query_without_required_trigram"
}
```

## 21. Testing Requirements

The POC MUST include tests for both compatibility and index correctness.

Unit tests SHALL cover:

1. Fixed-string trigram extraction.
2. Unseeded or unsupported regex queries fall back to ugrep.
3. Seeded regex candidate planning intersects concatenation seeds and unions
   fully seeded alternation branches without false negatives.
4. Ineligible option combinations.
5. Candidate intersection ordering.
6. Line-offset rendering and context de-duplication.
7. Freshness checks for deleted and modified files.
8. Structured error payloads for non-fallback memory errors.
9. Fuzzy seed partitioning into `distance + 1` searchable seed segments.
10. Fuzzy verifier insertion, deletion, and substitution behavior.
11. Fuzzy seed no-false-negative fixtures.
12. Invalid UTF-8 fuzzy scope fallback.

Integration tests SHALL cover:

1. Existing Search tests pass.
2. Eligible fixed-string searches use the in-memory backend.
3. Eligible fixed-string fuzzy searches use the in-memory backend.
4. Unsupported fuzzy searches fall back to ugrep with fuzzy-specific fallback
   diagnostics.
5. Eligible seeded regex searches use the in-memory backend.
6. Unsupported or unseeded regex falls back to ugrep.
7. Fuzzy memory results match ugrep `-Z<N>` parity fixtures for exact,
   insertion, deletion, substitution, no-match, and context cases.
8. `max_results` truncation remains success-shaped for exact, seeded regex, and
   fuzzy memory searches.
9. Timeout returns `isError=true` and `timed_out=true`.
10. Hidden, ignore, symlink, and glob behavior match the baseline for eligible
   fixtures.

Parity tests SHOULD compare memory-backed output against the `ugrep` backend on
fixture repositories. The exact text output and structured `matches` SHOULD
match for eligible queries, excluding additive diagnostic fields.
Fuzzy parity tests MUST use untruncated result sets when comparing match
membership, because memory result order is deterministic path/line order rather
than ranked fuzzy order.

## 22. Benchmark Requirements

Benchmarks SHOULD compare:

1. Cold first-query time.
2. Warm query latency.
3. Index build time.
4. Index heap size.
5. Candidate counts.
6. Fuzzy seed candidate counts.
7. Phase One time.
8. Phase Two time, including fuzzy verification cost.
9. Existing `ugrep` subprocess latency.

Benchmarks MUST include at least:

1. This repository.
2. A synthetic repository with many small files.
3. A synthetic repository with high-frequency literals.
4. A repository or fixture containing ignored, hidden, symlinked, binary, and
   oversized files.

The POC does not need to beat `ugrep` in every benchmark. It must show where the
architecture helps, where it does not, and whether memory costs are acceptable
for a later Hauberk design.

## 23. Documentation Requirements

If the POC is implemented, `README.md` MUST be updated to document:

1. Automatic backend selection.
2. The eligible memory-backend subset.
3. Fallback behavior.
4. New additive response metadata.
5. Any new system dependencies or Cargo dependencies.

The README MUST NOT claim full Hauberk behavior or full `ugrep` replacement.

## 24. Hauberk Follow-Up Requirements

Before moving this POC into Hauberk, a separate Hauberk plan MUST define:

1. IFA ownership and proof-carrying state boundaries.
2. Security and authority boundaries.
3. Approval, queueing, continuation, and resume interactions if search becomes
   part of harness execution.
4. Main/delta/tombstone mutation architecture.
5. File-system watcher or journal semantics.
6. Snapshot lifetime and reclamation under concurrent queries.
7. Persisted state compatibility, if any.
8. UI/rendering/snapshot downstream consumers.

The POC intentionally does not solve those Hauberk-specific concerns. Hauberk
IFA and security artifacts are absent from this repository, so fuzzy memory
support remains POC behavior until Hauberk authority boundaries and proof
ownership are defined.

## 25. Acceptance Criteria

The tools-mcp POC is acceptable only if:

1. Existing `Search` tests remain passing.
2. Eligible searches produce no false
   negatives compared with the current `ugrep` backend on test fixtures.
3. Phase Two eliminates all false positives returned by Phase One.
4. Unsupported queries delegate to `ugrep`.
5. Eligible fuzzy searches produce no false negatives compared with ugrep
   `-Z<N>` fixtures for the supported bounded edit-distance subset.
6. Unsupported fuzzy modes delegate to `ugrep` with fuzzy-specific fallback
   diagnostics.
7. Deleted or modified files are not returned from stale memory-backed results.
8. Added files are either visible after the next freshness scan or the query
   delegates/errors rather than returning incomplete success.
9. `max_results` truncation remains a successful truncated result.
10. Timeouts return structured tool errors.
11. File exclusions match existing Search behavior for eligible fixture cases.
12. Resource limits are configurable and tested.
13. Benchmarks compare the POC against the current `ugrep` backend, including
    fuzzy candidate counts and verification cost.
14. README documentation describes activation, fallback, and limitations.

## 26. Optional Future Architecture Notes

If the POC validates the approach, the Hauberk implementation SHOULD replace the
query-bound freshness model with a real main/delta/tombstone architecture:

1. Immutable published main generations.
2. A concurrent delta index for additions and modifications.
3. Tombstones for deleted or superseded DocIds.
4. Atomic snapshot publication.
5. Generation reclamation after active queries release references.
6. Explicit proof-carrying types for reviewed authority and snapshot ownership.

Those are future Hauberk requirements, not tools-mcp POC requirements.
