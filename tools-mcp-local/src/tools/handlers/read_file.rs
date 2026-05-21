//! File reading handler implementation.

use memchr::memchr2;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::ops::Range;
use std::path::Path;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::validation;

/// Return file text (optionally a line range) for quick browsing without uploads.
///
/// When `show_line_numbers` is true, output is line-numbered (similar to `nl -ba` / `cat -n`)
/// so callers can easily reference exact lines. By default, raw content is returned.
pub async fn handle_read_file(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReadRequest {
        path: String,
        #[serde(default)]
        start_line: Option<usize>,
        #[serde(default)]
        end_line: Option<usize>,
        #[serde(default)]
        show_line_numbers: Option<bool>,
    }

    let req = match ToolCallOutcome::parse_args::<ReadRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let start = req.start_line.unwrap_or(1);
    if start == 0 {
        return ToolCallOutcome::err("start_line must be >= 1");
    }

    if let Some(end) = req.end_line {
        if end == 0 {
            return ToolCallOutcome::err("end_line must be >= 1");
        }

        if start > end {
            return ToolCallOutcome::err("start_line cannot be greater than end_line");
        }
    }

    let path = Path::new(&req.path);
    let data = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            let msg = match err.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "file not found: {}. Remediation: check the path (paths are resolved relative to the MCP server's working directory) or use Glob/ListDir to locate it.",
                    path.display()
                ),
                std::io::ErrorKind::PermissionDenied => format!(
                    "permission denied reading {}. Remediation: check file permissions and whether another process is locking the file.",
                    path.display()
                ),
                std::io::ErrorKind::IsADirectory => format!(
                    "{} is a directory. Remediation: use ListDir to inspect it, or pass a file path.",
                    path.display()
                ),
                _ => format!("failed to read {}: {err}", path.display()),
            };
            return ToolCallOutcome::err(msg);
        }
    };

    if data.is_empty() {
        let payload = json!({
            "content": [{"type": "text", "text": ""}],
            "isError": false,
            "path": path.display().to_string(),
            "start_line": 0,
            "end_line": 0,
            "total_lines": 0
        });
        return ToolCallOutcome::ok(payload);
    }

    let scan = scan_line_range(&data, start, req.end_line);
    let line_count = scan.total_lines;
    let end = req.end_line.unwrap_or(line_count);

    if start > line_count {
        return ToolCallOutcome::err(format!(
            "start_line {start} exceeds file line count {line_count}"
        ));
    }

    let resolved_end = end.min(line_count);
    let show_line_numbers = req.show_line_numbers.unwrap_or(false);
    let selected_range = scan
        .selected_range()
        .expect("valid line range should have selected bytes");

    let body = if show_line_numbers {
        render_numbered_range(&data, selected_range, start, resolved_end)
    } else if selected_range.start == 0 && selected_range.end == data.len() {
        bytes_to_string_lossy(data)
    } else {
        bytes_slice_to_string_lossy(&data[selected_range])
    };

    let payload = json!({
        "content": [{"type": "text", "text": body}],
        "isError": false,
        "path": path.display().to_string(),
        "start_line": start,
        "end_line": resolved_end,
        "total_lines": line_count
    });

    ToolCallOutcome::ok(payload)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineRangeScan {
    total_lines: usize,
    selected_start: Option<usize>,
    selected_end: usize,
}

impl LineRangeScan {
    fn selected_range(&self) -> Option<Range<usize>> {
        self.selected_start.map(|start| start..self.selected_end)
    }
}

fn scan_line_range(bytes: &[u8], start_line: usize, end_line: Option<usize>) -> LineRangeScan {
    let mut selected_start = None;
    let mut selected_end = 0;

    let total_lines = for_each_line_with_endings(bytes, |line_no, line_start, line_end| {
        if line_no >= start_line && end_line.is_none_or(|end_line| line_no <= end_line) {
            selected_start.get_or_insert(line_start);
            selected_end = line_end;
        }
    });

    LineRangeScan {
        total_lines,
        selected_start,
        selected_end,
    }
}

