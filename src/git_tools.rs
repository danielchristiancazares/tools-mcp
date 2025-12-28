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
//! 3. Captures stdout/stderr with configurable byte limits
//! 4. Enforces timeout with graceful kill on expiration
//! 5. Returns structured results via [`GitExecResult`]
//!
//! # Tools Provided
//!
//! | Tool | Function | Description |
//! |------|----------|-------------|
//! | `GitStatus` | [`handle_git_status`] | Working tree status with porcelain output |
//! | `GitDiff` | [`handle_git_diff`] | Show changes in working tree or staging area |
//! | `GitRestore` | [`handle_git_restore`] | Discard uncommitted changes (destructive) |
//! | `GitAdd` | [`handle_git_add`] | Stage files for commit |
//! | `GitCommit` | [`handle_git_commit`] | Create conventional commits |
//!
//! # Response Format
//!
//! All handlers return MCP-compliant responses with:
//! - `content`: Array containing `{"type": "text", "text": "..."}` with human-readable output
//! - `isError`: Boolean indicating command failure
//! - `git_bin`: The Git executable used (`git` or `git.exe`)
//! - `args`: Full argument list passed to Git
//! - `working_dir`: Working directory if specified
//! - `exit_code`: Process exit code (null if terminated by signal)
//! - `timed_out`: Whether the command exceeded its timeout
//! - `stdout`/`stderr`: Raw captured output
//! - `truncated_stdout`/`truncated_stderr`: Whether output was truncated
//!
//! # Error Handling
//!
//! Errors are categorized as:
//! - **Spawn failures**: Git not installed or not on PATH
//! - **Timeout**: Command exceeded `timeout_ms`, process killed
//! - **Git errors**: Non-zero exit code with error in stderr
//! - **Validation errors**: Invalid parameters (empty paths, conflicting flags)
//!
//! All errors return valid MCP responses with `isError: true` rather than panicking.
//!
//! # Configuration
//!
//! Default limits from [`crate::config`]:
//! - `DEFAULT_GIT_TIMEOUT_MS`: 30,000ms (30 seconds)
//! - `MAX_GIT_TIMEOUT_MS`: 300,000ms (5 minutes)
//! - `DEFAULT_GIT_STDOUT_BYTES`: 200,000 bytes
//! - `DEFAULT_GIT_STDERR_BYTES`: 100,000 bytes
//! - `MAX_OUTPUT_BYTES`: 5,000,000 bytes

use crate::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
    MAX_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
