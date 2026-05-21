use crate::path_policy;
use std::ffi::OsStr;
use std::io;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PwshRequest {
    command: String,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn default_pwsh_exe() -> &'static OsStr {
    if cfg!(target_os = "windows") {
        OsStr::new("pwsh.exe")
    } else {
        OsStr::new("pwsh")
    }
}

fn build_pwsh_command(program: &OsStr, command: &str, work_dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(["-NoLogo", "-Command", command]);
    cmd.current_dir(work_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

fn spawn_pwsh(req: &PwshRequest, work_dir: &Path) -> Result<tokio::process::Child, io::Error> {
    let mut cmd = build_pwsh_command(default_pwsh_exe(), &req.command, work_dir);
    cmd.spawn()
}

async fn execute_pwsh(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<PwshRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    let work_dir = req.working_dir.as_deref().unwrap_or(".");
    if let Some(working_dir) = req.working_dir.as_deref()
        && let Err(o) = validation::validate_non_empty(working_dir, "working_dir", None)
    {
        return o;
    }
    let work_dir = match path_policy::resolve_existing_directory(work_dir, "working_dir") {
        Ok(path) => path,
        Err(err) => return ToolCallOutcome::err(err.to_string()),
    };
    let timeout_ms = validation::clamp_timeout(
        req.timeout_ms,
        DEFAULT_PWSH_TIMEOUT_MS,
        100,
        MAX_PWSH_TIMEOUT_MS,
    );

    info!("Pwsh tool: executing command in {}", work_dir.display());

    let child = match spawn_pwsh(&req, &work_dir) {
        Ok(c) => c,
        Err(e) => {
            error!("Pwsh tool: failed to spawn pwsh: {}", e);
            return ToolCallOutcome::err(format!(
                "failed to run pwsh: failed to spawn pwsh: {e}. Remediation: install PowerShell 7 (pwsh) and ensure it is on PATH."
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

#[cfg(test)]
mod tests {
    use super::execute_pwsh;
    use serde_json::json;

    #[tokio::test]
    async fn pwsh_rejects_parent_traversal_working_dir_before_spawn() {
        let outcome = execute_pwsh(
            None,
            json!({
                "command": "Write-Output should-not-run",
                "working_dir": ".."
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], true);
        let msg = outcome.0["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("outside the server working directory"));
    }
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
