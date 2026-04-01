//! Git tool handler implementations.
//!
//! Each handler corresponds to an MCP tool and delegates to [`super::run_git`]
//! for actual Git command execution.

use super::run_git;
use super::types::build_git_response;
use crate::tool_outcome::ToolCallOutcome;
use crate::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
use crate::validation;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use tokio::fs;

/// Sanitize a file path for use as a filename (replace path separators).
fn sanitize_path_for_filename(path: &str) -> String {
    path.replace(['/', '\\'], "__")
}

/// File diff entry for the summary JSON.
#[derive(Serialize)]
struct FileDiffEntry {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
    insertions: u32,
    deletions: u32,
    patch_file: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    binary: bool,
}

/// Summary JSON structure written to _summary.json.
#[derive(Serialize)]
struct DiffSummary {
    from_ref: String,
    to_ref: String,
    generated_at: String,
    files: Vec<FileDiffEntry>,
    summary: DiffStats,
}

/// Aggregate diff statistics.
#[derive(Serialize)]
struct DiffStats {
    files_changed: usize,
    insertions: u32,
    deletions: u32,
}

/// Parse a line from `git diff --numstat` output.
/// Format: "<insertions>\t<deletions>\t<path>" or "-\t-\t<path>" for binary.
fn parse_numstat_line(line: &str) -> Option<(u32, u32, String, bool)> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 3 {
        return None;
    }
    let path = parts[2..].join("\t"); // Handle paths with tabs
    if parts[0] == "-" && parts[1] == "-" {
        // Binary file
        Some((0, 0, path, true))
    } else {
        let ins = parts[0].parse().ok()?;
        let del = parts[1].parse().ok()?;
        Some((ins, del, path, false))
    }
}

/// Write per-file patches to a directory and generate _summary.json.
async fn write_patches_to_dir(
    working_dir: Option<&str>,
    from_ref: &str,
    to_ref: &str,
    output_dir: &str,
    timeout_ms: u64,
) -> Result<Value, String> {
    // Create output directory
    let out_path = Path::new(output_dir);
    fs::create_dir_all(out_path)
        .await
        .map_err(|e| format!("Failed to create output directory: {e}"))?;

    // Get file list with numstat
    let numstat_args = vec![
        "diff".into(),
        format!("{from_ref}..{to_ref}"),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--numstat".into(),
    ];
    let numstat_exec = run_git(
        working_dir.map(|s| s.to_string()),
        numstat_args,
        timeout_ms,
        MAX_OUTPUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|e| format!("git numstat error: {e:#}"))?;

    if !numstat_exec.success {
        return Err(format!(
            "git diff --numstat failed: {}",
            numstat_exec.stderr.trim()
        ));
    }

    let mut files: Vec<FileDiffEntry> = Vec::new();
    let mut total_insertions: u32 = 0;
    let mut total_deletions: u32 = 0;

    // Parse numstat output and get patches for each file
    for line in numstat_exec.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some((ins, del, path, is_binary)) = parse_numstat_line(line) else {
            continue;
        };

        total_insertions += ins;
        total_deletions += del;

        let patch_filename = format!("{}.patch", sanitize_path_for_filename(&path));
        let patch_path = out_path.join(&patch_filename);

        // Get the patch for this file
        let patch_args = vec![
            "diff".into(),
            format!("{from_ref}..{to_ref}"),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--".into(),
            path.clone(),
        ];
        let patch_exec = run_git(
            working_dir.map(|s| s.to_string()),
            patch_args,
            timeout_ms,
            MAX_OUTPUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .map_err(|e| format!("git diff error for {path}: {e:#}"))?;

        let patch_content = if is_binary {
            format!("Binary file: {path}\n")
        } else {
            patch_exec.stdout.clone()
        };

        // Write patch file
        fs::write(&patch_path, &patch_content)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", patch_path.display()))?;

        // Determine status from diff header
        let status = if patch_exec.stdout.contains("new file mode") {
            "added"
        } else if patch_exec.stdout.contains("deleted file mode") {
            "deleted"
        } else if patch_exec.stdout.contains("rename from") {
            "renamed"
        } else {
            "modified"
        };

        // Extract old path for renames
        let old_path = if status == "renamed" {
            patch_exec
                .stdout
                .lines()
                .find(|l| l.starts_with("rename from "))
                .map(|l| l.strip_prefix("rename from ").unwrap_or("").to_string())
        } else {
            None
        };

        files.push(FileDiffEntry {
            path,
            status: status.to_string(),
            old_path,
            insertions: ins,
            deletions: del,
            patch_file: patch_filename,
            binary: is_binary,
        });
    }

    // Write _summary.json
    let summary = DiffSummary {
        from_ref: from_ref.to_string(),
        to_ref: to_ref.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        summary: DiffStats {
            files_changed: files.len(),
            insertions: total_insertions,
            deletions: total_deletions,
        },
        files,
    };

    let summary_json = serde_json::to_string_pretty(&summary)
        .map_err(|e| format!("Failed to serialize summary: {e}"))?;
    let summary_path = out_path.join("_summary.json");
    fs::write(&summary_path, &summary_json)
        .await
        .map_err(|e| format!("Failed to write summary: {e}"))?;

    Ok(json!(summary))
}

