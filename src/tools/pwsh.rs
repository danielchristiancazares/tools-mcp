use crate::tool_outcome::ToolCallOutcome;
use crate::config::{
    DEFAULT_PWSH_TIMEOUT_MS, MAX_PWSH_STDERR_BYTES, MAX_PWSH_STDOUT_BYTES, MAX_PWSH_TIMEOUT_MS,
};
use crate::define_mcp_tool;
use crate::process_utils;
use crate::validation;
use serde::Deserialize;
use serde_json::Value;
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
    let req = match ToolCallOutcome::parse_args::<PwshRequest>(args) {
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

    let result = match process_utils::run_pwsh_command(
        &req.command,
        work_dir,
        timeout_ms,
        MAX_PWSH_STDOUT_BYTES,
        MAX_PWSH_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Pwsh tool: {}", e);
            let msg = if e.to_ascii_lowercase().contains("failed to spawn pwsh") {
                format!(
                    "failed to run pwsh: {e}. Remediation: install PowerShell 7 (pwsh) and ensure it is on PATH."
                )
            } else {
                format!("failed to run pwsh: {e}")
            };
            return ToolCallOutcome::err(msg);
        }
    };

    if !result.success {
        error!(
            "Pwsh tool: command failed (exit_code={:?}, timed_out={})",
            result.exit_code, result.timed_out
        );
    }

    let payload = process_utils::build_process_result_response(&result, None);
    ToolCallOutcome::ok_json_content(payload, !result.success)
}

define_mcp_tool! {
    PwshTool,
    name: "Pwsh",
    aliases: ["pwsh", "powershell"],
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
                "maximum": 300000
            }
        },
        "required": ["command"],
        "additionalProperties": false
    },
    handler: execute_pwsh
}
