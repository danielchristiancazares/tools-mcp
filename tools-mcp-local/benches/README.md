# Local Tool Memory Benchmarks

Run the Search backend benchmarks with Criterion:

```bash
cargo bench -p tools-mcp-local --bench search_memory -- --save-baseline before
cargo bench -p tools-mcp-local --bench search_memory -- --baseline before
```

The benchmarks exercise the public `Search` tool path so they include request normalization, file discovery, index lookup/build, verification, freshness validation, and response rendering. Compare baselines on the same machine and treat small deltas as noise.

Run the `Read` numbered-output renderer benchmarks with Criterion:

```bash
cargo bench -p tools-mcp-local --bench read_file_memory -- --save-baseline before
cargo bench -p tools-mcp-local --bench read_file_memory -- --baseline before
```

These benchmarks isolate the line-numbered render path for valid UTF-8 and lossy UTF-8 file
contents, so they are useful for allocation and formatting changes without filesystem noise.
