//! Semantic crate benchmark harness.
//!
//! Establishes a baseline for the semantic indexing + search pipeline before the optimization
//! passes called out in the triage land. Scenarios are designed to isolate the hot paths flagged
//! in the analysis:
//!
//! * `cold_index_*` — full indexing path: discovery, read, chunking, embedding, LanceDB write.
//! * `incremental_unchanged_*` — exercises the manifest skip path on warm files.
//! * `incremental_5_changed_of_*` — realistic delta indexing (mutate a handful of files, reindex).
//! * `warm_query*` — query latency against a warm LanceDB table with the model already loaded.
//! * `search_during_background_index` — search latency while a forced reindex runs concurrently.
//! * `embed_documents_batch_size_*` — direct FastEmbed throughput at varying internal batch sizes
//!   (gated on the `bench-api` feature; required for the B2 batch-size investigation).
//!
//! ## First-run cost
//!
//! On the first invocation per machine, FastEmbed downloads the jina-embeddings-v2-base-code
//! model (~160 MB) into `<sys_temp>/tools-mcp-bench-workspace/.tools-mcp/semantic-index/models`.
//! The workspace persists across `cargo bench` runs (and across `cargo clean`), so subsequent
//! runs reuse the cached model and warm fixtures. To reset, delete the directory manually.
//!
//! The workspace lives in the system temp dir rather than under `target/` because
//! `discovery::should_skip_path` excludes any path with a `target` component, which would cause
//! the bench fixtures to vanish from indexing.
//!
//! ## Targets
//!
//! Two bench binaries share this source file, isolated by cargo features so each writes its
//! own release-mode artifact and there's no accidental cross-contamination:
//!
//! * `semantic_cpu` — requires `bench-api`. Pure CPU. Use as the comparison baseline.
//! * `semantic_gpu` — requires `bench-api` and `gpu-cuda`. Registers the CUDA execution
//!   provider on the embedding model. Setup steps below.
//!
//! Run side-by-side baselines with criterion's `--save-baseline`:
//!
//! ```powershell
//! cargo bench --bench semantic_cpu -p tools-mcp-semantic --features bench-api -- --save-baseline cpu
//! # ...then switch env for the GPU build...
//! cargo bench --bench semantic_gpu -p tools-mcp-semantic --features bench-api,gpu-cuda -- --save-baseline gpu
//! # compare later
//! cargo bench --bench semantic_gpu ... -- --baseline cpu
//! ```
//!
//! The scenarios worth comparing first (embedding-dominated): `semantic_cold_index`,
//! `semantic_embed_documents_batch_size`, `semantic_incremental_index_delta`.
//! `semantic_warm_query` is dominated by LanceDB lookup + manifest validation overhead, so
//! the GPU-vs-CPU delta there will be small even when CUDA is fully engaged.
//!
//! ## Running with CUDA (gpu-cuda feature)
//!
//! When the `gpu-cuda` feature is enabled, the embedding model is registered with the NVIDIA
//! CUDA execution provider. Three things must be in place:
//!
//! 1. **Build-time env vars** so `ort-sys` links against your locally-built ONNX Runtime
//!    instead of downloading pyke's prebuilt binaries. Set these in the shell that runs
//!    `cargo bench`:
//!    ```powershell
//!    $env:ORT_LIB_LOCATION = "C:\Users\Daniel\onnxruntime\build\Windows\Release\Release"
//!    $env:ORT_CUDA_VERSION = "13"
//!    ```
//!
//! 2. **PATH must include `onnxruntime.dll`'s directory before the bench process starts.**
//!    The DLL is resolved by the Windows loader at process startup (via the import table
//!    `onnxruntime.lib` creates), so a runtime `PATH` mutation inside the bench is too late.
//!    Prepend the ORT build directory in the same shell:
//!    ```powershell
//!    $env:PATH = "C:\Users\Daniel\onnxruntime\build\Windows\Release\Release;" + $env:PATH
//!    ```
//!
//! 3. **cuDNN must be reachable.** `onnxruntime_providers_cuda.dll` loads cuDNN dynamically
//!    after the bench enters Rust code, so the harness prepends the cuDNN bin directory to
//!    `PATH` at startup via `ensure_gpu_dll_paths()`. The CUDA toolkit bin (containing
//!    `cudart64_13.dll`, etc.) is expected to already be on `PATH` from the installer.
//!
//! Run:
//! ```powershell
//! cargo bench --bench semantic_gpu -p tools-mcp-semantic --features bench-api,gpu-cuda
//! ```
//!
//! ## CWD coupling
//!
//! `discovery::resolve_scope` anchors the workspace at `std::env::current_dir()`. The harness
//! `set_current_dir`s once during `bench_workspace()` initialization so every scenario shares
//! one workspace and one model cache. All scenario fixtures live as sub-paths under that
//! workspace; the indexed `path` argument is always relative.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tools_mcp_core::ToolRegistry;
use tools_mcp_semantic::bench::FastEmbedProvider;

