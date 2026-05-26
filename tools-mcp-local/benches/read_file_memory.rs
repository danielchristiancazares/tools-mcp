use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;

const WARM_UP_TIME: Duration = Duration::from_millis(100);
const MEASUREMENT_TIME: Duration = Duration::from_millis(300);
const SAMPLE_SIZE: usize = 10;
const LINE_COUNT: usize = 8192;

fn bench_numbered_read_rendering(c: &mut Criterion) {
    let valid_utf8 = valid_utf8_fixture(LINE_COUNT);
    let lossy_utf8 = lossy_utf8_fixture(LINE_COUNT);

    let mut group = c.benchmark_group("read_file_numbered_render");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("valid_utf8_8192_lines", |b| {
        b.iter(|| {
            black_box(tools_mcp_local::benchmark_render_numbered_range(
                black_box(&valid_utf8),
                1,
                LINE_COUNT,
            ));
        });
    });

    group.bench_function("lossy_utf8_8192_lines", |b| {
        b.iter(|| {
            black_box(tools_mcp_local::benchmark_render_numbered_range(
                black_box(&lossy_utf8),
                1,
                LINE_COUNT,
            ));
        });
    });

    group.finish();
}

fn valid_utf8_fixture(lines: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(lines * 64);
    for line_no in 1..=lines {
        bytes.extend_from_slice(
            format!("line-{line_no:05} payload with stable benchmark text\n").as_bytes(),
        );
    }
    bytes
}

fn lossy_utf8_fixture(lines: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(lines * 64);
    for line_no in 1..=lines {
        bytes.extend_from_slice(format!("line-{line_no:05} ").as_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(b" payload with stable benchmark text\n");
    }
    bytes
}

criterion_group!(benches, bench_numbered_read_rendering);
criterion_main!(benches);
