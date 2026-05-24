# Tools-MCP Benchmark Harness — Handoff

This document is the complete picture of the benchmark harness as it stands in this branch. It exists so the next person picking up the optimization work (which may be you, six months from now) does not have to reconstruct it from commit messages and code archaeology.

It covers:

1. Why the harness exists (the optimization context it serves).
2. Everything the harness measures, and why each scenario is shaped the way it is.
3. How to build and run it, end to end, on a fresh Windows machine.
4. Every footgun encountered during construction, with the diagnosis and the fix.
5. The CPU and GPU baseline numbers captured on the reference machine.
6. The optimization conclusions those baselines actually justify.

Read it as a runbook + design rationale combined. If you change the harness, update the relevant section here so the next person doesn't re-discover the same dead ends.

---

## 1. Why this exists

The work started from an optimization triage over `tools-mcp-semantic` and `tools-mcp-local`. The triage identified concrete hot paths in both crates and flagged that ranking the fixes by impact required actual measurement, not intuition. The semantic crate had **zero benchmark coverage** at the start. The local crate had one bench (`tools-mcp-local/benches/search_memory.rs`) but did not cover the warm-query freshness-validation path called out in the triage.

The first principle established was: **profile or bench before optimizing.** A handful of items were low-risk enough to ship without measurement (FastEmbed init dedup, semantic timeout wrappers, allocation cleanups), but every structural change — bounded indexing pipeline, scope-cache metadata reuse, freshness model rework, model pooling, batch-size tuning — needed numbers behind it.

The harness exists to produce those numbers.

The original triage's "recommended patch order" was:

1. FastEmbed initialization deduplication.
2. **Add semantic benchmarks before changing batching or model concurrency** ← this work.
3. Move Search cold index / scope builds off the async runtime.
4. Rework Search freshness validation with a dirty-generation or watcher model (SDD-scale work).
5. Batch semantic indexing end-to-end.
6. Smaller allocation / downcast cleanups.

This branch started by delivering step 2. After the benchmark baseline was captured, the first measured optimization from Section 7.1 also landed: execution-provider-aware embedding batch sizing. The baseline numbers captured here (Section 6) are still the gate the other items have to pass.

---

## 2. What the harness measures

Two bench binaries live in `tools-mcp-semantic`, sharing one source file (`benches/semantic.rs`):

| Target | Required features | Purpose |
|---|---|---|
| `semantic_cpu` | `bench-api` | CPU baseline. No GPU code is compiled in. |
| `semantic_gpu` | `bench-api`, `gpu-cuda` | Same scenarios with the CUDA execution provider registered on the embedding model. |

A third bench lives in `tools-mcp-local/benches/search_memory.rs` and covers the Search side. That target is unchanged from before this branch except for one added scenario (Section 2.6).

### 2.1 `semantic_cold_index/{50,200}_files`

Each iteration creates a fresh tempdir under the bench workspace with `N` deterministically-generated Rust files and runs `SemanticIndex` against it. The fresh per-iteration directory guarantees zero manifest cache hits, so every iteration exercises the full pipeline: discovery, file read, hash, tree-sitter chunking, embedding, LanceDB add, manifest write.

This is the headline end-to-end scenario. Because embedding dominates, it is the cleanest read on whether a change moved the indexing pipeline.

Sample size 10, measurement time 60 s. Cold runs are expensive on CPU (28+ s per iteration at 200 files) so criterion's default sample size would have made the bench multi-hour.

### 2.2 `semantic_incremental_index/unchanged_200`

Re-runs `SemanticIndex` against the warm `incremental_baseline/` fixture without modifying any files. Every file's hash matches the manifest, so the manifest skip path fires for all 200 entries. Measures the pure overhead of discovery + per-file `fs::metadata` + manifest validation + the no-op LanceDB write path.

If C1 (move cold builds off async runtime) lands, this is the scenario that will show whether the async-runtime contention was real overhead or noise.

Default sample size, 10 s measurement.

### 2.3 `semantic_incremental_index_delta/5_changed_of_200`

Each iteration rewrites 5 of the 200 fixture files with revised content (rotating which 5 across iterations to spread the load), then runs `SemanticIndex`. The dirty set's hashes change, 5 files get re-chunked and re-embedded, the other 195 hit the skip path.

This is the realistic incremental scenario — the common case for a long-lived watch + reindex workflow.

Sample size 10, measurement time 30 s. Each iteration must do real work, so the sample-size cap matters.

### 2.4 `semantic_warm_query/{default, with_language_filter}`

Each iteration runs `SemanticSearch` against the warm `warm/` index with a fixed query. Default sample size, 10 s measurement.

**This scenario is NOT a good headline GPU metric.** Most of the time is LanceDB lookup + manifest validation overhead, with only one short embedding call. On the reference machine: CPU ≈ 27 ms, GPU ≈ 25 ms. The CUDA EP is active during this run — the limited delta is real, not a configuration miss.

