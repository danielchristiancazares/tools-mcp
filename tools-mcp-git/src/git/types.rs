//! Git operation types and response builders.
//!
//! This module contains the core types used by Git tool handlers:
//! - [`GitExecResult`]: Captures the result of a Git command execution
//! - [`build_git_response`]: Constructs MCP-compatible response payloads

use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PorcelainStatusSummary {
    entry_count: usize,
}

impl PorcelainStatusSummary {
    pub(crate) fn parse_v1(stdout: &str) -> Self {
        let entry_count = stdout
            .lines()
            .filter(|line| status_entry_line(line))
            .count();
        Self { entry_count }
    }

    pub(crate) fn is_clean(self) -> bool {
        self.entry_count == 0
    }
}

fn status_entry_line(line: &str) -> bool {
    !(line.trim().is_empty() || line.starts_with("##"))
}

/// Result of executing a Git command via [`super::run_git`].
///
/// Captures all information needed to construct an MCP response, including
/// the exact command executed, output streams, and execution metadata.
///
/// # Fields
///
/// - `git_bin`: The Git executable name (`git` on Unix, `git.exe` on Windows)
/// - `args`: Complete argument vector including `--no-pager` and color config
/// - `working_dir`: The working directory if one was specified
/// - `exit_code`: Process exit code, or `None` if terminated by signal
/// - `success`: `true` if exit code is 0 and no timeout occurred
/// - `stdout`/`stderr`: Captured output as UTF-8 strings (lossy conversion)
/// - `truncated_stdout`/`truncated_stderr`: Whether output exceeded byte limits
/// - `timed_out`: Whether the command was killed due to timeout
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct GitExecResult {
    pub git_bin: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
    pub timed_out: bool,
}

pub(crate) fn trim_git_output(output: &str) -> &str {
    output.trim_end_matches(&['\r', '\n'][..])
}

pub(crate) fn git_response_text(exec: &GitExecResult) -> String {
    let output = if exec.success || exec.stderr.trim().is_empty() {
        &exec.stdout
    } else {
        &exec.stderr
    };
    trim_git_output(output).to_string()
}

/// Build a standard MCP-compatible Git response payload.
///
/// Consolidates the response building pattern across all Git tools by handling
/// common fields and allowing optional extra fields per-tool.
///
/// # Arguments
///
/// * `exec` - The Git execution result containing output and metadata
/// * `text` - The human-readable text to include in the response
/// * `extra_fields` - Optional `HashMap` of additional fields to include
///
/// # Returns
///
/// A JSON Value representing the complete MCP response payload.
///
/// # Example
///
/// ```ignore
/// let payload = build_git_response(
///     &exec,
///     "Changes staged for commit".to_string(),
///     Some([("staged_files", json!(5))].into_iter().collect())
/// );
/// ```
pub fn build_git_response(
    exec: &GitExecResult,
    text: &str,
    extra_fields: Option<HashMap<&str, Value>>,
) -> Value {
    let mut payload = base_git_response(exec, text);

    if let Some(extra) = extra_fields {
        insert_extra_fields(&mut payload, extra);
    }

    payload
}

pub(crate) fn build_git_response_with_extra_fields<K, I>(
    exec: &GitExecResult,
    text: &str,
    extra_fields: I,
) -> Value
where
    K: AsRef<str>,
    I: IntoIterator<Item = (K, Value)>,
{
    let mut payload = base_git_response(exec, text);
    insert_extra_fields(&mut payload, extra_fields);
    payload
}

fn base_git_response(exec: &GitExecResult, text: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "truncated_stdout": exec.truncated_stdout,
        "truncated_stderr": exec.truncated_stderr,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    })
}

fn insert_extra_fields<K, I>(payload: &mut Value, extra_fields: I)
where
    K: AsRef<str>,
    I: IntoIterator<Item = (K, Value)>,
{
    if let Some(obj) = payload.as_object_mut() {
        for (key, value) in extra_fields {
            obj.insert(key.as_ref().to_string(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GitExecResult, PorcelainStatusSummary, build_git_response_with_extra_fields,
        git_response_text,
    };
    use serde_json::json;

    fn exec_result(success: bool, stdout: &str, stderr: &str) -> GitExecResult {
        GitExecResult {
            git_bin: "git".to_string(),
            args: vec!["--no-pager".to_string(), "status".to_string()],
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
    fn porcelain_status_summary_ignores_branch_headers() {
        let summary = PorcelainStatusSummary::parse_v1("## main...origin/main [ahead 1]\n\n");
        assert!(summary.is_clean());
    }

    #[test]
    fn porcelain_status_summary_counts_status_entries() {
        let summary = PorcelainStatusSummary::parse_v1(
            "## main\n M src/lib.rs\nR  old.rs -> moved.rs\n?? scratch.md\n",
        );
        assert!(!summary.is_clean());
    }

    #[test]
    fn git_response_text_prefers_stdout_for_success() {
        let exec = exec_result(true, "status output\r\n", "warning\r\n");
        assert_eq!(git_response_text(&exec), "status output");
    }

    #[test]
    fn git_response_text_prefers_stderr_for_failure_when_present() {
        let exec = exec_result(false, "stdout fallback\r\n", "fatal: not a repository\r\n");
        assert_eq!(git_response_text(&exec), "fatal: not a repository");
    }

    #[test]
    fn git_response_text_uses_stdout_for_failure_without_stderr() {
        let exec = exec_result(false, "stdout error\r\n", "");
        assert_eq!(git_response_text(&exec), "stdout error");
    }

    #[test]
    fn extra_field_response_builder_preserves_git_contract_fields() {
        let exec = exec_result(true, "clean\n", "");
        let payload =
            build_git_response_with_extra_fields(&exec, "clean", [("clean", json!(true))]);

        assert_eq!(payload["content"][0]["text"], "clean");
        assert_eq!(payload["isError"], false);
        assert_eq!(payload["stdout"], "clean\n");
        assert_eq!(payload["clean"], true);
        assert_eq!(payload["args"][1], "status");
    }
}
