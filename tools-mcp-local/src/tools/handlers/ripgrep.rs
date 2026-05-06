//! ugrep search handler implementation.

use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{self, Instant};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::validation;

use super::search_memory::handle_memory_search;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchRequest {
    /// Regex (or literal if `fixed_strings=true`).
    pub(super) pattern: String,
    /// Root path (file or directory). Defaults to ".".
    #[serde(default)]
    pub(super) path: Option<String>,
    /// Case handling: "smart" (default), "sensitive", or "insensitive".
    #[serde(default)]
    pub(super) case: Option<String>,
    /// Treat pattern as literal text (-F).
    #[serde(default)]
    pub(super) fixed_strings: Option<bool>,
    /// Whole-word search (-w).
    #[serde(default)]
    pub(super) word_regexp: Option<bool>,
    /// Add `--glob <pattern>` entries.
    #[serde(default)]
    pub(super) glob: Option<Vec<String>>,
    /// Include hidden files/directories (`--hidden`).
    #[serde(default)]
    pub(super) hidden: Option<bool>,
    /// Follow symlinks (`--follow`).
    #[serde(default)]
    pub(super) follow: Option<bool>,
    /// Do not respect ignore files (`--no-ignore`).
    #[serde(default)]
    pub(super) no_ignore: Option<bool>,
    /// Context lines around each match (`-C`).
    #[serde(default)]
    pub(super) context: Option<usize>,
    /// Maximum number of match/context events to return (global). Defaults to 200.
    #[serde(default)]
    pub(super) max_results: Option<usize>,
    /// Kill the search if it runs longer than this (ms). Defaults to `20_000`.
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
    /// Fuzzy match tolerance (1-4 edits). Uses the memory backend when eligible, otherwise ugrep.
    #[serde(default)]
    pub(super) fuzzy: Option<u8>,
}

impl SearchRequest {
    pub(super) fn root(&self) -> &str {
        self.path.as_deref().unwrap_or(".")
    }

    pub(super) fn max_results(&self) -> usize {
        validation::clamp_limit(self.max_results, 200, 1, 10_000)
    }

    pub(super) fn timeout_ms(&self) -> u64 {
        validation::clamp_timeout(self.timeout_ms, 20_000, 100, 300_000)
    }

    pub(super) fn fuzzy_distance(&self) -> Option<u8> {
        self.fuzzy.map(|f| f.clamp(1, 4))
    }

    pub(super) fn case_mode(&self) -> String {
        self.case.as_deref().unwrap_or("smart").to_ascii_lowercase()
    }
}

fn parse_and_validate(args: &Value) -> Result<SearchRequest, ToolCallOutcome> {
    let req = ToolCallOutcome::parse_args::<SearchRequest>(args)?;

    validation::validate_non_empty(&req.pattern, "pattern", None)?;
    validation::validate_non_empty(req.root(), "path", None)?;

    Ok(req)
}

fn add_fallback_metadata(mut outcome: ToolCallOutcome, fallback_reason: &str) -> ToolCallOutcome {
    if let Some(obj) = outcome.0.as_object_mut() {
        obj.insert("backend".to_string(), Value::String("ugrep".to_string()));
        obj.insert(
            "fallback_reason".to_string(),
            Value::String(fallback_reason.to_string()),
        );
    }
    outcome
}

