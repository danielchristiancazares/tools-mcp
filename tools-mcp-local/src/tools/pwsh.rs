use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_PWSH_TIMEOUT_MS, MAX_PWSH_STDERR_BYTES, MAX_PWSH_STDOUT_BYTES, MAX_PWSH_TIMEOUT_MS,
};
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::process::wait_with_limits;
use tools_mcp_core::text::strip_ansi_codes;
use tools_mcp_core::validation;
use tracing::{error, info};

const WSL_KERNEL_RELEASE_PATH: &str = "/proc/sys/kernel/osrelease";
const WSL2_COMMON_PWSH_PATH: &str = "/mnt/c/Program Files/PowerShell/7/pwsh.exe";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PwshRequest {
    command: String,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug)]
enum PwshSpawnError {
    Primary(io::Error),
    Fallback {
        primary: io::Error,
        fallback_path: &'static Path,
        fallback: io::Error,
    },
}

impl fmt::Display for PwshSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primary(error) => write!(f, "{error}"),
            Self::Fallback {
                primary,
                fallback_path,
                fallback,
            } => write!(
                f,
                "{primary}; fallback {} also failed: {fallback}",
                fallback_path.display()
            ),
        }
    }
}

impl PwshSpawnError {
    fn remediation(&self) -> &'static str {
        match self {
            Self::Primary(_) => "install PowerShell 7 (pwsh) and ensure it is on PATH.",
            Self::Fallback { .. } => {
                "install PowerShell 7 (pwsh) on the Linux PATH or enable WSL Windows executable interoperability for the fallback path."
            }
        }
    }
}

fn default_pwsh_exe() -> &'static OsStr {
    if cfg!(target_os = "windows") {
        OsStr::new("pwsh.exe")
    } else {
        OsStr::new("pwsh")
    }
}

fn wsl2_common_pwsh_path() -> &'static Path {
    Path::new(WSL2_COMMON_PWSH_PATH)
}

fn read_kernel_release() -> Option<String> {
    std::fs::read_to_string(WSL_KERNEL_RELEASE_PATH).ok()
}

fn is_wsl2_kernel_release(kernel_release: &str) -> bool {
    let kernel_release = kernel_release.to_ascii_lowercase();
    kernel_release.contains("microsoft") && kernel_release.contains("wsl2")
}

fn select_wsl2_pwsh_fallback(
    spawn_error_kind: ErrorKind,
    is_linux_target: bool,
    kernel_release: Option<&str>,
    fallback_exists: bool,
) -> Option<&'static Path> {
    if spawn_error_kind == ErrorKind::NotFound
        && is_linux_target
        && fallback_exists
        && kernel_release.is_some_and(is_wsl2_kernel_release)
    {
        Some(wsl2_common_pwsh_path())
    } else {
        None
    }
}

fn select_current_wsl2_pwsh_fallback(spawn_error_kind: ErrorKind) -> Option<&'static Path> {
    let kernel_release = read_kernel_release();
    select_wsl2_pwsh_fallback(
        spawn_error_kind,
        cfg!(target_os = "linux"),
        kernel_release.as_deref(),
        wsl2_common_pwsh_path().is_file(),
    )
}

