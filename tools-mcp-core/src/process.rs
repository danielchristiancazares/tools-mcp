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
use std::time::Duration;
use tokio::process::Child;
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
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr"))?;

    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(res) => Some(res.context("wait failed")?),
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            match time::timeout(Duration::from_millis(KILL_GRACE_MS), child.wait()).await {
                Ok(res) => Some(res.context("wait failed after kill")?),
                Err(_) => {
                    return Err(anyhow!(
                        "process timed out after {timeout_ms} ms and did not terminate"
                    ));
                }
            }
        }
    };

    let (stdout_bytes, truncated_stdout) = stdout_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))
        .unwrap_or((Vec::new(), false));
    let (stderr_bytes, truncated_stderr) = stderr_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))
        .unwrap_or((Vec::new(), false));

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);
    let success = status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success)
        && !timed_out;

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

#[cfg(test)]
mod tests {
    use super::read_to_end_limited;

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
}
