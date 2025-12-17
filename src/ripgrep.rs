use crate::{RpcResponse, err_text};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time;

/// Run ripgrep (`rg`) and return both a readable summary and structured matches.
///
/// Notes:
/// - This tool executes `rg` directly (no shell).
/// - It uses `rg --json` so results are machine-readable and robust across platforms.
/// - Exit code semantics: 0 = matches found, 1 = no matches, 2 = error.
pub async fn handle_ripgrep(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct RgRequest {
        /// Regex (or literal if `fixed_strings=true`).
        pattern: String,
        /// Root path (file or directory). Defaults to ".".
        #[serde(default)]
        path: Option<String>,
        /// Case handling: "smart" (default), "sensitive", or "insensitive".
        #[serde(default)]
        case: Option<String>,
        /// Treat pattern as literal text (equivalent to `rg -F`).
        #[serde(default)]
        fixed_strings: Option<bool>,
        /// Whole-word search (`rg -w`).
        #[serde(default)]
        word_regexp: Option<bool>,
        /// Add `--glob <pattern>` entries.
        #[serde(default)]
        glob: Option<Vec<String>>,
        /// Include hidden files/directories (`--hidden`).
        #[serde(default)]
        hidden: Option<bool>,
        /// Follow symlinks (`--follow`).
        #[serde(default)]
        follow: Option<bool>,
        /// Do not respect ignore files (`--no-ignore`).
        #[serde(default)]
        no_ignore: Option<bool>,
        /// Context lines around each match (`-C`).
        #[serde(default)]
        context: Option<usize>,
        /// Maximum number of match/context events to return (global). Defaults to 200.
        #[serde(default)]
        max_results: Option<usize>,
        /// Kill the search if it runs longer than this (ms). Defaults to 20_000.
        #[serde(default)]
        timeout_ms: Option<u64>,
    }

    let req = match serde_json::from_value::<RgRequest>(args) {
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

    if req.pattern.trim().is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("pattern is required")),
            error: None,
        };
    }

    let root = req.path.as_deref().unwrap_or(".");
    if root.trim().is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("path must be non-empty")),
            error: None,
        };
    }

    let max_results = req.max_results.unwrap_or(200).clamp(1, 10_000);
    let timeout_ms = req.timeout_ms.unwrap_or(20_000).clamp(100, 300_000);

    let rg_bin = if cfg!(target_os = "windows") {
        "rg.exe"
    } else {
        "rg"
    };

    let run = async {
        // Build `rg` command.
        let mut cmd = Command::new(rg_bin);
        cmd.arg("--json").arg("--line-number").arg("--no-messages");

        if req.fixed_strings.unwrap_or(false) {
            cmd.arg("-F");
        }
        if req.word_regexp.unwrap_or(false) {
            cmd.arg("-w");
        }

        // Case mode
        match req
            .case
            .as_deref()
            .unwrap_or("smart")
            .to_ascii_lowercase()
            .as_str()
        {
            "sensitive" | "case-sensitive" | "case_sensitive" => {
                // default behavior (no flags)
            }
            "insensitive" | "ignore" | "ignore-case" | "ignore_case" => {
                cmd.arg("-i");
            }
            // ripgrep's smart-case mode
            _ => {
                cmd.arg("-S");
            }
        }

        if req.hidden.unwrap_or(false) {
            cmd.arg("--hidden");
        }
        if req.follow.unwrap_or(false) {
            cmd.arg("--follow");
        }
        if req.no_ignore.unwrap_or(false) {
            cmd.arg("--no-ignore");
        }
        if let Some(c) = req.context {
            if c > 0 {
                cmd.arg("-C").arg(c.to_string());
            }
        }
        if let Some(globs) = &req.glob {
            for g in globs {
                if !g.trim().is_empty() {
                    cmd.arg("--glob").arg(g);
                }
            }
        }

        // Pattern and root path. (No shell - arguments passed verbatim.)
        cmd.arg(&req.pattern).arg(root);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn {rg_bin}. Is ripgrep installed and on PATH error: {e}")
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture rg stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture rg stderr"))?;

        // Read stderr concurrently to avoid deadlocks.
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut r = BufReader::new(stderr);
            let _ = r.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let mut matches: Vec<Value> = Vec::new();
        let mut rendered_lines: Vec<String> = Vec::new();
        let mut truncated = false;

        let mut reader = BufReader::new(stdout).lines();
        while let Some(line) = reader.next_line().await? {
            // Each line is a JSON object.
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty != "match" && ty != "context" {
                continue;
            }

            // Extract essentials for a helpful text view.
            if let Some(data) = v.get("data") {
                let path = data
                    .get("path")
                    .and_then(|p| p.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let line_no = data
                    .get("line_number")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let text = data
                    .get("lines")
                    .and_then(|l| l.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                // Match lines typically end with a newline; trim for one-line rendering.
                let one_line = text.trim_end_matches(&['\r', '\n'][..]);

                // Mirror rg's convention: ':' for matches, '-' for context.
                let sep = if ty == "match" { ":" } else { "-" };
                rendered_lines.push(format!("{path}{sep}{line_no}{sep}{one_line}"));
            }

            matches.push(v);

            if matches.len() >= max_results {
                truncated = true;
                let _ = child.kill().await;
                break;
            }
        }

        // Wait for process to exit (even if killed).
        let status = child.wait().await?;
        let exit_code = status.code();

        let stderr_text = stderr_task
            .await
            .unwrap_or_else(|_| String::new())
            .trim()
            .to_string();

        // ripgrep: 0 = matches, 1 = no matches, 2 = error
        let success = status.success() || exit_code == Some(1) || truncated;

        Ok::<_, anyhow::Error>((
            matches,
            rendered_lines,
            truncated,
            exit_code,
            stderr_text,
            success,
        ))
    };

    let result = match time::timeout(Duration::from_millis(timeout_ms), run).await {
        Ok(Ok((matches, rendered_lines, truncated, exit_code, stderr_text, success))) => {
            let text_view = if rendered_lines.is_empty() {
                String::new()
            } else {
                rendered_lines.join("\n")
            };

            let mut payload = json!({
                "content": [{"type": "text", "text": text_view}],
                "isError": !success,
                "pattern": req.pattern,
                "path": root,
                "exit_code": exit_code,
                "truncated": truncated,
                "count": matches.len(),
                "matches": matches,
            });

            if !stderr_text.is_empty() {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("stderr".to_string(), Value::String(stderr_text));
                }
            }

            payload
        }
        Ok(Err(e)) => err_text(&format!("ripgrep error: {e:#}")),
        Err(_) => err_text(&format!("ripgrep timed out after {} ms", timeout_ms)),
    };

    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}