fn build_pwsh_command(program: &OsStr, command: &str, work_dir: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(["-NoLogo", "-Command", command]);
    cmd.current_dir(work_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

fn spawn_pwsh(req: &PwshRequest, work_dir: &str) -> Result<tokio::process::Child, PwshSpawnError> {
    let mut cmd = build_pwsh_command(default_pwsh_exe(), &req.command, work_dir);
    match cmd.spawn() {
        Ok(child) => Ok(child),
        Err(primary) => {
            let primary_kind = primary.kind();
            let Some(fallback_path) = select_current_wsl2_pwsh_fallback(primary_kind) else {
                return Err(PwshSpawnError::Primary(primary));
            };

            info!(
                "Pwsh tool: retrying with WSL2 common PowerShell path {}",
                fallback_path.display()
            );

            let mut fallback_cmd =
                build_pwsh_command(fallback_path.as_os_str(), &req.command, work_dir);
            match fallback_cmd.spawn() {
                Ok(child) => Ok(child),
                Err(fallback) => Err(PwshSpawnError::Fallback {
                    primary,
                    fallback_path,
                    fallback,
                }),
            }
        }
    }
}

async fn execute_pwsh(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<PwshRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    let work_dir = req.working_dir.as_deref().unwrap_or(".");
    let timeout_ms = validation::clamp_timeout(
        req.timeout_ms,
        DEFAULT_PWSH_TIMEOUT_MS,
        100,
        MAX_PWSH_TIMEOUT_MS,
    );

    info!("Pwsh tool: executing command in {}", work_dir);

    let child = match spawn_pwsh(&req, work_dir) {
        Ok(c) => c,
        Err(e) => {
            error!("Pwsh tool: failed to spawn pwsh: {}", e);
            return ToolCallOutcome::err(format!(
                "failed to run pwsh: failed to spawn pwsh: {e}. Remediation: {}",
                e.remediation()
            ));
        }
    };

    let mut result = match wait_with_limits(
        child,
        timeout_ms,
        MAX_PWSH_STDOUT_BYTES,
        MAX_PWSH_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Pwsh tool: {}", e);
            return ToolCallOutcome::err(format!("failed to run pwsh: {e}"));
        }
    };

    result.stdout = strip_ansi_codes(&result.stdout);
    result.stderr = strip_ansi_codes(&result.stderr);

    if !result.success {
        error!(
            "Pwsh tool: command failed (exit_code={:?}, timed_out={})",
            result.exit_code, result.timed_out
        );
    }

    let payload = serde_json::json!({
        "exit_code": result.exit_code,
        "success": result.success,
        "timed_out": result.timed_out,
        "truncated_stdout": result.truncated_stdout,
        "truncated_stderr": result.truncated_stderr,
        "stdout": result.stdout,
        "stderr": result.stderr,
    });
    ToolCallOutcome::ok_json_content(&payload, !result.success)
}

define_mcp_tool! {
    PwshTool,
    name: "Pwsh",
    description: "Execute a PowerShell command via pwsh",
    schema: {
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The PowerShell command or expression to execute"
            },
            "working_dir": {
                "type": "string",
                "description": "Working directory for the command (default: current directory)"
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 60000, max: 300000)",
                "minimum": 100,
                "maximum": 300_000
            }
        },
        "required": ["command"],
        "additionalProperties": false
    },
    handler: execute_pwsh
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn detects_wsl2_kernel_release() {
        assert!(is_wsl2_kernel_release(
            "5.15.167.4-microsoft-standard-WSL2\n"
        ));
    }

    #[test]
    fn treats_wsl1_and_regular_linux_as_non_wsl2() {
        assert!(!is_wsl2_kernel_release("4.4.0-19041-Microsoft"));
        assert!(!is_wsl2_kernel_release(
            "6.8.0-31-generic #31-Ubuntu SMP PREEMPT_DYNAMIC"
        ));
    }

    #[test]
    fn falls_back_to_common_windows_pwsh_when_wsl2_path_lookup_misses() {
        assert_eq!(
            select_wsl2_pwsh_fallback(
                ErrorKind::NotFound,
                true,
                Some("5.15.167.4-microsoft-standard-WSL2"),
                true
            ),
            Some(wsl2_common_pwsh_path())
        );
    }

    #[test]
    fn skips_common_windows_pwsh_fallback_for_other_spawn_conditions() {
        assert_eq!(
            select_wsl2_pwsh_fallback(
                ErrorKind::PermissionDenied,
                true,
                Some("5.15.167.4-microsoft-standard-WSL2"),
                true
            ),
            None
        );
        assert_eq!(
            select_wsl2_pwsh_fallback(
                ErrorKind::NotFound,
                true,
                Some("4.4.0-19041-Microsoft"),
                true
            ),
            None
        );
        assert_eq!(
            select_wsl2_pwsh_fallback(
                ErrorKind::NotFound,
                false,
                Some("5.15.167.4-microsoft-standard-WSL2"),
                true
            ),
            None
        );
        assert_eq!(
            select_wsl2_pwsh_fallback(
                ErrorKind::NotFound,
                true,
                Some("5.15.167.4-microsoft-standard-WSL2"),
                false
            ),
            None
        );
    }
}
