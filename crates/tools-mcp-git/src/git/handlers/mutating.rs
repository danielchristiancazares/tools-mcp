use super::super::run_git;
use super::super::types::build_git_response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
};
use tools_mcp_core::validation;

/// Handle the `GitRestore` MCP tool request.
///
/// Executes `git restore` to discard uncommitted changes. This is a **destructive
/// operation** that cannot be undone for working tree changes.
pub async fn handle_git_restore(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
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

    let req = match ToolCallOutcome::parse_args::<GitRestoreRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if req.paths.is_empty() {
        return ToolCallOutcome::err("paths must be non-empty");
    }

    let staged = req.staged.unwrap_or(false);
    let worktree = req.worktree.unwrap_or(true);

    if !staged && !worktree {
        return ToolCallOutcome::err("at least one of staged/worktree must be true");
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
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

    let payload = build_git_response(&exec, &text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitAdd` MCP tool request.
///
/// Executes `git add` to stage files for the next commit. Supports staging
/// specific paths, all changes, or only tracked file updates.
pub async fn handle_git_add(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
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

    let req = match ToolCallOutcome::parse_args::<GitAddRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let use_all = req.all.unwrap_or(false);
    let use_update = req.update.unwrap_or(false);
    let paths = req.paths.unwrap_or_default();

    if !use_all && !use_update && paths.is_empty() {
        return ToolCallOutcome::err("paths required unless 'all' or 'update' is true");
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = if exec.success {
        "ok".to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, &text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitCommit` MCP tool request.
///
/// Creates a Git commit using the Conventional Commits format. The commit message
/// is automatically formatted as `type(scope): message` or `type: message`.
pub async fn handle_git_commit(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
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

    let req = match ToolCallOutcome::parse_args::<GitCommitRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.commit_type, "type", None) {
        return o;
    }
    if let Err(o) = validation::validate_non_empty(&req.message, "message", None) {
        return o;
    }

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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

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

    let mut extra_fields = HashMap::new();
    extra_fields.insert("commit_message", json!(commit_msg));
    extra_fields.insert("commit_hash", json!(commit_hash));

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitBranch` MCP tool request.
///
/// Executes `git branch` to list, create, or delete branches.
pub async fn handle_git_branch(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitBranchRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        list_all: Option<bool>,
        #[serde(default)]
        list_remote: Option<bool>,
        #[serde(default)]
        create: Option<String>,
        #[serde(default)]
        delete: Option<String>,
        #[serde(default)]
        force_delete: Option<String>,
        #[serde(default)]
        rename: Option<String>,
        #[serde(default)]
        new_name: Option<String>,
    }

    let req = match ToolCallOutcome::parse_args::<GitBranchRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let mut cmd_args: Vec<String> = vec!["branch".into()];

    if let Some(name) = &req.create {
        cmd_args.push(name.clone());
    } else if let Some(name) = &req.delete {
        cmd_args.push("-d".into());
        cmd_args.push(name.clone());
    } else if let Some(name) = &req.force_delete {
        cmd_args.push("-D".into());
        cmd_args.push(name.clone());
    } else if let Some(old_name) = &req.rename {
        cmd_args.push("-m".into());
        cmd_args.push(old_name.clone());
        if let Some(new_name) = &req.new_name {
            cmd_args.push(new_name.clone());
        } else {
            return ToolCallOutcome::err("new_name required when renaming a branch");
        }
    } else {
        if req.list_all.unwrap_or(false) {
            cmd_args.push("-a".into());
        } else if req.list_remote.unwrap_or(false) {
            cmd_args.push("-r".into());
        }
        cmd_args.push("-v".into());
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

    let text = if exec.success {
        if exec.stdout.trim().is_empty() {
            "ok".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, &text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitCheckout` MCP tool request.
///
/// Executes `git checkout` to switch branches or restore files.
pub async fn handle_git_checkout(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitCheckoutRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        create_branch: Option<String>,
        #[serde(default)]
        commit: Option<String>,
        #[serde(default)]
        paths: Option<Vec<String>>,
    }

    let req = match ToolCallOutcome::parse_args::<GitCheckoutRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let mut cmd_args: Vec<String> = vec!["checkout".into()];

    if let Some(branch) = &req.create_branch {
        cmd_args.push("-b".into());
        cmd_args.push(branch.clone());
    } else if let Some(branch) = &req.branch {
        cmd_args.push(branch.clone());
    } else if let Some(commit) = &req.commit {
        cmd_args.push(commit.clone());
    }

    if let Some(paths) = &req.paths
        && !paths.is_empty()
    {
        cmd_args.push("--".into());
        for p in paths {
            if !p.trim().is_empty() {
                cmd_args.push(p.clone());
            }
        }
    }

    if cmd_args.len() == 1 {
        return ToolCallOutcome::err(
            "at least one of branch, create_branch, commit, or paths is required",
        );
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

    let text = if exec.success {
        if exec.stdout.trim().is_empty() && exec.stderr.trim().is_empty() {
            "ok".to_string()
        } else if !exec.stderr.trim().is_empty() {
            exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, &text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitStash` MCP tool request.
///
/// Executes `git stash` to save and restore work in progress.
pub async fn handle_git_stash(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitStashRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        index: Option<u32>,
        #[serde(default)]
        include_untracked: Option<bool>,
    }

    let req = match ToolCallOutcome::parse_args::<GitStashRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let action = req.action.as_deref().unwrap_or("push");

    let mut cmd_args: Vec<String> = vec!["stash".into()];

    match action {
        "push" | "save" => {
            cmd_args.push("push".into());
            if req.include_untracked.unwrap_or(false) {
                cmd_args.push("-u".into());
            }
            if let Some(msg) = &req.message {
                cmd_args.push("-m".into());
                cmd_args.push(msg.clone());
            }
        }
        "pop" => {
            cmd_args.push("pop".into());
            if let Some(idx) = req.index {
                cmd_args.push(format!("stash@{{{idx}}}"));
            }
        }
        "apply" => {
            cmd_args.push("apply".into());
            if let Some(idx) = req.index {
                cmd_args.push(format!("stash@{{{idx}}}"));
            }
        }
        "drop" => {
            cmd_args.push("drop".into());
            if let Some(idx) = req.index {
                cmd_args.push(format!("stash@{{{idx}}}"));
            }
        }
        "list" => {
            cmd_args.push("list".into());
        }
        "show" => {
            cmd_args.push("show".into());
            cmd_args.push("-p".into());
            if let Some(idx) = req.index {
                cmd_args.push(format!("stash@{{{idx}}}"));
            }
        }
        "clear" => {
            cmd_args.push("clear".into());
        }
        _ => {
            return ToolCallOutcome::err(format!(
                "unknown stash action '{action}'. Valid: push, pop, apply, drop, list, show, clear"
            ));
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

    let text = if exec.success {
        if exec.stdout.trim().is_empty() {
            match action {
                "list" => "no stashes".to_string(),
                _ => "ok".to_string(),
            }
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("action", json!(action));

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}
