# Optimization Candidate Failures

This file records performance candidates that were implemented, benchmarked, and removed because
they failed to produce a useful improvement. Do not retry these ideas as-is; revisit only with a
different hypothesis, benchmark shape, or broader design change.

## 2026-05-25: Local Search Result-Document Membership

- **Candidate:** Replace `BTreeSet<DocId>` result tracking in
  `tools-mcp-local/src/tools/handlers/search_memory.rs` with a sorted `Vec<DocId>` and a scan
  cursor in `check_targeted_snapshot_fresh`.
- **Hypothesis:** `BTreeSet::contains` during a full indexed-document freshness scan was adding
  measurable overhead to warm memory-search queries.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-local --bench search_memory -- --save-baseline before_local_wave2`
  - Comparison: `cargo bench -p tools-mcp-local --bench search_memory -- --baseline before_local_wave2`
- **Result:**
  - `cold_index_build_public_tool`: `41.555 ms` to `42.978 ms`, `+3.4249%`
  - `warm_query_default_ignore_validation`: `18.467 ms` to `19.271 ms`, `+4.3564%`
  - `large_postings_intersection`: `36.354 ms` to `37.972 ms`, `+4.4502%`
  - `warm_query_large_workspace_with_ignore`: `159.49 ms` to `160.06 ms`, `+0.3611%`, no statistically significant change
- **Conclusion:** Result-doc membership is not the limiting cost in these scenarios. The likely
  remaining cost is filesystem metadata and ignore/directory freshness work, not set lookup
  mechanics.
- **If revisiting:** Target per-file `fs::metadata` reduction, directory/ignore fingerprint
  caching, or a benchmark that isolates membership lookup from filesystem work.

## 2026-05-25: Semantic Markdown Heading Symbol Builder

- **Candidate:** Replace Markdown heading symbol normalization in
  `tools-mcp-semantic/src/chunking.rs` from `split_whitespace().collect::<Vec<_>>().join(" ")`
  to a single-pass `String` builder.
- **Hypothesis:** Avoiding the temporary `Vec<&str>` would reduce allocation overhead for Markdown
  files with many headings.
- **Benchmark:**
  - Temporary benchmark target: `cargo bench -p tools-mcp-semantic --bench semantic_chunking`
  - Baseline: `--save-baseline before_semantic_markdown_symbol`
  - Comparison: `--baseline before_semantic_markdown_symbol`
- **Result:**
  - `semantic_chunking/markdown_many_headings`: `1.7030 ms` to `1.8753 ms`, `+8.4709%`
- **Conclusion:** The single-pass builder regressed the focused benchmark. The existing
  `collect::<Vec<_>>().join(" ")` implementation should remain unless a broader chunking redesign
  changes the surrounding allocation profile.
- **If revisiting:** Focus on larger costs in `markdown_chunks`, `join_lines_trimmed`,
  `fallback_line_chunks`, or tree-sitter tag extraction, and preserve chunk ID and line-boundary
  semantics.

## 2026-05-25: WebFetch LF-Only Borrowed Chunk Sections

- **Candidate:** Replace `tools-mcp-webfetch/src/webfetch/chunker.rs`'s per-section
  `String` accumulator with a borrowed slice path for LF-only Markdown, while normalizing
  CR-containing input before the same splitter.
- **Hypothesis:** Avoiding the duplicate section buffer would reduce peak memory and improve
  large Markdown chunking throughput.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- --save-baseline before_chunker_lf_slices`
  - Comparison: `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- --baseline before_chunker_lf_slices`
- **Result:**
  - `webfetch_chunker/large_single_section`: `47.934 ms` to `49.200 ms`, `+2.6407%`,
    statistically significant regression (`p = 0.00 < 0.05`).
  - `webfetch_browser/browser_available`: no performance change detected.
- **Conclusion:** The removed section-buffer copy is not the limiting cost in this benchmark.
  Tokenization and token-window decoding dominate, and the extra slice/normalization dispatch
  worsened the measured hot path.
- **If revisiting:** Target tokenization/window decoding allocations or create a benchmark that
  directly measures peak RSS for much larger LF-only documents before changing section assembly.

## 2026-05-25: Local Search Targeted Freshness Narrowing

- **Candidate:** Narrow `tools-mcp-local/src/tools/handlers/search_memory.rs`'s targeted freshness
  validation from scanning every indexed document to checking only result documents plus a shared
  file-set comparison step.
