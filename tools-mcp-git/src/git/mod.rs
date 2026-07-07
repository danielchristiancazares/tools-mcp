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
//! Stdin-fed patch operations use [`run_git_with_stdin`]. Ordinary git commands
//! run with `stdin(Stdio::null())`; patch commands receive bounded piped stdin
//! and preserve raw stdout/stderr bytes internally for hunk parsing.
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
//! | `git_snapshot` | [`handlers::handle_git_snapshot`] | Read-only status and diff-stat summary |
//! | `GitStatus` | [`handlers::handle_git_status`] | Working tree status with porcelain output |
//! | `GitDiff` | [`handlers::handle_git_diff`] | Show changes in working tree or staging area |
//! | `GitApply` | [`handlers::handle_git_apply`] | Apply supported tracked-file textual patches |
//! | `GitHunks` | [`handlers::handle_git_hunks`] | Enumerate selectable diff hunks with snapshot IDs |
//! | `GitStageHunks` | [`handlers::handle_git_stage_hunks`] | Stage or unstage selected hunk IDs |
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
//! Default limits from [`tools_mcp_core::config`]:
//! - `DEFAULT_GIT_TIMEOUT_MS`: 30,000ms (30 seconds)
//! - `MAX_GIT_TIMEOUT_MS`: 300,000ms (5 minutes)
//! - `DEFAULT_GIT_STDOUT_BYTES`: 200,000 bytes
//! - `DEFAULT_GIT_STDERR_BYTES`: 100,000 bytes
//! - `MAX_OUTPUT_BYTES`: 5,000,000 bytes
//! - `MAX_GIT_STDIN_BYTES`: 5,000,000 bytes

mod handlers;
mod path_policy;
mod types;

#[cfg(feature = "bench-api")]
pub(crate) use handlers::benchmark_parse_diff_manifest;
pub(crate) use handlers::{
    handle_git_add, handle_git_apply, handle_git_blame, handle_git_branch, handle_git_checkout,
    handle_git_commit, handle_git_diff, handle_git_hunks, handle_git_log, handle_git_restore,
    handle_git_show, handle_git_snapshot, handle_git_stage_hunks, handle_git_stash,
    handle_git_status,
};

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::Stdio;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
use tools_mcp_core::config::MAX_GIT_STDIN_BYTES;
use tools_mcp_core::config::{MAX_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES};
use tools_mcp_core::process::read_to_end_limited;
use tracing::{debug, warn};

const GIT_AUTHORITY_ENV_KEYS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_EXEC_PATH",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
    "GIT_QUARANTINE_PATH",
    "GIT_REPLACE_REF_BASE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_DIFF_OPTS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_LITERAL_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitInfraError {
    StdinTooLarge {
        max_bytes: usize,
    },
    SpawnFailed {
        git_bin: String,
        error: String,
    },
    MissingPipe {
        stream_name: &'static str,
    },
    WaitFailed {
        error: String,
    },
    TimeoutKillFailed {
        timeout_ms: u64,
        error: String,
    },
    TimeoutReapFailed {
        timeout_ms: u64,
    },
    #[cfg(test)]
    MissingStatus,
    CaptureTaskFailed {
        stream_name: &'static str,
        error: String,
    },
    CaptureReadFailed {
        stream_name: &'static str,
        error: String,
    },
    CaptureJoinTimedOut {
        stream_name: &'static str,
    },
}

impl std::fmt::Display for GitInfraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StdinTooLarge { max_bytes } => {
                write!(
                    f,
                    "git stdin payload exceeds MAX_GIT_STDIN_BYTES ({max_bytes})"
                )
            }
            Self::SpawnFailed { git_bin, error } => {
                write!(
                    f,
                    "failed to spawn {git_bin}. Is Git installed and on PATH? error: {error}"
                )
            }
            Self::MissingPipe { stream_name } => write!(f, "failed to capture git {stream_name}"),
            Self::WaitFailed { error } => write!(f, "git wait failed: {error}"),
            Self::TimeoutKillFailed { timeout_ms, error } => write!(
                f,
                "git command timed out after {timeout_ms} ms and could not be killed: {error}"
            ),
            Self::TimeoutReapFailed { timeout_ms } => write!(
                f,
                "git command timed out after {timeout_ms} ms and did not terminate"
            ),
            #[cfg(test)]
            Self::MissingStatus => write!(f, "git command completed without a trustworthy status"),
            Self::CaptureTaskFailed { stream_name, error } => {
                write!(f, "git {stream_name} capture task failed: {error}")
            }
            Self::CaptureReadFailed { stream_name, error } => {
                write!(f, "git {stream_name} capture failed: {error}")
            }
            Self::CaptureJoinTimedOut { stream_name } => {
                write!(
                    f,
                    "git {stream_name} capture did not finish after process exit"
                )
            }
        }
    }
}

impl std::error::Error for GitInfraError {}

const GIT_BASE_ARGS: &[&str] = &[
    "--no-pager",
    "-c",
    "color.ui=false",
    "-c",
    "diff.external=",
    "-c",
    "core.fsmonitor=",
];

static GIT_CONFIG_SPOOFING_ENV_KEYS: OnceLock<Vec<OsString>> = OnceLock::new();

#[cfg(test)]
struct GitSpawnObserver {
    required_arg: &'static str,
    pid: Option<u32>,
}

#[cfg(test)]
static GIT_SPAWN_OBSERVER: OnceLock<Mutex<Option<GitSpawnObserver>>> = OnceLock::new();

#[cfg(test)]
fn git_spawn_observer() -> &'static Mutex<Option<GitSpawnObserver>> {
    GIT_SPAWN_OBSERVER.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn observe_next_git_spawn_with_arg(required_arg: &'static str) {
    *git_spawn_observer()
        .lock()
        .expect("git spawn observer lock should not be poisoned") = Some(GitSpawnObserver {
        required_arg,
        pid: None,
    });
}

