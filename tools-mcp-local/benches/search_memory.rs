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
                let result = runtime.block_on(search_once(
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
                ));
                // Return the fixture so the TempDir's recursive delete happens
                // outside the timed region; dropping it here would charge ~256
                // file deletions to the "cold index" measurement.
                (result, fixture)
            },
            BatchSize::SmallInput,
        )
    });

    // Create every warm fixture up front, then age them together past the
    // git-style racy window (`SCOPE_STAMP_RACY_SLACK`, 2 s): file-set
    // certification only engages once directory stamps are provably non-racy.
    // Without the aging sleep, early iterations pay a full rediscovery walk
    // (uncertified) while later ones skip it (certified), and the measured
    // distribution becomes a timing-dependent mixture of the two regimes.
    // Two priming queries per fixture: the first builds the index and grants
    // certification, the second confirms the steady certified regime.
    let warm_fixture = fixture_dir("warm", 512, 4);
    let postings_fixture = fixture_dir("postings", 1024, 2);
    let large_ignored_fixture = large_ignored_fixture_dir("large_ignored", 16, 256);
    let many_match_fixture = many_match_fixture_dir("many_match", 24_576);
    std::thread::sleep(Duration::from_millis(2200));
    for _ in 0..2 {
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
    }
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

    for _ in 0..2 {
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
    }
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
    // (Fixture created above so all warm fixtures age past the racy window together.)
    for _ in 0..2 {
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
    }
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

    // Dense-match render scenarios: one 32k-line document where every second
    // line matches. The default-budget variant guards the common path; the
    // 10k-budget variant exercises the per-match render-budget bookkeeping in
    // `matching_line_indexes_with_budget` at the `max_results` clamp ceiling,
    // where re-deriving the render expansion per match is quadratic in the
    // number of matches needed to fill the budget.
    for _ in 0..2 {
        runtime.block_on(search_once(
            many_match_fixture.path(),
            json!({
                "pattern": "qmatch",
                "path": many_match_fixture.path().display().to_string(),
                "fixed_strings": true,
                "case": "sensitive",
                "hidden": false,
                "no_ignore": false,
                "max_results": 100,
                "timeout_ms": 300000
            }),
        ));
    }
    group.bench_function("budget_render_dense_default_100", |b| {
        b.iter(|| {
            runtime.block_on(search_once(
                many_match_fixture.path(),
                json!({
                    "pattern": "qmatch",
                    "path": many_match_fixture.path().display().to_string(),
                    "fixed_strings": true,
                    "case": "sensitive",
                    "hidden": false,
                    "no_ignore": false,
                    "max_results": 100,
                    "context": 1,
                    "timeout_ms": 300000
                }),
            ))
        })
    });

    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("budget_render_many_match_10k", |b| {
        b.iter(|| {
            runtime.block_on(search_once(
                many_match_fixture.path(),
                json!({
                    "pattern": "qmatch",
                    "path": many_match_fixture.path().display().to_string(),
                    "fixed_strings": true,
                    "case": "sensitive",
                    "hidden": false,
                    "no_ignore": false,
                    "max_results": 10000,
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

/// Builds a fixture with one `lines`-line document where every second line
/// contains the `qmatch` needle (12k matching lines at the default 24k), so
/// verification visits every line and a 10k-event budget fills from a single
/// document's matches. Lines are kept short so the document stays under the
/// memory backend's 1 MiB `max_file_bytes` eligibility cap — a larger file
/// silently delegates the whole scenario to the ugrep fallback.
fn many_match_fixture_dir(prefix: &str, lines: usize) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    fs::write(dir.path().join(".gitignore"), "# benchmark fixture\n").expect("gitignore");
    let mut content = String::with_capacity(lines * 20);
    for line in 0..lines {
        if line % 2 == 0 {
            content.push_str(&format!("qmatch line_{line}\n"));
        } else {
            content.push_str(&format!("filler line_{line}\n"));
        }
    }
    fs::write(dir.path().join("dense.txt"), content).expect("fixture file");
    dir
}

criterion_group!(benches, bench_search_memory);
criterion_main!(benches);
