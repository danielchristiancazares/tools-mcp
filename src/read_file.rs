use crate::{RpcResponse, err_text};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Return file text (optionally a line range) for quick browsing without uploads.
///
/// Output is line-numbered (similar to `nl -ba` / `cat -n`) so callers can easily
/// reference exact lines.
pub async fn handle_read_file(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct ReadRequest {
        path: String,
        #[serde(default)]
        start_line: Option<usize>,
        #[serde(default)]
        end_line: Option<usize>,
    }

    let req = match serde_json::from_value::<ReadRequest>(args) {
        Ok(req) => req,
        Err(err) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("invalid arguments: {err}"))),
                error: None,
            };
        }
    };

    if req.path.trim().is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("path is required")),
            error: None,
        };
    }

    let path = Path::new(&req.path);
    let data = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "failed to read {}: {err}",
                    path.display()
                ))),
                error: None,
            };
        }
    };

    let text = String::from_utf8_lossy(&data);

    // Handle empty files explicitly to avoid range confusion.
    if text.is_empty() {
        let payload = json!({
            "content": [{"type": "text", "text": ""}],
            "isError": false,
            "path": path.display().to_string(),
            "start_line": 0,
            "end_line": 0,
            "total_lines": 0
        });
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(payload),
            error: None,
        };
    }

    let line_count = text.split_inclusive('\n').count();

    let start = req.start_line.unwrap_or(1);
    let end = req.end_line.unwrap_or(line_count);

    if start == 0 {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("start_line must be >= 1")),
            error: None,
        };
    }

    if end == 0 {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("end_line must be >= 1")),
            error: None,
        };
    }

    if start > end {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("start_line cannot be greater than end_line")),
            error: None,
        };
    }

    if start > line_count {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text(&format!(
                "start_line {start} exceeds file line count {line_count}"
            ))),
            error: None,
        };
    }

    let resolved_end = end.min(line_count);
    let width = resolved_end.max(1).to_string().len();

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
        let _ = write!(body, "{:>width$}\t{}", line_no, line, width = width);
    }

    let payload = json!({
        "content": [{"type": "text", "text": body}],
        "isError": false,
        "path": path.display().to_string(),
        "start_line": start,
        "end_line": resolved_end,
        "total_lines": line_count
    });

    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(payload),
        error: None,
    }
}