- **Hypothesis:** Warm-query latency was dominated by per-indexed-file metadata sweeps, so limiting
  the targeted path to returned results should reduce CPU on the common case.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-local --bench search_memory -- --save-baseline before_targeted`
  - Comparison: `cargo bench -p tools-mcp-local --bench search_memory -- --baseline before_targeted`
- **Result:**
  - `warm_query_default_ignore_validation`: `2.0623 ms` to `2.0601 ms`, statistically
    insignificant (`p = 0.96`).
  - `warm_query_large_workspace_with_ignore`: `12.899 ms` to `13.029 ms`, no statistically
    significant change (`p = 0.90`).
- **Conclusion:** The targeted freshness narrowing did not move the benchmark in a meaningful way.
  The remaining cost in these scenarios is not solved by result-file-only freshness validation plus
  file-set comparison.
- **If revisiting:** Target a different search hot path, or rework freshness around a broader
  snapshot/index-state design change rather than another targeted-scan tweak.

## 2026-05-26: Core Process Drain-Loop Refactors

- **Candidate:** After adding the under-limit early return in
  `tools-mcp-core/src/process.rs::read_to_end_limited`, refactor the drain loop to return
  `false` on the first EOF read and `true` after observing any drained byte, instead of maintaining
  a `truncated` flag.
- **Hypothesis:** Removing repeated `truncated = true` writes in the drain loop would offset the
  early-return branch overhead on the over-limit path.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-core --bench core_hot_paths -- --save-baseline before_core_process`
  - Comparison: `cargo bench -p tools-mcp-core --bench core_hot_paths -- process/read_to_end_limited --baseline before_core_process`
- **Result:**
  - `process/read_to_end_limited_under_limit`: `234.71 ns` to `172.03 ns`, `-26.687%`
  - `process/read_to_end_limited_truncated`: `543.28 ns` to `614.44 ns`, `+12.911%`
- **Conclusion:** The drain-loop refactor preserved the under-limit gain but materially regressed
  the truncated path. Keep the original drain-loop shape unless a broader capture-loop rewrite is
  benchmarked.
- **If revisiting:** Benchmark a full custom capture loop against both under-limit and truncated
  cases; do not retry this return-after-first-drain-byte structure as-is.

## 2026-05-26: Core Process `Take::limit()` Early-Return Guard

- **Candidate:** Use `limited_reader.limit() > 0` after Tokio `Take::read_to_end` to detect
  under-limit EOF in `tools-mcp-core/src/process.rs::read_to_end_limited`.
- **Hypothesis:** Checking the `Take` adapter's remaining limit directly would preserve behavior and
  compile at least as efficiently as comparing the returned byte count with the configured limit.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-core --bench core_hot_paths -- --save-baseline before_core_process`
  - Comparison: `cargo bench -p tools-mcp-core --bench core_hot_paths -- process/read_to_end_limited --baseline before_core_process`
- **Result:**
  - `process/read_to_end_limited_under_limit`: `234.71 ns` to `171.50 ns`, `-26.960%`
  - `process/read_to_end_limited_truncated`: `543.28 ns` to `613.97 ns`, `+12.902%`
- **Conclusion:** The `Take::limit()` guard preserved the under-limit gain but regressed the
  truncated path substantially more than the retained returned-byte-count guard.
- **If revisiting:** Prefer the returned-byte-count guard, or redesign the capture loop and benchmark
  both process cases together.

## 2026-05-26: Local Read Number Prefix Batching

- **Candidate:** Replace `tools-mcp-local/src/tools/handlers/read_file.rs`'s numbered-read
  `push_number_prefix` implementation with a single stack prefix buffer containing padding, digits,
  and the trailing tab, then append it with one `push_str`.
- **Hypothesis:** Reducing per-line `String::push` calls for padding and tab insertion would improve
  CPU time in large numbered `Read` output rendering.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-local --bench read_file_memory read_file_numbered_render -- --save-baseline before_local_numbered_read`
  - Comparison: `cargo bench -p tools-mcp-local --bench read_file_memory read_file_numbered_render -- --baseline before_local_numbered_read`
- **Result:**
  - `read_file_numbered_render/valid_utf8_8192_lines`: `176.38 µs` to `187.71 µs`, `+6.2528%`,
    statistically significant regression (`p = 0.00 < 0.05`).
  - `read_file_numbered_render/lossy_utf8_8192_lines`: `227.66 µs` to `231.12 µs`, no statistically
    significant change (`p = 0.55 > 0.05`).
- **Conclusion:** Prefix batching makes the valid UTF-8 hot path slower. The current split between
  padding pushes, digit slice append, and tab push should remain for this benchmark shape.
