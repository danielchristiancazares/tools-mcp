//! File reading handler implementation.

use crate::tool_outcome::ToolCallOutcome;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::Path;

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

    let req = match ToolCallOutcome::parse_args::<ReadRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
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

    let text = String::from_utf8_lossy(&data);

    if text.is_empty() {
        return ToolCallOutcome::err(format!("file is empty: {}", path.display()));
    }

    let line_count = text.split_inclusive('\n').count();

    let start = req.start_line.unwrap_or(1);
    let end = req.end_line.unwrap_or(line_count);

    if start == 0 {
        return ToolCallOutcome::err("start_line must be >= 1");
    }

    if end == 0 {
        return ToolCallOutcome::err("end_line must be >= 1");
    }

    if start > end {
        return ToolCallOutcome::err("start_line cannot be greater than end_line");
    }

    if start > line_count {
        return ToolCallOutcome::err(format!(
            "start_line {start} exceeds file line count {line_count}"
        ));
    }

    let resolved_end = end.min(line_count);
    let width = resolved_end.max(1).to_string().len();
    let show_line_numbers = req.show_line_numbers.unwrap_or(true);

    let mut body = String::new();
    for (idx, line) in text.split_inclusive('\n').enumerate() {
        let line_no = idx + 1;
        if line_no < start {
            continue;
        }
        if line_no > resolved_end {
            break;
        }

        // Keep the file's original line endings (split_inclusive keeps the trailing '\n').
        if show_line_numbers {
            let _ = write!(body, "{:>width$}\t{}", line_no, line, width = width);
        } else {
            body.push_str(line);
        }
    }

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