// ---------------------------------------------------------------------------
// Workspace + fixture infrastructure
// ---------------------------------------------------------------------------

/// Number of files in the warm-query and incremental-baseline fixtures. Picked so a single
/// indexing pass completes in a few seconds on a typical dev box, keeping per-iteration setup
/// tractable while still producing enough chunks to exercise batching paths.
const WARM_FIXTURE_FILES: usize = 200;

/// Files written into the per-iteration cold-index fixtures. Two sizes give a scaling signal.
const COLD_SMALL_FILES: usize = 50;
const COLD_LARGE_FILES: usize = 200;

/// How many files to mutate per iteration in the small-delta incremental scenario.
const INCREMENTAL_CHANGED_FILES: usize = 5;

/// Bench timeout long enough to absorb worst-case cold-start variability without false failures.
const BENCH_TIMEOUT_MS: u64 = 600_000;

static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();
static FIXTURES_READY: OnceLock<()> = OnceLock::new();
static INCREMENTAL_REVISION: AtomicUsize = AtomicUsize::new(0);

/// Returns the persistent bench workspace, creating it and switching the process CWD into it on
/// first access. Idempotent.
///
/// The workspace deliberately lives in the system temp directory (not under `target/`) because
/// `discovery::should_skip_path` excludes any path containing `target` as a component. Placing
/// fixtures under `target/` would silently make every bench return zero indexed files.
fn bench_workspace() -> &'static Path {
    WORKSPACE_ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join("tools-mcp-bench-workspace");
        fs::create_dir_all(&root).expect("create bench workspace root");
        // `discovery::resolve_scope` canonicalizes CWD before comparing paths; canonicalize once
        // here so every subsequent comparison succeeds deterministically across platforms.
        let canonical = fs::canonicalize(&root).expect("canonicalize bench workspace");
        std::env::set_current_dir(&canonical).expect("set process CWD to bench workspace");
        canonical
    })
}

/// Prepends directories from the `BENCH_GPU_DLL_PATHS` env var to the process `PATH` so the
/// Windows loader can resolve transitively-loaded GPU libraries (cuDNN, CUDA runtime extras)
/// once `onnxruntime_providers_cuda.dll` is loaded.
///
/// `BENCH_GPU_DLL_PATHS` is a semicolon-separated list. When unset, this is a no-op so CPU
/// builds and non-Windows targets are unaffected. The ORT build directory itself (containing
/// `onnxruntime.dll`) MUST be on the shell's `PATH` *before* invoking `cargo bench` — it is
/// resolved by the OS loader at process startup, before any Rust code runs, so a runtime
/// mutation here is too late.
fn ensure_gpu_dll_paths() {
    let Some(extra) = std::env::var_os("BENCH_GPU_DLL_PATHS") else {
        return;
    };
    if extra.is_empty() {
        return;
    }
    let separator = if cfg!(windows) { ";" } else { ":" };
    let mut new_path = extra;
    if let Some(existing) = std::env::var_os("PATH") {
        new_path.push(separator);
        new_path.push(existing);
    }
    // SAFETY: set_var is sound here; this runs on the main thread before any other thread is
    // spawned (criterion has not yet been entered). No concurrent readers of PATH exist.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
}