Useful for measuring LanceDB-side and freshness-validation-side changes.

### 2.5 `semantic_search_under_load/search_during_background_index`

Spawns a background indexer that continuously reindexes `concurrent_indexer_target/` with `force: true` (so it does real embedding work on every loop). Measures `SemanticSearch` latency against the warm `warm/` index while the indexer is busy.

This is the scenario for evaluating model-pool work or concurrent-EP contention. Stops the indexer cleanly via a `tokio::sync::watch` channel.

Sample size 10, measurement time 30 s.

### 2.6 `semantic_embed_documents_batch_size/batch_{16,32,64,128,256}`

Calls `FastEmbedProvider::embed_documents_with_batch_size` directly against a synthesized 256-document corpus, varying the internal batch size handed to FastEmbed.

This is the pure-embedding throughput scenario. It bypasses discovery, chunking, manifest, and LanceDB so the only variable is the embedding step itself.

The internal batch size is the parameter the original triage's B2 hypothesis was about: production code originally had `INDEX_EMBEDDING_BATCH_SIZE = 128` in `model.rs` and `DEFAULT_BATCH_SIZE = 32` in `embedding.rs`. This scenario isolated the question of which value was right.

Sample size 10, measurement time 30 s per batch size.

### 2.7 `tools-mcp-local` Search extension: `warm_query_large_workspace_with_ignore`

Added one scenario to `tools-mcp-local/benches/search_memory.rs`:

- 16 subdirectories × 256 files per subdirectory = 4096 indexed files.
- Each subdirectory has its own `.gitignore` excluding `*.tmp` and `scratch/`.
- A few `.tmp` decoy files in each subdirectory to exercise the ignore path.

Exercises the warm-query freshness-validation hot path (per-indexed-file `fs::metadata` sweep + directory fingerprint check + ignore fingerprint rebuild) at a scale where those costs are measurable.

The existing `cold_index_build_public_tool`, `warm_query_default_ignore_validation`, and `large_postings_intersection` scenarios were left untouched.

---

## 3. Quick start (the runbook)

### 3.1 One-time prerequisites

