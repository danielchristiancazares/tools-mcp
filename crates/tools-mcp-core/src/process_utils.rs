//! Process execution utilities with timeout control and bounded output capture.
//!
//! This module provides a set of utilities for spawning and managing external processes
//! in an async context. Key features include:
//!
//! - **Timeout enforcement**: All process execution functions accept a timeout parameter
//!   and will forcibly terminate processes that exceed their allotted time.
//! - **Output capture with limits**: Stdout and stderr are captured up to configurable
//!   byte limits to prevent memory exhaustion from runaway processes.
//! - **ANSI code stripping**: Terminal escape sequences are automatically removed from
//!   captured output for clean text processing.
//! - **Cross-platform support**: `PowerShell` execution uses `pwsh`/`pwsh.exe` across
//!   supported platforms.
//!
//! # Error Handling
//!
//! Functions in this module return `Result<ProcessResult, String>` where:
//! - `Ok(ProcessResult)` indicates the process was spawned successfully (even if it
//!   failed, timed out, or returned a non-zero exit code)
//! - `Err(String)` indicates a failure to spawn the process or wait on it
//!
//! The [`ProcessResult`] struct contains detailed information about the execution,
//! including whether the process timed out, its exit code, and any captured output.
//!
//! # Examples
//!
//! ```no_run
//! use tools_mcp_core::process_utils::run_pwsh_command;
//!
//! # async fn example() -> Result<(), String> {
//! // Run a PowerShell command
//! let result = run_pwsh_command(
//!     "Get-Process | Select-Object -First 5",
//!     ".",
//!     10_000,
//!     100_000,
//!     100_000,
//! ).await?;
//! # Ok(())
//! # }
//! ```

use std::io;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;

