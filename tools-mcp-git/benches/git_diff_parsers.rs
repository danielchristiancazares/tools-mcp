use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::OnceLock;
use std::time::Duration;

const WARM_UP_TIME: Duration = Duration::from_millis(100);
const MEASUREMENT_TIME: Duration = Duration::from_millis(300);
const SAMPLE_SIZE: usize = 10;
const FIXTURE_ENTRIES: usize = 2_000;

struct DiffFixture {
    name_status: String,
    numstat: String,
}

static FIXTURE: OnceLock<DiffFixture> = OnceLock::new();

fn fixture() -> &'static DiffFixture {
    FIXTURE.get_or_init(|| {
        let mut name_status = String::with_capacity(FIXTURE_ENTRIES * 48);
        let mut numstat = String::with_capacity(FIXTURE_ENTRIES * 48);
        for index in 0..FIXTURE_ENTRIES {
            match index % 10 {
                0 => {
                    let old_path = format!("src/old_{index:05}.rs");
                    let new_path = format!("src/new_{index:05}.rs");
                    name_status.push_str("R100\0");
                    name_status.push_str(&old_path);
                    name_status.push('\0');
                    name_status.push_str(&new_path);
                    name_status.push('\0');
                    numstat.push_str("12\t4\t\0");
                    numstat.push_str(&old_path);
                    numstat.push('\0');
                    numstat.push_str(&new_path);
                    numstat.push('\0');
                }
                1 => {
                    let old_path = format!("src/source_{index:05}.rs");
                    let new_path = format!("src/copy_{index:05}.rs");
                    name_status.push_str("C100\0");
                    name_status.push_str(&old_path);
                    name_status.push('\0');
                    name_status.push_str(&new_path);
                    name_status.push('\0');
                    numstat.push_str("6\t2\t\0");
                    numstat.push_str(&old_path);
                    numstat.push('\0');
                    numstat.push_str(&new_path);
                    numstat.push('\0');
                }
                2 => {
                    let path = format!("assets/blob_{index:05}.bin");
                    name_status.push_str("M\0");
                    name_status.push_str(&path);
                    name_status.push('\0');
                    numstat.push_str("-\t-\t");
                    numstat.push_str(&path);
                    numstat.push('\0');
                }
                3 => {
                    let path = format!("src/added_{index:05}.rs");
                    name_status.push_str("A\0");
                    name_status.push_str(&path);
                    name_status.push('\0');
                    numstat.push_str("40\t0\t");
                    numstat.push_str(&path);
                    numstat.push('\0');
                }
                4 => {
                    let path = format!("src/deleted_{index:05}.rs");
                    name_status.push_str("D\0");
                    name_status.push_str(&path);
                    name_status.push('\0');
                    numstat.push_str("0\t18\t");
                    numstat.push_str(&path);
                    numstat.push('\0');
                }
                _ => {
                    let path = format!("src/module_{index:05}.rs");
                    name_status.push_str("M\0");
                    name_status.push_str(&path);
                    name_status.push('\0');
                    numstat.push_str("8\t3\t");
                    numstat.push_str(&path);
                    numstat.push('\0');
                }
            }
        }
        DiffFixture {
            name_status,
            numstat,
        }
    })
}

fn bench_diff_parsers(c: &mut Criterion) {
    let fixture = fixture();
    let mut group = c.benchmark_group("git_diff_parsers");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("parse_manifest_large", |b| {
        b.iter(|| {
            let weight = tools_mcp_git::bench::parse_diff_manifest_weight(
                black_box(fixture.name_status.as_str()),
                black_box(fixture.numstat.as_str()),
            );
            black_box(weight);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_diff_parsers);
criterion_main!(benches);
