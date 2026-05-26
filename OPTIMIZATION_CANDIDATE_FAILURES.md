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
