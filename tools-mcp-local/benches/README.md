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
They run 10 samples over 300 ms by default, which is too little to distinguish small changes
on a busy machine — pass `--sample-size 100 --measurement-time 5` for decisions.

Run the `Glob` tool benchmarks with Criterion:

```bash
cargo bench -p tools-mcp-local --bench glob_memory -- --save-baseline before
cargo bench -p tools-mcp-local --bench glob_memory -- --baseline before
```

The cold benchmark exercises the scope walk plus matching on a fresh root each iteration; the
warm benchmarks reuse the cached scope snapshot, isolating scope freshness validation, pattern
matching, and payload rendering.
