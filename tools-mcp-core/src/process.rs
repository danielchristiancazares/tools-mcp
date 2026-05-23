//! Process execution primitives shared by tool crates.
//!
//! This module intentionally contains only the low-level plumbing: bounded
//! stream capture and a single helper, [`wait_with_limits`], that drives a
//! spawned [`tokio::process::Child`] to completion with a timeout and
//! captures its output within configurable byte limits.
//!
//! Callers own `Command` construction and `spawn()` (so they control the
//! error context for "executable not found" failures), then hand the
//! resulting `Child` to [`wait_with_limits`].

use anyhow::{Context, Result, anyhow};
use std::io;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::Child;
use tokio::task::JoinHandle;
use tokio::time;

/// Grace period to wait for a killed process to exit before giving up.
const KILL_GRACE_MS: u64 = 2_000;

/// Result of running an external process to completion (or timeout).
///
/// `stdout`/`stderr` contain raw UTF-8-lossy captures, exactly as produced
/// by the child. ANSI stripping is not performed here — callers that want
/// clean text should apply [`crate::text::strip_ansi_codes`] themselves.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProcessResult {
    /// Exit code, or `None` if killed by signal or timeout.
    pub exit_code: Option<i32>,
    /// `true` iff exit code is 0 and no timeout occurred.
    pub success: bool,
    /// `true` iff the process was killed because it exceeded the timeout.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
}