/// Parse a grep-style output line: "path:line:text" (match) or "path-line-text" (context)
/// Returns (path, `line_number`, text, `is_match`)
fn parse_grep_line(line: &str) -> (String, u64, String, bool) {
    fn separator_candidates(line: &str, sep: u8) -> Vec<(String, u64, String, bool)> {
        let bytes = line.as_bytes();
        let mut candidates = Vec::new();
        for i in 0..bytes.len() {
            if bytes[i] != sep {
                continue;
            }

            // Check if followed by digits then another matching separator.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }

            if j > i + 1
                && j < bytes.len()
                && bytes[j] == sep
                && let Ok(line_no) = line[i + 1..j].parse::<u64>()
            {
                candidates.push((
                    line[..i].to_string(),
                    line_no,
                    line[j + 1..].to_string(),
                    sep == b':',
                ));
            }
        }
        candidates
    }

    fn parse_with_sep_from_end(line: &str, sep: u8) -> Option<(String, u64, String, bool)> {
        separator_candidates(line, sep).into_iter().next_back()
    }

    fn parse_match_line(line: &str) -> Option<(String, u64, String, bool)> {
        let candidates = separator_candidates(line, b':');
        if candidates.len() > 1
            && let Some(existing_path) = candidates
                .iter()
                .rev()
                .find(|(path, _, _, _)| Path::new(path).exists())
        {
            return Some(existing_path.clone());
        }
        candidates.into_iter().next()
    }

    // Parse match lines first ("path:line:text") by scanning from the start.
    // This avoids selecting a later `:<digits>:` sequence in the matched text.
    if let Some(parsed) = parse_match_line(line) {
        return parsed;
    }
    // Parse context lines ("path-line-text") by scanning from the end.
    // This avoids misparsing filenames containing "-<digits>-", e.g. "foo-1-bar.txt-10-text".
    if let Some(parsed) = parse_with_sep_from_end(line, b'-') {
        return parsed;
    }

    // Couldn't parse, return empty
    (String::new(), 0, String::new(), true)
}

fn classify_success(
    status: Option<std::process::ExitStatus>,
    exit_code: Option<i32>,
    truncated: bool,
    timed_out: bool,
) -> bool {
    (truncated
        || status
            .as_ref()
            .is_some_and(|s| s.success() || exit_code == Some(1)))
        && !timed_out
}