/// Ensures the warm/incremental/concurrent fixtures exist on disk and that each has been indexed
/// at least once. Runs the model pre-warm as a side effect. Idempotent; safe to call from every
/// scenario but normally invoked from `main` before benches start.
fn ensure_fixtures(runtime: &Runtime, registry: &ToolRegistry) {
    FIXTURES_READY.get_or_init(|| {
        let workspace = bench_workspace();

        // Warm-query corpus — never mutated after creation.
        let warm_dir = workspace.join("warm");
        ensure_rust_fixture(&warm_dir, WARM_FIXTURE_FILES);

        // Incremental-baseline corpus — mutated 5-at-a-time during the delta scenario.
        let incremental_dir = workspace.join("incremental_baseline");
        ensure_rust_fixture(&incremental_dir, WARM_FIXTURE_FILES);

        // Concurrent-indexer target — separate so background indexing does not contend on the
        // same manifest entries as the warm-query scenarios.
        let concurrent_dir = workspace.join("concurrent_indexer_target");
        ensure_rust_fixture(&concurrent_dir, COLD_SMALL_FILES);

        // Pre-warm the FastEmbed model so the very first benched iteration does not eat the
        // model-load cost (or, on a fresh machine, the model-download cost).
        runtime.block_on(async {
            run_index(registry, "warm", false).await;
            run_index(registry, "incremental_baseline", false).await;
            run_index(registry, "concurrent_indexer_target", false).await;
        });
    });
}

/// Writes `count` deterministically-generated Rust files into `dir`, creating the directory if
/// missing. Idempotent: files that already exist with matching content are left alone.
fn ensure_rust_fixture(dir: &Path, count: usize) {
    fs::create_dir_all(dir).expect("create fixture directory");
    for index in 0..count {
        let path = dir.join(format!("file_{index:05}.rs"));
        if path.exists() {
            continue;
        }
        fs::write(&path, generate_rust_file(index, 0)).expect("write fixture file");
    }
}

/// Builds a fresh tempdir under the bench workspace with `count` Rust files. Returned `TempDir`
/// is auto-cleaned when dropped — used by cold-index scenarios so every iteration sees
/// previously-unseen paths.
fn fresh_cold_fixture(count: usize) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("cold_")
        .tempdir_in(bench_workspace())
        .expect("create cold fixture tempdir");
    for index in 0..count {
        let path = dir.path().join(format!("file_{index:05}.rs"));
        fs::write(&path, generate_rust_file(index, 0)).expect("write cold fixture file");
    }
    dir
}

/// Rewrites `INCREMENTAL_CHANGED_FILES` files in the incremental-baseline fixture with revised
/// content so their hashes change. Rotates the touched-file window across calls so different
/// files participate over time.
fn mutate_incremental_fixture() {
    let revision = INCREMENTAL_REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    let dir = bench_workspace().join("incremental_baseline");
    for offset in 0..INCREMENTAL_CHANGED_FILES {
        let file_index = (revision * INCREMENTAL_CHANGED_FILES + offset) % WARM_FIXTURE_FILES;
        let path = dir.join(format!("file_{file_index:05}.rs"));
        fs::write(&path, generate_rust_file(file_index, revision)).expect("rewrite incremental");
    }
}

/// Generates a syntactically-valid Rust file with multiple top-level symbols so the tree-sitter
/// tags query produces several chunks per file. The `revision` parameter mutates the content
/// (and therefore the file hash) without changing the symbol shape.
fn generate_rust_file(index: usize, revision: usize) -> String {
    format!(
        r#"//! Auto-generated fixture file {index}, revision {revision}.
//! Used by tools-mcp-semantic benches. Do not depend on the exact shape.

use std::collections::HashMap;

/// Computes a total over the input range using a backing hash map.
pub fn compute_total_{index}(input: u32) -> u32 {{
    let mut map: HashMap<u32, u32> = HashMap::new();
    for i in 0..input {{
        map.insert(i, i.saturating_mul(2).wrapping_add({revision} as u32));
    }}
    map.values().sum()
}}

/// Reverses a string by character, allocating a new owned `String`.
pub fn reverse_string_{index}(input: &str) -> String {{
    input.chars().rev().collect()
}}

/// Simple owned container exercised by the bench fixtures.
pub struct Container_{index} {{
    pub items: Vec<u32>,
    pub label: String,
}}

impl Container_{index} {{
    pub fn new(label: impl Into<String>) -> Self {{
        Self {{
            items: Vec::new(),
            label: label.into(),
        }}
    }}

    pub fn add(&mut self, value: u32) {{
        self.items.push(value);
    }}

    pub fn total(&self) -> u32 {{
        self.items.iter().sum::<u32>().wrapping_add({revision} as u32)
    }}
}}
"#
    )
}

// ---------------------------------------------------------------------------
// Tool-call helpers
// ---------------------------------------------------------------------------

fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    tools_mcp_semantic::register_tools(&mut registry);
    registry
}

