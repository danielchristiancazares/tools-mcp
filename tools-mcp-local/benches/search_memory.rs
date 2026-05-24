use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tools_mcp_core::ToolRegistry;

fn bench_search_memory(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("search_memory");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("cold_index_build_public_tool", |b| {
        b.iter_batched(
            || fixture_dir("cold", 256, 8),
            |fixture| {
                runtime.block_on(search_once(
                    fixture.path(),
                    json!({
                        "pattern": "needle",
                        "path": fixture.path().display().to_string(),
                        "fixed_strings": true,
                        "case": "sensitive",
                        "hidden": false,
                        "no_ignore": false,
                        "max_results": 10,
                        "timeout_ms": 300000
                    }),
                ))
            },
            BatchSize::SmallInput,
        )
    });

    let warm_fixture = fixture_dir("warm", 512, 4);
    runtime.block_on(search_once(
        warm_fixture.path(),
        json!({
            "pattern": "needle",
            "path": warm_fixture.path().display().to_string(),
            "fixed_strings": true,
            "case": "sensitive",
            "hidden": false,
            "no_ignore": false,
            "max_results": 10,
            "timeout_ms": 300000
        }),
    ));
    group.bench_function("warm_query_default_ignore_validation", |b| {
        b.iter(|| {
            runtime.block_on(search_once(
                warm_fixture.path(),
                json!({
                    "pattern": "needle",
                    "path": warm_fixture.path().display().to_string(),
                    "fixed_strings": true,
                    "case": "sensitive",
                    "hidden": false,
                    "no_ignore": false,
                    "max_results": 10,
                    "timeout_ms": 300000
                }),
            ))
        })
    });

    let postings_fixture = fixture_dir("postings", 1024, 2);
    runtime.block_on(search_once(
        postings_fixture.path(),
        json!({
            "pattern": "needle common token",
            "path": postings_fixture.path().display().to_string(),
            "fixed_strings": true,
            "case": "sensitive",
            "hidden": false,
            "no_ignore": true,
            "max_results": 20,
            "timeout_ms": 300000
        }),
    ));
    group.bench_function("large_postings_intersection", |b| {
        b.iter(|| {
            runtime.block_on(search_once(
                postings_fixture.path(),
                json!({
                    "pattern": "needle common token",
                    "path": postings_fixture.path().display().to_string(),
                    "fixed_strings": true,
                    "case": "sensitive",
                    "hidden": false,
                    "no_ignore": true,
                    "max_results": 20,
                    "timeout_ms": 300000
                }),
            ))
        })
    });

    // Larger fixture with per-subdirectory ignore rules. Exercises the warm-query freshness
    // validation hot path flagged in the optimization triage — directory fingerprint checks,
    // ignore-fingerprint rebuilds, and per-indexed-file metadata sweeps all scale with the
    // indexed corpus, so a bigger corpus gives a clearer signal than the 512-file warm fixture.
    let large_ignored_fixture = large_ignored_fixture_dir("large_ignored", 16, 256);
    runtime.block_on(search_once(
        large_ignored_fixture.path(),
        json!({
            "pattern": "needle",
            "path": large_ignored_fixture.path().display().to_string(),
            "fixed_strings": true,
            "case": "sensitive",
            "hidden": false,
            "no_ignore": false,
            "max_results": 20,
            "timeout_ms": 300000
        }),
    ));
    group.bench_function("warm_query_large_workspace_with_ignore", |b| {
        b.iter(|| {
            runtime.block_on(search_once(
                large_ignored_fixture.path(),
                json!({
                    "pattern": "needle",
                    "path": large_ignored_fixture.path().display().to_string(),
                    "fixed_strings": true,
                    "case": "sensitive",
                    "hidden": false,
                    "no_ignore": false,
                    "max_results": 20,
                    "timeout_ms": 300000
                }),
            ))
        })
    });

    group.finish();
}

async fn search_once(_root: &Path, args: Value) -> Value {
    let mut registry = ToolRegistry::new();
    tools_mcp_local::register_tools(&mut registry);
    registry
        .call("Search", None, args)
        .await
        .expect("Search tool registered")
        .result
        .expect("Search response result")
}

fn fixture_dir(prefix: &str, files: usize, lines_per_file: usize) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    fs::write(dir.path().join(".gitignore"), "# benchmark fixture\n").expect("gitignore");
    for index in 0..files {
        let mut content = String::new();
        for line in 0..lines_per_file {
            content.push_str(&format!(
                "needle common token file_{index} line_{line} trailing text\n"
            ));
        }
        fs::write(dir.path().join(format!("file_{index:05}.txt")), content).expect("fixture file");
    }
    dir
}

/// Builds a fixture with `subdirs` subdirectories of `files_per_subdir` indexable files each.
/// Each subdirectory carries its own `.gitignore` excluding `*.tmp`, plus a few `*.tmp` decoy
/// files. This produces multiple ignore-fingerprint inputs and a non-trivial directory tree so
/// the freshness-validation hot path has realistic work per query.
fn large_ignored_fixture_dir(prefix: &str, subdirs: usize, files_per_subdir: usize) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    fs::write(
        dir.path().join(".gitignore"),
        "# root ignore rules\n*.bak\n",
    )
    .expect("root gitignore");

    for subdir_index in 0..subdirs {
        let subdir = dir.path().join(format!("module_{subdir_index:03}"));
        fs::create_dir_all(&subdir).expect("subdir");
        fs::write(
            subdir.join(".gitignore"),
            "# per-module ignore rules\n*.tmp\nscratch/\n",
        )
        .expect("subdir gitignore");

        for file_index in 0..files_per_subdir {
            let content = format!(
                "needle common token module_{subdir_index} file_{file_index} payload\n\
                 needle common token module_{subdir_index} file_{file_index} trailing text\n",
            );
            fs::write(subdir.join(format!("file_{file_index:05}.txt")), content)
                .expect("fixture file");
        }
        // Decoy files that must be excluded by the per-module ignore rules.
        for tmp_index in 0..2 {
            fs::write(
                subdir.join(format!("scratch_{tmp_index}.tmp")),
                "needle SHOULD NOT MATCH\n",
            )
            .expect("decoy tmp");
        }
    }
    dir
}

criterion_group!(benches, bench_search_memory);
criterion_main!(benches);