#[cfg(test)]
fn take_observed_git_spawn_pid() -> Option<u32> {
    git_spawn_observer()
        .lock()
        .expect("git spawn observer lock should not be poisoned")
        .as_mut()
        .and_then(|observer| observer.pid.take())
}

#[cfg(test)]
fn clear_git_spawn_observer() {
    *git_spawn_observer()
        .lock()
        .expect("git spawn observer lock should not be poisoned") = None;
}

#[cfg(test)]
fn record_git_child_spawn(args: &[String], pid: Option<u32>) {
    let mut observer = git_spawn_observer()
        .lock()
        .expect("git spawn observer lock should not be poisoned");
    if let Some(observer) = observer.as_mut()
        && observer.pid.is_none()
        && args.iter().any(|arg| arg == observer.required_arg)
    {
        observer.pid = pid;
    }
}

pub(crate) fn build_git_args(subcommand_args: Vec<String>) -> Vec<String> {
    let mut args: Vec<String> = Vec::with_capacity(GIT_BASE_ARGS.len() + 2 + subcommand_args.len());
    args.extend(GIT_BASE_ARGS.iter().map(|arg| (*arg).to_owned()));
    args.push("-c".to_string());
    args.push(format!("core.attributesFile={}", git_null_device()));
    args.extend(subcommand_args);
    args
}

#[cfg(windows)]
fn git_null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn git_null_device() -> &'static str {
    "/dev/null"
}

pub(crate) fn trim_git_line_end(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

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
/// git --no-pager -c color.ui=false -c diff.external= -c core.fsmonitor= -c core.attributesFile=<null-device> <subcommand_args...>
/// ```
///
/// The `--no-pager` flag prevents interactive pagers, `color.ui=false` ensures
/// no ANSI escape codes, and explicit config/environment overrides disable
/// external helper execution pathways (for example `diff.external` and
/// `core.fsmonitor`) and system/global attribute files for safer machine
/// execution.
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
    run_git_with_stdin(
        working_dir,
        subcommand_args,
        None,
        timeout_ms,
        max_stdout_bytes,
        max_stderr_bytes,
    )
    .await
}