fn bytes_to_string_lossy(data: Vec<u8>) -> String {
    match String::from_utf8(data) {
        Ok(text) => text,
        Err(err) => bytes_slice_to_string_lossy(&err.into_bytes()),
    }
}

fn bytes_slice_to_string_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn render_numbered_range(
    bytes: &[u8],
    selected_range: Range<usize>,
    start_line: usize,
    resolved_end: usize,
) -> String {
    let selected_bytes = &bytes[selected_range];
    let width = resolved_end.max(1).to_string().len();
    let line_count = resolved_end - start_line + 1;
    let prefix_capacity = line_count.saturating_mul(width + 1);
    let mut body = String::with_capacity(selected_bytes.len().saturating_add(prefix_capacity));
    let mut line_no = start_line;

    for_each_line_with_endings(selected_bytes, |_, line_start, line_end| {
        let line = String::from_utf8_lossy(&selected_bytes[line_start..line_end]);
        let _ = write!(body, "{line_no:>width$}\t{line}");
        line_no += 1;
    });

    body
}

fn for_each_line_with_endings(bytes: &[u8], mut visit: impl FnMut(usize, usize, usize)) -> usize {
    let mut line_count = 0;
    let mut line_start = 0;

    while let Some(relative_break) = memchr2(b'\n', b'\r', &bytes[line_start..]) {
        let line_break = line_start + relative_break;
        let line_end = if bytes[line_break] == b'\r'
            && bytes.get(line_break + 1).is_some_and(|byte| *byte == b'\n')
        {
            line_break + 2
        } else {
            line_break + 1
        };

        line_count += 1;
        visit(line_count, line_start, line_end);
        line_start = line_end;
    }

    if line_start < bytes.len() {
        line_count += 1;
        visit(line_count, line_start, bytes.len());
    }

    line_count
}

#[cfg(test)]
mod tests {
    use super::{LineRangeScan, for_each_line_with_endings, scan_line_range};

    fn lines_with_endings(text: &str) -> Vec<&str> {
        let mut lines = Vec::new();
        for_each_line_with_endings(text.as_bytes(), |_, start, end| {
            lines.push(&text[start..end]);
        });
        lines
    }