/// Reads from an async stream into memory, stopping at a byte limit.
///
/// Continues draining the reader after the limit is reached so the child
/// process does not block on a full pipe buffer.
///
/// Returns `(captured_bytes, truncated)`.
pub async fn read_to_end_limited<R>(mut reader: R, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(16 * 1024));
    let mut truncated = false;
    let mut buf = [0u8; 16 * 1024];

    loop {
        let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await?;
        if n == 0 {
            break;
        }

        if out.len() < limit {
            let remaining = limit - out.len();
            let take = remaining.min(n);
            out.extend_from_slice(&buf[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    Ok((out, truncated))
}

/// Drives a spawned child to completion with a timeout, capturing stdout and
/// stderr within the given byte limits.
///
/// The child must have been spawned with `Stdio::piped()` for both stdout and
/// stderr. On timeout the child is killed and given a 2-second grace period
/// to exit before returning.
///
/// Captured bytes are capped before UTF-8 lossy conversion and before any
/// caller-side ANSI stripping. This keeps the process-level output bounds
/// independent from presentation cleanup.
///
/// # Errors
///
/// Returns an error if the child's stdout/stderr handles could not be taken,
/// if `wait()` fails, or if the child does not terminate within the grace
/// period after a timeout kill.
pub async fn wait_with_limits(
    mut child: Child,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<ProcessResult> {
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_after_setup_error(&mut child).await;
            return Err(anyhow!("failed to capture stdout"));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_after_setup_error(&mut child).await;
            return Err(anyhow!("failed to capture stderr"));
        }
    };

    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(res) => res.context("wait failed")?,
        Err(_) => {
            timed_out = true;
            wait_after_timeout_kill(&mut child, timeout_ms).await?
        }
    };

    let (stdout_bytes, truncated_stdout) = collect_capture(stdout_task, "stdout").await?;
    let (stderr_bytes, truncated_stderr) = collect_capture(stderr_task, "stderr").await?;

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    let exit_code = status.code();
    let success = status.success() && !timed_out;

    Ok(ProcessResult {
        exit_code,
        success,
        timed_out,
        stdout,
        stderr,
        truncated_stdout,
        truncated_stderr,
    })
}

async fn terminate_after_setup_error(child: &mut Child) {
    let _ = child.start_kill();
    let _ = time::timeout(Duration::from_millis(KILL_GRACE_MS), child.wait()).await;
}

async fn wait_after_timeout_kill(child: &mut Child, timeout_ms: u64) -> Result<ExitStatus> {
    let kill_error = child.start_kill().err();

    match time::timeout(Duration::from_millis(KILL_GRACE_MS), child.wait()).await {
        Ok(res) => res.context("wait failed after kill"),
        Err(_) => {
            if let Some(err) = kill_error {
                return Err(anyhow!(
                    "process timed out after {timeout_ms} ms, kill failed: {err}, and the process did not terminate"
                ));
            }

            Err(anyhow!(
                "process timed out after {timeout_ms} ms and did not terminate"
            ))
        }
    }
}

async fn collect_capture(
    task: JoinHandle<io::Result<(Vec<u8>, bool)>>,
    stream_name: &'static str,
) -> Result<(Vec<u8>, bool)> {
    task.await
        .with_context(|| format!("{stream_name} capture task failed"))?
        .with_context(|| format!("{stream_name} capture failed"))
}

#[cfg(test)]
mod tests {
    use super::{read_to_end_limited, wait_with_limits};
    use std::process::Stdio;
    use tokio::process::Command;

    #[cfg(target_os = "windows")]
    fn shell_command(windows_script: &str, _unix_script: &str) -> Command {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/C", windows_script]);
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    fn shell_command(_windows_script: &str, unix_script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", unix_script]);
        cmd
    }

    #[cfg(target_os = "windows")]
    fn long_running_command() -> Command {
        let mut cmd = Command::new("ping.exe");
        cmd.args(["127.0.0.1", "-n", "6"]);
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    fn long_running_command() -> Command {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        cmd
    }

    fn piped(mut cmd: Command) -> Command {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }

    #[tokio::test]
    async fn read_to_end_limited_captures_input_under_limit() {
        let input = b"hello";
        let (captured, truncated) = read_to_end_limited(&input[..], 16).await.unwrap();

        assert_eq!(captured, b"hello");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn read_to_end_limited_truncates_but_keeps_prefix() {
        let input = b"hello world";
        let (captured, truncated) = read_to_end_limited(&input[..], 5).await.unwrap();

        assert_eq!(captured, b"hello");
        assert!(truncated);
    }

    #[tokio::test]
    async fn read_to_end_limited_zero_limit_marks_nonempty_input_truncated() {
        let input = b"hello";
        let (captured, truncated) = read_to_end_limited(&input[..], 0).await.unwrap();

        assert!(captured.is_empty());
        assert!(truncated);
    }

    #[tokio::test]
    async fn read_to_end_limited_preserves_raw_ansi_bytes() {
        let input = b"\x1b[31mred\x1b[0m";
        let (captured, truncated) = read_to_end_limited(&input[..], input.len()).await.unwrap();

        assert_eq!(captured, input);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn wait_with_limits_caps_stdout_and_stderr_independently() {
        let child = piped(shell_command(
            "echo stdout-data & echo stderr-data 1>&2",
            "echo stdout-data; echo stderr-data >&2",
        ))
        .spawn()
        .unwrap();

        let result = wait_with_limits(child, 5_000, 6, 6).await.unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
        assert!(!result.timed_out);
        assert_eq!(result.stdout, "stdout");
        assert_eq!(result.stderr, "stderr");
        assert!(result.truncated_stdout);
        assert!(result.truncated_stderr);
    }

    #[tokio::test]
    async fn wait_with_limits_requires_piped_stdout_and_stderr() {
        let mut cmd = shell_command("echo stdout-data", "echo stdout-data");
        cmd.stdout(Stdio::null()).stderr(Stdio::piped());
        let child = cmd.spawn().unwrap();

        let err = wait_with_limits(child, 5_000, 16, 16).await.unwrap_err();

        assert!(err.to_string().contains("failed to capture stdout"));
    }

    #[tokio::test]
    async fn wait_with_limits_reports_timeout_after_bounded_kill_wait() {
        let child = piped(long_running_command()).spawn().unwrap();

        let result = wait_with_limits(child, 50, 64, 64).await.unwrap();

        assert!(!result.success);
        assert!(result.timed_out);
    }
}