- **If revisiting:** Target line splitting or allocation sizing instead of repacking the prefix bytes,
  and benchmark valid UTF-8 separately from lossy UTF-8 because the lossy case is noisier.

## 2026-05-26: Local Search Matched-Line `Vec`

- **Candidate:** Replace `tools-mcp-local/src/tools/handlers/search_memory.rs` matched-line
  `BTreeSet<usize>` storage with an ordered `Vec<usize>` in `matching_line_indexes_with_budget`,
  passing slices through render helpers.
- **Hypothesis:** Matched line indexes are discovered by a single ascending line scan, so avoiding
  tree-node allocation and `BTreeSet::insert` should reduce CPU in memory search rendering.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-local --bench search_memory -- --save-baseline before_local_line_vec`
  - Comparison: `cargo bench -p tools-mcp-local --bench search_memory -- --baseline before_local_line_vec`
- **Result:**
  - `search_memory/cold_index_build_public_tool`: `34.127 ms` to `33.989 ms`, no statistically
    significant change (`p = 0.50 > 0.05`).
  - `search_memory/warm_query_default_ignore_validation`: `2.2287 ms` to `2.2602 ms`, no
    statistically significant change (`p = 0.74 > 0.05`).
  - `search_memory/large_postings_intersection`: `2.6348 ms` to `2.5715 ms`, no statistically
    significant change (`p = 0.76 > 0.05`).
  - `search_memory/warm_query_large_workspace_with_ignore`: `15.948 ms` to `16.286 ms`, no
    statistically significant change (`p = 0.91 > 0.05`).
- **Conclusion:** Matched-line set mechanics are not a measurable CPU bottleneck in the existing
  `search_memory` benchmark scenarios. The apparent median movement is within noise.
- **If revisiting:** Use a focused rendering-heavy benchmark with many matched lines and low
  filesystem/freshness noise, or target search event construction and line text/path ownership
  instead of the matched-line collection type.

## 2026-05-26: Core JSON-Content Result Manual Builder

- **Candidate:** Route `tools-mcp-core/src/tool_outcome.rs::ToolCallOutcome::ok_json_content`
  through the manual MCP text-content result builder used for text/error responses.
- **Hypothesis:** Moving the serialized `json_text` string directly into `Value::String` would avoid
  the `serde_json::json!` expression copy and improve large JSON-content response construction.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-core --bench core_hot_paths -- tool_outcome --save-baseline before_tool_outcome_owned`
  - Comparison: `cargo bench -p tools-mcp-core --bench core_hot_paths -- tool_outcome --baseline before_tool_outcome_owned`
- **Result:**
  - Initial comparison regressed `tool_outcome/tool_call_ok_json_content_large`: `7.9717 ms` to
    `8.6074 ms`, `+7.3869%`, statistically significant (`p = 0.00 < 0.05`).
  - Adding inline hints changed this to no statistically significant movement:
    `7.9717 ms` to `8.2748 ms`, `p = 0.82 > 0.05`.
- **Conclusion:** JSON serialization dominates this benchmark, and the manual result builder did not
  produce a useful improvement for `ok_json_content`; leave this path on the existing `json!`
  construction.
- **If revisiting:** Target JSON serialization, pretty/compact formatting selection, or benchmark a
  smaller already-serialized payload path instead of reusing the text-content builder alone.

## 2026-05-26: WebFetch Markdown Cleanup Redundant Newline Check

- **Candidate:** Remove the `result.ends_with('\n')` guard in
  `tools-mcp-webfetch/src/webfetch/extract.rs::clean_markdown` when emitting a deferred blank line.
- **Hypothesis:** The normalized output builder always appends `'\n'` after non-empty lines before
  handling deferred blanks, so the guard is redundant in the hot loop.
- **Benchmark:**
  - Baseline: `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- webfetch_extraction --save-baseline before_webfetch_extract_cleanup`
  - Comparison: `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- webfetch_extraction --baseline before_webfetch_extract_cleanup`
- **Result:**
  - `webfetch_extraction/clean_markdown_whitespace_large`: `3.2410 ms` to `3.3438 ms`, reported as
    within Criterion's noise threshold.
- **Conclusion:** Removing this branch did not produce a useful improvement in the cleanup fixture.
  Keep the existing branch structure unless a broader cleanup rewrite changes the loop shape.
- **If revisiting:** Benchmark a broader whitespace-normalization strategy and include output parity
  tests for leading blanks, trailing blanks, blank-line runs, and trailing-newline behavior.

## 2026-05-26: Core Text Result Map Preallocation

