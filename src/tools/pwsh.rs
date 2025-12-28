use crate::config::{DEFAULT_PWSH_TIMEOUT_MS, MAX_PWSH_STDERR_BYTES, MAX_PWSH_STDOUT_BYTES, MAX_PWSH_TIMEOUT_MS};
use crate::process_utils;
use crate::tool_registry::McpTool;
use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use tracing::{error, info};

pub struct PwshTool;

#[derive(Deserialize)]
struct PwshRequest {
    command: String,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

impl McpTool for PwshTool {
    const NAME: &'static str = "Pwsh";
    const ALIASES: &'static [&'static str] = &["pwsh", "powershell"];
    const DESCRIPTION: &'static str = "Execute a PowerShell command via pwsh";

    fn input_schema() -> Value {
        json!({
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
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { execute_pwsh(id, args).await })
    }
}

async fn execute_pwsh(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<PwshRequest>(id.clone(), args) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let work_dir = req.working_dir.as_deref().unwrap_or(".");
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_PWSH_TIMEOUT_MS).min(MAX_PWSH_TIMEOUT_MS);

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
            return RpcResponse::err(id, format!("failed to run pwsh: {e}"));
        }
    };

    if !result.success {
        error!(
            "Pwsh tool: command failed (exit_code={:?}, timed_out={})",
            result.exit_code, result.timed_out
        );
    }

    let payload = process_utils::build_process_result_response(&result, None);
    RpcResponse::ok_json_content(id, payload, !result.success)
}