/// Run ugrep and return both a readable summary and structured matches.
///
/// Notes:
/// - This tool executes `ugrep` directly (no shell).
/// - Uses text output with -n -H for simpler parsing.
/// - Exit code semantics: 0 = matches found, 1 = no matches, 2 = error.
pub async fn handle_search(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match parse_and_validate(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    match handle_memory_search(&req).await {
        Ok(outcome) => outcome,
        Err(err) if err.fallback_allowed => {
            add_fallback_metadata(handle_search_ugrep(req).await, err.fallback_reason)
        }
        Err(err) => err.into_tool_outcome(&req),
    }
}

async fn handle_search_ugrep(req: SearchRequest) -> ToolCallOutcome {
    let root = req.root().to_string();
    let max_results = req.max_results();
    let timeout_ms = req.timeout_ms();
    let fuzzy_distance = req.fuzzy_distance();
    let bin = if cfg!(target_os = "windows") {
        "ugrep.exe"
    } else {
        "ugrep"
    };

    let run = async {
        let mut cmd = Command::new(bin);

        // ugrep: use text output with -n -H for simpler parsing
        cmd.arg("-r").arg("-n").arg("-H");

        // Fuzzy flag
        if let Some(dist) = fuzzy_distance {
            cmd.arg(format!("-Z{dist}"));
        }

        if req.fixed_strings.unwrap_or(false) {
            cmd.arg("-F");
        }
        if req.word_regexp.unwrap_or(false) {
            cmd.arg("-w");
        }

        // Case mode: -j for smart-case, -i for insensitive
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
                cmd.arg("-j"); // ugrep smart-case
            }
        }

        if req.hidden.unwrap_or(false) {
            cmd.arg("--hidden");
        }
        if req.follow.unwrap_or(false) {
            cmd.arg("--dereference");
        }
        if req.no_ignore.unwrap_or(false) {
            cmd.arg("--no-ignore-files");
        }
        if let Some(c) = req.context
            && c > 0
        {
            cmd.arg("-C").arg(c.to_string());
        }
        if let Some(globs) = &req.glob {
            for g in globs {
                if !g.trim().is_empty() {
                    cmd.arg("-g").arg(g);
                }
            }
        }

        // End of options marker prevents patterns like "//" or "-foo" from being
        // interpreted as flags
        cmd.arg("--").arg(&req.pattern).arg(&root);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: {e}")
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stderr"))?;

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
        let mut terminated_for_limit = false;

        let mut reader = BufReader::new(stdout).lines();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            tokio::select! {
                maybe_line = reader.next_line() => {
                    let Some(line) = maybe_line? else { break; };

                    // ugrep text output: "path:line:text" or "path-line-text" for context
                    let (path, line_no, text, is_match) = parse_grep_line(&line);
                    if path.is_empty() {
                        continue;
                    }

                    let sep = if is_match { ":" } else { "-" };
                    rendered_lines.push(format!("{path}{sep}{line_no}{sep}{text}"));

                    let match_obj = json!({
                        "type": if is_match { "match" } else { "context" },
                        "data": {
                            "path": {"text": path},
                            "line_number": line_no,
                            "lines": {"text": text}
                        }
                    });
                    matches.push(match_obj);

                    if matches.len() >= max_results {
                        truncated = true;
                        terminated_for_limit = true;
                        let _ = child.kill().await;
                        break;
                    }
                }
                () = time::sleep_until(deadline) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }

        // Wait for process to exit (even if killed).
        let status =
            if let Ok(res) = time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Some(res?)
            } else {
                // If we intentionally terminated after collecting enough results,
                // a slow process shutdown should not be reported as a user-visible timeout.
                if !terminated_for_limit {
                    timed_out = true;
                }
                let _ = child.kill().await;
                None
            };
        let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);

        let mut stderr_task = stderr_task;
        let stderr_text = if let Ok(joined) =
            time::timeout(Duration::from_millis(2_000), &mut stderr_task).await
        {
            joined.unwrap_or_else(|_| String::new())
        } else {
            stderr_task.abort();
            String::new()
        }
        .trim()
        .to_string();

        // ugrep: 0 = matches, 1 = no matches, 2 = error
        let success = classify_success(status, exit_code, truncated, timed_out);

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
            let text_view = if !success && !stderr_text.is_empty() {
                // Show error message when search failed
                format!("Search error: {stderr_text}")
            } else if rendered_lines.is_empty() {
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

            ToolCallOutcome::ok(payload)
        }
        Err(e) => ToolCallOutcome::err(format!("ugrep error: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_success, parse_grep_line};

    #[test]
    #[cfg(unix)]
    fn truncated_without_timeout_is_success_even_after_forced_termination() {
        use std::os::unix::process::ExitStatusExt as _;
        let terminated = std::process::ExitStatus::from_raw(9);
        assert!(classify_success(Some(terminated), None, true, false));
    }

    #[test]
    fn truncated_without_status_is_success_when_not_timed_out() {
        assert!(classify_success(None, None, true, false));
    }

    #[test]
    #[cfg(unix)]
    fn truncated_with_timeout_is_error() {
        use std::os::unix::process::ExitStatusExt as _;
        let terminated = std::process::ExitStatus::from_raw(9);
        assert!(!classify_success(Some(terminated), None, true, true));
    }

    #[test]
    fn parse_grep_line_prefers_match_separator_when_filename_contains_hyphen_digits() {
        let line = "src/foo-1-bar.rs:42:let x = 1;";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/foo-1-bar.rs");
        assert_eq!(line_no, 42);
        assert_eq!(text, "let x = 1;");
        assert!(is_match);
    }

    #[test]
    fn parse_grep_line_parses_context_lines() {
        let line = "src/main.rs-7-use std::time::Duration;";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/main.rs");
        assert_eq!(line_no, 7);
        assert_eq!(text, "use std::time::Duration;");
        assert!(!is_match);
    }

    #[test]
    fn parse_grep_line_match_text_can_contain_colon_number_colon() {
        let line = "src/main.rs:12:timestamp 10:23:59";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/main.rs");
        assert_eq!(line_no, 12);
        assert_eq!(text, "timestamp 10:23:59");
        assert!(is_match);
    }

    #[test]
    fn parse_grep_line_preserves_existing_filename_with_colon_number_colon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("foo:1:bar.txt");
        std::fs::write(&path, "needle\n").expect("write");
        let line = format!("{}:7:needle", path.display());

        let (parsed_path, line_no, text, is_match) = parse_grep_line(&line);

        assert_eq!(parsed_path, path.display().to_string());
        assert_eq!(line_no, 7);
        assert_eq!(text, "needle");
        assert!(is_match);
    }
}