/// Strips ANSI escape codes from a string, returning clean plaintext.
///
/// Terminal output often contains ANSI escape sequences for colors, cursor
/// positioning, and other formatting. This function removes these sequences
/// to produce human-readable text suitable for logging, display, or further
/// processing.
///
/// # Supported Escape Sequences
///
/// - **CSI (Control Sequence Introducer)**: `ESC [` followed by parameters and
///   a final byte. Covers colors (`ESC[31m`), cursor movement (`ESC[2J`), etc.
/// - **OSC (Operating System Command)**: `ESC ]` followed by data and terminated
///   by BEL (`\x07`) or ST (`ESC \`). Used for window titles and hyperlinks.
/// - **Character Set Designation**: `ESC (` or `ESC )` followed by a designator
///   character. Used for character encoding selection.
/// - **Other Sequences**: Single-character escapes like `ESC M` (reverse linefeed).
///
/// # Arguments
///
/// * `s` - The input string potentially containing ANSI escape codes.
///
/// # Returns
///
/// A new `String` with all recognized ANSI escape sequences removed.
///
/// # Performance
///
/// Pre-allocates output capacity equal to input length to minimize reallocations.
/// Uses a single-pass character iterator for O(n) time complexity.
///
/// # Examples
///
/// ```
/// use tools_mcp_core::process_utils::strip_ansi_codes;
///
/// // Remove color codes
/// let colored = "\x1b[31mError:\x1b[0m file not found";
/// assert_eq!(strip_ansi_codes(colored), "Error: file not found");
///
/// // Handle nested/complex sequences
/// let complex = "\x1b[1;31;40mBold red on black\x1b[0m";
/// assert_eq!(strip_ansi_codes(complex), "Bold red on black");
///
/// // Pass through clean text unchanged
/// let plain = "Hello, world!";
/// assert_eq!(strip_ansi_codes(plain), "Hello, world!");
/// ```
pub fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC character (0x1B) marks the start of an escape sequence
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ <params> <final_byte>
                    // Parameters are bytes in range 0x30-0x3F, intermediates 0x20-0x2F,
                    // final byte terminates the sequence (0x40-0x7E: '@' through '~')
                    chars.next(); // consume '['
                    while let Some(&ch) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&ch) {
                            break; // final byte reached
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] <data> <terminator>
                    // Data continues until BEL (0x07) or ST (ESC \)
                    chars.next(); // consume ']'
                    while let Some(&ch) = chars.peek() {
                        if ch == '\x07' {
                            // BEL (bell character) terminates OSC
                            chars.next();
                            break;
                        }

                        if ch == '\x1b' {
                            // Check for ST (String Terminator): ESC followed by backslash
                            chars.next(); // consume ESC
                            if chars.peek() == Some(&'\\') {
                                chars.next(); // consume backslash
                                break;
                            }
                            // Not an ST, continue parsing the OSC sequence
                            continue;
                        }

                        chars.next();
                    }
                }
                Some('(' | ')') => {
                    // Character set designation: ESC ( G or ESC ) G
                    // Two bytes follow ESC: the designator type and character set
                    chars.next(); // consume '(' or ')'
                    chars.next(); // consume the character set designator (e.g., 'B' for ASCII)
                }
                _ => {
                    // Other escape sequences (e.g., ESC M for reverse linefeed)
                    // consume one following character if present
                    chars.next();
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Result of running an external process.
///
/// This struct captures comprehensive information about a process execution,
/// including its exit status, captured output, and whether any limits were
/// exceeded. It is returned by process execution helpers such as [`run_pwsh_command`].
///
/// # Determining Execution Outcome
///
/// The `success` field provides a convenient boolean, but for detailed
/// diagnostics, examine multiple fields:
///
/// | Scenario | `success` | `timed_out` | `exit_code` |
/// |----------|-----------|-------------|-------------|
/// | Normal exit (code 0) | `true` | `false` | `Some(0)` |
/// | Normal exit (code N) | `false` | `false` | `Some(N)` |
/// | Killed by signal | `false` | `false` | `None` |
/// | Timed out and killed | `false` | `true` | `None` |
///
/// # Output Handling
///
/// The `stdout` and `stderr` fields contain captured output with ANSI escape
/// codes stripped. If the output exceeded the byte limit specified at
/// invocation, the corresponding `truncated_*` flag is set to `true`.
///
/// # Examples
///
/// ```no_run
/// # use tools_mcp_core::process_utils::ProcessResult;
/// # async fn example() {
/// # let result: ProcessResult = todo!();
/// // Check for successful completion
/// if result.success {
///     println!("Output: {}", result.stdout);
/// }
///
/// // Handle timeout specifically
/// if result.timed_out {
///     eprintln!("Process exceeded time limit");
/// }
///
/// // Check for truncated output
/// if result.truncated_stdout {
///     eprintln!("Warning: stdout was truncated");
/// }
/// # }
/// ```
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProcessResult {
    /// The process exit code, or `None` if killed by signal or timeout.
    ///
    /// On Unix, processes terminated by signals do not have an exit code.
    /// On Windows, killed processes typically return exit code 1.
    pub exit_code: Option<i32>,

    /// `true` if the process exited successfully (code 0) without timing out.
    ///
    /// This is `false` if:
    /// - The process returned a non-zero exit code
    /// - The process was killed by a signal
    /// - The process was terminated due to timeout
    pub success: bool,

    /// `true` if the process was forcibly killed due to exceeding the timeout.
    ///
    /// When a timeout occurs, a `SIGKILL` is sent (or `TerminateProcess` on
    /// Windows), followed by a 2-second grace period for cleanup.
    pub timed_out: bool,

    /// Captured standard output with ANSI escape codes stripped.
    ///
    /// May be truncated if output exceeded the byte limit; check
    /// `truncated_stdout` to detect this condition.
    pub stdout: String,

    /// Captured standard error with ANSI escape codes stripped.
    ///
    /// May be truncated if output exceeded the byte limit; check
    /// `truncated_stderr` to detect this condition.
    pub stderr: String,

    /// `true` if stdout output was truncated due to exceeding the byte limit.
    pub truncated_stdout: bool,

    /// `true` if stderr output was truncated due to exceeding the byte limit.
    pub truncated_stderr: bool,
}

/// Reads from an async stream into memory, stopping at a byte limit.
///
/// This function reads data from an async reader in 16KB chunks until either
/// EOF is reached or the byte limit is exceeded. Unlike `read_to_end`, this
/// function continues draining the reader even after hitting the limit to
/// prevent pipe buffer blockage in child processes.
///
/// # Arguments
///
/// * `reader` - Any type implementing `AsyncRead + Unpin` (e.g., `ChildStdout`).
/// * `limit` - Maximum number of bytes to capture. Bytes beyond this are read
///   but discarded.
///
/// # Returns
///
/// A tuple of `(captured_bytes, truncated)` where:
/// - `captured_bytes`: Up to `limit` bytes of data read from the stream
/// - `truncated`: `true` if any bytes were discarded due to exceeding the limit
///
/// # Errors
///
/// Returns `io::Error` if a read operation fails.
///
/// # Implementation Notes
///
/// The function uses a 16KB stack buffer for reads, which balances memory
/// efficiency with syscall overhead. Even after `limit` is reached, the
/// function continues reading to EOF - this prevents the child process from
/// blocking on a full pipe buffer.
///
/// # Examples
///
/// ```no_run
/// use tokio::io::AsyncReadExt;
/// use tools_mcp_core::process_utils::read_to_end_limited;
///
/// # async fn example() -> std::io::Result<()> {
/// let data = b"Hello, world! This is a long string.";
/// let cursor = std::io::Cursor::new(data.to_vec());
/// let reader = tokio::io::BufReader::new(cursor);
///
/// // Read with a 10-byte limit
/// let (bytes, truncated) = read_to_end_limited(reader, 10).await?;
/// assert_eq!(bytes.len(), 10);
/// assert!(truncated);
/// # Ok(())
/// # }
/// ```
pub async fn read_to_end_limited<R>(mut reader: R, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out: Vec<u8> = Vec::new();
    let mut truncated = false;

    // Use 16KB buffer to balance memory usage with read efficiency
    let mut buf = [0u8; 16 * 1024];

    loop {
        let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await?;
        if n == 0 {
            break; // EOF reached
        }

        if out.len() < limit {
            // Calculate how many bytes we can still accept
            let remaining = limit - out.len();
            let take = remaining.min(n);
            out.extend_from_slice(&buf[..take]);

            // If we couldn't take all bytes, we've hit the limit
            if take < n {
                truncated = true;
            }
        } else {
            // Already at limit; discard data but keep reading to drain the pipe
            truncated = true;
        }
    }

    Ok((out, truncated))
}

/// Executes a `PowerShell` command string with timeout enforcement and output capture.
///
/// This function runs an inline `PowerShell` command (not a script file) using
/// `PowerShell` Core (`pwsh`). It is cross-platform: `PowerShell` Core runs on
/// Windows, macOS, and Linux when installed.
///
/// Use this function when you need PowerShell-specific features (cmdlets, pipeline,
/// object handling) or cross-platform consistency with `PowerShell` syntax.
///
/// # Platform Behavior
///
/// - **Windows**: Runs `pwsh.exe -NoLogo -Command <command>`
/// - **Unix/Linux/macOS**: Runs `pwsh -NoLogo -Command <command>`
///
/// Note: On non-Windows systems, `PowerShell` Core must be installed separately.
///
/// # Arguments
///
/// * `command` - The `PowerShell` command string to execute. Can include pipelines,
///   cmdlets, and complex expressions. The entire string is passed as-is to `-Command`.
/// * `working_dir` - Working directory for the `PowerShell` process.
/// * `timeout_ms` - Maximum execution time in milliseconds before forcible termination.
/// * `max_stdout_bytes` - Maximum bytes to capture from stdout.
/// * `max_stderr_bytes` - Maximum bytes to capture from stderr.
///
/// # Returns
///
/// - `Ok(ProcessResult)` - Command was executed (check fields for success/failure)
/// - `Err(String)` - Failed to spawn `PowerShell` or wait on the process
///
/// # Examples
///
/// ```no_run
/// use tools_mcp_core::process_utils::run_pwsh_command;
///
/// # async fn example() -> Result<(), String> {
/// // Simple command
/// let result = run_pwsh_command(
///     "Get-Date -Format 'yyyy-MM-dd'",
///     ".",
///     5_000,
///     10_000,
///     10_000,
/// ).await?;
/// println!("Date: {}", result.stdout.trim());
///
/// // Complex pipeline
/// let result = run_pwsh_command(
///     "Get-Process | Where-Object { $_.CPU -gt 10 } | Select-Object Name, CPU -First 5",
///     ".",
///     30_000,
///     100_000,
///     100_000,
/// ).await?;
///
/// // Error handling
/// let result = run_pwsh_command(
///     "throw 'Custom error'",
///     ".",
///     5_000,
///     10_000,
///     10_000,
/// ).await?;
/// if !result.success {
///     eprintln!("PowerShell error: {}", result.stderr);
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns an error string if:
/// - `PowerShell` Core (`pwsh` / `pwsh.exe`) is not installed or not in PATH
/// - Pipe setup fails
/// - The `wait()` syscall fails unexpectedly
pub async fn run_pwsh_command(
    command: &str,
    working_dir: &str,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<ProcessResult, String> {
    // Use platform-appropriate executable name
    let pwsh_exe = if cfg!(target_os = "windows") {
        "pwsh.exe"
    } else {
        "pwsh"
    };

    let mut cmd = Command::new(pwsh_exe);
    // -NoLogo: suppress the PowerShell startup banner
    // -Command: interpret remaining args as a command string (vs -File for scripts)
    cmd.args(["-NoLogo", "-Command", command]);
    cmd.current_dir(working_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn pwsh: {e}"))?;

    // Take ownership of stdout/stderr handles for async reading
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    // Spawn concurrent tasks to drain both pipes simultaneously
    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    // Wait for process completion with timeout enforcement
    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(e)) => return Err(format!("wait failed: {e}")),
        Err(_) => {
            // Timeout elapsed - forcibly terminate the process
            timed_out = true;
            let _ = child.kill().await;
            // Allow 2 seconds for zombie cleanup and pipe buffer drain
            let _ = time::timeout(Duration::from_millis(2_000), child.wait()).await;
            None
        }
    };

    // Collect output from the reader tasks, defaulting to empty on failure
    let (stdout_bytes, truncated_stdout) = stdout_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))
        .unwrap_or((Vec::new(), false));
    let (stderr_bytes, truncated_stderr) = stderr_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))
        .unwrap_or((Vec::new(), false));

    // Convert raw bytes to clean strings
    let stdout = strip_ansi_codes(&String::from_utf8_lossy(&stdout_bytes));
    let stderr = strip_ansi_codes(&String::from_utf8_lossy(&stderr_bytes));
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

