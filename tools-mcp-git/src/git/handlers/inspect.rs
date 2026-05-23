use super::super::run_git;
use super::super::trim_git_line_end;
use super::super::types::{GitExecResult, build_git_response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
use tools_mcp_core::validation;

fn validate_non_option_arg(value: &str, field_name: &str) -> Result<(), ToolCallOutcome> {
    validation::validate_non_empty(value, field_name, None)?;
    if value.starts_with('-') {
        return Err(ToolCallOutcome::err(format!(
            "{field_name} must not start with '-'"
        )));
    }
    Ok(())
}

fn inspect_output_text(exec: &GitExecResult, empty_success_text: Option<&str>) -> String {
    let stdout = trim_git_line_end(&exec.stdout);
    if exec.success {
        if stdout.trim().is_empty() {
            empty_success_text.unwrap_or(stdout).to_string()
        } else {
            stdout.to_string()
        }
    } else {
        let stderr = trim_git_line_end(&exec.stderr);
        if stderr.trim().is_empty() {
            stdout.to_string()
        } else {
            stderr.to_string()
        }
    }
}

fn max_bytes_fields(max_bytes: usize) -> HashMap<&'static str, Value> {
    let mut extra_fields = HashMap::with_capacity(1);
    extra_fields.insert("max_bytes", json!(max_bytes));
    extra_fields
}

/// Handle the `GitLog` MCP tool request.
///
/// Executes `git log` to show commit history with configurable format and filters.
pub async fn handle_git_log(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitLogRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        max_count: Option<u32>,
        #[serde(default)]
        oneline: Option<bool>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        until: Option<String>,
        #[serde(default)]
        grep: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitLogRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["log".into()];

    if let Some(n) = req.max_count {
        cmd_args.push(format!("-{n}"));
    }
    if req.oneline.unwrap_or(false) {
        cmd_args.push("--oneline".into());
    }
    if let Some(fmt) = &req.format {
        cmd_args.push(format!("--format={fmt}"));
    }
    if let Some(author) = &req.author {
        cmd_args.push(format!("--author={author}"));
    }
    if let Some(since) = &req.since {
        cmd_args.push(format!("--since={since}"));
    }
    if let Some(until) = &req.until {
        cmd_args.push(format!("--until={until}"));
    }
    if let Some(grep) = &req.grep {
        cmd_args.push(format!("--grep={grep}"));
    }
    if let Some(path) = &req.path {
        cmd_args.push("--".into());
        cmd_args.push(path.clone());
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        max_bytes,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = inspect_output_text(&exec, Some("no commits"));
    let payload = build_git_response(&exec, &text, Some(max_bytes_fields(max_bytes)));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitShow` MCP tool request.
///
/// Executes `git show` to display a commit's contents.
pub async fn handle_git_show(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitShowRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        commit: Option<String>,
        #[serde(default)]
        stat: Option<bool>,
        #[serde(default)]
        name_only: Option<bool>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitShowRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["show".into()];

    if req.stat.unwrap_or(false) {
        cmd_args.push("--stat".into());
    }
    if req.name_only.unwrap_or(false) {
        cmd_args.push("--name-only".into());
    }
    if let Some(fmt) = &req.format {
        cmd_args.push(format!("--format={fmt}"));
    }
    if let Some(commit) = &req.commit {
        if let Err(o) = validate_non_option_arg(commit, "commit") {
            return o;
        }
        cmd_args.push("--end-of-options".into());
        cmd_args.push(commit.clone());
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        max_bytes,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = inspect_output_text(&exec, None);
    let payload = build_git_response(&exec, &text, Some(max_bytes_fields(max_bytes)));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitBlame` MCP tool request.
///
/// Executes `git blame` to show line-by-line authorship.
pub async fn handle_git_blame(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitBlameRequest {
        path: String,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        start_line: Option<u32>,
        #[serde(default)]
        end_line: Option<u32>,
        #[serde(default)]
        commit: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitBlameRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["blame".into()];

    if let (Some(start), Some(end)) = (req.start_line, req.end_line) {
        cmd_args.push(format!("-L{start},{end}"));
    } else if let Some(start) = req.start_line {
        cmd_args.push(format!("-L{start},"));
    } else if let Some(end) = req.end_line {
        cmd_args.push(format!("-L1,{end}"));
    }

    if let Some(commit) = &req.commit {
        if let Err(o) = validate_non_option_arg(commit, "commit") {
            return o;
        }
        cmd_args.push(commit.clone());
    }

    cmd_args.push("--".into());
    cmd_args.push(req.path.clone());

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        max_bytes,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = inspect_output_text(&exec, None);

    let mut extra_fields = HashMap::with_capacity(2);
    extra_fields.insert("path", json!(req.path));
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

#[cfg(test)]
mod tests {
    use super::super::super::types::GitExecResult;
    use super::{handle_git_blame, handle_git_show, inspect_output_text};
    use serde_json::json;

    fn exec_result(success: bool, stdout: &str, stderr: &str) -> GitExecResult {
        GitExecResult {
            git_bin: "git".to_string(),
            args: Vec::new(),
            working_dir: None,
            exit_code: Some(if success { 0 } else { 1 }),
            success,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated_stdout: false,
            truncated_stderr: false,
            timed_out: false,
        }
    }

    #[test]
    fn inspect_output_text_preserves_success_stdout_trimming() {
        let exec = exec_result(true, "commit abc\r\n", "");

        assert_eq!(inspect_output_text(&exec, Some("no commits")), "commit abc");
    }

    #[test]
    fn inspect_output_text_uses_empty_success_text_when_configured() {
        let exec = exec_result(true, "\r\n", "");

        assert_eq!(inspect_output_text(&exec, Some("no commits")), "no commits");
        assert_eq!(inspect_output_text(&exec, None), "");
    }

    #[test]
    fn inspect_output_text_prefers_failure_stderr_when_present() {
        let exec = exec_result(false, "stdout detail\n", "fatal: bad revision\n");

        assert_eq!(
            inspect_output_text(&exec, Some("no commits")),
            "fatal: bad revision"
        );
    }

    #[test]
    fn inspect_output_text_falls_back_to_failure_stdout() {
        let exec = exec_result(false, "stdout detail\n", "\n");

        assert_eq!(inspect_output_text(&exec, None), "stdout detail");
    }

    #[tokio::test]
    async fn git_show_rejects_option_like_commit() {
        let outcome = handle_git_show(None, json!({"commit": "--output=target/side-effect"})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("must not start with '-'")
        );
    }

    #[tokio::test]
    async fn git_blame_rejects_option_like_commit() {
        let outcome = handle_git_blame(
            None,
            json!({"path": "src/lib.rs", "commit": "--contents=Cargo.toml"}),
        )
        .await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("must not start with '-'")
        );
    }
}