use crate::git_utils::{build_git_response, GitExecResult};
use crate::process_utils::read_to_end_limited;
use crate::validation;
use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;

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
/// git --no-pager -c color.ui=false <subcommand_args...>
/// ```
///
/// The `--no-pager` flag prevents interactive pagers, and `color.ui=false`
/// ensures no ANSI escape codes in output, making it safe for machine parsing.
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
///
/// # Example
///
/// ```ignore
/// let result = run_git(
///     Some("/path/to/repo".to_string()),
///     vec!["status".into(), "--porcelain=1".into()],
///     30_000,  // 30 second timeout
///     200_000, // 200KB stdout limit
///     100_000, // 100KB stderr limit
/// ).await?;
///
/// if result.success {
///     println!("Status: {}", result.stdout);
/// } else {
///     eprintln!("Git error: {}", result.stderr);
/// }
/// ```
async fn run_git(
    working_dir: Option<String>,
    subcommand_args: Vec<String>,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<GitExecResult, anyhow::Error> {
    let timeout_ms = timeout_ms.clamp(100, MAX_GIT_TIMEOUT_MS);
    let max_stdout_bytes = max_stdout_bytes.clamp(1, MAX_OUTPUT_BYTES);
    let max_stderr_bytes = max_stderr_bytes.clamp(1, MAX_OUTPUT_BYTES);

    let git_bin = if cfg!(target_os = "windows") {
        "git.exe".to_string()
    } else {
        "git".to_string()
    };

    // Force deterministic, non-ANSI output for machine consumption.
    let mut args: Vec<String> = vec!["--no-pager".into(), "-c".into(), "color.ui=false".into()];
    args.extend(subcommand_args);

    let mut cmd = Command::new(&git_bin);
    cmd.args(&args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

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
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(res) => res?,
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            match time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Ok(res) => res?,
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "git command timed out after {} ms and did not terminate",
                        timeout_ms
                    ));
                }
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

    Ok(GitExecResult {
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

/// Handle the `GitStatus` MCP tool request.
///
/// Executes `git status` and returns working tree state in a structured format.
/// By default, uses porcelain output for reliable machine parsing.
///
/// # Parameters
///
/// | Name | Type | Default | Description |
/// |------|------|---------|-------------|
/// | `working_dir` | `string` | CWD | Directory containing the Git repository |
/// | `timeout_ms` | `integer` | 30000 | Maximum execution time (100-300000ms) |
/// | `porcelain` | `boolean` | `true` | Use `--porcelain=1` for stable output format |
/// | `branch` | `boolean` | `true` | Include branch info via `-b` (porcelain only) |
/// | `untracked` | `boolean` | `true` | Include untracked files; `false` uses `-uno` |
///
/// # Porcelain Output Format
///
/// When `porcelain: true` (default), output follows Git's porcelain v1 format:
///
/// ```text
/// ## main...origin/main [ahead 2]
/// M  src/lib.rs
///  M src/main.rs
/// ?? new_file.txt
/// ```
///
/// Status codes (first two columns):
/// - ` ` (space): Unmodified
/// - `M`: Modified
/// - `A`: Added
/// - `D`: Deleted
/// - `R`: Renamed
/// - `C`: Copied
/// - `U`: Updated but unmerged
/// - `?`: Untracked
/// - `!`: Ignored
///
/// First column = staging area, second column = working tree.
///
/// # Response Fields
///
/// In addition to standard fields, includes:
/// - `clean`: `true` if working tree has no changes (stdout empty on success)
///
/// # Example
///
/// Request:
/// ```json
/// {
///   "working_dir": "/path/to/repo",
///   "porcelain": true,
///   "branch": true
/// }
/// ```
///
/// Response (clean repository):
/// ```json
/// {
///   "content": [{"type": "text", "text": "clean"}],
///   "isError": false,
///   "clean": true,
///   "exit_code": 0
/// }
/// ```
///
/// Response (with changes):
/// ```json
/// {
///   "content": [{"type": "text", "text": "## main\n M src/lib.rs"}],
///   "isError": false,
///   "clean": false,
///   "exit_code": 0
/// }
/// ```
pub async fn handle_git_status(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitStatusRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        porcelain: Option<bool>,
        #[serde(default)]
        branch: Option<bool>,
        #[serde(default)]
        untracked: Option<bool>,
    }

    let req = match RpcResponse::parse::<GitStatusRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let porcelain = req.porcelain.unwrap_or(true);
    let branch = req.branch.unwrap_or(true);
    let untracked = req.untracked.unwrap_or(true);

    let mut cmd_args: Vec<String> = vec!["status".into()];
    if porcelain {
        cmd_args.push("--porcelain=1".into());
        if branch {
            cmd_args.push("-b".into());
        }
        if !untracked {
            cmd_args.push("-uno".into());
        }
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let clean = exec.success && exec.stdout.trim().is_empty();
    let text = if exec.success {
        if clean {
            "clean".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("clean", json!(clean));

    let payload = build_git_response(&exec, text, Some(extra_fields));

    RpcResponse::ok(id, payload)
}

/// Handle the `GitDiff` MCP tool request.
///
/// Executes `git diff` to show changes between commits, the staging area, and
/// the working tree. Supports various output formats and path filtering.
///
/// # Parameters
///
/// | Name | Type | Default | Description |
/// |------|------|---------|-------------|
/// | `working_dir` | `string` | CWD | Directory containing the Git repository |
/// | `timeout_ms` | `integer` | 30000 | Maximum execution time (100-300000ms) |
/// | `cached` | `boolean` | `false` | Show staged changes (`--cached`) |
/// | `stat` | `boolean` | `false` | Show diffstat summary only (`--stat`) |
/// | `name_only` | `boolean` | `false` | Show only changed file names (`--name-only`) |
/// | `unified` | `integer` | Git default (3) | Context lines around changes (`-U<N>`) |
/// | `paths` | `string[]` | all | Limit diff to specific paths |
/// | `max_bytes` | `integer` | 200000 | Maximum stdout capture (1-5000000) |
///
/// # Diff Modes
///
/// - **Working tree vs staging** (default): Shows unstaged changes
/// - **Staging vs HEAD** (`cached: true`): Shows what will be committed
///
/// # Output Formats
///
/// - **Unified diff** (default): Full patch with context
/// - **Stat** (`stat: true`): Summary like `file.rs | 10 ++--`
/// - **Name only** (`name_only: true`): Just file paths, one per line
///
/// # Response Fields
///
/// In addition to standard fields, includes:
/// - `max_bytes`: The effective byte limit used for stdout capture
///
/// # Example
///
/// Request (staged changes, stat only):
/// ```json
/// {
///   "cached": true,
///   "stat": true
/// }
/// ```
///
/// Response:
/// ```json
/// {
///   "content": [{"type": "text", "text": " src/lib.rs | 15 +++++++++------\n 1 file changed, 9 insertions(+), 6 deletions(-)"}],
///   "isError": false,
///   "exit_code": 0
/// }
/// ```
///
/// Request (specific file with more context):
/// ```json
/// {
///   "paths": ["src/main.rs"],
///   "unified": 10
/// }
/// ```
pub async fn handle_git_diff(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitDiffRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        cached: Option<bool>,
        #[serde(default)]
        stat: Option<bool>,
        #[serde(default)]
        name_only: Option<bool>,
        #[serde(default)]
        unified: Option<i64>,
        #[serde(default)]
        paths: Option<Vec<String>>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match RpcResponse::parse::<GitDiffRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes = validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["diff".into()];

    if req.cached.unwrap_or(false) {
        cmd_args.push("--cached".into());
    }
    if req.stat.unwrap_or(false) {
        cmd_args.push("--stat".into());
    }
    if req.name_only.unwrap_or(false) {
        cmd_args.push("--name-only".into());
    }
    if let Some(u) = req.unified
        && u >= 0
    {
        cmd_args.push(format!("-U{u}"));
    }

    if let Some(paths) = &req.paths
        && !paths.is_empty()
    {
        cmd_args.push("--".into());
        for p in paths {
            if !p.trim().is_empty() {
                cmd_args.push(p.clone());
            }
        }
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
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        if exec.stdout.trim().is_empty() {
            "no diff".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, text, Some(extra_fields));

    RpcResponse::ok(id, payload)
}

/// Handle the `GitRestore` MCP tool request.
///
/// Executes `git restore` to discard uncommitted changes. This is a **destructive
/// operation** that cannot be undone for working tree changes.
///
/// # Warning
///
/// This tool permanently discards changes:
/// - Working tree changes are lost forever (not recoverable)
/// - Staged changes can be recovered from the index if only `--staged` is used
///
/// # Parameters
///
/// | Name | Type | Default | Required | Description |
/// |------|------|---------|----------|-------------|
/// | `paths` | `string[]` | - | **Yes** | Files to restore (passed after `--`) |
/// | `working_dir` | `string` | CWD | No | Directory containing the Git repository |
/// | `timeout_ms` | `integer` | 30000 | No | Maximum execution time (100-300000ms) |
/// | `staged` | `boolean` | `false` | No | Restore the staging area (`--staged`) |
/// | `worktree` | `boolean` | `true` | No | Restore the working tree (`--worktree`) |
///
/// # Restore Modes
///
/// | `staged` | `worktree` | Effect |
/// |----------|------------|--------|
/// | `false` | `true` | Discard working tree changes (revert to staged or HEAD) |
/// | `true` | `false` | Unstage files (keep working tree changes) |
/// | `true` | `true` | Discard all changes (revert to HEAD) |
/// | `false` | `false` | **Error**: At least one must be true |
///
/// # Validation
///
/// Returns an error if:
/// - `paths` is empty
/// - Both `staged` and `worktree` are `false`
///
/// # Example
///
/// Request (discard working tree changes):
/// ```json
/// {
///   "paths": ["src/lib.rs", "src/main.rs"],
///   "worktree": true
/// }
/// ```
///
/// Request (unstage files, keep changes):
/// ```json
/// {
///   "paths": ["src/lib.rs"],
///   "staged": true,
///   "worktree": false
/// }
/// ```
///
/// Response (success):
/// ```json
/// {
///   "content": [{"type": "text", "text": "ok"}],
///   "isError": false,
///   "exit_code": 0
/// }
/// ```
pub async fn handle_git_restore(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitRestoreRequest {
        paths: Vec<String>,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        staged: Option<bool>,
        #[serde(default)]
        worktree: Option<bool>,
    }

    let req = match RpcResponse::parse::<GitRestoreRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if req.paths.is_empty() {
        return RpcResponse::err(id, "paths must be non-empty");
    }

    let staged = req.staged.unwrap_or(false);
    let worktree = req.worktree.unwrap_or(true);

    if !staged && !worktree {
        return RpcResponse::err(id, "at least one of staged/worktree must be true");
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let mut cmd_args: Vec<String> = vec!["restore".into()];
    if staged {
        cmd_args.push("--staged".into());
    }
    if worktree {
        cmd_args.push("--worktree".into());
    }

    cmd_args.push("--".into());
    for p in &req.paths {
        if !p.trim().is_empty() {
            cmd_args.push(p.clone());
        }
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        if exec.stdout.trim().is_empty() && exec.stderr.trim().is_empty() {
            "ok".to_string()
        } else if exec.stdout.trim().is_empty() {
            exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, text, None);

    RpcResponse::ok(id, payload)
}

/// Handle the `GitAdd` MCP tool request.
///
/// Executes `git add` to stage files for the next commit. Supports staging
/// specific paths, all changes, or only tracked file updates.
///
/// # Parameters
///
/// | Name | Type | Default | Description |
/// |------|------|---------|-------------|
/// | `paths` | `string[]` | - | Specific files to stage |
/// | `working_dir` | `string` | CWD | Directory containing the Git repository |
/// | `timeout_ms` | `integer` | 30000 | Maximum execution time (100-300000ms) |
/// | `all` | `boolean` | `false` | Stage all changes including untracked (`-A`) |
/// | `update` | `boolean` | `false` | Stage modifications/deletions only (`-u`) |
///
/// # Staging Modes
///
/// | Mode | Flag | Behavior |
/// |------|------|----------|
/// | Specific paths | `paths: [...]` | Stage only listed files |
/// | All changes | `all: true` | Stage all: new, modified, and deleted files |
/// | Update only | `update: true` | Stage tracked files only (no new files) |
///
/// When `all: true`, the `-A` flag is used. When `update: true`, the `-u` flag
/// is used. If both are specified, `all` takes precedence.
///
/// # Validation
///
/// Returns an error if:
/// - `paths` is empty AND `all` is false AND `update` is false
///
/// This prevents accidental no-op calls.
///
/// # Example
///
/// Request (stage specific files):
/// ```json
/// {
///   "paths": ["src/lib.rs", "src/main.rs"]
/// }
/// ```
///
/// Request (stage all changes):
/// ```json
/// {
///   "all": true
/// }
/// ```
///
/// Request (stage only modified/deleted tracked files):
/// ```json
/// {
///   "update": true
/// }
/// ```
///
/// Response (success):
/// ```json
/// {
///   "content": [{"type": "text", "text": "ok"}],
///   "isError": false,
///   "exit_code": 0
/// }
/// ```
pub async fn handle_git_add(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitAddRequest {
        #[serde(default)]
        paths: Option<Vec<String>>,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        all: Option<bool>,
        #[serde(default)]
        update: Option<bool>,
    }

    let req = match RpcResponse::parse::<GitAddRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    let use_all = req.all.unwrap_or(false);
    let use_update = req.update.unwrap_or(false);
    let paths = req.paths.unwrap_or_default();

    if !use_all && !use_update && paths.is_empty() {
        return RpcResponse::err(id, "paths required unless 'all' or 'update' is true");
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let mut cmd_args: Vec<String> = vec!["add".into()];

    if use_all {
        cmd_args.push("-A".into());
    } else if use_update {
        cmd_args.push("-u".into());
    }

    if !paths.is_empty() {
        cmd_args.push("--".into());
        for p in &paths {
            if !p.trim().is_empty() {
                cmd_args.push(p.clone());
            }
        }
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        "ok".to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, text, None);

    RpcResponse::ok(id, payload)
}

/// Handle the `GitCommit` MCP tool request.
///
/// Creates a Git commit using the [Conventional Commits](https://www.conventionalcommits.org/)
/// format. The commit message is automatically formatted as `type(scope): message`
/// or `type: message` if no scope is provided.
///
/// # Parameters
///
/// | Name | Type | Default | Required | Description |
/// |------|------|---------|----------|-------------|
/// | `type` | `string` | - | **Yes** | Commit type (feat, fix, docs, etc.) |
/// | `message` | `string` | - | **Yes** | Commit description |
/// | `scope` | `string` | - | No | Optional scope/area of change |
/// | `working_dir` | `string` | CWD | No | Directory containing the Git repository |
/// | `timeout_ms` | `integer` | 30000 | No | Maximum execution time (100-300000ms) |
///
/// # Conventional Commit Types
///
/// Common types (not enforced, but recommended):
/// - `feat`: New feature
/// - `fix`: Bug fix
/// - `docs`: Documentation only
/// - `style`: Code style (formatting, semicolons, etc.)
/// - `refactor`: Code change that neither fixes a bug nor adds a feature
/// - `perf`: Performance improvement
/// - `test`: Adding or correcting tests
/// - `chore`: Maintenance tasks, dependency updates
/// - `ci`: CI/CD configuration changes
/// - `build`: Build system or external dependency changes
///
/// # Message Format
///
/// The final commit message is constructed as:
/// - With scope: `type(scope): message`
/// - Without scope: `type: message`
///
/// All components are trimmed of leading/trailing whitespace.
///
/// # Validation
///
/// Returns an error if:
/// - `type` is empty or whitespace-only
/// - `message` is empty or whitespace-only
///
/// # Response Fields
///
/// In addition to standard fields, includes:
/// - `commit_message`: The formatted conventional commit message
/// - `commit_hash`: Extracted commit SHA (if parseable from Git output)
///
/// # Commit Hash Extraction
///
/// The handler attempts to extract the commit hash from Git's output, which
/// typically looks like `[main abc1234] commit message`. The hash is extracted
/// by finding a token of at least 7 hex characters.
///
/// # Example
///
/// Request:
/// ```json
/// {
///   "type": "feat",
///   "scope": "auth",
///   "message": "add OAuth2 login support"
/// }
/// ```
///
/// Response:
/// ```json
/// {
///   "content": [{"type": "text", "text": "[main abc1234] feat(auth): add OAuth2 login support"}],
///   "isError": false,
///   "commit_message": "feat(auth): add OAuth2 login support",
///   "commit_hash": "abc1234",
///   "exit_code": 0
/// }
/// ```
///
/// Request (without scope):
/// ```json
/// {
///   "type": "fix",
///   "message": "resolve null pointer exception in parser"
/// }
/// ```
pub async fn handle_git_commit(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitCommitRequest {
        #[serde(rename = "type")]
        commit_type: String,
        #[serde(default)]
        scope: Option<String>,
        message: String,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    }

    let req = match RpcResponse::parse::<GitCommitRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.commit_type, "type", id.clone()) {
        return resp;
    }
    if let Err(resp) = validation::validate_non_empty(&req.message, "message", id.clone()) {
        return resp;
    }

    // Build conventional commit message: type(scope): message
    let commit_msg = match &req.scope {
        Some(scope) if !scope.trim().is_empty() => {
            format!(
                "{}({}): {}",
                req.commit_type.trim(),
                scope.trim(),
                req.message.trim()
            )
        }
        _ => format!("{}: {}", req.commit_type.trim(), req.message.trim()),
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let cmd_args: Vec<String> = vec!["commit".into(), "-m".into(), commit_msg.clone()];

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    // Try to extract commit hash from stdout (e.g., "[main abc1234] message")
    let commit_hash = exec
        .stdout
        .split_whitespace()
        .find(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit() || c == ']'))
        .map(|s| s.trim_end_matches(']').to_string());

    let text = if exec.success {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("commit_message", json!(commit_msg));
    extra_fields.insert("commit_hash", json!(commit_hash));

    let payload = build_git_response(&exec, text, Some(extra_fields));

    RpcResponse::ok(id, payload)
}
