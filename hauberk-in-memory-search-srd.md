# System Requirements Document: Hauberk In-Memory Search Architecture

As requested for technical specifications, the key words **MUST**, **MUST NOT**,
**REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**,
**RECOMMENDED**, **MAY**, and **OPTIONAL** in this document are to be
interpreted as described in BCP 14.

## 1. Introduction and Context

This document specifies a high-performance, in-memory regular expression search
architecture for agentic workflows within the Hauberk framework.

The design combines ideas from Boolean retrieval systems and Log-Structured
Merge-Tree (LSM) mutation patterns. Traditional inverted indices map discrete
terms to documents. Source code search, however, often requires arbitrary
substring and regular expression matching rather than natural-language term
ranking. This system therefore uses trigram filtering as a conservative
candidate-selection mechanism before exact regular expression verification.

The index is an optimization, not the source of truth for correctness. Exact
regex verification over candidate files remains authoritative.

## 2. Goals

The system SHALL provide:

1. Fast local code search for agentic workflows.
2. Conservative trigram-based prefiltering with no false negatives for supported
   regex queries.
3. Exact Phase Two regex verification for every candidate returned by Phase One.
4. Immediate visibility for deletions and bounded visibility latency for
   additions and modifications.
5. Bounded resource usage for hostile, broad, malformed, or accidental queries.
6. Consistent query snapshots while background indexing or compaction is active.
7. A foundation for accelerating line-range reads through file metadata and
   line-offset caching.

## 3. Non-Goals

The system SHALL NOT attempt to provide semantic search, ranking by relevance,
BM25 scoring, embeddings, vector search, or natural-language code understanding.

The system SHALL NOT silently emulate unsupported regular expression features.
Unsupported queries MUST either fail with a structured error or be delegated to a
compatible backend.

## 4. Assumptions

This specification proceeds under the following assumptions:

1. **Implementation language:** The system SHALL be implemented in Rust.
2. **Search model:** The primary user-facing behavior is exact local code search,
   not semantic retrieval.
3. **Mutation frequency:** File system mutations are bursty and relatively small
   in volume during an agent interaction cycle.
4. **Host paging:** The host operating system page cache can efficiently serve
   raw file bytes for Phase Two verification. The application heap SHOULD NOT
   need to retain raw text for the entire codebase.
5. **Correctness priority:** The search engine MUST prefer returning a structured
   "too broad", timeout, or unsupported-query error over returning incomplete or
   incorrect results.

## 5. Definitions

- **Document:** A file eligible for search after ignore rules and file-selection
  filters are applied.
- **DocId:** A stable internal identifier assigned to a document for the lifetime
  of an index generation.
- **Main Index:** Immutable trigram-to-DocId postings structure used by most
  queries.
- **Delta Index:** Concurrent postings structure containing recently added or
  modified documents not yet compacted into the Main Index.
- **Tombstones:** A bitmap of DocIds that MUST be ignored because their documents
  were deleted or superseded by a newer version.
- **Phase One:** Conservative trigram candidate filtering.
- **Phase Two:** Exact regex verification over candidate files.
- **Compaction:** Background process that merges the Delta Index into a new
  immutable Main Index generation.

## 6. File Selection and Exclusions

The indexer MUST respect repository ignore rules, including `.gitignore` and any
equivalent ignore mechanisms already supported by Hauberk Search.

The indexer SHOULD exclude:

1. VCS metadata directories such as `.git/`.
2. Generated and build output directories.
3. Dependency/vendor directories when configured.
4. Binary files.
5. Files exceeding a configurable maximum size.
6. Symlink targets unless symlink following is explicitly enabled.
7. Hidden files or directories when consistent with existing Search behavior.

Excluded files MUST NOT appear in search results.

The system MUST maintain file metadata sufficient to validate index freshness,
including path, size, mtime, binary/text classification, and generation. A
content hash MAY be stored when needed to disambiguate same-size/same-mtime
changes.

## 7. Regex Dialect and Query Semantics

The system MUST document the supported regex dialect.

If Rust's `regex` crate is used, the system MUST document that unsupported
features such as look-around and backreferences are unavailable. If
`regex-automata` is used for lower-level control, the system MUST document the
specific automata configuration, Unicode behavior, multiline behavior, and size
limits.

Queries outside the supported dialect MUST fail with a structured error or be
delegated to an existing compatible backend. They MUST NOT silently produce
partial semantics.

The regex compiler MUST enforce a configured size limit, such as a DFA or
automata size limit, to prevent compilation-phase memory exhaustion.

## 8. Phase One: Trigram Indexing and Filtering

The system SHALL use a two-phase search architecture to balance memory density
and query execution speed.

Phase One SHALL act as a conservative candidate filter. For every supported
regex query, every file that could contain a match MUST be included in the
candidate set passed to Phase Two. The system MAY include false positives, which
Phase Two MUST eliminate through exact regex verification.

### 8.1 Index Structure