async fn run_index(registry: &ToolRegistry, path: &str, force: bool) -> Value {
    let args = json!({
        "path": path,
        "force": force,
        "timeout_ms": BENCH_TIMEOUT_MS,
    });
    let value = registry
        .call("SemanticIndex", None, args)
        .await
        .expect("SemanticIndex tool registered")
        .result
        .expect("SemanticIndex returned a result");
    assert_tool_ok(&value, "SemanticIndex", path);
    value
}

async fn run_search(registry: &ToolRegistry, path: &str, query: &str) -> Value {
    let args = json!({
        "query": query,
        "path": path,
        "limit": 10,
        "include_content": true,
        "timeout_ms": BENCH_TIMEOUT_MS,
    });
    let value = registry
        .call("SemanticSearch", None, args)
        .await
        .expect("SemanticSearch tool registered")
        .result
        .expect("SemanticSearch returned a result");
    assert_tool_ok(&value, "SemanticSearch", path);
    value
}

async fn run_search_with_filter(
    registry: &ToolRegistry,
    path: &str,
    query: &str,
    language: &str,
) -> Value {
    let args = json!({
        "query": query,
        "path": path,
        "limit": 10,
        "language": language,
        "include_content": true,
        "timeout_ms": BENCH_TIMEOUT_MS,
    });
    let value = registry
        .call("SemanticSearch", None, args)
        .await
        .expect("SemanticSearch tool registered")
        .result
        .expect("SemanticSearch returned a result");
    assert_tool_ok(&value, "SemanticSearch", path);
    value
}

/// Panics if the tool returned a tool-level error. Without this the bench would silently
/// measure the error-path latency (which is microseconds) and report misleading numbers.
fn assert_tool_ok(value: &Value, tool: &str, path: &str) {
    if value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let text = value
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("<no error text>");
        panic!("{tool} on path '{path}' returned error: {text}");
    }
}

// ---------------------------------------------------------------------------
// Bench scenarios
// ---------------------------------------------------------------------------