/// Handle the `GitStatus` MCP tool request.
///
/// Executes `git status` and returns working tree state in a structured format.
/// By default, uses porcelain output for reliable machine parsing.
pub async fn handle_git_status(_id: Option<Value>, args: Value) -> ToolCallOutcome {
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

    let req = match ToolCallOutcome::parse_args::<GitStatusRequest>(args) {
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

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitDiff` MCP tool request.
///
/// Executes `git diff` to show changes between commits, the staging area, and
/// the working tree. Supports various output formats and path filtering.
///
/// When `from_ref` and `to_ref` are provided with `output_dir`, writes per-file
/// patches to the specified directory along with a `_summary.json` file.
pub async fn handle_git_diff(_id: Option<Value>, args: Value) -> ToolCallOutcome {
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
        #[serde(default)]
        from_ref: Option<String>,
        #[serde(default)]
        to_ref: Option<String>,
        #[serde(default)]
        output_dir: Option<String>,
    }

    let req = match ToolCallOutcome::parse_args::<GitDiffRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);

    // Handle ref-to-ref diff with file output
    if let (Some(from_ref), Some(to_ref), Some(output_dir)) =
        (&req.from_ref, &req.to_ref, &req.output_dir)
    {
        match write_patches_to_dir(
            req.working_dir.as_deref(),
            from_ref,
            to_ref,
            output_dir,
            timeout_ms,
        )
        .await
        {
            Ok(summary) => {
                let files_changed = summary["summary"]["files_changed"].as_u64().unwrap_or(0);
                let text = format!(
                    "Diff between {} and {}: {} files changed. Patches written to {}",
                    from_ref, to_ref, files_changed, output_dir
                );
                let mut response = serde_json::Map::new();
                response.insert(
                    "content".to_string(),
                    json!([{"type": "text", "text": text}]),
                );
                response.insert("isError".to_string(), json!(false));
                response.insert("from_ref".to_string(), json!(from_ref));
                response.insert("to_ref".to_string(), json!(to_ref));
                response.insert("output_dir".to_string(), json!(output_dir));
                response.insert("summary".to_string(), summary["summary"].clone());
                response.insert("files".to_string(), summary["files"].clone());
                return ToolCallOutcome::ok(Value::Object(response));
            }
            Err(e) => return ToolCallOutcome::err(e),
        }
    }

    // Validate: if from_ref or to_ref provided without output_dir, it's an error
    if req.from_ref.is_some() || req.to_ref.is_some() {
        if req.output_dir.is_none() {
            return ToolCallOutcome::err("output_dir is required when using from_ref and to_ref");
        }
        if req.from_ref.is_none() || req.to_ref.is_none() {
            return ToolCallOutcome::err("both from_ref and to_ref are required together");
        }
    }

    // Standard diff behavior (working tree / staging area)
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec![
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
    ];

    if req.cached.unwrap_or(false) {
        cmd_args.push("--cached".into());
    }
    if req.stat.unwrap_or(false) {
        cmd_args.push("--stat".into());
    }
    if req.name_only.unwrap_or(false) {
        cmd_args.push("--name-only".into());
    }
    if let Some(u) = req.unified
        && u >= 0
    {
        cmd_args.push(format!("-U{u}"));
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
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

    let mut extra_fields = HashMap::new();
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitRestore` MCP tool request.
///
/// Executes `git restore` to discard uncommitted changes. This is a **destructive
/// operation** that cannot be undone for working tree changes.
pub async fn handle_git_restore(_id: Option<Value>, args: Value) -> ToolCallOutcome {
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

    let req = match ToolCallOutcome::parse_args::<GitRestoreRequest>(args) {
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

    let payload = build_git_response(&exec, text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitAdd` MCP tool request.
///
/// Executes `git add` to stage files for the next commit. Supports staging
/// specific paths, all changes, or only tracked file updates.
pub async fn handle_git_add(_id: Option<Value>, args: Value) -> ToolCallOutcome {
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

    let req = match ToolCallOutcome::parse_args::<GitAddRequest>(args) {
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

    let payload = build_git_response(&exec, text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitCommit` MCP tool request.
///
/// Creates a Git commit using the Conventional Commits format. The commit message
/// is automatically formatted as `type(scope): message` or `type: message`.
pub async fn handle_git_commit(_id: Option<Value>, args: Value) -> ToolCallOutcome {
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

    let req = match ToolCallOutcome::parse_args::<GitCommitRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.commit_type, "type", None) {
        return o;
    }
    if let Err(o) = validation::validate_non_empty(&req.message, "message", None) {
        return o;
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
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

    let mut extra_fields = HashMap::new();
    extra_fields.insert("commit_message", json!(commit_msg));
    extra_fields.insert("commit_hash", json!(commit_hash));

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitLog` MCP tool request.
///
/// Executes `git log` to show commit history with configurable format and filters.
pub async fn handle_git_log(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    struct GitLogRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        max_count: Option<u32>,
        #[serde(default)]
        oneline: Option<bool>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        until: Option<String>,
        #[serde(default)]
        grep: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitLogRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["log".into()];

    if let Some(n) = req.max_count {
        cmd_args.push(format!("-{n}"));
    }
    if req.oneline.unwrap_or(false) {
        cmd_args.push("--oneline".into());
    }
    if let Some(fmt) = &req.format {
        cmd_args.push(format!("--format={fmt}"));
    }
    if let Some(author) = &req.author {
        cmd_args.push(format!("--author={author}"));
    }
    if let Some(since) = &req.since {
        cmd_args.push(format!("--since={since}"));
    }
    if let Some(until) = &req.until {
        cmd_args.push(format!("--until={until}"));
    }
    if let Some(grep) = &req.grep {
        cmd_args.push(format!("--grep={grep}"));
    }
    if let Some(path) = &req.path {
        cmd_args.push("--".into());
        cmd_args.push(path.clone());
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = if exec.success {
        if exec.stdout.trim().is_empty() {
            "no commits".to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitBranch` MCP tool request.
///
/// Executes `git branch` to list, create, or delete branches.
pub async fn handle_git_branch(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
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

    let req = match ToolCallOutcome::parse_args::<GitBranchRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let mut cmd_args: Vec<String> = vec!["branch".into()];

    // Determine the operation
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
        // List mode
        if req.list_all.unwrap_or(false) {
            cmd_args.push("-a".into());
        } else if req.list_remote.unwrap_or(false) {
            cmd_args.push("-r".into());
        }
        cmd_args.push("-v".into()); // Show commit info
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

    let payload = build_git_response(&exec, text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitCheckout` MCP tool request.
///
/// Executes `git checkout` to switch branches or restore files.
pub async fn handle_git_checkout(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
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

    let req = match ToolCallOutcome::parse_args::<GitCheckoutRequest>(args) {
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
            // git checkout often outputs to stderr even on success
            exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
        } else {
            exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
        }
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let payload = build_git_response(&exec, text, None);
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitStash` MCP tool request.
///
/// Executes `git stash` to save and restore work in progress.
pub async fn handle_git_stash(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    struct GitStashRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        action: Option<String>, // push, pop, apply, drop, list, show, clear
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        index: Option<u32>,
        #[serde(default)]
        include_untracked: Option<bool>,
    }

    let req = match ToolCallOutcome::parse_args::<GitStashRequest>(args) {
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
            cmd_args.push("-p".into()); // Show patch
            if let Some(idx) = req.index {
                cmd_args.push(format!("stash@{{{idx}}}"));
            }
        }
        "clear" => {
            cmd_args.push("clear".into());
        }
        _ => {
            return ToolCallOutcome::err(format!(
                "unknown stash action '{}'. Valid: push, pop, apply, drop, list, show, clear",
                action
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

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitShow` MCP tool request.
///
/// Executes `git show` to display a commit's contents.
pub async fn handle_git_show(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    struct GitShowRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        commit: Option<String>,
        #[serde(default)]
        stat: Option<bool>,
        #[serde(default)]
        name_only: Option<bool>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitShowRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["show".into()];

    if let Some(commit) = &req.commit {
        cmd_args.push(commit.clone());
    }
    if req.stat.unwrap_or(false) {
        cmd_args.push("--stat".into());
    }
    if req.name_only.unwrap_or(false) {
        cmd_args.push("--name-only".into());
    }
    if let Some(fmt) = &req.format {
        cmd_args.push(format!("--format={fmt}"));
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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = if exec.success {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

/// Handle the `GitBlame` MCP tool request.
///
/// Executes `git blame` to show line-by-line authorship.
pub async fn handle_git_blame(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    struct GitBlameRequest {
        path: String,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        start_line: Option<u32>,
        #[serde(default)]
        end_line: Option<u32>,
        #[serde(default)]
        commit: Option<String>,
        #[serde(default)]
        max_bytes: Option<usize>,
    }

    let req = match ToolCallOutcome::parse_args::<GitBlameRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);

    let mut cmd_args: Vec<String> = vec!["blame".into()];

    // Line range
    if let (Some(start), Some(end)) = (req.start_line, req.end_line) {
        cmd_args.push(format!("-L{start},{end}"));
    } else if let Some(start) = req.start_line {
        cmd_args.push(format!("-L{start},"));
    }

    // Specific commit
    if let Some(commit) = &req.commit {
        cmd_args.push(commit.clone());
    }

    cmd_args.push("--".into());
    cmd_args.push(req.path.clone());

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
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let text = if exec.success {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else if !exec.stderr.trim().is_empty() {
        exec.stderr.trim_end_matches(&['\r', '\n'][..]).to_string()
    } else {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
    };

    let mut extra_fields = HashMap::new();
    extra_fields.insert("path", json!(req.path));
    extra_fields.insert("max_bytes", json!(max_bytes));

    let payload = build_git_response(&exec, text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}