The Main Index MUST map trigrams to compressed DocId bitsets. Roaring Bitmaps are
RECOMMENDED to minimize memory overhead for high-frequency trigrams while
supporting efficient set operations.

The system MUST maintain document frequency (DF) metadata for trigrams in the
Main Index and SHOULD maintain equivalent approximate or exact DF metadata for
the Delta Index.

### 8.2 Query Optimization

The query execution engine MUST NOT evaluate trigrams only in textual order. It
MUST order trigram intersections by ascending document frequency where possible,
evaluating rarer trigrams first to reduce intermediate candidate sets.

The system SHOULD short-circuit Phase One when an intermediate candidate set
becomes empty.

The system MAY skip high-frequency trigrams at query time if doing so only
weakens filtering and cannot introduce false negatives.

### 8.3 Trigram Pruning

Index-time pruning of high-frequency trigrams is OPTIONAL. If implemented, the
system MUST preserve no-false-negative behavior by treating pruned trigrams as
unavailable filters and relying on remaining trigrams or bounded fallback
verification.

The system MUST distinguish index-time pruning from query-time skipping.
Query-time skipping does not reduce stored index memory; index-time pruning does.

### 8.4 Queries Without Required Trigrams

Queries lacking a required literal substring of at least three bytes render the
trigram index ineffective.

For such queries, the system MUST bypass Phase One and use a bounded verification
path. This path MUST enforce limits for wall-clock time, scanned bytes, file
count, match count, and output size.

If the query cannot complete within configured limits, the system MUST return a
structured "too broad" or timeout error unless partial results are explicitly
part of the product contract.

## 9. Phase Two: Exact Regex Verification

Phase Two MUST sweep a compiled regex automaton over every candidate file
returned by Phase One.

Phase Two MUST be authoritative. A search result MUST NOT be returned unless
Phase Two verifies an exact match.

The implementation SHOULD use memory-mapped files where appropriate to read
candidate file bytes without copying entire file contents into application heap
memory. The implementation MAY use normal buffered reads when mmap is
unavailable, unsafe, or not beneficial for the platform.

Phase Two MUST enforce:

1. Maximum candidate file count.
2. Maximum total scanned bytes.
3. Maximum per-file size.
4. Maximum match count.
5. Maximum output bytes.
6. Wall-clock query deadline.

## 10. Main-Delta Mutation Architecture

To support real-time updates without blocking search queries, the system SHALL
use a Main-Delta architecture inspired by LSM principles.

### 10.1 Immutability

The Main Index MUST be immutable after publication. File additions,
modifications, and deletions MUST NOT mutate published Main Index bitsets
directly.

### 10.2 Tombstones

The system MUST maintain a global Tombstones bitmap or generation-equivalent
structure.

When a file is deleted or superseded by a modified version, its old DocId MUST be
marked tombstoned. Tombstones MUST become visible to new queries immediately
after the mutation is committed.

The query engine MUST apply `AND NOT Tombstones` or an equivalent mask to Phase
One results before Phase Two verification.

### 10.3 Delta Index

The system MUST route trigrams for added or modified files to a concurrent Delta
Index.

The Delta Index MAY be implemented using a lock-free data structure, a
sharded-lock concurrent map, or another concurrency-safe structure. Strict
lock-freedom is OPTIONAL unless separately justified by performance testing.

Additions and modifications SHALL become searchable after their Delta Index
insertion and metadata publication complete.

### 10.4 Scatter-Gather Querying

The query engine MUST query both the Main Index and the Delta Index for every
eligible indexed query.

The system MUST union Main Index and Delta Index candidates, apply tombstones,
and then run Phase Two verification over the resulting candidate set.

### 10.5 Compaction

The system MUST trigger background compaction when the Delta Index exceeds a
configured threshold. The threshold MAY be based on memory usage, number of
documents, number of postings, or elapsed time since the last compaction.

Compaction MUST build a new immutable Main Index generation without blocking
active queries.

Compaction MUST publish the new Main Index atomically. Queries MUST execute
against a consistent snapshot of Main Index, Delta Index, Tombstones, and file
metadata.

After successful publication, the system MAY reclaim obsolete generations once no
active query references them.

## 11. Document Identity and File Lifecycle

Each indexed file SHALL have a stable DocId for the lifetime of an index
generation.

The system MUST maintain:

1. A path-to-DocId map for current documents.
2. A DocId-to-file-metadata table.
3. A generation identifier for each published index snapshot.
4. A tombstone record for deleted or superseded DocIds.

Rename handling MUST preserve correctness. The system MAY implement a rename as
a delete plus add, provided stale DocIds are tombstoned before the new path is
published as searchable.

Modification handling MUST preserve correctness. If a file changes while being
indexed or verified, the system MUST detect the race through metadata checks or
content hashes and either retry, use a consistent snapshot, or return a
structured stale-file error.

## 12. Read Acceleration

Although this SRD primarily specifies Search, the same metadata layer SHOULD
support faster Read operations.

The system SHOULD maintain a line-offset table for indexed text files. The table
SHOULD allow line-range reads to seek directly to byte offsets rather than
scanning from the beginning of the file on every request.