    fn content_text(outcome: &tools_mcp_core::ToolCallOutcome) -> &str {
        outcome.0["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn line_scanner_handles_cr_only_files() {
        let lines = lines_with_endings("line1\rline2\rline3");
        assert_eq!(lines, vec!["line1\r", "line2\r", "line3"]);
    }

    #[test]
    fn line_scanner_handles_mixed_newlines() {
        let lines = lines_with_endings("a\r\nb\nc\rd");
        assert_eq!(lines, vec!["a\r\n", "b\n", "c\r", "d"]);
    }

    #[test]
    fn line_scanner_handles_crlf_without_counting_lf_twice() {
        let lines = lines_with_endings("a\r\nb\r\nc\r\n");
        assert_eq!(lines, vec!["a\r\n", "b\r\n", "c\r\n"]);
    }

    #[test]
    fn line_scanner_returns_empty_count_for_empty_input() {
        assert_eq!(for_each_line_with_endings(b"", |_, _, _| {}), 0);
    }

    #[test]
    fn scan_line_range_counts_all_lines_without_collecting_them() {
        let scan = scan_line_range(b"a\nb\nc\nd\n", 2, Some(3));
        assert_eq!(
            scan,
            LineRangeScan {
                total_lines: 4,
                selected_start: Some(2),
                selected_end: 6
            }
        );
        assert_eq!(scan.selected_range().unwrap(), 2..6);
    }

    // REGRESSION: show_line_numbers should default to false (raw content).
    #[tokio::test]
    async fn read_file_show_line_numbers_defaults_to_false() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello\nworld\n").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
        });

        let outcome = super::handle_read_file(None, args).await;
        let text = outcome.0["content"][0]["text"].as_str().unwrap();

        // Default should be raw content (no line numbers).
        assert!(
            !text.contains("\t"),
            "show_line_numbers should default to false, but output contains tab-separated line numbers"
        );
    }

    // REGRESSION: Empty files should return empty content, not an error.
    #[tokio::test]
    async fn read_file_empty_file_returns_empty_content() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, b"").expect("write empty file");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
        });

        let outcome = super::handle_read_file(None, args).await;
        let is_error = outcome.0["isError"].as_bool().unwrap();
        let text = outcome.0["content"][0]["text"].as_str().unwrap();

        assert!(!is_error, "empty file should not be an error");
        assert_eq!(text, "", "empty file should return empty content");
    }

    #[tokio::test]
    async fn read_file_end_line_zero_returns_error() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello\nworld\n").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "end_line": 0,
        });

        let outcome = super::handle_read_file(None, args).await;
        let is_error = outcome.0["isError"].as_bool().unwrap();

        assert!(is_error, "end_line=0 should be an error");
    }

    #[tokio::test]
    async fn read_file_start_line_zero_is_validated_before_file_read() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing.txt");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 0,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(outcome.0["isError"].as_bool().unwrap());
        assert_eq!(content_text(&outcome), "start_line must be >= 1");
    }

    #[tokio::test]
    async fn read_file_end_line_zero_is_validated_before_file_read() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing.txt");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "end_line": 0,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(outcome.0["isError"].as_bool().unwrap());
        assert_eq!(content_text(&outcome), "end_line must be >= 1");
    }

    #[tokio::test]
    async fn read_file_start_greater_than_end_is_validated_before_file_read() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing.txt");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 3,
            "end_line": 2,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(outcome.0["isError"].as_bool().unwrap());
        assert_eq!(
            content_text(&outcome),
            "start_line cannot be greater than end_line"
        );
    }

    #[tokio::test]
    async fn read_file_range_preserves_mixed_newline_endings() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("mixed.txt");
        std::fs::write(&path, "a\r\nb\nc\rd").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 2,
            "end_line": 3,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(!outcome.0["isError"].as_bool().unwrap());
        assert_eq!(outcome.0["content"][0]["text"].as_str().unwrap(), "b\nc\r");
        assert_eq!(outcome.0["start_line"], 2);
        assert_eq!(outcome.0["end_line"], 3);
        assert_eq!(outcome.0["total_lines"], 4);
    }

    #[tokio::test]
    async fn read_file_range_preserves_invalid_utf8_lossy_replacement() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("invalid-utf8.txt");
        std::fs::write(&path, b"valid\n\xFFbad\nlast\n").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 2,
            "end_line": 2,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(!outcome.0["isError"].as_bool().unwrap());
        assert_eq!(content_text(&outcome), "\u{FFFD}bad\n");
        assert_eq!(outcome.0["start_line"], 2);
        assert_eq!(outcome.0["end_line"], 2);
        assert_eq!(outcome.0["total_lines"], 3);
    }

    #[tokio::test]
    async fn read_file_full_file_preserves_invalid_utf8_lossy_replacement() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("invalid-full-utf8.txt");
        std::fs::write(&path, b"valid\n\xFFbad\nlast\n").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(!outcome.0["isError"].as_bool().unwrap());
        assert_eq!(content_text(&outcome), "valid\n\u{FFFD}bad\nlast\n");
        assert_eq!(outcome.0["start_line"], 1);
        assert_eq!(outcome.0["end_line"], 3);
        assert_eq!(outcome.0["total_lines"], 3);
    }

    #[tokio::test]
    async fn read_file_numbered_range_uses_resolved_end_width() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("numbered.txt");
        std::fs::write(&path, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").expect("write");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 9,
            "end_line": 10,
            "show_line_numbers": true,
        });

        let outcome = super::handle_read_file(None, args).await;

        assert!(!outcome.0["isError"].as_bool().unwrap());
        assert_eq!(
            outcome.0["content"][0]["text"].as_str().unwrap(),
            " 9\t9\n10\t10\n"
        );
        assert_eq!(outcome.0["start_line"], 9);
        assert_eq!(outcome.0["end_line"], 10);
        assert_eq!(outcome.0["total_lines"], 10);
    }
}
