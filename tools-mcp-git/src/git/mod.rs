//! Git operation wrappers for MCP tool execution.
//!
//! This module provides safe, structured wrappers around common Git commands, designed
//! for use as MCP (Model Context Protocol) tools. Each wrapper executes Git in a
//! subprocess with configurable timeouts and output limits, returning machine-parseable
//! JSON responses.
//!
//! # Architecture
//!
//! All Git operations flow through [`run_git`], which:
//! 1. Spawns `git` (or `git.exe` on Windows) as a child process
//! 2. Configures deterministic output (`--no-pager`, `color.ui=false`)
//! 3. Disables config/env-driven external Git helpers for safer execution
//! 4. Captures stdout/stderr with configurable byte limits
//! 5. Enforces timeout with graceful kill on expiration
//! 6. Returns structured results via [`types::GitExecResult`]
//!
//! # Module Structure
//!
//! - [`types`]: Core types (`GitExecResult`) and response builders
//! - [`handlers`]: MCP tool handler implementations
//!
//! # Tools Provided
//!
//! | Tool | Function | Description |
//! |------|----------|-------------|
//! | `GitStatus` | [`handlers::handle_git_status`] | Working tree status with porcelain output |
//! | `GitDiff` | [`handlers::handle_git_diff`] | Show changes in working tree or staging area |
//! | `GitRestore` | [`handlers::handle_git_restore`] | Discard uncommitted changes (destructive) |
//! | `GitAdd` | [`handlers::handle_git_add`] | Stage files for commit |
//! | `GitCommit` | [`handlers::handle_git_commit`] | Create conventional commits |
//! | `GitLog` | [`handlers::handle_git_log`] | View commit history |
//! | `GitBranch` | [`handlers::handle_git_branch`] | List, create, or delete branches |
//! | `GitCheckout` | [`handlers::handle_git_checkout`] | Switch branches or restore files |
//! | `GitStash` | [`handlers::handle_git_stash`] | Save and restore work in progress |
//! | `GitShow` | [`handlers::handle_git_show`] | Display commit contents |
//! | `GitBlame` | [`handlers::handle_git_blame`] | Line-by-line authorship |
//!
//! # Configuration
//!
//! Default limits from [`crate::config`]:
//! - `DEFAULT_GIT_TIMEOUT_MS`: 30,000ms (30 seconds)
//! - `MAX_GIT_TIMEOUT_MS`: 300,000ms (5 minutes)
//! - `DEFAULT_GIT_STDOUT_BYTES`: 200,000 bytes
//! - `DEFAULT_GIT_STDERR_BYTES`: 100,000 bytes
//! - `MAX_OUTPUT_BYTES`: 5,000,000 bytes

mod handlers;
mod types;

pub use handlers::{
    handle_git_add, handle_git_blame, handle_git_branch, handle_git_checkout, handle_git_commit,
    handle_git_diff, handle_git_log, handle_git_restore, handle_git_show, handle_git_stash,
    handle_git_status,
};

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;
use tools_mcp_core::config::{MAX_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES};
use tools_mcp_core::process_utils::read_to_end_limited;

/// Execute a Git command with timeout and output capture.
///
/// This is the core execution engine for all Git tools. It spawns Git as a
/// subprocess with deterministic output settings and captures results within
/// configured limits.
///
/// # Arguments
///
/// * `working_dir` - Optional working directory for the Git command. If `None`,
///   uses the current process working directory.
/// * `subcommand_args` - Arguments to pass to Git after the standard prefixes.
///   Example: `["status", "--porcelain=1", "-b"]`
/// * `timeout_ms` - Maximum execution time in milliseconds. Clamped to
///   `[100, MAX_GIT_TIMEOUT_MS]`. On timeout, the process is killed.
/// * `max_stdout_bytes` - Maximum bytes to capture from stdout. Clamped to
///   `[1, MAX_OUTPUT_BYTES]`. Excess output is discarded.
/// * `max_stderr_bytes` - Maximum bytes to capture from stderr. Same limits.
///
/// # Command Construction
///
/// The final command is constructed as:
/// ```text
/// git --no-pager -c color.ui=false -c diff.external= -c core.fsmonitor= <subcommand_args...>
/// ```
///
/// The `--no-pager` flag prevents interactive pagers, `color.ui=false` ensures
/// no ANSI escape codes, and explicit config/environment overrides disable
/// external helper execution pathways (for example `diff.external` and
/// `core.fsmonitor`) for safer machine execution.
///
/// # Timeout Behavior
///
/// 1. If the command completes within `timeout_ms`, normal exit handling occurs
/// 2. On timeout, `SIGKILL` (or equivalent) is sent to the process
/// 3. A 2-second grace period allows the process to terminate
/// 4. If still running after grace period, returns an error
///
/// # Errors
///
/// Returns `Err` only for infrastructure failures:
/// - Git executable not found on PATH
/// - Failed to capture stdout/stderr handles
/// - Process did not terminate after kill signal
///
/// Git command failures (non-zero exit) are returned as `Ok(GitExecResult)`
/// with `success: false`.
pub(crate) async fn run_git(
    working_dir: Option<String>,
    subcommand_args: Vec<String>,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<types::GitExecResult, anyhow::Error> {
    let timeout_ms = timeout_ms.clamp(100, MAX_GIT_TIMEOUT_MS);
    let max_stdout_bytes = max_stdout_bytes.clamp(1, MAX_OUTPUT_BYTES);
    let max_stderr_bytes = max_stderr_bytes.clamp(1, MAX_OUTPUT_BYTES);

    let git_bin = if cfg!(target_os = "windows") {
        "git.exe".to_string()
    } else {
        "git".to_string()
    };

    // Force deterministic, non-ANSI output and disable config-driven external helpers.
    let mut args: Vec<String> = vec![
        "--no-pager".into(),
        "-c".into(),
        "color.ui=false".into(),
        "-c".into(),
        "diff.external=".into(),
        "-c".into(),
        "core.fsmonitor=".into(),
    ];
    args.extend(subcommand_args);

    let mut cmd = Command::new(&git_bin);
    cmd.args(&args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.env(
        "GIT_CONFIG_GLOBAL",
        if cfg!(target_os = "windows") {
            "NUL"
        } else {
            "/dev/null"
        },
    );
    cmd.env("GIT_EXTERNAL_DIFF", "");
    cmd.env_remove("GIT_CONFIG_COUNT");

    if let Some(dir) = &working_dir {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!("failed to spawn {git_bin}. Is Git installed and on PATH? error: {e}")
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture git stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture git stderr"))?;

    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    let mut timed_out = false;
    let status =
        if let Ok(res) = time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
            res?
        } else {
            timed_out = true;
            let _ = child.kill().await;
            match time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Ok(res) => res?,
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "git command timed out after {timeout_ms} ms and did not terminate"
                    ));
                }
            }
        };

    let exit_code = status.code();

    let (stdout_bytes, truncated_stdout) = stdout_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))?;
    let (stderr_bytes, truncated_stderr) = stderr_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))?;

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    Ok(types::GitExecResult {
        git_bin,
        args,
        working_dir,
        exit_code,
        success: status.success() && !timed_out,
        stdout,
        stderr,
        truncated_stdout,
        truncated_stderr,
        timed_out,
    })
}
