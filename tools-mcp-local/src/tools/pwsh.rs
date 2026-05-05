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

    let pwsh_exe = if cfg!(target_os = "windows") {
        "pwsh.exe"
    } else {
        "pwsh"
    };
    let mut cmd = Command::new(pwsh_exe);
    cmd.args(["-NoLogo", "-Command", &req.command]);
    cmd.current_dir(work_dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = match cmd.spawn() {
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