| Requirement | Why | Where I got it |
|---|---|---|
| Rust toolchain matching workspace `rust-version = "1.94"` | The workspace pins this. | rustup |
| `protoc` (Protocol Buffers compiler) | `lance` (LanceDB's storage layer) build script hard-requires it. Without it, every dependent compile fails. | `winget install Google.Protobuf` (v34.1 worked) |
| CUDA Toolkit 13.2 | Only needed for `semantic_gpu`. Provides `nvcc`, headers, and the CUDA runtime DLLs (`cudart64_13.dll`, etc.) on `PATH`. | NVIDIA installer; puts itself at `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.2` |
| cuDNN ≥ 9.19 (we used 9.22) | Only needed for `semantic_gpu`. Loaded dynamically by `onnxruntime_providers_cuda.dll`. Has to be reachable at runtime; not on `PATH` after default installer. | NVIDIA Developer download; landed at `C:\Program Files\NVIDIA\CUDNN\v9.22\bin\12.9\x64` |
| Locally-built ONNX Runtime with CUDA EP | Only needed for `semantic_gpu`. Pyke's prebuilt `cu13` distribution is ORT 1.17.1 which is too old for `ort 2.0.0-rc.12` (Section 5.4). Build from source. | Section 3.2 |

### 3.2 Building ONNX Runtime for the GPU bench

Skip this section if you only need the CPU bench.

From an ONNX Runtime source checkout:

```powershell
.\build.bat --config Release --use_cuda `
  --cuda_home "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.2" `
  --cudnn_home "C:\Program Files\NVIDIA\CUDNN\v9.22" `
  --build_shared_lib --parallel `
  --cmake_extra_defines CMAKE_CUDA_ARCHITECTURES=120 `
  --skip_tests
```

`CMAKE_CUDA_ARCHITECTURES=120` is the load-bearing flag for RTX 50-series (Blackwell consumer, sm_120). Without it the binary excludes sm_120 PTX, the runtime JITs the first kernel launch (slow once, cached after), and cuDNN 9.x has been known to fail outright in this state. Other architecture numbers if you have different hardware: 80 (Ampere A100), 89 (Ada Lovelace consumer), 90 (Hopper). Include multiple by separating with semicolons.

The result you need lives at `build/Windows/Release/Release/`:

- `onnxruntime.dll` (~16 MB)
- `onnxruntime.lib` (~2 KB — import library, not a static lib)
- `onnxruntime_providers_cuda.dll` (~80 MB)
- `onnxruntime_providers_shared.dll` (~10 KB)

Record that directory path; the bench setup needs it as `$env:ORT_LIB_LOCATION`.

### 3.3 CPU bench

```powershell
$env:PROTOC = "C:\Users\Daniel\AppData\Local\Microsoft\WinGet\Packages\Google.Protobuf_Microsoft.Winget.Source_8wekyb3d8bbwe\bin\protoc.exe"

# Optional: keep ORT_LIB_LOCATION set to share the same ORT build as the GPU bench, for
# apples-to-apples ORT versioning across the comparison. If unset, ort-sys downloads pyke's
# CPU-only prebuilt and uses that instead.
$env:ORT_LIB_LOCATION = "C:\Users\Daniel\onnxruntime\build\Windows\Release\Release"

cargo bench --bench semantic_cpu -p tools-mcp-semantic --features bench-api -- `
  "semantic_embed_documents_batch_size|semantic_cold_index/200_files|semantic_incremental_index_delta/5_changed_of_200" `
  --save-baseline cpu
```

The first run after `cargo clean` will compile the release-mode bench binary; budget 3-5 minutes for that. After that, the run itself takes ~10 minutes for the three scenarios above.

The first run on a new machine also downloads the FastEmbed model (~160 MB jina-embeddings-v2-base-code) into `<sys_temp>/tools-mcp-bench-workspace/.tools-mcp/semantic-index/models/`. This is one-time; the workspace persists across `cargo bench` runs and across `cargo clean`. Delete the directory manually to reset.

### 3.4 GPU bench

```powershell
$env:PROTOC              = "C:\Users\Daniel\AppData\Local\Microsoft\WinGet\Packages\Google.Protobuf_Microsoft.Winget.Source_8wekyb3d8bbwe\bin\protoc.exe"
$env:ORT_LIB_LOCATION    = "C:\Users\Daniel\onnxruntime\build\Windows\Release\Release"
$env:ORT_CUDA_VERSION    = "13"
$env:PATH                = "C:\Users\Daniel\onnxruntime\build\Windows\Release\Release;" + $env:PATH
$env:BENCH_GPU_DLL_PATHS = "C:\Program Files\NVIDIA\CUDNN\v9.22\bin\12.9\x64"

cargo bench --bench semantic_gpu -p tools-mcp-semantic --features bench-api,gpu-cuda -- `
  "semantic_embed_documents_batch_size|semantic_cold_index/200_files|semantic_incremental_index_delta/5_changed_of_200" `
  --baseline cpu
```

`--baseline cpu` makes criterion print `change: [...]` lines comparing each scenario against the CPU baseline saved by the previous run. `Performance has improved` / `Performance has regressed` annotations are direct CPU↔GPU deltas.

The build directory env vars MUST be set in the parent shell *before* `cargo bench`. The OS loader resolves `onnxruntime.dll`'s import table at process startup, before any Rust code runs, so a runtime `set_var` from inside the bench is too late. (See Section 5.6 for the gory detail.)

### 3.5 What output to expect

Criterion writes per-scenario results to `target/criterion/<group>/<scenario>/`. With `--save-baseline cpu`, the CPU run also writes to `target/criterion/<group>/<scenario>/cpu/`, which the GPU run reads back. Outputs include `estimates.json`, `sample.json`, `report/index.html` — the HTML reports are useful for distribution visualization (criterion needs `gnuplot` for nicer plots; without it, it falls back to `plotters` which produces functional but plainer output).

The bench also asserts (via `assert_tool_ok` in `benches/semantic.rs`) that every `SemanticIndex` / `SemanticSearch` tool call returned `isError: false`. If you see panics with messages like "returned error: semantic index is empty for model …" — the bench fixtures were not indexed before the scenario started. See Section 5.2 for the likely cause.

---

## 4. Architecture decisions and what they cost

### 4.1 Persistent workspace in system temp

Workspace lives at `<std::env::temp_dir()>/tools-mcp-bench-workspace/`. Persistent across `cargo bench` invocations and `cargo clean` so the 160 MB FastEmbed model and the warm fixtures are amortized to once-per-machine.

**Why not under `target/`:** `tools-mcp-semantic/src/discovery.rs::should_skip_path` excludes any path with `target` as a component. Placing fixtures under `target/` would make every bench silently return zero indexed files (the discovery walker just skips them) and the search would error with `semantic index is empty for model …`. This was the first footgun encountered; see Section 5.2.

**Why not a fresh tempdir per run:** the FastEmbed model cache lives at `<index_dir>/models/`. A fresh tempdir would re-download the 160 MB model every invocation.

**Cost:** state accumulates over runs. The cold-index scenario per-iteration tempdirs do get cleaned up via `tempfile::TempDir::Drop`, but the LanceDB table for the warm/incremental fixtures grows across runs. Negligible in practice.

### 4.2 CWD-based workspace anchoring

`discovery::resolve_scope` reads `std::env::current_dir()` to anchor the workspace. The harness sets CWD to the bench workspace exactly once in `bench_workspace()`, behind a `OnceLock`. Every scenario uses sub-paths relative to that CWD.

**Why this is OK in a process-shared bench harness:** criterion runs all scenarios in a single process, single-threaded for the main bench function. The CWD change happens before any scenario; no concurrent reader sees the transition.

**What this rules out:** running the bench under a wrapper that itself depends on the original CWD. Not a concern for `cargo bench` directly.

### 4.3 `bench-api` feature for crate-internal exposure

`FastEmbedProvider` is `pub(crate)`. The bench needs to call `embed_documents_with_batch_size` directly (for the B2 batch-size scenario). Options were:

1. Make the type and methods `pub`. **Rejected** — production callers would gain access to an internal-only API.
2. Add a public wrapper module gated by a feature. **Chosen.**

The wrapper is at `tools-mcp-semantic/src/lib.rs::bench`. It exposes a thin `FastEmbedProvider` struct that delegates to the internal one. Methods exposed: `new`, `embed_documents`, `embed_documents_with_batch_size`, `model_id`. The `embed_documents_with_batch_size` method on the internal `FastEmbedProvider` is itself gated `#[cfg(feature = "bench-api")]` because it's a bench-only need.

**Cost:** the bench binary is the only consumer of the `bench-api` feature. The feature is declared but adds zero compilation cost for normal builds (no extra deps gated by it).

### 4.4 `gpu-cuda` feature, default-off, with optional `ort` dep

The `gpu-cuda` feature on `tools-mcp-semantic` is the entry point for GPU acceleration:

```toml
[features]
gpu-cuda = ["dep:ort", "ort/cuda"]

[dependencies]
ort = { workspace = true, optional = true }
```

When enabled, the production `FastEmbedProvider::new` registers the CUDA execution provider via `InitOptions::with_execution_providers([CUDA::default().build().error_on_failure()])`. When disabled, the production code path has no reference to `ort` at all and the dependency is not pulled into the build graph.

`.error_on_failure()` is important. ORT's default behavior is to silently fall back to CPU if EP registration fails. That would have hidden the System32 DLL hijack completely; the bench would have reported "GPU is engaged" while actually running on CPU. With `error_on_failure`, registration failures panic loudly.

**Why `dep:ort` rather than relying on fastembed's transitive `ort`:** fastembed's `cuda` cargo feature is *candle-only*, not ORT. It does nothing for ORT-path models like Jina v2. To enable `ort/cuda` on the unified dep graph, we needed `ort` as a direct dependency.

**Cost:** when `gpu-cuda` is on, cargo unifies `ort/cuda` with fastembed's transitive `ort/download-binaries`, and both features apply. The `download-binaries` side becomes a no-op when `ORT_LIB_LOCATION` is set, so we don't actually pay for a duplicate download.

### 4.5 Two bench targets, one source file

```toml
[[bench]]
name = "semantic_cpu"
path = "benches/semantic.rs"
harness = false
required-features = ["bench-api"]

[[bench]]
name = "semantic_gpu"
path = "benches/semantic.rs"
harness = false
required-features = ["bench-api", "gpu-cuda"]
```

Both targets point at `benches/semantic.rs`. The source gates GPU-specific behavior internally via `#[cfg(feature = "gpu-cuda")]` (only in `src/embedding.rs::initialize_model`), so the same source file serves both.

**Why two targets rather than one:** with one target, switching between CPU and GPU runs requires toggling `required-features` in `Cargo.toml`, which is bad ergonomics for repeatable side-by-side baselines and risks accidentally measuring the wrong configuration. Two targets give clean separate commands.

**Cost:** Cargo emits one warning per bench invocation: `file 'benches/semantic.rs' found to be present in multiple build targets`. It's informational, not an error. The alternative — moving shared logic into `benches/common/mod.rs` with tiny driver files — is more maintenance overhead than it's worth.

### 4.6 `build.rs` for ORT DLL co-location

`tools-mcp-semantic/build.rs` copies `onnxruntime.dll` and `onnxruntime_providers_shared.dll` from `$ORT_LIB_LOCATION` to `target/<profile>/deps/` whenever `ORT_LIB_LOCATION` is set. Adds `onnxruntime_providers_cuda.dll` (~80 MB) when `CARGO_FEATURE_GPU_CUDA` is also set.

**Why this exists:** see Section 5.6 (System32 hijack). Short version: Windows ships its own `onnxruntime.dll` (version 1.17.x, Windows ML stack) in `C:\Windows\System32\`. PATH manipulation cannot win against System32 in the DLL search order. The only reliable fix is to put our DLL in the binary's application directory, which is searched first.

**Why not rely on ort's `copy-dylibs` feature:** it works for `cargo run` binaries but does not place DLLs where bench binaries can find them. ort-sys also doesn't copy when using `system` linking strategy (with `ORT_LIB_LOCATION` set) — it assumes the user has it on PATH, which on Windows is insufficient.

**Cost:** ~16 MB copy on every relevant build for the CPU target, ~96 MB for the GPU target. Cached by cargo via OUT_DIR change detection, so only re-copies when the source DLLs change.

### 4.7 `BENCH_GPU_DLL_PATHS` for runtime cuDNN reachability

The harness calls `ensure_gpu_dll_paths()` at startup, which prepends every directory listed in `$BENCH_GPU_DLL_PATHS` (semicolon-separated) to the process PATH. The intended use is to make cuDNN's bin directory reachable.

**Why this works at runtime when `onnxruntime.dll` itself doesn't:** `onnxruntime_providers_cuda.dll` is loaded *lazily* by `onnxruntime.dll` when the CUDA EP is first registered, which happens after `main()` and after our PATH prepend. Once `providers_cuda.dll` is loaded, *its* import resolutions (for `cudnn*.dll`, `cudart64_13.dll`, etc.) use the *current* PATH, which now includes the cuDNN directory.

CUDA toolkit's own bin (`cudart64_13.dll`, etc.) is typically on PATH already from the installer, but cuDNN is not — the user manually drops it into a versioned `bin/12.9/x64/` subdirectory that nothing else knows about. Hence the env var.

**Cost:** zero when unset. A few microseconds when set.

---

## 5. Footguns encountered (and their fixes)

The order here is the order they bit.

### 5.1 `protoc` missing from the build environment

**Symptom:** `cargo check -p tools-mcp-semantic --tests` (or any compile of the semantic crate) fails with `Could not find 'protoc'`. The error comes from `lance`'s build script — `lance` is a transitive dep of `lancedb`, and its protobuf code generation hard-requires `protoc`.

**Fix:** `winget install Google.Protobuf`. Confirmed working on this machine with v34.1. After install, restart the shell or set `$env:PROTOC` to `<install-path>\bin\protoc.exe`. The install adds itself to PATH but existing shells don't pick it up automatically.

**Why this is its own item:** a pre-existing environment gap, not something my bench code introduced. Anyone setting up the workspace fresh hits this regardless of bench work.

### 5.2 Bench workspace under `target/` makes discovery silently return zero files

**Symptom:** Bench scenarios report success but per-iteration latencies are absurdly fast (initially 82 µs for warm_query). `SemanticSearch` returns `isError: true` with the message "semantic index is empty for model jina-embeddings-v2-base-code", but without explicit error-checking in the bench helpers, the failure looks like fast successful runs.

**Diagnosis:** `discovery::should_skip_path` excludes any path containing `target` as a component. The bench workspace was originally at `target/bench-semantic-workspace/`. Discovery walked into it, hit the `target` component check, and skipped every file. The indexing pass produced zero chunks, never created a LanceDB table, never set `manifest.table_name`, so subsequent searches errored out with "index is empty."

**Fix:** Move the bench workspace to `std::env::temp_dir().join("tools-mcp-bench-workspace")`. No `target` component anywhere in the path.

**Diagnostic hardening:** added `assert_tool_ok` in the bench helpers that panics if any tool returns `isError: true`. Without this assertion, the bench would silently measure the error path (which is microseconds) and report misleading numbers. Carry this pattern into any new bench helpers.

### 5.3 fastembed's `cuda` cargo feature is candle-only

**Symptom:** N/A — caught during pre-implementation reading of fastembed's `Cargo.toml`.

**Wrong path I almost took:** enabling fastembed's `cuda` feature thinking it would enable CUDA on the embedding model.

**Reality:** the fastembed `[features]` section makes this explicit:

```toml
cuda = ["qwen3", "nomic-v2-moe", "candle-core/cuda", "candle-nn/cuda"]
directml = ["ort/directml"]
```

`cuda` enables CUDA *only* for fastembed's candle-backed models (qwen3, nomic-v2-moe). Jina v2 base code runs through the ORT path, where there's no `cuda` feature at the fastembed level. The only ORT-path GPU feature fastembed exposes is `directml`.

**Fix:** add `ort` as a direct optional dependency on `tools-mcp-semantic` and enable `ort/cuda` via the `gpu-cuda` feature. Pass `CUDA::default().build()` to fastembed's `InitOptions::with_execution_providers(...)`, which it forwards to the ORT SessionBuilder underneath.

### 5.4 `ORT_STRATEGY` env var doesn't exist in `ort 2.0.0-rc.12`

**Symptom:** Set `$env:ORT_STRATEGY = "system"` + `$env:ORT_LIB_LOCATION = <build dir>` based on documentation found in WebSearch results. Build runs; `ort-sys` still chose "Using prebuilt binaries". Inspecting `target/release/build/ort-sys-*/output` showed the build script never read `ORT_STRATEGY`.

**Diagnosis:** Read `ort-sys 2.0.0-rc.12`'s actual `build/vars.rs`:

```rust
pub const SYSTEM_LIB_PATH: &[&str] = &["ORT_LIB_PATH", "ORT_LIB_LOCATION"];
pub const PREFER_DYNAMIC_LINK: &str = "ORT_PREFER_DYNAMIC_LINK";
pub const SKIP_DOWNLOAD: &[&str] = &["CARGO_NET_OFFLINE", "ORT_SKIP_DOWNLOAD", "ORT_OFFLINE"];
pub const CUDA_VERSION: &str = "ORT_CUDA_VERSION";
```

No `ORT_STRATEGY`. The variable name is from older ort versions; pyke's docs and most search results have not caught up. `ORT_LIB_LOCATION` alone is sufficient — `build/main.rs` checks it first, and if set, links against the system lib instead of downloading.

**Fix:** Drop `ORT_STRATEGY`. Set `ORT_LIB_LOCATION` (and optionally `ORT_CUDA_VERSION` for CUDA disambiguation).

**Bonus catch:** the original (uncleaned) build cached the "prebuilt" decision because the env var hadn't been set when the build script first ran. Cargo's `rerun-if-env-changed` does trigger re-runs, but the safer move when changing env-driven build behavior is `cargo clean -p ort-sys -p ort -p tools-mcp-semantic`.

### 5.5 Pyke's prebuilt `cu13` distribution is ORT 1.17.1 — too old for ort 2.0.0-rc.12

**Symptom:** Once `ORT_LIB_LOCATION` was unset (deliberately, to test the pyke download path), build succeeded. Bench failed at runtime with:

```
The requested API version [24] is not available, only API versions [1, 17] are
supported in this build. Current ORT Version is: 1.17.1
```

**Diagnosis:** Pyke distributes prebuilt ORT binaries via `https://parcel.pyke.io/.../ort/<version>/release`. For the `cu13` (CUDA 13.x) feature set on `ort 2.0.0-rc.12`, the bundled ORT is version 1.17.1. The Rust `ort` crate is compiled for ORT API version 24, which corresponds to ORT 1.19+. ABI mismatch.

**Fix:** Build ONNX Runtime from source against your local CUDA toolkit (Section 3.2). There is no working precompiled CUDA path on Windows for `ort 2.0.0-rc.12` at the time of writing.

### 5.6 `C:\Windows\System32\onnxruntime.dll` hijacks DLL resolution

**Symptom:** Even after successful build against ORT 1.27.0 (the source build), runtime still reported "Current ORT Version is: 1.17.1". Same ABI mismatch error as 5.5 but for a different reason.

**Diagnosis:** Searched the system for every `onnxruntime.dll` and found:

```
C:\Windows\System32\onnxruntime.dll          version 1.17.260311-1434.1.os-germanium.6bcd20d
C:\Windows\SysWOW64\onnxruntime.dll          version 1.17.260311-1434.1.os-germanium.6bcd20d
```

Windows itself ships an `onnxruntime.dll` as part of its OS ML stack (Windows ML uses it). It's in `System32`, which is step 2 of the Windows DLL search order. PATH is step 6. So no amount of PATH manipulation can win — the OS DLL is found first.

The only DLL search step that beats `System32` is step 1: the binary's application directory. Putting our `onnxruntime.dll` next to the bench `.exe` (which lives in `target/<profile>/deps/`) is the fix.

**Fix:** `tools-mcp-semantic/build.rs` copies our ORT DLLs from `$ORT_LIB_LOCATION` to `target/<profile>/deps/` (derived from `OUT_DIR`) when `ORT_LIB_LOCATION` is set. Generalized to handle both CPU and GPU bench targets so they share an ORT version (Section 4.6).

**Why this is uniquely a Windows problem:** macOS and Linux do not ship an OS-level `libonnxruntime` and have different dynamic loader semantics. On those platforms, `rpath` settings or `LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH` are sufficient.

**Diagnostic value:** without `.error_on_failure()` on the CUDA EP registration, this entire failure would have manifested as "CUDA EP silently fell back to CPU" — bench numbers would have looked CPU-shaped (Section 4.4 covers why `error_on_failure` was added).

### 5.7 (Non-issue, documented for future you) Cargo warning about multiple bench targets

```
warning: ... `benches/semantic.rs` found to be present in multiple build targets
```

Informational. Section 4.5 explains why two targets share the source. The warning is fixed-once-per-build, not per-iteration; the build itself succeeds.

---

## 6. Baseline numbers (May 2026, reference machine)

**Reference machine:** Windows 11, AMD Radeon iGPU + NVIDIA GeForce RTX 5090 (32 GB), CUDA 13.2 toolkit, cuDNN 9.22, locally-built ORT 1.27.0.

All measurements via `cargo bench --bench semantic_{cpu,gpu}` with `--save-baseline cpu` then `--baseline cpu`, sample size 10, criterion-default warmup. Three iterations were observed during the GPU run to confirm GPU engagement via `nvidia-smi`: 64% utilization, 9.8 GB VRAM at peak.

`ORT_LIB_LOCATION` was set for both passes so the only variable between CPU and GPU runs is the registered execution provider.

### 6.1 Cold index

| Scenario | CPU median | GPU median | Speedup |
|---|---:|---:|---:|
| `semantic_cold_index/200_files` | **28.9 s** | **1.10 s** | **~26×** |

End-to-end pipeline. Discovery + read + chunk + embed + LanceDB write + manifest save, on 200 files producing roughly 1000 chunks. Embedding dominates; the 26× speedup is essentially the embedding speedup, with the non-embedding tail proportionally larger on GPU.

### 6.2 Incremental delta

| Scenario | CPU median | GPU median | Speedup |
|---|---:|---:|---:|
| `semantic_incremental_index_delta/5_changed_of_200` | **815 ms** | **237 ms** | **~3.4×** |

Realistic incremental workload. 5 files modified, 195 hash-current. The 3.4× is much smaller than cold-index because the non-embedding work (per-file `fs::metadata`, manifest validation, file reads for hashing, LanceDB delete-then-add) is a much larger fraction of total time when only 5 files need embedding. This is the scenario where the C-bucket optimizations from the original triage become *more* important on GPU.

### 6.3 Embedding batch-size sweep

| Batch size | CPU median | GPU median | GPU speedup |
|---:|---:|---:|---:|
| 16  | 4.36 s | 226 ms | 19× |
| 32  | **4.17 s** ← CPU optimum | 148 ms | 28× |
| 64  | 4.42 s | 131 ms | 34× |
| 128 | 4.37 s | **127 ms** ← GPU optimum | 35× |
| 256 | 5.27 s | 133 ms | 40× |

Pure embedding throughput on a fixed 256-document corpus.

**CPU shape:** fairly flat 16-128 with batch_32 narrowly winning. Clear regression at 256 (likely cache effects). The CPU/default production batch size of 32 is correct.

**GPU shape:** monotonic improvement 16 → 32 → 64 → 128 (each step ~10-15% faster), marginal regression at 256. The optimum is batch_128. Running batch_32 on GPU costs ~14% of throughput (148 ms vs 127 ms). batch_16 costs ~44% (226 ms vs 127 ms).

**B2 hypothesis from the original triage: validated, with a twist.** The original mismatch between `INDEX_EMBEDDING_BATCH_SIZE = 128` (model.rs) and `DEFAULT_BATCH_SIZE = 32` (embedding.rs) was real. The right answer is execution-provider-dependent: 32 for CPU, 128 for GPU. The resumed pass now resolves this through a shared batch-size selector.

### 6.4 Warm query (informational only)

Not measured in the comparison pass because it's not a useful headline metric. Earlier smoke runs showed CPU ≈ 27 ms, GPU ≈ 25 ms — a ~10% delta, because warm-query is dominated by LanceDB lookup + manifest validation, not by the single short query embedding. CUDA is active during the GPU warm-query run; the limited delta is real, not a configuration miss.

---

## 7. What the numbers actually justify

In rough priority order, gated by the baseline above:

### 7.1 EP-aware batch size landed (free win)

Status after resuming this handoff: landed.

Implementation shape:

- `embedding.rs::default_embedding_batch_size()` returns 32 for CPU/default builds and 128 when `gpu-cuda` is compiled.
- `FastEmbedProvider::embed_documents`, `FastEmbedProvider::embed_query`, and `model.rs::embed_index_chunks` all consume the same selector so the FastEmbed internal batch size and the semantic indexing batch size stay aligned.
- Tool schemas, response shapes, manifest format, and LanceDB record format are unchanged.

The bench proves ~14% GPU embedding-throughput win at batch_128 vs batch_32. CPU stays on batch_32. The next iteration could make this environment-driven so production users can tune it without recompiling.

### 7.2 Re-rank the C-bucket items against GPU numbers

The original triage's C-bucket (medium blast radius, structural) items were:

- C1: move Search cold builds off the async runtime
- C2: bounded batch pipeline for semantic indexing
- C3: bounded concurrent reads + blocking chunking for semantic
- C4: fuzzy candidate borrow refactor

With embedding 30× faster on GPU, the *relative* cost of every non-embedding step rose proportionally. The 237 ms incremental_delta number on GPU is now dominated by manifest validation, LanceDB delete-then-add, file reads for hashing, and async-runtime contention. Worth running a flamegraph on the GPU incremental_delta scenario to identify the new hot paths before committing to a C-bucket order.

The CPU baseline still validates C1 and C2 — they were structural wins on CPU and they remain wins on GPU.

### 7.3 Probably skip model pooling (Sem #2 second half) for now

With a single GPU model already delivering 30× speedup on cold_index, parallel embedding via multiple model copies is unlikely to be the bottleneck. The 5090 was at 64% utilization during cold_index — there's headroom, but the gain from filling that headroom is bounded.

The `search_during_background_index` scenario can confirm this directly: if search latency under indexer load is dramatically worse than warm search latency, model pooling has a case. If they're close, skip it.

### 7.4 The D-bucket SDD (freshness rework) is unchanged

The Search freshness validation rework (D1 in the original triage) is unaffected by these GPU numbers — it's about a different code path. Still the largest design question outstanding, still needs an SDD.

### 7.5 Setup tasks worth automating

If this work moves into shared infrastructure, the env-var ceremony in Sections 3.3 and 3.4 will bite people. Two options:

- A `.cargo/config.toml` per-developer override with `[env]` entries pointing at their local ORT build. Per-machine, not in source control.
- A small `bench.ps1` / `bench.sh` wrapper that sets the env vars and exec's `cargo bench`. Source-controlled, with paths as variables.

Either is fine. Not blocking but worth doing before more people pick this up.

---

## 8. File inventory

What changed in this branch:

| Path | Change |
|---|---|
| `Cargo.toml` (workspace) | Added `ort = { version = "=2.0.0-rc.12", default-features = false }` to `[workspace.dependencies]`. |
| `tools-mcp-semantic/Cargo.toml` | Added `bench-api` and `gpu-cuda` features. Added optional `ort` dep. Added two `[[bench]]` entries (`semantic_cpu`, `semantic_gpu`). Added dev-deps for criterion + futures + tempfile + tokio (rt-multi-thread). |
| `tools-mcp-semantic/src/lib.rs` | Added `#[cfg(feature = "bench-api")] pub mod bench` wrapping `FastEmbedProvider` for bench access. |
| `tools-mcp-semantic/src/embedding.rs` | Extracted `batch_size` as a parameter of `embed_prefixed`. Added `embed_documents_with_batch_size` gated on `bench-api`. Added `default_embedding_batch_size()`, returning 32 for CPU/default builds and 128 for `gpu-cuda`. In `initialize_model`, registers CUDA EP via `with_execution_providers([CUDA::default().build().error_on_failure()])` under `#[cfg(feature = "gpu-cuda")]`. |
| `tools-mcp-semantic/src/model.rs` | Uses `default_embedding_batch_size()` for semantic indexing batch assembly instead of a hardcoded indexing batch constant. |
| `tools-mcp-semantic/build.rs` | New file. Copies ORT DLLs from `$ORT_LIB_LOCATION` to `target/<profile>/deps/` to defeat the System32 hijack. Conditional on `CARGO_FEATURE_GPU_CUDA` for the CUDA provider DLL. |
| `tools-mcp-semantic/benches/semantic.rs` | New file. The harness itself. 8 scenarios across 5 groups. |
| `tools-mcp-local/benches/search_memory.rs` | Added `warm_query_large_workspace_with_ignore` scenario + `large_ignored_fixture_dir` helper. |
| `tools-mcp-semantic/BENCHMARK_HANDOFF.md` | This file. |

What stayed unchanged: every other crate, every production code path in `tools-mcp-local`, every public API of `tools-mcp-semantic` (except the bench-api gated additions), every tool schema, and every response shape. Default CPU builds now use batch_32 consistently across FastEmbed and semantic indexing; `gpu-cuda` builds use batch_128 consistently.

---

## 9. Reference: env var quick lookup

| Env var | When set | What it does |
|---|---|---|
| `PROTOC` | All builds of `tools-mcp-semantic` | Absolute path to `protoc.exe`. Required because `lance`'s build script can't find it on PATH on some systems. |
| `ORT_LIB_LOCATION` | GPU bench, optionally CPU bench | Tells `ort-sys` to link against the user-built ORT in this directory instead of downloading the pyke prebuilt. Also triggers `build.rs` DLL co-location. |
| `ORT_CUDA_VERSION` | GPU bench only | `"13"` or `"12"`. Tells `ort-sys` which CUDA major version your build was compiled against, used during dist resolution if a download path is taken. |
| `PATH` (prepended) | GPU bench only | Must include the directory containing `onnxruntime.dll` *before* `cargo bench` is invoked. The OS loader resolves the import at process startup, before any Rust runs. |
| `BENCH_GPU_DLL_PATHS` | GPU bench only | Semicolon-separated list of additional directories to prepend to PATH at *runtime* (after Rust starts). Intended for cuDNN, which is loaded lazily by `providers_cuda.dll`. |
| `CARGO_FEATURE_GPU_CUDA` | Automatic | Set by cargo when the `gpu-cuda` feature is active. Used by `build.rs` to decide whether to copy the CUDA provider DLL. |

---

## 10. Reference: relevant external links

- ORT 2.x docs: https://ort.pyke.io/
- ort-sys 2.0.0-rc.12 build script source (the authoritative source for env-var behavior): `~/.cargo/registry/src/index.crates.io-*/ort-sys-2.0.0-rc.12/build/`
- fastembed-rs Cargo.toml (to confirm what `cuda`/`directml` features actually do): https://github.com/Anush008/fastembed-rs/blob/main/Cargo.toml
- ONNX Runtime build instructions for Windows + CUDA: https://onnxruntime.ai/docs/build/eps.html#cuda
- The original optimization triage that started this work: see the conversation history that produced this branch. The triage's recommended patch order is summarized in Section 1.