/// Build a standard MCP-compatible process result response payload.
///
/// This consolidates the repeated pattern of building JSON responses from
/// `ProcessResult` structs across tools such as `Pwsh`. It handles
/// the common fields (`exit_code`, success, `timed_out`, truncated flags, stdout, stderr)
/// and allows optional extra fields to be added per-tool.
///
/// # Arguments
///
/// * `result` - The `ProcessResult` from process execution
/// * `extra_fields` - Optional `HashMap` of additional fields to include
///
/// # Returns
///
/// A `serde_json::Value` containing the complete response payload
///
/// # Example
///
/// ```ignore
/// use serde_json::json;
/// use std::collections::HashMap;
///
/// let result = run_pwsh_command(...).await?;
/// let mut extra = HashMap::new();
/// extra.insert("command", json!("Get-Process"));
/// let payload = build_process_result_response(&result, Some(extra));
/// ```
pub fn build_process_result_response(
    result: &ProcessResult,
    extra_fields: Option<std::collections::HashMap<&str, serde_json::Value>>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "exit_code": result.exit_code,
        "success": result.success,
        "timed_out": result.timed_out,
        "truncated_stdout": result.truncated_stdout,
        "truncated_stderr": result.truncated_stderr,
        "stdout": result.stdout,
        "stderr": result.stderr,
    });

    if let Some(extra) = extra_fields
        && let Some(obj) = payload.as_object_mut()
    {
        for (key, value) in extra {
            obj.insert(key.to_string(), value);
        }
    }

    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_codes_nested_osc_csi() {
        // OSC sequence containing an ESC but not an ST (ESC \), followed by BEL.
        // Old buggy behavior would stop at the first ESC and leave "[31mcolor" in output.
        let input = "\x1b]test\x1b[31mcolor\x07actual_content";
        let result = strip_ansi_codes(input);
        assert_eq!(result, "actual_content");
    }
}
