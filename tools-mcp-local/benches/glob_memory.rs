use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::fs;
use std::time::Duration;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tools_mcp_core::ToolRegistry;

fn bench_glob_memory(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("glob_memory");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    // Cold: every iteration uses a fresh root, so the scope cache misses and
    // the full walk plus matching runs.
    group.bench_function("cold_scope_walk_and_match", |b| {
        b.iter_batched(
            || fixture_dir("glob-cold", 16, 32),
            |fixture| {
                runtime.block_on(glob_once(json!({
                    "pattern": "**/*.rs",
                    "path": fixture.path().display().to_string(),
                })))
            },
            BatchSize::SmallInput,
        )
    });

    // Warm: repeated queries against one root reuse the cached scope
    // snapshot, isolating pattern matching and payload rendering.
    let warm_fixture = fixture_dir("glob-warm", 16, 64);
    runtime.block_on(glob_once(json!({
        "pattern": "**/*.rs",
        "path": warm_fixture.path().display().to_string(),
    })));
    group.bench_function("warm_recursive_extension_match", |b| {
        b.iter(|| {
            runtime.block_on(glob_once(json!({
                "pattern": "**/*.rs",
                "path": warm_fixture.path().display().to_string(),
            })))
        })
    });

    group.bench_function("warm_brace_multi_pattern_match", |b| {
        b.iter(|| {
            runtime.block_on(glob_once(json!({
                "pattern": "**/*.{rs,txt,toml}",
                "path": warm_fixture.path().display().to_string(),
            })))
        })
    });

    group.finish();
}

async fn glob_once(args: Value) -> Value {
    let mut registry = ToolRegistry::new();
    tools_mcp_local::register_tools(&mut registry);
    registry
        .call("Glob", None, args)
        .await
        .expect("Glob tool registered")
        .result
        .expect("Glob response result")
}

fn fixture_dir(prefix: &str, subdirs: usize, files_per_subdir: usize) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    for subdir_index in 0..subdirs {
        let subdir = dir.path().join(format!("module_{subdir_index:03}"));
        fs::create_dir_all(&subdir).expect("subdir");
        for file_index in 0..files_per_subdir {
            let extension = match file_index % 3 {
                0 => "rs",
                1 => "txt",
                _ => "json",
            };
            fs::write(
                subdir.join(format!("file_{file_index:05}.{extension}")),
                "content\n",
            )
            .expect("fixture file");
        }
    }
    dir
}

criterion_group!(benches, bench_glob_memory);
criterion_main!(benches);