pub(crate) async fn run_git_with_stdin(
    working_dir: Option<String>,
    subcommand_args: Vec<String>,
    stdin: Option<Vec<u8>>,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<types::GitExecResult, anyhow::Error> {
    if stdin
        .as_ref()
        .is_some_and(|stdin| stdin.len() > MAX_GIT_STDIN_BYTES)
    {
        return Err(GitInfraError::StdinTooLarge {
            max_bytes: MAX_GIT_STDIN_BYTES,
        }
        .into());
    }

    let resolved_working_dir =
        path_policy::resolve_working_dir(working_dir.as_deref()).map_err(anyhow::Error::msg)?;
    let effective_working_dir = resolved_working_dir
        .as_ref()
        .map(|path| path.display().to_string());
    let command_working_dir = match &resolved_working_dir {
        Some(path) => path.clone(),
        None => path_policy::authority_root_path().map_err(anyhow::Error::msg)?,
    };

    let timeout_ms = timeout_ms.clamp(100, MAX_GIT_TIMEOUT_MS);
    let max_stdout_bytes = max_stdout_bytes.clamp(1, MAX_OUTPUT_BYTES);
    let max_stderr_bytes = max_stderr_bytes.clamp(1, MAX_OUTPUT_BYTES);

    let git_bin = git_bin();

    // Force deterministic, non-ANSI output and disable config-driven external helpers.
    let args = build_git_args(subcommand_args);

    debug!(
        git_bin = %git_bin,
        args = ?args,
        working_dir = ?effective_working_dir,
        command_working_dir = %command_working_dir.display(),
        timeout_ms,
        "running git command"
    );

    let mut cmd = Command::new(git_bin);
    cmd.args(&args);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    remove_git_authority_env(&mut cmd);
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
    cmd.env("GIT_ATTR_NOSYSTEM", "1");
    cmd.env("GIT_NO_LAZY_FETCH", "1");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd.env("GIT_NO_REPLACE_OBJECTS", "1");

    cmd.current_dir(&command_working_dir);

    let mut child = cmd.spawn().map_err(|e| {
        warn!(git_bin = %git_bin, error = %e, "failed to spawn git");
        GitInfraError::SpawnFailed {
            git_bin: git_bin.to_string(),
            error: e.to_string(),
        }
    })?;
    #[cfg(test)]
    record_git_child_spawn(&args, child.id());

    let requested_stdin_bytes = stdin.as_ref().map(Vec::len);
    let stdin_task = if let Some(stdin_bytes) = stdin {
        let child_stdin = child.stdin.take().ok_or_else(|| {
            child.start_kill().ok();
            GitInfraError::MissingPipe {
                stream_name: "stdin",
            }
        })?;
        Some(tokio::spawn(write_git_stdin(child_stdin, stdin_bytes)))
    } else {
        None
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| setup_capture_error(&mut child, "stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| setup_capture_error(&mut child, "stderr"))?;

    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    let mut timed_out = false;
    let status =
        if let Ok(res) = time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
            res.map_err(|err| GitInfraError::WaitFailed {
                error: err.to_string(),
            })?
        } else {
            timed_out = true;
            warn!(timeout_ms, args = ?args, "git command timed out, killing process");
            child
                .kill()
                .await
                .map_err(|err| GitInfraError::TimeoutKillFailed {
                    timeout_ms,
                    error: err.to_string(),
                })?;
            match time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Ok(res) => res.map_err(|err| GitInfraError::WaitFailed {
                    error: err.to_string(),
                })?,
                Err(_) => return Err(GitInfraError::TimeoutReapFailed { timeout_ms }.into()),
            }
        };

    let exit_code = status.code();

    let (stdout_bytes, truncated_stdout) =
        collect_capture_with_grace(stdout_task, "stdout").await?;
    let (stderr_bytes, truncated_stderr) =
        collect_capture_with_grace(stderr_task, "stderr").await?;
    let stdin = match (requested_stdin_bytes, stdin_task) {
        (Some(requested), Some(task)) => {
            let report = collect_stdin_with_grace(task).await;
            types::GitStdinSummary::from_report(requested, report)
        }
        _ => types::GitStdinSummary::none(),
    };

    if truncated_stdout {
        warn!(max_stdout_bytes, args = ?args, "git stdout truncated");
    }
    if truncated_stderr {
        warn!(max_stderr_bytes, args = ?args, "git stderr truncated");
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if !status.success() && !timed_out && requested_stdin_bytes.is_some() {
        debug!(
            exit_code = ?exit_code,
            args = ?args,
            stderr_head_redacted = true,
            "git command exited non-zero"
        );
    } else if !status.success() && !timed_out {
        let stderr_head: String = stderr
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        debug!(
            exit_code = ?exit_code,
            args = ?args,
            stderr_head = %stderr_head,
            "git command exited non-zero"
        );
    }

    Ok(types::GitExecResult {
        git_bin: git_bin.to_owned(),
        args,
        working_dir: effective_working_dir,
        exit_code,
        success: status.success() && !timed_out,
        stdout,
        stderr,
        stdout_bytes,
        stderr_bytes,
        truncated_stdout,
        truncated_stderr,
        timed_out,
        stdin,
    })
}

fn setup_capture_error(
    child: &mut tokio::process::Child,
    stream_name: &'static str,
) -> anyhow::Error {
    let _ = child.start_kill();
    GitInfraError::MissingPipe { stream_name }.into()
}

async fn write_git_stdin(
    stdin: tokio::process::ChildStdin,
    bytes: Vec<u8>,
) -> types::StdinWriteReport {
    write_git_stdin_to(stdin, bytes).await
}

async fn write_git_stdin_to<W>(mut stdin: W, bytes: Vec<u8>) -> types::StdinWriteReport
where
    W: AsyncWrite + Unpin,
{
    let mut written_bytes = 0usize;
    let mut offset = 0usize;

    while offset < bytes.len() {
        let end = (offset + 16 * 1024).min(bytes.len());
        match stdin.write(&bytes[offset..end]).await {
            Ok(0) => {
                return types::StdinWriteReport {
                    written_bytes,
                    fully_delivered: false,
                    write_error: Some("git stdin write returned zero bytes".to_string()),
                    broken_pipe: false,
                };
            }
            Ok(n) => {
                written_bytes += n;
                offset += n;
            }
            Err(err) => {
                let broken_pipe = err.kind() == io::ErrorKind::BrokenPipe;
                return types::StdinWriteReport {
                    written_bytes,
                    fully_delivered: false,
                    write_error: Some(err.to_string()),
                    broken_pipe,
                };
            }
        }
    }

    match stdin.shutdown().await {
        Ok(()) => types::StdinWriteReport {
            written_bytes,
            fully_delivered: true,
            write_error: None,
            broken_pipe: false,
        },
        Err(err) => {
            let broken_pipe = err.kind() == io::ErrorKind::BrokenPipe;
            types::StdinWriteReport {
                written_bytes,
                fully_delivered: false,
                write_error: Some(err.to_string()),
                broken_pipe,
            }
        }
    }
}

async fn collect_capture_with_grace(
    mut task: JoinHandle<io::Result<(Vec<u8>, bool)>>,
    stream_name: &'static str,
) -> Result<(Vec<u8>, bool), GitInfraError> {
    tokio::select! {
        joined = &mut task => joined
            .map_err(|err| GitInfraError::CaptureTaskFailed {
                stream_name,
                error: err.to_string(),
            })?
            .map_err(|err| GitInfraError::CaptureReadFailed {
                stream_name,
                error: err.to_string(),
            }),
        () = time::sleep(Duration::from_millis(2_000)) => {
            task.abort();
            Err(GitInfraError::CaptureJoinTimedOut { stream_name })
        }
    }
}

async fn collect_stdin_with_grace(
    mut task: JoinHandle<types::StdinWriteReport>,
) -> types::StdinWriteReport {
    tokio::select! {
        joined = &mut task => match joined {
            Ok(report) => report,
            Err(err) => types::StdinWriteReport {
                written_bytes: 0,
                fully_delivered: false,
                write_error: Some(format!("git stdin writer task failed: {err}")),
                broken_pipe: false,
            },
        },
        () = time::sleep(Duration::from_millis(2_000)) => {
            task.abort();
            types::StdinWriteReport {
                written_bytes: 0,
                fully_delivered: false,
                write_error: Some("git stdin writer did not finish after process exit".to_string()),
                broken_pipe: false,
            }
        }
    }
}

fn git_bin() -> &'static str {
    if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
    }
}

fn remove_git_authority_env(cmd: &mut Command) {
    for key in GIT_AUTHORITY_ENV_KEYS {
        cmd.env_remove(key);
    }

    for key in git_env_keys_to_scrub() {
        cmd.env_remove(key);
    }

    for key in git_config_spoofing_env_keys() {
        cmd.env_remove(key);
    }
}

fn git_env_keys_to_scrub() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| is_denied_git_env_key(key))
        .collect()
}

fn git_config_spoofing_env_keys() -> &'static [OsString] {
    GIT_CONFIG_SPOOFING_ENV_KEYS
        .get_or_init(|| {
            std::env::vars_os()
                .map(|(key, _)| key)
                .filter(|key| is_git_config_spoofing_env_key(key))
                .collect()
        })
        .as_slice()
}

fn is_git_config_spoofing_env_key(key: &OsStr) -> bool {
    is_git_env_name_with_prefix(key, &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"])
}

fn is_denied_git_env_key(key: &OsStr) -> bool {
    if let Some(key_text) = key.to_str() {
        for denied in GIT_AUTHORITY_ENV_KEYS {
            if git_env_name_eq(key_text, denied) {
                return true;
            }
        }
    }

    is_git_env_name_with_prefix(
        key,
        &[
            "GIT_CONFIG_KEY_",
            "GIT_CONFIG_VALUE_",
            "GIT_TRACE",
            "GIT_TRACE2",
        ],
    )
}

fn is_git_env_name_with_prefix(key: &OsStr, prefixes: &[&str]) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    prefixes
        .iter()
        .any(|prefix| git_env_name_starts_with(key, prefix))
}

