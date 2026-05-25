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

criterion_group!(benches, bench_browser_discovery);
criterion_main!(benches);