Read cache entries MUST be invalidated when file metadata indicates a possible
change.

Read acceleration MUST preserve exact file contents and line numbering. It MUST
NOT return stale content after a file modification is visible to the system.

## 13. Security, Bounded Execution, and Guardrails

Autonomous agents may generate broad, malformed, or pathological inputs. The
system MUST prioritize host stability over query completion.

### 13.1 Too-Broad Circuit Breaker

The system MUST enforce configurable limits for:

1. Phase One candidate count.
2. Phase Two scanned bytes.
3. Per-file scanned bytes.
4. Match count.
5. Output bytes.
6. Wall-clock time.
7. Regex compilation size.

If a query exceeds configured limits, the system MUST abort the query and return
a structured error requiring a narrower query, unless partial results are
explicitly supported by the product contract.

### 13.2 Timeout Handling

The system MUST enforce query deadlines.

If a timeout is reached, the query SHALL be cancelled cooperatively at defined
interruption points, such as between files, chunks, or regex search iterations
where supported.

The system MUST return a structured timeout error.

The system MUST NOT rely on unsafe in-process thread termination. If hard
termination is REQUIRED, regex execution SHALL occur in an isolated worker
process that can be safely killed.

### 13.3 Memory Bounds

The system SHOULD avoid unbounded per-query dynamic allocation during bitmap
intersections.

The system SHOULD use bounded arenas, reusable buffers, or a query memory budget
to isolate query memory footprints.

If a query exceeds its memory budget, the system MUST return a structured
resource-limit error.

## 14. API and Error Behavior

The search API MUST return structured errors for at least:

1. Unsupported regex dialect.
2. Regex compilation limit exceeded.
3. Query too broad.
4. Query timeout.
5. Search index unavailable.
6. File changed during verification.
7. Internal index inconsistency.

Error responses SHOULD include actionable guidance, such as asking for a more
specific literal string when a query is too broad.

The API MUST NOT return success-shaped responses for failed or incomplete
queries unless partial-result semantics are explicitly requested and clearly
marked.

## 15. Observability and Diagnostics

The system SHOULD expose diagnostic counters for:

1. Indexed file count.
2. Indexed byte count.
3. Main Index memory usage.
4. Delta Index memory usage.
5. Tombstone count.
6. Compaction count and duration.
7. Query candidate counts.
8. Phase One duration.
9. Phase Two duration.
10. Timeout and too-broad error counts.

These diagnostics SHOULD be available without exposing file contents.

## 16. Limitations

Pure wildcard or structural queries without required literal trigrams cannot
benefit from the trigram index and MUST use bounded verification.

High-frequency literal queries may produce large candidate sets even with DF
ordering. The system MUST rely on resource limits and too-broad errors to
protect host stability.

Trigram indexes accelerate candidate discovery, but performance is determined by
postings density, bitmap representation, query selectivity, candidate file size,
and Phase Two verification cost. The system MUST NOT claim logarithmic search
complexity as a general guarantee.

## 17. Opposing Viewpoints and Responses

### 17.1 Off-the-Shelf Alternatives

A valid criticism is that a custom engine may be unnecessary when robust engines
such as Tantivy exist.

Tantivy is optimized primarily for natural-language tokenization, ranking, and
disk-backed segments. Hauberk's use case requires exact local regex search,
substring-oriented filtering, in-memory mutation visibility, and agent-specific
guardrails. A purpose-built trigram and regex verification pipeline MAY provide a
simpler and more controllable architecture for this workload.

This does not preclude using Tantivy or another engine if benchmarking shows it
meets the same correctness, latency, mutation, and guardrail requirements.

### 17.2 Git Grep, Ripgrep, or Ugrep Subprocesses

Another valid criticism is that shelling out to mature tools such as `git grep`,
`ripgrep`, or `ugrep` may be sufficient.

These tools are highly optimized and SHOULD remain important baselines for
benchmarking. However, subprocess execution introduces process startup overhead,
can complicate cross-platform behavior, and limits the framework's ability to
enforce fine-grained memory budgets, query snapshots, mutation visibility, and
structured timeout behavior.

The system MAY retain an existing subprocess backend as a fallback for cold
indexes, unsupported regex dialects, or validation comparisons.

## 18. Acceptance Criteria

The implementation SHALL be considered acceptable only if:

1. Supported regex searches produce no false negatives compared with an
   authoritative full verification baseline.
2. Phase Two eliminates all false positives returned by Phase One.
3. Deletes are masked from new queries immediately after tombstone publication.
4. Added or modified files become searchable after Delta Index publication.
5. Searches observe consistent index snapshots during compaction.
6. Too-broad and timeout cases return structured errors rather than partial
   success.
7. File exclusions match documented ignore behavior.
8. Resource limits are configurable and tested.
9. Read acceleration, if enabled, preserves exact content and line numbering.
10. Benchmarks compare the implementation against the current subprocess search
    backend on representative repositories.