fn git_env_name_eq(key: &str, expected: &str) -> bool {
    if cfg!(windows) {
        key.eq_ignore_ascii_case(expected)
    } else {
        key == expected
    }
}

fn git_env_name_starts_with(key: &str, expected_prefix: &str) -> bool {
    if cfg!(windows) {
        key.to_ascii_uppercase()
            .starts_with(&expected_prefix.to_ascii_uppercase())
    } else {
        key.starts_with(expected_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GIT_AUTHORITY_ENV_KEYS, GitInfraError, build_git_args, clear_git_spawn_observer,
        collect_capture_with_grace, collect_stdin_with_grace, git_null_device,
        is_denied_git_env_key, is_git_config_spoofing_env_key, observe_next_git_spawn_with_arg,
        run_git, run_git_with_stdin, take_observed_git_spawn_pid, trim_git_line_end,
        write_git_stdin_to,
    };
    use std::ffi::OsStr;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::process::{Command, Stdio};
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};
    use tools_mcp_core::config::{
        DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
        MAX_GIT_STDIN_BYTES,
    };

    fn git_bin() -> &'static str {
        if cfg!(target_os = "windows") {
            "git.exe"
        } else {
            "git"
        }
    }

    fn git_available() -> bool {
        Command::new(git_bin())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn process_is_alive(pid: u32) -> bool {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("kill -0 {pid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(not(any(unix, windows)))]
    fn process_is_alive(_pid: u32) -> bool {
        false
    }

    fn wait_for_process_exit_without_runtime_yield(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !process_is_alive(pid)
    }

    struct FailingWrite {
        kind: io::ErrorKind,
    }

    impl tokio::io::AsyncWrite for FailingWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::new(self.kind, "scripted write failure")))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct ZeroWrite;

    impl tokio::io::AsyncWrite for ZeroWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(0))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct ShutdownFailingWrite {
        kind: io::ErrorKind,
        written: Vec<u8>,
    }

    impl tokio::io::AsyncWrite for ShutdownFailingWrite {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(self.kind, "scripted shutdown failure")))
        }
    }

    #[derive(Debug, Clone)]
    enum ScriptedWait {
        Exit { success: bool },
        Timeout { kill_fails: bool, reap_fails: bool },
        WaitError,
        MissingStatus,
    }

    #[derive(Debug, Clone)]
    enum ScriptedCapture {
        Complete,
        TaskFailed,
        ReadFailed,
        JoinTimedOut,
    }

    #[derive(Debug, Clone)]
    struct ScriptedLifecycle {
        setup_error: Option<GitInfraError>,
        stdin_pipe_present: bool,
        stdout_pipe_present: bool,
        stderr_pipe_present: bool,
        wait: ScriptedWait,
        stdout: ScriptedCapture,
        stderr: ScriptedCapture,
        requested_stdin_bytes: Option<usize>,
        stdin_report: Option<super::types::StdinWriteReport>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ScriptedTerminal {
        success: bool,
        timed_out: bool,
        stdin: super::types::GitStdinSummary,
    }

    impl ScriptedLifecycle {
        fn success() -> Self {
            Self {
                setup_error: None,
                stdin_pipe_present: true,
                stdout_pipe_present: true,
                stderr_pipe_present: true,
                wait: ScriptedWait::Exit { success: true },
                stdout: ScriptedCapture::Complete,
                stderr: ScriptedCapture::Complete,
                requested_stdin_bytes: None,
                stdin_report: None,
            }
        }

        fn nonzero() -> Self {
            Self {
                wait: ScriptedWait::Exit { success: false },
                ..Self::success()
            }
        }

        fn timeout() -> Self {
            Self {
                wait: ScriptedWait::Timeout {
                    kill_fails: false,
                    reap_fails: false,
                },
                ..Self::success()
            }
        }
    }

    fn scripted_lifecycle_result(
        script: ScriptedLifecycle,
    ) -> Result<ScriptedTerminal, GitInfraError> {
        if let Some(error) = script.setup_error {
            return Err(error);
        }

        if script.requested_stdin_bytes.is_some() && !script.stdin_pipe_present {
            return Err(GitInfraError::MissingPipe {
                stream_name: "stdin",
            });
        }
        if !script.stdout_pipe_present {
            return Err(GitInfraError::MissingPipe {
                stream_name: "stdout",
            });
        }
        if !script.stderr_pipe_present {
            return Err(GitInfraError::MissingPipe {
                stream_name: "stderr",
            });
        }

        let (success, timed_out) = match script.wait {
            ScriptedWait::Exit { success } => (success, false),
            ScriptedWait::Timeout {
                kill_fails,
                reap_fails,
            } => {
                if kill_fails {
                    return Err(GitInfraError::TimeoutKillFailed {
                        timeout_ms: 100,
                        error: "scripted kill failure".to_string(),
                    });
                }
                if reap_fails {
                    return Err(GitInfraError::TimeoutReapFailed { timeout_ms: 100 });
                }
                (false, true)
            }
            ScriptedWait::WaitError => {
                return Err(GitInfraError::WaitFailed {
                    error: "scripted wait failure".to_string(),
                });
            }
            ScriptedWait::MissingStatus => return Err(GitInfraError::MissingStatus),
        };

        scripted_capture_result("stdout", script.stdout)?;
        scripted_capture_result("stderr", script.stderr)?;

        let stdin = match (script.requested_stdin_bytes, script.stdin_report) {
            (Some(requested), Some(report)) => {
                super::types::GitStdinSummary::from_report(requested, report)
            }
            _ => super::types::GitStdinSummary::none(),
        };

        Ok(ScriptedTerminal {
            success: success && !timed_out,
            timed_out,
            stdin,
        })
    }

    fn scripted_capture_result(
        stream_name: &'static str,
        capture: ScriptedCapture,
    ) -> Result<(), GitInfraError> {
        match capture {
            ScriptedCapture::Complete => Ok(()),
            ScriptedCapture::TaskFailed => Err(GitInfraError::CaptureTaskFailed {
                stream_name,
                error: "scripted join failure".to_string(),
            }),
            ScriptedCapture::ReadFailed => Err(GitInfraError::CaptureReadFailed {
                stream_name,
                error: "scripted read failure".to_string(),
            }),
            ScriptedCapture::JoinTimedOut => {
                Err(GitInfraError::CaptureJoinTimedOut { stream_name })
            }
        }
    }

    #[test]
    fn git_authority_env_denylist_includes_repository_and_helper_controls() {
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_EXEC_PATH",
            "GIT_IMPLICIT_WORK_TREE",
            "GIT_PREFIX",
            "GIT_SHALLOW_FILE",
            "GIT_GRAFT_FILE",
            "GIT_QUARANTINE_PATH",
            "GIT_REPLACE_REF_BASE",
            "GIT_NO_REPLACE_OBJECTS",
            "GIT_DIFF_OPTS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_LITERAL_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_ASKPASS",
            "SSH_ASKPASS",
        ] {
            assert!(GIT_AUTHORITY_ENV_KEYS.contains(&key), "missing {key}");
        }
    }

    #[test]
    fn git_config_spoofing_env_key_matches_indexed_key_value_patterns() {
        assert!(is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_KEY_0"
        )));
        assert!(is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_VALUE_0"
        )));
        assert!(is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_KEY_MALICIOUS"
        )));
        assert!(is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_VALUE_MALICIOUS"
        )));
        assert!(!is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_COUNT"
        )));
        assert!(!is_git_config_spoofing_env_key(OsStr::new(
            "GIT_CONFIG_GLOBAL"
        )));
        assert!(!is_git_config_spoofing_env_key(OsStr::new("PATH")));
    }

    #[test]
    fn git_env_scrub_matches_trace_and_mixed_case_git_env_on_windows() {
        assert!(is_denied_git_env_key(OsStr::new("GIT_TRACE")));
        assert!(is_denied_git_env_key(OsStr::new("GIT_TRACE2_EVENT")));
        assert!(is_denied_git_env_key(OsStr::new("GIT_CONFIG_KEY_0")));
        assert!(is_denied_git_env_key(OsStr::new("GIT_DIFF_OPTS")));
        assert!(is_denied_git_env_key(OsStr::new("GIT_NO_REPLACE_OBJECTS")));

        if cfg!(windows) {
            assert!(is_denied_git_env_key(OsStr::new("git_trace2_event")));
            assert!(is_denied_git_env_key(OsStr::new("git_config_value_0")));
            assert!(is_denied_git_env_key(OsStr::new("git_no_replace_objects")));
        } else {
            assert!(!is_denied_git_env_key(OsStr::new("git_trace2_event")));
        }
    }

    #[test]
    fn build_git_args_preserves_standard_safety_prefix() {
        let attributes_file_arg = format!("core.attributesFile={}", git_null_device());
        assert_eq!(
            build_git_args(vec!["status".to_string(), "--porcelain=1".to_string()]),
            vec![
                "--no-pager".to_string(),
                "-c".to_string(),
                "color.ui=false".to_string(),
                "-c".to_string(),
                "diff.external=".to_string(),
                "-c".to_string(),
                "core.fsmonitor=".to_string(),
                "-c".to_string(),
                attributes_file_arg,
                "status".to_string(),
                "--porcelain=1".to_string(),
            ]
        );
    }

    #[test]
    fn trim_git_line_end_removes_only_newline_suffixes() {
        assert_eq!(trim_git_line_end("message\r\n\n"), "message");
        assert_eq!(trim_git_line_end(" message "), " message ");
    }

    #[test]
    fn scripted_lifecycle_reports_spawn_and_setup_failures_before_child_result() {
        let spawn_error = GitInfraError::SpawnFailed {
            git_bin: "git.exe".to_string(),
            error: "scripted spawn failure".to_string(),
        };
        let err = scripted_lifecycle_result(ScriptedLifecycle {
            setup_error: Some(spawn_error.clone()),
            wait: ScriptedWait::Exit { success: true },
            stdout: ScriptedCapture::TaskFailed,
            stderr: ScriptedCapture::ReadFailed,
            requested_stdin_bytes: Some(3),
            stdin_report: Some(super::types::StdinWriteReport {
                written_bytes: 0,
                fully_delivered: false,
                write_error: Some("scripted writer failure".to_string()),
                broken_pipe: false,
            }),
            ..ScriptedLifecycle::success()
        })
        .expect_err("spawn failure should win before child/capture/stdin handling");
        assert_eq!(err, spawn_error);

        let missing_pipe = GitInfraError::MissingPipe {
            stream_name: "stdout",
        };
        let err = scripted_lifecycle_result(ScriptedLifecycle {
            setup_error: Some(missing_pipe.clone()),
            ..ScriptedLifecycle::success()
        })
        .expect_err("missing post-spawn pipe should be infrastructure");
        assert_eq!(err, missing_pipe);
    }

    #[test]
    fn scripted_lifecycle_reports_missing_pipe_setup_failures_before_child_result() {
        let err = scripted_lifecycle_result(ScriptedLifecycle {
            stdin_pipe_present: false,
            stdout: ScriptedCapture::TaskFailed,
            stderr: ScriptedCapture::ReadFailed,
            requested_stdin_bytes: Some(3),
            stdin_report: Some(super::types::StdinWriteReport {
                written_bytes: 3,
                fully_delivered: true,
                write_error: None,
                broken_pipe: false,
            }),
            ..ScriptedLifecycle::success()
        })
        .expect_err("missing requested stdin pipe should be setup infrastructure");
        assert_eq!(
            err,
            GitInfraError::MissingPipe {
                stream_name: "stdin"
            }
        );

        let err = scripted_lifecycle_result(ScriptedLifecycle {
            stdout_pipe_present: false,
            stdout: ScriptedCapture::TaskFailed,
            wait: ScriptedWait::Timeout {
                kill_fails: false,
                reap_fails: false,
            },
            ..ScriptedLifecycle::success()
        })
        .expect_err("missing stdout pipe should be setup infrastructure");
        assert_eq!(
            err,
            GitInfraError::MissingPipe {
                stream_name: "stdout"
            }
        );

        let err = scripted_lifecycle_result(ScriptedLifecycle {
            stderr_pipe_present: false,
            stderr: ScriptedCapture::TaskFailed,
            wait: ScriptedWait::Exit { success: false },
            ..ScriptedLifecycle::success()
        })
        .expect_err("missing stderr pipe should be setup infrastructure");
        assert_eq!(
            err,
            GitInfraError::MissingPipe {
                stream_name: "stderr"
            }
        );

        let err = scripted_lifecycle_result(ScriptedLifecycle {
            stdin_pipe_present: false,
            stdout_pipe_present: false,
            stderr_pipe_present: false,
            requested_stdin_bytes: Some(1),
            ..ScriptedLifecycle::success()
        })
        .expect_err("requested stdin setup should be checked before output pipes");
        assert_eq!(
            err,
            GitInfraError::MissingPipe {
                stream_name: "stdin"
            }
        );

        let err = scripted_lifecycle_result(ScriptedLifecycle {
            stdout_pipe_present: false,
            stderr_pipe_present: false,
            ..ScriptedLifecycle::success()
        })
        .expect_err("stdout setup should be checked before stderr setup");
        assert_eq!(
            err,
            GitInfraError::MissingPipe {
                stream_name: "stdout"
            }
        );
    }

    #[test]
    fn scripted_lifecycle_reports_wait_timeout_cleanup_and_missing_status_as_infra() {
        for (wait, expected) in [
            (
                ScriptedWait::WaitError,
                GitInfraError::WaitFailed {
                    error: "scripted wait failure".to_string(),
                },
            ),
            (
                ScriptedWait::Timeout {
                    kill_fails: true,
                    reap_fails: false,
                },
                GitInfraError::TimeoutKillFailed {
                    timeout_ms: 100,
                    error: "scripted kill failure".to_string(),
                },
            ),
            (
                ScriptedWait::Timeout {
                    kill_fails: false,
                    reap_fails: true,
                },
                GitInfraError::TimeoutReapFailed { timeout_ms: 100 },
            ),
            (ScriptedWait::MissingStatus, GitInfraError::MissingStatus),
        ] {
            let err = scripted_lifecycle_result(ScriptedLifecycle {
                wait,
                ..ScriptedLifecycle::success()
            })
            .expect_err("scripted lifecycle should fail as infrastructure");
            assert_eq!(err, expected);
        }
    }

    #[test]
    fn scripted_lifecycle_capture_failures_override_child_success_nonzero_and_timeout() {
        for wait in [
            ScriptedWait::Exit { success: true },
            ScriptedWait::Exit { success: false },
            ScriptedWait::Timeout {
                kill_fails: false,
                reap_fails: false,
            },
        ] {
            for (stdout, stderr, expected) in [
                (
                    ScriptedCapture::TaskFailed,
                    ScriptedCapture::Complete,
                    GitInfraError::CaptureTaskFailed {
                        stream_name: "stdout",
                        error: "scripted join failure".to_string(),
                    },
                ),
                (
                    ScriptedCapture::ReadFailed,
                    ScriptedCapture::Complete,
                    GitInfraError::CaptureReadFailed {
                        stream_name: "stdout",
                        error: "scripted read failure".to_string(),
                    },
                ),
                (
                    ScriptedCapture::Complete,
                    ScriptedCapture::JoinTimedOut,
                    GitInfraError::CaptureJoinTimedOut {
                        stream_name: "stderr",
                    },
                ),
            ] {
                let err = scripted_lifecycle_result(ScriptedLifecycle {
                    wait: wait.clone(),
                    stdout,
                    stderr,
                    ..ScriptedLifecycle::success()
                })
                .expect_err("capture failures should override every child terminal result");
                assert_eq!(err, expected);
            }
        }
    }

    #[test]
    fn scripted_lifecycle_writer_failures_remain_diagnostic_with_trustworthy_child_result() {
        for mut script in [
            ScriptedLifecycle::success(),
            ScriptedLifecycle::nonzero(),
            ScriptedLifecycle::timeout(),
        ] {
            let expect_timeout = matches!(&script.wait, ScriptedWait::Timeout { .. });
            let expect_success = matches!(&script.wait, ScriptedWait::Exit { success: true });
            let expect_broken_pipe = matches!(&script.wait, ScriptedWait::Exit { success: false });
            script.requested_stdin_bytes = Some(5);
            script.stdin_report = Some(super::types::StdinWriteReport {
                written_bytes: 2,
                fully_delivered: false,
                write_error: Some("scripted writer failure".to_string()),
                broken_pipe: expect_broken_pipe,
            });

            let terminal = scripted_lifecycle_result(script.clone())
                .expect("writer failure should not mask trustworthy child result");

            assert_eq!(terminal.timed_out, expect_timeout);
            assert_eq!(terminal.success, expect_success);
            assert_eq!(terminal.stdin.requested_bytes, Some(5));
            assert_eq!(terminal.stdin.written_bytes, Some(2));
            assert_eq!(terminal.stdin.fully_delivered, Some(false));
            assert_eq!(
                terminal.stdin.write_error.as_deref(),
                Some("scripted writer failure")
            );
            assert_eq!(terminal.stdin.broken_pipe, expect_broken_pipe);
        }
    }

    #[tokio::test]
    async fn run_git_with_stdin_delivers_and_closes_stdin() {
        if !git_available() {
            eprintln!("Skipping run_git_with_stdin test: git not found on PATH");
            return;
        }

        let exec = run_git_with_stdin(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            Some(b"abc".to_vec()),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(
            trim_git_line_end(&exec.stdout),
            "f2ba8f84ab5c1bce84a7b441cb1959cfc7093b7f"
        );
        assert_eq!(exec.stdout_bytes, exec.stdout.as_bytes());
        assert_eq!(exec.stdin.requested_bytes, Some(3));
        assert_eq!(exec.stdin.written_bytes, Some(3));
        assert_eq!(exec.stdin.fully_delivered, Some(true));
        assert_eq!(exec.stdin.write_error, None);
    }

    #[tokio::test]
    async fn run_git_with_empty_stdin_delivers_eof() {
        if !git_available() {
            eprintln!("Skipping run_git empty-stdin test: git not found on PATH");
            return;
        }

        let exec = run_git_with_stdin(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            Some(Vec::new()),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(
            trim_git_line_end(&exec.stdout),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(exec.stdin.requested_bytes, Some(0));
        assert_eq!(exec.stdin.written_bytes, Some(0));
        assert_eq!(exec.stdin.fully_delivered, Some(true));
        assert_eq!(exec.stdin.write_error, None);
    }

    #[tokio::test]
    async fn run_git_with_binary_stdin_preserves_nul_bytes() {
        if !git_available() {
            eprintln!("Skipping run_git binary-stdin test: git not found on PATH");
            return;
        }

        let exec = run_git_with_stdin(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            Some(b"a\0b".to_vec()),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(
            trim_git_line_end(&exec.stdout),
            "20b5be91886d0b6f26dc98a225c0dac05fe2c86e"
        );
        assert_eq!(exec.stdin.requested_bytes, Some(3));
        assert_eq!(exec.stdin.written_bytes, Some(3));
        assert_eq!(exec.stdin.fully_delivered, Some(true));
        assert_eq!(exec.stdin.write_error, None);
    }

    #[tokio::test]
    async fn run_git_with_stdin_accepts_exact_cap_boundary() {
        if !git_available() {
            eprintln!("Skipping run_git exact-cap stdin test: git not found on PATH");
            return;
        }

        let exec = run_git_with_stdin(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            Some(vec![b'x'; MAX_GIT_STDIN_BYTES]),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should accept the exact stdin cap");

        assert!(exec.success, "{exec:?}");
        assert_eq!(trim_git_line_end(&exec.stdout).len(), 40);
        assert_eq!(exec.stdin.requested_bytes, Some(MAX_GIT_STDIN_BYTES));
        assert_eq!(exec.stdin.written_bytes, Some(MAX_GIT_STDIN_BYTES));
        assert_eq!(exec.stdin.fully_delivered, Some(true));
        assert_eq!(exec.stdin.write_error, None);
    }

    #[tokio::test]
    async fn write_git_stdin_reports_non_broken_pipe_write_error() {
        let report = write_git_stdin_to(
            FailingWrite {
                kind: io::ErrorKind::Other,
            },
            b"abc".to_vec(),
        )
        .await;

        assert_eq!(report.written_bytes, 0);
        assert!(!report.fully_delivered);
        assert!(!report.broken_pipe);
        assert!(
            report
                .write_error
                .as_deref()
                .is_some_and(|error| error.contains("scripted write failure")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn write_git_stdin_reports_broken_pipe_write_error() {
        let report = write_git_stdin_to(
            FailingWrite {
                kind: io::ErrorKind::BrokenPipe,
            },
            b"abc".to_vec(),
        )
        .await;

        assert_eq!(report.written_bytes, 0);
        assert!(!report.fully_delivered);
        assert!(report.broken_pipe);
        assert!(
            report
                .write_error
                .as_deref()
                .is_some_and(|error| error.contains("scripted write failure")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn write_git_stdin_reports_zero_write_as_incomplete_delivery() {
        let report = write_git_stdin_to(ZeroWrite, b"abc".to_vec()).await;

        assert_eq!(report.written_bytes, 0);
        assert!(!report.fully_delivered);
        assert!(!report.broken_pipe);
        assert_eq!(
            report.write_error.as_deref(),
            Some("git stdin write returned zero bytes")
        );
    }

    #[tokio::test]
    async fn write_git_stdin_reports_shutdown_error_after_full_write() {
        let report = write_git_stdin_to(
            ShutdownFailingWrite {
                kind: io::ErrorKind::Other,
                written: Vec::new(),
            },
            b"abc".to_vec(),
        )
        .await;

        assert_eq!(report.written_bytes, 3);
        assert!(!report.fully_delivered);
        assert!(!report.broken_pipe);
        assert!(
            report
                .write_error
                .as_deref()
                .is_some_and(|error| error.contains("scripted shutdown failure")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn write_git_stdin_reports_broken_pipe_shutdown_error() {
        let report = write_git_stdin_to(
            ShutdownFailingWrite {
                kind: io::ErrorKind::BrokenPipe,
                written: Vec::new(),
            },
            b"abc".to_vec(),
        )
        .await;

        assert_eq!(report.written_bytes, 3);
        assert!(!report.fully_delivered);
        assert!(report.broken_pipe);
        assert!(
            report
                .write_error
                .as_deref()
                .is_some_and(|error| error.contains("scripted shutdown failure")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn run_git_rejects_stdin_above_cap_before_spawn() {
        let err = run_git_with_stdin(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            Some(vec![b'x'; MAX_GIT_STDIN_BYTES + 1]),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect_err("oversized stdin should be rejected");

        assert!(err.to_string().contains("MAX_GIT_STDIN_BYTES"));
    }

    #[tokio::test]
    async fn run_git_without_stdin_observes_eof() {
        if !git_available() {
            eprintln!("Skipping run_git stdin-null test: git not found on PATH");
            return;
        }

        let exec = run_git(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(
            trim_git_line_end(&exec.stdout),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(exec.stdin.requested_bytes, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_git_with_stdin_drop_kills_direct_child_before_writer_can_close_stdin() {
        if !git_available() {
            eprintln!("Skipping run_git drop-cleanup test: git not found on PATH");
            return;
        }

        observe_next_git_spawn_with_arg("--literally");
        let mut future = Box::pin(run_git_with_stdin(
            None,
            vec![
                "hash-object".to_string(),
                "--stdin".to_string(),
                "--literally".to_string(),
            ],
            Some(vec![b'x'; MAX_GIT_STDIN_BYTES]),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        ));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                clear_git_spawn_observer();
                panic!("git future completed before drop proof could run: {result:?}");
            }
            Poll::Pending => {}
        }

        let pid = take_observed_git_spawn_pid().expect("git child PID should be observed");
        drop(future);
        clear_git_spawn_observer();

        assert!(
            wait_for_process_exit_without_runtime_yield(pid, Duration::from_secs(2)),
            "dropping the run_git_with_stdin future should kill direct child pid {pid} before the detached stdin writer can close stdin normally"
        );
    }

    #[tokio::test]
    async fn collect_stdin_with_grace_reports_writer_join_error() {
        let task: tokio::task::JoinHandle<super::types::StdinWriteReport> =
            tokio::spawn(async { panic!("writer task panic for coverage") });

        let report = collect_stdin_with_grace(task).await;

        assert_eq!(report.written_bytes, 0);
        assert!(!report.fully_delivered);
        assert!(!report.broken_pipe);
        assert!(
            report
                .write_error
                .as_deref()
                .is_some_and(|error| error.contains("git stdin writer task failed")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn collect_stdin_with_grace_reports_writer_timeout() {
        let task: tokio::task::JoinHandle<super::types::StdinWriteReport> = tokio::spawn(async {
            std::future::pending::<()>().await;
            unreachable!("pending writer task should be aborted");
        });

        let report = collect_stdin_with_grace(task).await;

        assert_eq!(report.written_bytes, 0);
        assert!(!report.fully_delivered);
        assert!(!report.broken_pipe);
        assert_eq!(
            report.write_error.as_deref(),
            Some("git stdin writer did not finish after process exit")
        );
    }

    #[tokio::test]
    async fn collect_capture_with_grace_reports_reader_join_error() {
        let task: tokio::task::JoinHandle<io::Result<(Vec<u8>, bool)>> =
            tokio::spawn(async { panic!("capture task panic for coverage") });

        let err = collect_capture_with_grace(task, "stdout")
            .await
            .expect_err("capture task panic should be reported");

        assert!(
            err.to_string().contains("git stdout capture task failed"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn collect_capture_with_grace_reports_reader_read_error() {
        let task: tokio::task::JoinHandle<io::Result<(Vec<u8>, bool)>> =
            tokio::spawn(async { Err(io::Error::other("scripted read failure")) });

        let err = collect_capture_with_grace(task, "stdout")
            .await
            .expect_err("capture read failure should be reported");

        assert!(
            err.to_string().contains("git stdout capture failed"),
            "{err:#}"
        );
        assert!(err.to_string().contains("scripted read failure"), "{err:#}");
    }

    #[tokio::test]
    async fn collect_capture_with_grace_reports_reader_timeout() {
        let task: tokio::task::JoinHandle<io::Result<(Vec<u8>, bool)>> = tokio::spawn(async {
            std::future::pending::<()>().await;
            unreachable!("pending capture task should be aborted");
        });

        let err = collect_capture_with_grace(task, "stderr")
            .await
            .expect_err("pending capture task should time out");

        assert!(
            err.to_string()
                .contains("git stderr capture did not finish after process exit"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn run_git_stdout_exact_cap_is_not_truncated() {
        if !git_available() {
            eprintln!("Skipping run_git stdout exact-cap test: git not found on PATH");
            return;
        }

        let exec = run_git(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            DEFAULT_GIT_TIMEOUT_MS,
            41,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(exec.stdout_bytes.len(), 41);
        assert!(!exec.truncated_stdout);
        assert_eq!(
            trim_git_line_end(&exec.stdout),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[tokio::test]
    async fn run_git_stdout_just_over_cap_is_truncated_prefix() {
        if !git_available() {
            eprintln!("Skipping run_git stdout truncation test: git not found on PATH");
            return;
        }

        let exec = run_git(
            None,
            vec!["hash-object".to_string(), "--stdin".to_string()],
            DEFAULT_GIT_TIMEOUT_MS,
            40,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git hash-object should run");

        assert!(exec.success, "{exec:?}");
        assert_eq!(exec.stdout_bytes.len(), 40);
        assert!(exec.truncated_stdout);
        assert_eq!(exec.stdout, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[tokio::test]
    async fn run_git_stderr_exact_and_just_over_cap_track_truncation() {
        if !git_available() {
            eprintln!("Skipping run_git stderr truncation test: git not found on PATH");
            return;
        }

        let args = vec!["--tools-mcp-invalid-option-for-stderr-test".to_string()];
        let baseline = run_git(
            None,
            args.clone(),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git invalid-option command should produce a normal exec result");
        assert!(!baseline.success, "{baseline:?}");
        assert!(!baseline.truncated_stderr);
        assert!(
            baseline.stderr_bytes.len() > 1,
            "fixture must produce enough stderr for truncation assertions"
        );

        let exact_cap = baseline.stderr_bytes.len();
        let exact = run_git(
            None,
            args.clone(),
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            exact_cap,
        )
        .await
        .expect("git invalid-option exact-cap command should run");

        assert!(!exact.success, "{exact:?}");
        assert_eq!(exact.stderr_bytes, baseline.stderr_bytes);
        assert!(!exact.truncated_stderr);

        let truncated = run_git(
            None,
            args,
            DEFAULT_GIT_TIMEOUT_MS,
            DEFAULT_GIT_STDOUT_BYTES,
            exact_cap - 1,
        )
        .await
        .expect("git invalid-option truncated command should run");

        assert!(!truncated.success, "{truncated:?}");
        assert_eq!(
            truncated.stderr_bytes,
            baseline.stderr_bytes[..exact_cap - 1]
        );
        assert!(truncated.truncated_stderr);
    }
}
