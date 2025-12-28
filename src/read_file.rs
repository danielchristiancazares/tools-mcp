use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::Path;

/// Return file text (optionally a line range) for quick browsing without uploads.
///
/// When `show_line_numbers` is true, output is line-numbered (similar to `nl -ba` / `cat -n`)
/// so callers can easily reference exact lines. By default, raw content is returned.
pub async fn handle_read_file(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct ReadRequest {
        path: String,
        #[serde(default)]
        start_line: Option<usize>,
        #[serde(default)]
        end_line: Option<usize>,
        #[serde(default)]
        show_line_numbers: Option<bool>,
    }

    let req = match RpcResponse::parse::<ReadRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if req.path.trim().is_empty() {
        return RpcResponse::err(id, "path is required");
    }

    let path = Path::new(&req.path);
    let data = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return RpcResponse::err(id, format!("failed to read {}: {err}", path.display()));
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
        return RpcResponse::ok(id, payload);
    }

    let line_count = text.split_inclusive('\n').count();

    let start = req.start_line.unwrap_or(1);
    let end = req.end_line.unwrap_or(line_count);

    if start == 0 {
        return RpcResponse::err(id, "start_line must be >= 1");
    }

    if end == 0 {
        return RpcResponse::err(id, "end_line must be >= 1");
    }

    if start > end {
        return RpcResponse::err(id, "start_line cannot be greater than end_line");
    }

    if start > line_count {
        return RpcResponse::err(
            id,
            format!("start_line {start} exceeds file line count {line_count}"),
        );
    }

    let resolved_end = end.min(line_count);
    let width = resolved_end.max(1).to_string().len();
    let show_line_numbers = req.show_line_numbers.unwrap_or(false);

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

    RpcResponse::ok(id, payload)
}
