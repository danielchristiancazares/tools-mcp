use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;

const WARM_UP_TIME: Duration = Duration::from_millis(100);
const MEASUREMENT_TIME: Duration = Duration::from_millis(300);
const SAMPLE_SIZE: usize = 10;

fn bench_browser_discovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("webfetch_browser");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("browser_available", |b| {
        b.iter(|| black_box(tools_mcp_webfetch::benchmark_browser_available()));
    });

    group.finish();
}

fn bench_chunking(c: &mut Criterion) {
    let markdown = large_markdown_fixture();
    let mut group = c.benchmark_group("webfetch_chunker");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("large_single_section", |b| {
        b.iter(|| {
            black_box(tools_mcp_webfetch::benchmark_chunk_markdown(
                black_box(&markdown),
                600,
            ));
        });
    });

    group.finish();
}

fn large_markdown_fixture() -> String {
    let mut markdown = String::with_capacity(256 * 1024);
    markdown.push_str("# Large Document\n\n");
    for index in 0..4096 {
        markdown.push_str("This paragraph contains enough prose to exercise tokenization and chunk splitting while preserving stable benchmark input. ");
        markdown.push_str("The quick brown fox jumps over the lazy dog near section ");
        markdown.push_str(&index.to_string());
        markdown.push_str(".\n");
    }
    markdown
}

criterion_group!(benches, bench_browser_discovery, bench_chunking);
criterion_main!(benches);
