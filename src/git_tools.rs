use crate::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
    MAX_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
use crate::process_utils::read_to_end_limited;
use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;

struct GitExecResult {
    git_bin: String,
    args: Vec<String>,
    working_dir: Option<String>,
    exit_code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
    truncated_stdout: bool,
    truncated_stderr: bool,
    timed_out: bool,
}

async fn run_git(
    working_dir: Option<String>,
    subcommand_args: Vec<String>,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<GitExecResult, anyhow::Error> {
    let timeout_ms = timeout_ms.clamp(100, MAX_GIT_TIMEOUT_MS);
    let max_stdout_bytes = max_stdout_bytes.clamp(1, MAX_OUTPUT_BYTES);
    let max_stderr_bytes = max_stderr_bytes.clamp(1, MAX_OUTPUT_BYTES);

    let git_bin = if cfg!(target_os = "windows") {
        "git.exe".to_string()
    } else {
        "git".to_string()
    };

    // Force deterministic, non-ANSI output for machine consumption.
    let mut args: Vec<String> = vec!["--no-pager".into(), "-c".into(), "color.ui=false".into()];
    args.extend(subcommand_args);

    let mut cmd = Command::new(&git_bin);
    cmd.args(&args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    if let Some(dir) = &working_dir {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!("failed to spawn {git_bin}. Is Git installed and on PATH? error: {e}")
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture git stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture git stderr"))?;

    let stdout_task =
        tokio::spawn(async move { read_to_end_limited(stdout, max_stdout_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_to_end_limited(stderr, max_stderr_bytes).await });

    let mut timed_out = false;
    let status = match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(res) => res?,
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            match time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Ok(res) => res?,
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "git command timed out after {} ms and did not terminate",
                        timeout_ms
                    ));
                }
            }
        }
    };

    let exit_code = status.code();

    let (stdout_bytes, truncated_stdout) = stdout_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))?;
    let (stderr_bytes, truncated_stderr) = stderr_task
        .await
        .unwrap_or_else(|_| Ok((Vec::new(), false)))?;

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    Ok(GitExecResult {
        git_bin,
        args,
        working_dir,
        exit_code,
        success: status.success() && !timed_out,
        stdout,
        stderr,
        truncated_stdout,
        truncated_stderr,
        timed_out,
    })
}

pub async fn handle_git_status(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
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

    let req = match RpcResponse::parse::<GitStatusRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
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
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
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

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "truncated_stdout": exec.truncated_stdout,
        "truncated_stderr": exec.truncated_stderr,
        "clean": clean,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    });

    RpcResponse::ok(id, payload)
}

