use crate::RpcResponse;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{self, Instant};

/// Parse a grep-style output line: "path:line:text" (match) or "path-line-text" (context)
/// Returns (path, line_number, text, is_match)
fn parse_grep_line(line: &str) -> (String, u64, String, bool) {
    // Try match format first: "path:123:text"
    // Need to handle paths with colons (e.g., C:\foo on Windows)

    // Find the pattern ":digits:" which separates path from line number from text
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b':' || bytes[i] == b'-' {
            let sep = bytes[i];
            let is_match = sep == b':';

            // Check if followed by digits then another separator
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }

            // Must have at least one digit and be followed by the same separator
            if j > i + 1 && j < bytes.len() && bytes[j] == sep {
                let path = &line[..i];
                let line_no: u64 = line[i + 1..j].parse().unwrap_or(0);
                let text = &line[j + 1..];
                return (path.to_string(), line_no, text.to_string(), is_match);
            }
        }
        i += 1;
    }

    // Couldn't parse, return empty
    (String::new(), 0, String::new(), true)
}

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
        /// Fuzzy match tolerance (1-4 edits). Uses ugrep backend.
        #[serde(default)]
        fuzzy: Option<u8>,
    }

    let req = match RpcResponse::parse::<RgRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.pattern, "pattern", id.clone()) {
        return resp;
    }

    let root = req.path.as_deref().unwrap_or(".");
    if let Err(resp) = validation::validate_non_empty(root, "path", id.clone()) {
        return resp;
    }

    let max_results = req.max_results.unwrap_or(200).clamp(1, 10_000);
    let timeout_ms = req.timeout_ms.unwrap_or(20_000).clamp(100, 300_000);
    let fuzzy_distance = req.fuzzy.map(|f| f.clamp(1, 4));
    let use_ugrep = fuzzy_distance.is_some();

    let bin = if use_ugrep {
        if cfg!(target_os = "windows") {
            "ugrep.exe"
        } else {
            "ugrep"
        }
    } else if cfg!(target_os = "windows") {
        "rg.exe"
    } else {
        "rg"
    };

    let run = async {
        let mut cmd = Command::new(bin);

        if use_ugrep {
            // ugrep: use text output with -n -H for simpler parsing
            cmd.arg("-r").arg("-n").arg("-H");

            // Fuzzy flag
            if let Some(dist) = fuzzy_distance {
                cmd.arg(format!("-Z{}", dist));
            }
        } else {
            // ripgrep: use JSON output
            cmd.arg("--json").arg("--line-number").arg("--no-messages");
        }

        if req.fixed_strings.unwrap_or(false) {
            cmd.arg("-F");
        }
        if req.word_regexp.unwrap_or(false) {
            cmd.arg("-w");
        }

        // Case mode (flag differs: rg uses -S, ugrep uses -j for smart-case)
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
            _ => {
                cmd.arg(if use_ugrep { "-j" } else { "-S" });
            }
        }

        if req.hidden.unwrap_or(false) {
            cmd.arg("--hidden");
        }
        if req.follow.unwrap_or(false) {
            if use_ugrep {
                // ugrep: -R does recursive + follow (we already have -r, so add -S for symlinks)
                // Actually, ugrep's --dereference or just rely on -R instead of -r
                cmd.arg("--dereference");
            } else {
                cmd.arg("--follow");
            }
        }
        if req.no_ignore.unwrap_or(false) {
            cmd.arg(if use_ugrep {
                "--no-ignore-files"
            } else {
                "--no-ignore"
            });
        }
        if let Some(c) = req.context
            && c > 0
        {
            cmd.arg("-C").arg(c.to_string());
        }
        if let Some(globs) = &req.glob {
            for g in globs {
                if !g.trim().is_empty() {
                    cmd.arg(if use_ugrep { "-g" } else { "--glob" }).arg(g);
                }
            }
        }

        // Pattern and root path
        cmd.arg(&req.pattern).arg(root);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            if use_ugrep {
                anyhow::anyhow!("failed to spawn ugrep (required for fuzzy search). Install: winget install ugrep / brew install ugrep / apt install ugrep. Error: {e}")
            } else {
                anyhow::anyhow!("failed to spawn rg. Is ripgrep installed and on PATH? Error: {e}")
            }
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
        let mut timed_out = false;

        let mut reader = BufReader::new(stdout).lines();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            tokio::select! {
                maybe_line = reader.next_line() => {
                    let Some(line) = maybe_line? else { break; };

                    if use_ugrep {
                        // ugrep text output: "path:line:text" or "path-line-text" for context.
                        let (path, line_no, text, is_match) = parse_grep_line(&line);
                        if path.is_empty() {
                            continue;
                        }

                        let sep = if is_match { ":" } else { "-" };
                        rendered_lines.push(format!("{path}{sep}{line_no}{sep}{text}"));

                        // Build a match object compatible with rg JSON structure.
                        let match_obj = json!({
                            "type": if is_match { "match" } else { "context" },
                            "data": {
                                "path": {"text": path},
                                "line_number": line_no,
                                "lines": {"text": text}
                            }
                        });
                        matches.push(match_obj);
                    } else {
                        // ripgrep JSON output.
                        let v: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if ty != "match" && ty != "context" {
                            continue;
                        }

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

                            let one_line = text.trim_end_matches(&['\r', '\n'][..]);
                            let sep = if ty == "match" { ":" } else { "-" };
                            rendered_lines.push(format!("{path}{sep}{line_no}{sep}{one_line}"));
                        }

                        matches.push(v);
                    }

                    if matches.len() >= max_results {
                        truncated = true;
                        let _ = child.kill().await;
                        break;
                    }
                }
                _ = time::sleep_until(deadline) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }

        // Wait for process to exit (even if killed).
        let status = match time::timeout(Duration::from_millis(2_000), child.wait()).await {
            Ok(res) => Some(res?),
            Err(_) => {
                timed_out = true;
                let _ = child.kill().await;
                None
            }
        };
        let exit_code = status.as_ref().and_then(|s| s.code());

        let mut stderr_task = stderr_task;
        let stderr_text = match time::timeout(Duration::from_millis(2_000), &mut stderr_task).await
        {
            Ok(joined) => joined.unwrap_or_else(|_| String::new()),
            Err(_) => {
                stderr_task.abort();
                String::new()
            }
        }
        .trim()
        .to_string();

        // ripgrep: 0 = matches, 1 = no matches, 2 = error
        let success = status
            .as_ref()
            .is_some_and(|s| s.success() || exit_code == Some(1) || truncated)
            && !timed_out;

        Ok::<_, anyhow::Error>((
            matches,
            rendered_lines,
            truncated,
            exit_code,
            stderr_text,
            success,
            timed_out,
        ))
    };

    match run.await {
        Ok((matches, rendered_lines, truncated, exit_code, stderr_text, success, timed_out)) => {
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
                "timed_out": timed_out,
                "count": matches.len(),
                "matches": matches,
            });

            if !stderr_text.is_empty()
                && let Some(obj) = payload.as_object_mut()
            {
                obj.insert("stderr".to_string(), Value::String(stderr_text));
            }

            RpcResponse::ok(id, payload)
        }
        Err(e) => RpcResponse::err(id, format!("ripgrep error: {e:#}")),
    }
}
