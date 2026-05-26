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
