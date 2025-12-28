use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;

/// Result of running a shell script or process.
#[derive(Debug)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
}

/// Read an async stream into memory up to `limit` bytes.
/// Returns (captured_bytes, truncated).
pub async fn read_to_end_limited<R>(mut reader: R, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out: Vec<u8> = Vec::new();
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

/// Run a shell script with timeout and output capture.
///
/// On Windows, runs via `pwsh.exe -NoLogo -ExecutionPolicy Bypass -File <script>`.
/// On Unix, runs via `bash <script>`.
pub async fn run_shell_script(
    script_path: &Path,
    working_dir: &str,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<ProcessResult, String> {
    let is_windows = cfg!(target_os = "windows");

    let mut cmd = if is_windows {
        let mut c = Command::new("pwsh.exe");
        c.args(["-NoLogo", "-ExecutionPolicy", "Bypass", "-File"]);
        c.arg(script_path);
        c
    } else {
        let mut c = Command::new("bash");
        c.arg(script_path);
        c
    };

    cmd.current_dir(working_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    let stdout_task = tokio::spawn(async move {
        read_to_end_limited(stdout, max_stdout_bytes).await
    });
    let stderr_task = tokio::spawn(async move {
        read_to_end_limited(stderr, max_stderr_bytes).await
    });

    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(e)) => return Err(format!("wait failed: {e}")),
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            let _ = time::timeout(Duration::from_millis(2_000), child.wait()).await;
            None
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
    let exit_code = status.as_ref().and_then(|s| s.code());
    let success = status.as_ref().is_some_and(|s| s.success()) && !timed_out;

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

/// Run a PowerShell command with timeout and output capture.
///
/// Executes via `pwsh.exe -NoLogo -Command <command>` on Windows,
/// or `pwsh -NoLogo -Command <command>` on Unix (if pwsh is installed).
pub async fn run_pwsh_command(
    command: &str,
    working_dir: &str,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<ProcessResult, String> {
    let pwsh_exe = if cfg!(target_os = "windows") {
        "pwsh.exe"
    } else {
        "pwsh"
    };

    let mut cmd = Command::new(pwsh_exe);
    cmd.args(["-NoLogo", "-Command", command]);
    cmd.current_dir(working_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn pwsh: {e}"))?;

    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    let stdout_task = tokio::spawn(async move {
        read_to_end_limited(stdout, max_stdout_bytes).await
    });
    let stderr_task = tokio::spawn(async move {
        read_to_end_limited(stderr, max_stderr_bytes).await
    });

    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(e)) => return Err(format!("wait failed: {e}")),
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            let _ = time::timeout(Duration::from_millis(2_000), child.wait()).await;
            None
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
    let exit_code = status.as_ref().and_then(|s| s.code());
    let success = status.as_ref().is_some_and(|s| s.success()) && !timed_out;

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