fn bench_cold_index(c: &mut Criterion, runtime: &Runtime, registry: &ToolRegistry) {
    let mut group = c.benchmark_group("semantic_cold_index");
    // Cold-index iterations do real embedding work; cap sample size to keep wall-clock honest.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));

    group.bench_function("50_files", |b| {
        b.iter_batched(
            || fresh_cold_fixture(COLD_SMALL_FILES),
            |fixture| {
                let rel = relative_to_workspace(fixture.path());
                runtime.block_on(run_index(registry, &rel, false));
                // Hold `fixture` alive until after the call so files exist for discovery.
                drop(fixture);
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("200_files", |b| {
        b.iter_batched(
            || fresh_cold_fixture(COLD_LARGE_FILES),
            |fixture| {
                let rel = relative_to_workspace(fixture.path());
                runtime.block_on(run_index(registry, &rel, false));
                drop(fixture);
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

fn bench_incremental_index(c: &mut Criterion, runtime: &Runtime, registry: &ToolRegistry) {
    // No-changes path — every file is hash-current; manifest skips embedding entirely. Measures
    // pure overhead of discovery + manifest validation + LanceDB no-op write path.
    {
        let mut group = c.benchmark_group("semantic_incremental_index");
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(10));
        group.bench_function("unchanged_200", |b| {
            b.iter(|| {
                runtime.block_on(run_index(registry, "incremental_baseline", false));
            });
        });
        group.finish();
    }

    // Realistic delta: ~2.5% of files modified between calls. The dominant work here is reading
    // every file (to compute the hash) plus chunking + embedding the small dirty subset.
    let mut delta_group = c.benchmark_group("semantic_incremental_index_delta");
    delta_group.sample_size(10);
    delta_group.measurement_time(Duration::from_secs(30));
    delta_group.bench_function("5_changed_of_200", |b| {
        b.iter_batched(
            mutate_incremental_fixture,
            |_| {
                runtime.block_on(run_index(registry, "incremental_baseline", false));
            },
            BatchSize::PerIteration,
        );
    });
    delta_group.finish();
}

fn bench_warm_query(c: &mut Criterion, runtime: &Runtime, registry: &ToolRegistry) {
    let mut group = c.benchmark_group("semantic_warm_query");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("default", |b| {
        b.iter(|| {
            runtime.block_on(run_search(registry, "warm", "compute total over a range"));
        });
    });

    group.bench_function("with_language_filter", |b| {
        b.iter(|| {
            runtime.block_on(run_search_with_filter(
                registry,
                "warm",
                "compute total over a range",
                "rust",
            ));
        });
    });

    group.finish();
}

fn bench_search_during_background_index(
    c: &mut Criterion,
    runtime: &Runtime,
    registry: &'static ToolRegistry,
) {
    let mut group = c.benchmark_group("semantic_search_under_load");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("search_during_background_index", |b| {
        // Background indexer continuously reindexes `concurrent_indexer_target` with force=true
        // so the worker pool is steadily busy with embedding calls. Stops via a watch channel.
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let bg_handle = runtime.spawn(background_indexer(registry, stop_rx));

        // Brief warmup so the indexer is actually inside an embedding call before we start
        // measuring search latency.
        runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        b.iter(|| {
            runtime.block_on(run_search(registry, "warm", "compute total over a range"));
        });

        let _ = stop_tx.send(true);
        runtime.block_on(async {
            // Bounded wait so a stuck indexer never hangs the bench process.
            let _ = tokio::time::timeout(Duration::from_secs(30), bg_handle).await;
        });
    });

    group.finish();
}

async fn background_indexer(
    registry: &'static ToolRegistry,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            _ = run_index(registry, "concurrent_indexer_target", true) => {}
            _ = stop.changed() => return,
        }
    }
}

fn bench_embed_documents_batch_size(c: &mut Criterion, runtime: &Runtime) {
    let mut group = c.benchmark_group("semantic_embed_documents_batch_size");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    // Construct one provider, share it across batch-size variants. Re-uses the cached model.
    let provider = runtime.block_on(async {
        let index_dir = bench_workspace().join(".tools-mcp").join("semantic-index");
        fs::create_dir_all(&index_dir).expect("create index dir for bench provider");
        FastEmbedProvider::new(&index_dir)
            .await
            .expect("init FastEmbed provider for bench")
    });

    // Document corpus sized to give each internal batch enough work to amortize lock overhead
    // without making any single iteration take minutes. Documents are synthesized to roughly
    // match the chunk shape `embed_index_chunks` produces in production.
    let corpus = synthesize_embedding_corpus(256);

    for &batch_size in &[16usize, 32, 64, 128, 256] {
        group.bench_function(format!("batch_{batch_size}"), |b| {
            b.iter(|| {
                runtime.block_on(async {
                    provider
                        .embed_documents_with_batch_size(corpus.clone(), batch_size)
                        .await
                        .expect("embed corpus");
                });
            });
        });
    }

    group.finish();
}

fn synthesize_embedding_corpus(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            format!(
                "path: src/module_{index}.rs\n\
                 language: rust\n\
                 symbol: handle_request_{index}\n\
                 code:\n\
                 pub fn handle_request_{index}(input: &str) -> Result<String> {{\n\
                     let trimmed = input.trim();\n\
                     if trimmed.is_empty() {{\n\
                         bail!(\"empty input at index {index}\");\n\
                     }}\n\
                     Ok(trimmed.to_owned())\n\
                 }}"
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn relative_to_workspace(path: &Path) -> String {
    path.strip_prefix(bench_workspace())
        .expect("cold fixture must live under bench workspace")
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Criterion entry
// ---------------------------------------------------------------------------

fn run_all(c: &mut Criterion) {
    // Prepend GPU DLL paths to PATH before any fastembed code runs so transitive loads of
    // cuDNN / CUDA-runtime extras resolve correctly. Must happen before the runtime spins up.
    ensure_gpu_dll_paths();

    // Single shared multi-threaded runtime so background tasks (e.g. the concurrent indexer)
    // actually overlap with `block_on` work.
    let runtime = Runtime::new().expect("build multi-threaded tokio runtime for benches");
    let registry = Box::leak(Box::new(build_registry()));

    // Force workspace + fixture setup before the first scenario runs so its measurement is not
    // contaminated by one-time setup cost.
    bench_workspace();
    ensure_fixtures(&runtime, registry);

    bench_cold_index(c, &runtime, registry);
    bench_incremental_index(c, &runtime, registry);
    bench_warm_query(c, &runtime, registry);
    bench_search_during_background_index(c, &runtime, registry);
    bench_embed_documents_batch_size(c, &runtime);
}

criterion_group!(benches, run_all);
criterion_main!(benches);
