use serde_json::{Value, json};
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tools_mcp_core::ToolRegistry;

const DEFAULT_ITERATIONS: usize = 100;

fn main() {
    let iterations = std::env::var("READ_FILE_BENCH_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);

    let corpus = BenchCorpus::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut registry = ToolRegistry::new();
    tools_mcp_local::register_tools(&mut registry);

    let cases = [
        ("full_file", json!({ "path": corpus.full_file })),
        (
            "small_range",
            json!({ "path": corpus.full_file, "start_line": 4000, "end_line": 4010 }),
        ),
        ("crlf_heavy", json!({ "path": corpus.crlf_heavy })),
        (
            "invalid_utf8",
            json!({ "path": corpus.invalid_utf8, "start_line": 100, "end_line": 120 }),
        ),
        (
            "numbered_output",
            json!({
                "path": corpus.full_file,
                "start_line": 1,
                "end_line": 500,
                "show_line_numbers": true
            }),
        ),
    ];

    println!("read_file benchmark: {iterations} iterations per case");
    for (name, args) in cases {
        let elapsed = runtime.block_on(run_case(&registry, iterations, args));
        let per_iter = elapsed.as_secs_f64() / iterations as f64;
        println!(
            "{name:<16} total={elapsed:?} mean={:.3}us",
            per_iter * 1_000_000.0
        );
    }
}

async fn run_case(registry: &ToolRegistry, iterations: usize, args: Value) -> Duration {
    let started = Instant::now();

    for _ in 0..iterations {
        let response = registry
            .call("Read", None, black_box(args.clone()))
            .await
            .expect("Read tool registered");
        black_box(response);
    }

    started.elapsed()
}

struct BenchCorpus {
    _dir: TempDir,
    full_file: PathBuf,
    crlf_heavy: PathBuf,
    invalid_utf8: PathBuf,
}

impl BenchCorpus {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let full_file = dir.path().join("full-file.txt");
        let crlf_heavy = dir.path().join("crlf-heavy.txt");
        let invalid_utf8 = dir.path().join("invalid-utf8.txt");

        std::fs::write(&full_file, numbered_lines(12_000, "\n")).expect("write full file");
        std::fs::write(&crlf_heavy, numbered_lines(12_000, "\r\n")).expect("write crlf file");
        std::fs::write(&invalid_utf8, invalid_utf8_lines(2_000)).expect("write invalid utf8 file");

        Self {
            _dir: dir,
            full_file,
            crlf_heavy,
            invalid_utf8,
        }
    }
}

fn numbered_lines(count: usize, newline: &str) -> Vec<u8> {
    let mut data = Vec::new();

    for line_no in 1..=count {
        data.extend_from_slice(format!("line {line_no:05}: abcdefghijklmnopqrstuvwxyz").as_bytes());
        data.extend_from_slice(newline.as_bytes());
    }

    data
}

fn invalid_utf8_lines(count: usize) -> Vec<u8> {
    let mut data = Vec::new();

    for line_no in 1..=count {
        data.extend_from_slice(format!("line {line_no:05}: ").as_bytes());
        data.push(0xFF);
        data.extend_from_slice(b" invalid\n");
    }

    data
}