pub async fn handle_git_diff(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitDiffRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        cached: Option<bool>,
        #[serde(default)]
        stat: Option<bool>,
        #[serde(default)]
        name_only: Option<bool>,
        #[serde(default)]
        unified: Option<i64>,
        #[serde(default)]
        paths: Option<Vec<String>>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match RpcResponse::parse::<GitDiffRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes = req
        .max_bytes
        .unwrap_or(DEFAULT_GIT_STDOUT_BYTES)
        .clamp(1, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["diff".into()];

    if req.cached.unwrap_or(false) {
        cmd_args.push("--cached".into());
    }
    if req.stat.unwrap_or(false) {
        cmd_args.push("--stat".into());
    }
    if req.name_only.unwrap_or(false) {
        cmd_args.push("--name-only".into());
    }
    if let Some(u) = req.unified {
        if u >= 0 {
            cmd_args.push(format!("-U{u}"));
        }
    }

    if let Some(paths) = &req.paths {
        if !paths.is_empty() {
            cmd_args.push("--".into());
            for p in paths {
                if !p.trim().is_empty() {
                    cmd_args.push(p.clone());
                }
            }
        }
    }

    let exec = match run_git(
        req.working_dir.clone(),
        cmd_args,
        timeout_ms,
        max_bytes,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        if exec.stdout.trim().is_empty() {
            "no diff".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "max_bytes": max_bytes,
        "truncated_stdout": exec.truncated_stdout,
        "truncated_stderr": exec.truncated_stderr,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    });

    RpcResponse::ok(id, payload)
}

pub async fn handle_git_restore(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitRestoreRequest {
        paths: Vec<String>,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        staged: Option<bool>,
        #[serde(default)]
        worktree: Option<bool>,
    }

    let req = match RpcResponse::parse::<GitRestoreRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if req.paths.is_empty() {
        return RpcResponse::err(id, "paths must be non-empty");
    }

    let staged = req.staged.unwrap_or(false);
    let worktree = req.worktree.unwrap_or(true);

    if !staged && !worktree {
        return RpcResponse::err(id, "at least one of staged/worktree must be true");
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let mut cmd_args: Vec<String> = vec!["restore".into()];
    if staged {
        cmd_args.push("--staged".into());
    }
    if worktree {
        cmd_args.push("--worktree".into());
    }

    cmd_args.push("--".into());
    for p in &req.paths {
        if !p.trim().is_empty() {
            cmd_args.push(p.clone());
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
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        if exec.stdout.trim().is_empty() && exec.stderr.trim().is_empty() {
            "ok".to_string()
        } else if exec.stdout.trim().is_empty() {
            exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "truncated_stdout": exec.truncated_stdout,
        "truncated_stderr": exec.truncated_stderr,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    });

    RpcResponse::ok(id, payload)
}

pub async fn handle_git_add(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitAddRequest {
        #[serde(default)]
        paths: Option<Vec<String>>,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        all: Option<bool>,
        #[serde(default)]
        update: Option<bool>,
    }

    let req = match RpcResponse::parse::<GitAddRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    let use_all = req.all.unwrap_or(false);
    let use_update = req.update.unwrap_or(false);
    let paths = req.paths.unwrap_or_default();

    if !use_all && !use_update && paths.is_empty() {
        return RpcResponse::err(id, "paths required unless 'all' or 'update' is true");
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let mut cmd_args: Vec<String> = vec!["add".into()];

    if use_all {
        cmd_args.push("-A".into());
    } else if use_update {
        cmd_args.push("-u".into());
    }

    if !paths.is_empty() {
        cmd_args.push("--".into());
        for p in &paths {
            if !p.trim().is_empty() {
                cmd_args.push(p.clone());
            }
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
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    let text = if exec.success {
        "ok".to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    });

    RpcResponse::ok(id, payload)
}

pub async fn handle_git_commit(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    #[derive(Deserialize)]
    struct GitCommitRequest {
        #[serde(rename = "type")]
        commit_type: String,
        #[serde(default)]
        scope: Option<String>,
        message: String,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    }

    let req = match RpcResponse::parse::<GitCommitRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if req.commit_type.trim().is_empty() {
        return RpcResponse::err(id, "type is required");
    }

    if req.message.trim().is_empty() {
        return RpcResponse::err(id, "message is required");
    }

    // Build conventional commit message: type(scope): message
    let commit_msg = match &req.scope {
        Some(scope) if !scope.trim().is_empty() => {
            format!(
                "{}({}): {}",
                req.commit_type.trim(),
                scope.trim(),
                req.message.trim()
            )
        }
        _ => format!("{}: {}", req.commit_type.trim(), req.message.trim()),
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    let cmd_args: Vec<String> = vec!["commit".into(), "-m".into(), commit_msg.clone()];

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
        Err(e) => return RpcResponse::err(id, format!("git error: {e:#}")),
    };

    // Try to extract commit hash from stdout (e.g., "[main abc1234] message")
    let commit_hash = exec
        .stdout
        .split_whitespace()
        .find(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit() || c == ']'))
        .map(|s| s.trim_end_matches(']').to_string());

    let text = if exec.success {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": !exec.success,
        "git_bin": exec.git_bin,
        "args": exec.args,
        "working_dir": exec.working_dir,
        "exit_code": exec.exit_code,
        "timed_out": exec.timed_out,
        "commit_message": commit_msg,
        "commit_hash": commit_hash,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
    });

    RpcResponse::ok(id, payload)
}