- **Candidate:** Preallocate `serde_json::Map` values in
  `tools-mcp-core/src/response.rs::text_content_result`,
  `text_content_result_with_extra`, and `text_content_item` with their expected field counts.
- **Hypothesis:** Avoiding map growth during MCP text/error response construction would reduce CPU in
  the already-optimized text-content builders.
- **Benchmark:**
  - Baseline:
    `cargo bench -p tools-mcp-core --bench core_hot_paths -- tool_outcome --save-baseline before_core_text_map_capacity`
  - Comparison:
    `cargo bench -p tools-mcp-core --bench core_hot_paths -- tool_outcome --baseline before_core_text_map_capacity`
- **Result:**
  - `tool_outcome/tool_call_ok_text_with_large`: `433.85 ns` to `466.54 ns`, `+6.6204%`
  - `tool_outcome/tool_call_err_large`: `419.60 ns` to `434.73 ns`, `+4.1081%`
  - `tool_outcome/rpc_ok_text_with_large`: `492.95 ns` to `488.13 ns`, within Criterion's noise
    threshold
  - `tool_outcome/rpc_err_large`: `548.93 ns` to `491.31 ns`, `-10.627%`
- **Conclusion:** The capacity hints helped one RPC error constructor but regressed the direct tool
  response constructors, so they should not be applied to this shared hot path as-is.
- **If revisiting:** Isolate RPC-only response construction in a separate benchmark and avoid changing
  the common `ToolCallOutcome` text/error builder unless direct tool outcome benchmarks improve too.

## 2026-05-26: Semantic Directory Escaped Literal Reuse

- **Candidate:** In `tools-mcp-semantic/src/store.rs::push_path_filter_sql`, precompute a
  `Cow<'_, str>` escaped directory path once and reuse it for the three directory predicate
  insertions.
- **Hypothesis:** The directory filter appends the same path literal three times, so escaping once
  would reduce repeated scans while preserving the borrowed fast path when no apostrophe is present.
- **Benchmark:**
  - Baseline:
    `cargo bench -p tools-mcp-semantic --features bench-api --bench semantic_predicates -- semantic_predicates/directory_filter --save-baseline before_semantic_directory_escape_reuse`
  - Comparison:
    `cargo bench -p tools-mcp-semantic --features bench-api --bench semantic_predicates -- semantic_predicates/directory_filter --baseline before_semantic_directory_escape_reuse`
- **Result:**
  - `semantic_predicates/directory_filter`: `115.54 ns` to `129.18 ns`, `+6.8866%`
- **Conclusion:** Reusing a `Cow` literal added overhead in the no-escape hot case and regressed the
  benchmark, so the direct append helper remains faster.
- **If revisiting:** Only specialize for directory paths containing an apostrophe, and benchmark that
  case separately from the common no-escape directory path.

## 2026-05-26: WebFetch Lazy Markdown Cleanup

- **Candidate:** Split `tools-mcp-webfetch/src/webfetch/extract.rs::clean_markdown` normalization into
  a helper and add an `is_normalized_markdown` fast path that returns the owned Markdown string
  directly when the existing slow normalizer would be an identity transform.
- **Hypothesis:** Avoiding a second `String` allocation and line-by-line copy for already-normalized
  Markdown would reduce CPU in WebFetch cleanup.
- **Benchmark:**
  - Baseline:
    `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- webfetch_normalize --save-baseline before_webfetch_lazy_cleanup`
  - Comparison:
    `cargo bench -p tools-mcp-webfetch --bench webfetch_hot_paths -- webfetch_normalize --baseline before_webfetch_lazy_cleanup`
- **Result:**
  - Baseline:
    - `webfetch_normalize/already_clean_large`: `39.292 µs`
    - `webfetch_normalize/needs_cleanup_large`: `82.025 µs`
  - First comparison:
    - `already_clean_large`: `40.332 µs`, `+2.0505%`, no statistically significant change
    - `needs_cleanup_large`: `76.234 µs`, `-5.9706%`, statistically significant improvement
  - Recheck comparison:
    - `already_clean_large`: `40.323 µs`, `+1.1314%`, no statistically significant change
    - `needs_cleanup_large`: `78.267 µs`, `-3.0734%`, no statistically significant change
- **Conclusion:** The intended already-clean fast path did not produce a statistically significant
  improvement, and the cleanup-needed improvement from the first run did not reproduce. Rejected.
- **If revisiting:** Benchmark normalization separately from cloning/converter cost, or target
  `htmd` conversion and cleanup together with representative HTML fixtures instead of a generic
  identity detector.
