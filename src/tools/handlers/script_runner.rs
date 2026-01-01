//! Build/test script runner implementation.

use crate::RpcResponse;
use crate::config::{DEFAULT_SCRIPT_TIMEOUT_MS, MAX_SCRIPT_STDERR_BYTES, MAX_SCRIPT_STDOUT_BYTES};
use crate::process_utils;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use tracing::{error, info};

/// Configuration for a script-based tool.
pub struct ScriptConfig {
    /// Base script name without extension (e.g., "build", "test").
    pub script_base: &'static str,
    /// Label for logging/error messages (e.g., "Build", "Test").
    pub tool_label: &'static str,
}

#[derive(Deserialize)]
struct ScriptRequest {
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Generic script runner for build/test style tools.
/// Looks for `{script_base}.ps1` on Windows or `{script_base}.sh` on Unix.
pub async fn run_script_tool(
    id: Option<Value>,
    args: Value,
    config: ScriptConfig,
) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<ScriptRequest>(id.clone(), args) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let work_dir = req.working_dir.as_deref().unwrap_or(".");
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_MS);

    let is_windows = cfg!(target_os = "windows");
    let script_name = if is_windows {
        format!("{}.ps1", config.script_base)
    } else {
        format!("{}.sh", config.script_base)
    };
    let script_path = Path::new(work_dir).join(&script_name);

    if !script_path.exists() {
        return RpcResponse::err(
            id,
            format!(
                "{} script not found: {} (looked in {})",
                config.tool_label,
                script_name,
                Path::new(work_dir)
                    .canonicalize()
                    .unwrap_or_else(|_| work_dir.into())
                    .display()
            ),
        );
    }

    info!(
        "{} tool: running {} in {}",
        config.tool_label, script_name, work_dir
    );

    let result = match process_utils::run_shell_script(
        &script_path,
        work_dir,
        timeout_ms,
        MAX_SCRIPT_STDOUT_BYTES,
        MAX_SCRIPT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("{} tool: {}", config.tool_label, e);
            return RpcResponse::err(id, format!("failed to run {}: {}", script_name, e));
        }
    };

    if !result.success {
        error!(
            "{} tool: {} failed (exit_code={:?})",
            config.tool_label, script_name, result.exit_code
        );
    }

    let mut extra = HashMap::new();
    extra.insert("script", json!(script_name));
    extra.insert("working_dir", json!(work_dir));
    let payload = process_utils::build_process_result_response(&result, Some(extra));
    RpcResponse::ok_json_content(id, payload, !result.success)
}
