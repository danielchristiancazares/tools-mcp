use serde_json::{json, Value};
use std::collections::HashMap;

/// Result of executing a Git command via [`run_git`](crate::git_tools::run_git).
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

/// Build a standard MCP-compatible Git response payload.
///
/// This consolidates the repeated response building pattern across all Git tools
/// (GitStatus, GitDiff, GitRestore, GitAdd, GitCommit) by handling the common
/// fields and allowing optional extra fields to be added per-tool.
///
/// # Arguments
///
/// * `exec` - The Git execution result containing output and metadata
/// * `text` - The human-readable text to include in the response
/// * `extra_fields` - Optional HashMap of additional fields to include in the response
///
/// # Returns
///
/// A JSON Value representing the complete MCP response payload
///
/// # Example
///
/// ```ignore
/// let payload = build_git_response(
///     &exec,
///     "Changes staged for commit".to_string(),
///     Some([("staged_files", json!(5))].iter().cloned().collect())
/// );
/// ```
pub fn build_git_response(
    exec: &GitExecResult,
    text: String,
    extra_fields: Option<HashMap<&str, Value>>,
) -> Value {
    let mut payload = json!({
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
    });

    // Add any extra fields provided by the caller
    if let Some(extra) = extra_fields
        && let Some(obj) = payload.as_object_mut()
    {
        for (key, value) in extra {
            obj.insert(key.to_string(), value);
        }
    }

    payload
}
