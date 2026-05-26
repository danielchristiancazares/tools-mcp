use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::json;
use std::hint::black_box;
use std::time::Duration;
use tokio::io::BufReader;
use tools_mcp_core::process::read_to_end_limited;
use tools_mcp_core::text::{strip_ansi_codes, truncate_at_char_boundary};
use tools_mcp_core::{
    RpcResponse, ToolCallOutcome, read_mcp_message, write_mcp_response_with_mode,
};

const RAW_PING: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}
"#;
const FRAMED_PING: &[u8] =
    b"Content-Length: 52\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{}}\n";
const WARM_UP_TIME: Duration = Duration::from_millis(100);
const MEASUREMENT_TIME: Duration = Duration::from_millis(300);
const SAMPLE_SIZE: usize = 10;

fn bench_protocol_read(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("mcp");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("read_raw_json_ping", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let mut reader = BufReader::new(black_box(RAW_PING));
                let message = read_mcp_message(&mut reader)
                    .await
                    .expect("read should succeed")
                    .expect("message should be present");
                black_box(message);
            });
        });
    });

    group.bench_function("read_framed_json_ping", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let mut reader = BufReader::new(black_box(FRAMED_PING));
                let message = read_mcp_message(&mut reader)
                    .await
                    .expect("read should succeed")
                    .expect("message should be present");
                black_box(message);
            });
        });
    });
    group.finish();
}

fn bench_protocol_write(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let response = RpcResponse::ok(
        Some(json!(1)),
        json!({
            "content": [{"type": "text", "text": "pong"}],
            "isError": false
        }),
    );
    let mut group = c.benchmark_group("mcp");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("write_response_with_headers", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let mut sink = tokio::io::sink();
                write_mcp_response_with_mode(&mut sink, black_box(&response), false)
                    .await
                    .expect("write should succeed");
            });
        });
    });

    group.bench_function("write_response_without_headers", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let mut sink = tokio::io::sink();
                write_mcp_response_with_mode(&mut sink, black_box(&response), true)
                    .await
                    .expect("write should succeed");
            });
        });
    });
    group.finish();
}

fn bench_text(c: &mut Criterion) {
    let clean = "plain output without escapes ".repeat(128);
    let colored = "\x1b[1;31merror\x1b[0m: file not found\n".repeat(128);
    let long_unicode = "a".repeat(1200) + "€tail";
    let mut group = c.benchmark_group("text");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("strip_ansi_clean", |b| {
        b.iter(|| black_box(strip_ansi_codes(black_box(&clean))));
    });

    group.bench_function("strip_ansi_colored", |b| {
        b.iter(|| black_box(strip_ansi_codes(black_box(&colored))));
    });

    group.bench_function("truncate_unicode_boundary", |b| {
        b.iter(|| black_box(truncate_at_char_boundary(black_box(&long_unicode), 1200)));
    });
    group.finish();
}

fn bench_process_capture(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let under_limit = vec![b'a'; 8 * 1024];
    let over_limit = vec![b'b'; 64 * 1024];
    let mut group = c.benchmark_group("process");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("read_to_end_limited_under_limit", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let (captured, truncated) =
                    read_to_end_limited(black_box(under_limit.as_slice()), 16 * 1024)
                        .await
                        .expect("capture should succeed");
                black_box((captured, truncated));
            });
        });
    });

    group.bench_function("read_to_end_limited_truncated", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let (captured, truncated) =
                    read_to_end_limited(black_box(over_limit.as_slice()), 8 * 1024)
                        .await
                        .expect("capture should succeed");
                black_box((captured, truncated));
            });
        });
    });

    group.finish();
}

fn bench_tool_outcome(c: &mut Criterion) {
    let large_text = "large tool payload line\n".repeat(1024);
    let mut group = c.benchmark_group("tool_outcome");
    group.warm_up_time(WARM_UP_TIME);
    group.measurement_time(MEASUREMENT_TIME);
    group.sample_size(SAMPLE_SIZE);

    group.bench_function("tool_call_ok_text_with_large", |b| {
        b.iter(|| {
            black_box(ToolCallOutcome::ok_text_with(
                black_box(large_text.as_str()),
                std::iter::empty::<(&'static str, serde_json::Value)>(),
            ));
        });
    });

    group.bench_function("tool_call_err_large", |b| {
        b.iter(|| {
            black_box(ToolCallOutcome::err(black_box(large_text.as_str())));
        });
    });

    group.bench_function("rpc_ok_text_with_large", |b| {
        b.iter(|| {
            black_box(RpcResponse::ok_text_with(
                Some(json!(1)),
                black_box(large_text.as_str()),
                std::iter::empty::<(&'static str, serde_json::Value)>(),
            ));
        });
    });

    group.bench_function("rpc_err_large", |b| {
        b.iter(|| {
            black_box(RpcResponse::err(
                Some(json!(1)),
                black_box(large_text.as_str()),
            ));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_protocol_read,
    bench_protocol_write,
    bench_text,
    bench_process_capture,
    bench_tool_outcome
);
criterion_main!(benches);
