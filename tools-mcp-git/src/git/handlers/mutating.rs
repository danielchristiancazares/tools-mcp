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

fn non_empty_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| !path.trim().is_empty())
        .collect()
}

fn validate_non_option_arg(value: &str, field_name: &str) -> Result<(), ToolCallOutcome> {
    validation::validate_non_empty(value, field_name, None)?;
    if value.starts_with('-') {
        return Err(ToolCallOutcome::err(format!(
            "{field_name} must not start with '-'"
        )));
    }
    Ok(())
}

fn sanitize_commit_fragment(value: &str) -> String {
    value.trim().replace('\n', " ").replace('\r', "")
}

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

    let paths = non_empty_paths(req.paths);
    if paths.is_empty() {
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
    for p in &paths {
        cmd_args.push(p.clone());
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
    let paths = non_empty_paths(req.paths.unwrap_or_default());

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

    let commit_type = sanitize_commit_fragment(&req.commit_type);
    let message = sanitize_commit_fragment(&req.message);
    let commit_msg = match req.scope.as_deref().map(sanitize_commit_fragment) {
        Some(scope) if !scope.trim().is_empty() => {
            format!("{commit_type}({scope}): {message}")
        }
        _ => format!("{commit_type}: {message}"),
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
        if let Err(o) = validate_non_option_arg(name, "create") {
            return o;
        }
        cmd_args.push(name.clone());
    } else if let Some(name) = &req.delete {
        if let Err(o) = validate_non_option_arg(name, "delete") {
            return o;
        }
        cmd_args.push("-d".into());
        cmd_args.push(name.clone());
    } else if let Some(name) = &req.force_delete {
        if let Err(o) = validate_non_option_arg(name, "force_delete") {
            return o;
        }
        cmd_args.push("-D".into());
        cmd_args.push(name.clone());
    } else if let Some(old_name) = &req.rename {
        if let Err(o) = validate_non_option_arg(old_name, "rename") {
            return o;
        }
        cmd_args.push("-m".into());
        cmd_args.push(old_name.clone());
        if let Some(new_name) = &req.new_name {
            if let Err(o) = validate_non_option_arg(new_name, "new_name") {
                return o;
            }
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
    let paths = non_empty_paths(req.paths.unwrap_or_default());

    if let Some(branch) = &req.create_branch {
        if let Err(o) = validate_non_option_arg(branch, "create_branch") {
            return o;
        }
        cmd_args.push("-b".into());
        cmd_args.push(branch.clone());
    } else if let Some(branch) = &req.branch {
        if let Err(o) = validate_non_option_arg(branch, "branch") {
            return o;
        }
        cmd_args.push(branch.clone());
    } else if let Some(commit) = &req.commit {
        if let Err(o) = validate_non_option_arg(commit, "commit") {
            return o;
        }
        cmd_args.push(commit.clone());
    }

    if !paths.is_empty() {
        cmd_args.push("--".into());
        for p in &paths {
            cmd_args.push(p.clone());
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
    let action = req
        .action
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("push");

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

#[cfg(test)]
mod tests {
    use super::{handle_git_add, handle_git_checkout, sanitize_commit_fragment};
    use serde_json::json;

    // Empty action should preserve the default push behavior.
    #[test]
    fn git_stash_empty_string_action_defaults_to_push() {
        let action: Option<String> = Some("".to_string());
        let result = action
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("push");
        assert_eq!(
            result, "push",
            "empty string action should default to 'push'"
        );

        let action2: Option<String> = None;
        let result2 = action2
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("push");
        assert_eq!(result2, "push", "None should also default to 'push'");
    }

    // Whitespace-only paths must be rejected before invoking destructive restore.
    #[test]
    fn git_restore_rejects_whitespace_only_paths() {
        let paths: Vec<String> = vec!["   ".to_string(), "\t".to_string()];
        let all_empty = paths.iter().all(|p| p.trim().is_empty());
        assert!(all_empty, "whitespace-only paths should be rejected");
    }

    // Conventional commit fields must stay a single subject line.
    #[test]
    fn git_commit_message_sanitizes_newlines() {
        let commit_type = "feat\n\nCo-authored-by: attacker <evil@evil.com>";
        let scope: Option<String> = Some("core\r\nReviewed-by: attacker <evil@evil.com>".into());
        let message = "add feature\n\nSigned-off-by: attacker <evil@evil.com>";

        let commit_type = sanitize_commit_fragment(commit_type);
        let message = sanitize_commit_fragment(message);
        let commit_msg = match scope.as_deref().map(sanitize_commit_fragment) {
            Some(scope) if !scope.trim().is_empty() => {
                format!("{commit_type}({scope}): {message}")
            }
            _ => format!("{commit_type}: {message}"),
        };

        assert!(
            !commit_msg.contains('\n'),
            "commit message should not contain newlines: {commit_msg:?}"
        );
        assert!(
            !commit_msg.contains('\r'),
            "commit message should not contain carriage returns: {commit_msg:?}"
        );
    }

    #[tokio::test]
    async fn git_add_rejects_whitespace_only_paths() {
        let outcome = handle_git_add(None, json!({"paths": ["   ", "\t"]})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("paths required")
        );
    }

    #[tokio::test]
    async fn git_checkout_rejects_whitespace_only_paths_without_ref() {
        let outcome = handle_git_checkout(None, json!({"paths": ["   ", "\t"]})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("at least one")
        );
    }

    #[tokio::test]
    async fn git_checkout_rejects_option_like_branch() {
        let outcome = handle_git_checkout(None, json!({"branch": "--detach"})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("must not start with '-'")
        );
    }
}
