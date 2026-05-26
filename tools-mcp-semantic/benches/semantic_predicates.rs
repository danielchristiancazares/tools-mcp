use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::OnceLock;
use std::time::Duration;

const WARM_UP_TIME: Duration = Duration::from_millis(100);
const MEASUREMENT_TIME: Duration = Duration::from_millis(300);
const SAMPLE_SIZE: usize = 10;
const PATH_COUNT: usize = 256;

static PATHS: OnceLock<Vec<String>> = OnceLock::new();

fn paths() -> &'static [String] {
    PATHS.get_or_init(|| {
        (0..PATH_COUNT)
            .map(|index| {
                if index % 16 == 0 {
                    format!("src/feature_{index:05}/owner's_file.rs")
                } else {
                    format!("src/feature_{index:05}/module.rs")
                }
            })
            .collect()
    })
}

fn bench_semantic_predicates(c: &mut Criterion) {
    let paths = paths();
    let root = "C:/work/repo's/main";
    let directory = "src/feature_00042";
    let language = Some("rust's");

    let mut group = c.benchmark_group("semantic_predicates");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("delete_paths_batch", |b| {
        b.iter(|| {
            let len = tools_mcp_semantic::bench::delete_paths_predicate_len(
                black_box(root),
                black_box(paths),
            );
            black_box(len);
        });
    });

    group.bench_function("directory_filter", |b| {
        b.iter(|| {
            let len = tools_mcp_semantic::bench::directory_filter_predicate_len(
                black_box(root),
                black_box(directory),
                black_box(language),
            );
            black_box(len);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_semantic_predicates);
criterion_main!(benches);
