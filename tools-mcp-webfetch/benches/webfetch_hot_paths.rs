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

fn bench_extraction(c: &mut Criterion) {
    let valid_text = large_text_fixture();
    let invalid_text = lossy_text_fixture();
    let cleanup_html = cleanup_html_fixture();
    let mut group = c.benchmark_group("webfetch_extraction");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("extract_text_valid_large", |b| {
        b.iter(|| {
            black_box(tools_mcp_webfetch::benchmark_extract_text_len(black_box(
                valid_text.as_bytes(),
            )));
        });
    });

    group.bench_function("extract_text_lossy_large", |b| {
        b.iter(|| {
            black_box(tools_mcp_webfetch::benchmark_extract_text_len(black_box(
                invalid_text.as_slice(),
            )));
        });
    });

    group.bench_function("clean_markdown_whitespace_large", |b| {
        b.iter(|| {
            black_box(tools_mcp_webfetch::benchmark_clean_markdown_len(black_box(
                cleanup_html.as_str(),
            )));
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

fn large_text_fixture() -> String {
    let mut text = String::with_capacity(256 * 1024);
    for index in 0..4096 {
        text.push_str(
            "Plain text payload with enough content to exercise lossy extraction for line ",
        );
        text.push_str(&index.to_string());
        text.push_str(".\n");
    }
    text
}

fn lossy_text_fixture() -> Vec<u8> {
    let mut text = Vec::with_capacity(256 * 1024);
    for index in 0..4096 {
        text.extend_from_slice(b"Plain text payload with invalid byte ");
        text.push(0xFF);
        text.extend_from_slice(b" on line ");
        text.extend_from_slice(index.to_string().as_bytes());
        text.extend_from_slice(b".\n");
    }
    text
}

fn cleanup_html_fixture() -> String {
    let mut html = String::with_capacity(256 * 1024);
    html.push_str("<!doctype html><html><body>\n\n\n");
    for index in 0..2048 {
        html.push_str("<p>Line with trailing whitespace ");
        html.push_str(&index.to_string());
        html.push_str("     </p>\n\n\n\n");
    }
    html.push_str("</body></html>");
    html
}

criterion_group!(
    benches,
    bench_browser_discovery,
    bench_chunking,
    bench_extraction
);
criterion_main!(benches);
