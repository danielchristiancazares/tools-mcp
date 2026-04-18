use super::super::run_git;
use super::super::types::build_git_response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
};

/// Handle the `GitStatus` MCP tool request.
///
/// Executes `git status` and returns working tree state in a structured format.
/// By default, uses porcelain output for reliable machine parsing.
pub async fn handle_git_status(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitStatusRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        porcelain: Option<bool>,
        #[serde(default)]
        branch: Option<bool>,
        #[serde(default)]
        untracked: Option<bool>,
    }

    let req = match ToolCallOutcome::parse_args::<GitStatusRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let porcelain = req.porcelain.unwrap_or(true);
    let branch = req.branch.unwrap_or(true);
    let untracked = req.untracked.unwrap_or(true);

    let mut cmd_args: Vec<String> = vec!["status".into()];
    if porcelain {
        cmd_args.push("--porcelain=1".into());
        if branch {
            cmd_args.push("-b".into());
        }
        if !untracked {
            cmd_args.push("-uno".into());
        }
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let clean = exec.success && exec.stdout.trim().is_empty();
    let text = if exec.success {
        if clean {
            "clean".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("clean", json!(clean));

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}
