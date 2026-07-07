use super::super::types::GitExecResult;
use super::super::types::GitStdinSummary;
use super::super::{build_git_args, run_git, trim_git_line_end};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatusEntry {
    index_status: char,
    worktree_status: char,
    path: String,
    original_path: Option<String>,
}

impl StatusEntry {
    fn is_untracked(&self) -> bool {
        self.index_status == '?' && self.worktree_status == '?'
    }

    fn is_staged(&self) -> bool {
        !matches!(self.index_status, ' ' | '?' | '!')
    }

    fn is_unstaged(&self) -> bool {
        !matches!(self.worktree_status, ' ' | '?' | '!')
    }

    fn is_conflicted(&self) -> bool {
        matches!(
            (self.index_status, self.worktree_status),
            ('D', 'D')
                | ('A', 'U')
                | ('U', 'D')
                | ('U', 'A')
                | ('D', 'U')
                | ('A', 'A')
                | ('U', 'U')
        )
    }
}

#[derive(Default)]
struct StatusParse {
    branch: Option<String>,
    entries: Vec<StatusEntry>,
}

#[derive(Default)]
struct StatusCounts {
    staged: usize,
    unstaged: usize,
    untracked: usize,
    conflicted: usize,
}

/// Handle the `git_snapshot` MCP tool request.
///
/// Runs a read-only bundle of Git commands commonly used for worktree triage:
/// porcelain status, unstaged diffstat, and staged diffstat.
pub async fn handle_git_snapshot(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GitSnapshotRequest {
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        untracked: Option<bool>,
        #[serde(default)]
        include_diff_stats: Option<bool>,
        #[serde(default)]
        paths: Option<Vec<String>>,
    }

    let req = match ToolCallOutcome::parse_args::<GitSnapshotRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let untracked = req.untracked.unwrap_or(true);
    let include_diff_stats = req.include_diff_stats.unwrap_or(false);
    let paths = match requested_paths(req.paths) {
        Ok(paths) => paths,
        Err(o) => return o,
    };

    let status_exec = match run_git(
        req.working_dir.clone(),
        build_status_args(untracked, &paths),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(exec) => exec,
        Err(err) => return ToolCallOutcome::err(format!("git error: {err:#}")),
    };

    if !status_exec.success {
        let text = first_non_empty(&status_exec.stderr, &status_exec.stdout);
        return ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": text}],
            "isError": true,
            "working_dir": status_exec.working_dir,
            "status": command_summary(&status_exec),
        }));
    }

    let parsed = parse_porcelain_status(&status_exec.stdout);
    let counts = count_status_entries(&parsed.entries);
    let clean = parsed.entries.is_empty();

    let (unstaged_diff, staged_diff) = if include_diff_stats {
        let (unstaged, staged) = if clean && !status_exec.truncated_stdout {
            (
                empty_diff_stat_exec(&status_exec, false, &paths),
                empty_diff_stat_exec(&status_exec, true, &paths),
            )
        } else {
            let unstaged = run_diff_stat(req.working_dir.clone(), false, &paths, timeout_ms);
            let staged = run_diff_stat(req.working_dir.clone(), true, &paths, timeout_ms);
            let (unstaged, staged) = tokio::join!(unstaged, staged);
            let unstaged = match unstaged {
                Ok(exec) => exec,
                Err(err) => return ToolCallOutcome::err(format!("git diff --stat error: {err:#}")),
            };
            let staged = match staged {
                Ok(exec) => exec,
                Err(err) => {
                    return ToolCallOutcome::err(format!(
                        "git diff --cached --stat error: {err:#}"
                    ));
                }
            };
            (unstaged, staged)
        };

        if !unstaged.success || !staged.success {
            let text = if !unstaged.success {
                first_non_empty(&unstaged.stderr, &unstaged.stdout)
            } else {
                first_non_empty(&staged.stderr, &staged.stdout)
            };
            return ToolCallOutcome::ok(json!({
                "content": [{"type": "text", "text": text}],
                "isError": true,
                "working_dir": status_exec.working_dir,
                "status": command_summary(&status_exec),
                "unstaged_diff": command_summary(&unstaged),
                "staged_diff": command_summary(&staged),
            }));
        }

        (Some(unstaged), Some(staged))
    } else {
        (None, None)
    };

    let text = render_snapshot_text(
        parsed.branch.as_deref(),
        clean,
        &status_exec.stdout,
        unstaged_diff.as_ref().map(|exec| exec.stdout.as_str()),
        staged_diff.as_ref().map(|exec| exec.stdout.as_str()),
    );

    let payload = json!({
        "content": [{"type": "text", "text": text}],
        "isError": false,
        "working_dir": status_exec.working_dir,
        "clean": clean,
        "branch": parsed.branch,
        "counts": {
            "staged": counts.staged,
            "unstaged": counts.unstaged,
            "untracked": counts.untracked,
            "conflicted": counts.conflicted,
        },
        "entries": parsed.entries
            .into_iter()
            .map(|entry| json!({
                "index_status": entry.index_status.to_string(),
                "worktree_status": entry.worktree_status.to_string(),
                "path": entry.path,
                "original_path": entry.original_path,
            }))
            .collect::<Vec<_>>(),
        "status": command_summary(&status_exec),
        "unstaged_diff": unstaged_diff.as_ref().map(command_summary),
        "staged_diff": staged_diff.as_ref().map(command_summary),
    });
    ToolCallOutcome::ok(payload)
}

fn requested_paths(paths: Option<Vec<String>>) -> Result<Vec<String>, ToolCallOutcome> {
    match paths {
        Some(paths) => {
            let paths: Vec<String> = paths
                .into_iter()
                .filter(|path| !path.trim().is_empty())
                .collect();
            if paths.is_empty() {
                return Err(ToolCallOutcome::err(
                    "paths must include at least one non-empty path",
                ));
            }
            Ok(paths)
        }
        None => Ok(Vec::new()),
    }
}

fn build_status_args(untracked: bool, paths: &[String]) -> Vec<String> {
    let mut args = vec![
        "status".to_string(),
        "--porcelain=1".to_string(),
        "-b".to_string(),
    ];
    if !untracked {
        args.push("-uno".to_string());
    }
    append_pathspec(&mut args, paths);
    args
}

fn build_diff_stat_args(cached: bool, paths: &[String]) -> Vec<String> {
    let mut args = vec![
        "diff".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--stat".to_string(),
    ];
    if cached {
        args.push("--cached".to_string());
    }
    append_pathspec(&mut args, paths);
    args
}

async fn run_diff_stat(
    working_dir: Option<String>,
    cached: bool,
    paths: &[String],
    timeout_ms: u64,
) -> Result<GitExecResult, anyhow::Error> {
    run_git(
        working_dir,
        build_diff_stat_args(cached, paths),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
}

fn empty_diff_stat_exec(
    status_exec: &GitExecResult,
    cached: bool,
    paths: &[String],
) -> GitExecResult {
    GitExecResult {
        git_bin: status_exec.git_bin.clone(),
        args: build_git_args(build_diff_stat_args(cached, paths)),
        working_dir: status_exec.working_dir.clone(),
        exit_code: Some(0),
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: Vec::new(),
        stderr_bytes: Vec::new(),
        truncated_stdout: false,
        truncated_stderr: false,
        timed_out: false,
        stdin: GitStdinSummary::none(),
    }
}

fn append_pathspec(args: &mut Vec<String>, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    args.push("--".to_string());
    args.extend(paths.iter().cloned());
}

fn parse_porcelain_status(stdout: &str) -> StatusParse {
    let mut parsed = StatusParse::default();
    for line in stdout.lines() {
        if let Some(branch) = line.strip_prefix("## ") {
            parsed.branch = Some(branch.to_string());
            continue;
        }
        if line.len() < 3 {
            continue;
        }

        let mut chars = line.chars();
        let index_status = chars.next().unwrap_or(' ');
        let worktree_status = chars.next().unwrap_or(' ');
        let Some(path_text) = line.get(3..) else {
            continue;
        };
        let (path, original_path) = parse_porcelain_path(path_text);
        parsed.entries.push(StatusEntry {
            index_status,
            worktree_status,
            path,
            original_path,
        });
    }
    parsed
}

fn parse_porcelain_path(path_text: &str) -> (String, Option<String>) {
    if let Some((original, path)) = path_text.split_once(" -> ") {
        (path.to_string(), Some(original.to_string()))
    } else {
        (path_text.to_string(), None)
    }
}

fn count_status_entries(entries: &[StatusEntry]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for entry in entries {
        if entry.is_untracked() {
            counts.untracked += 1;
        }
        if entry.is_staged() {
            counts.staged += 1;
        }
        if entry.is_unstaged() {
            counts.unstaged += 1;
        }
        if entry.is_conflicted() {
            counts.conflicted += 1;
        }
    }
    counts
}

fn render_snapshot_text(
    branch: Option<&str>,
    clean: bool,
    status_stdout: &str,
    unstaged_diff: Option<&str>,
    staged_diff: Option<&str>,
) -> String {
    let status = trim_git_line_end(status_stdout);
    let unstaged_diff = unstaged_diff.map(trim_git_line_end);
    let staged_diff = staged_diff.map(trim_git_line_end);
    let mut text = String::with_capacity(snapshot_text_capacity(
        branch,
        status,
        unstaged_diff,
        staged_diff,
    ));
    let _ = writeln!(text, "branch: {}", branch.unwrap_or("<unknown>"));
    let _ = writeln!(text, "clean: {clean}");
    text.push_str("status:\n");
    if status.is_empty() {
        text.push_str("  <clean>\n");
    } else {
        text.push_str(status);
        text.push('\n');
    }

    if let Some(diff) = unstaged_diff {
        text.push_str("\nunstaged diff stat:\n");
        push_optional_block(&mut text, diff, "  <none>");
    }
    if let Some(diff) = staged_diff {
        text.push_str("\nstaged diff stat:\n");
        push_optional_block(&mut text, diff, "  <none>");
    }
    text
}

fn snapshot_text_capacity(
    branch: Option<&str>,
    status: &str,
    unstaged_diff: Option<&str>,
    staged_diff: Option<&str>,
) -> usize {
    let mut capacity = "branch: \nclean: false\nstatus:\n".len()
        + branch.unwrap_or("<unknown>").len()
        + status.len();
    if let Some(diff) = unstaged_diff {
        capacity += "\nunstaged diff stat:\n".len() + diff.len().max("  <none>".len()) + 1;
    }
    if let Some(diff) = staged_diff {
        capacity += "\nstaged diff stat:\n".len() + diff.len().max("  <none>".len()) + 1;
    }
    capacity
}

fn push_optional_block(output: &mut String, block: &str, empty_text: &str) {
    if block.is_empty() {
        output.push_str(empty_text);
    } else {
        output.push_str(block);
    }
    output.push('\n');
}

fn first_non_empty(first: &str, second: &str) -> String {
    let first = trim_git_line_end(first);
    if first.is_empty() {
        trim_git_line_end(second).to_string()
    } else {
        first.to_string()
    }
}

fn command_summary(exec: &super::super::types::GitExecResult) -> Value {
    json!({
        "args": exec.args,
        "stdout": exec.stdout,
        "stderr": exec.stderr,
        "exit_code": exec.exit_code,
        "success": exec.success,
        "timed_out": exec.timed_out,
        "truncated_stdout": exec.truncated_stdout,
        "truncated_stderr": exec.truncated_stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        build_git_args,
        types::{GitExecResult, GitStdinSummary},
    };
    use super::{
        build_diff_stat_args, command_summary, count_status_entries, empty_diff_stat_exec,
        parse_porcelain_status, render_snapshot_text,
    };
    use serde_json::json;

    #[test]
    fn parse_porcelain_status_extracts_branch_and_entries() {
        let parsed = parse_porcelain_status(
            "## main...origin/main [ahead 1]\n M src/lib.rs\nA  new.rs\nR  old.rs -> moved.rs\n?? scratch.md\n",
        );

        assert_eq!(
            parsed.branch.as_deref(),
            Some("main...origin/main [ahead 1]")
        );
        assert_eq!(parsed.entries.len(), 4);
        assert_eq!(parsed.entries[0].path, "src/lib.rs");
        assert_eq!(parsed.entries[2].path, "moved.rs");
        assert_eq!(parsed.entries[2].original_path.as_deref(), Some("old.rs"));

        let counts = count_status_entries(&parsed.entries);
        assert_eq!(counts.staged, 2);
        assert_eq!(counts.unstaged, 1);
        assert_eq!(counts.untracked, 1);
        assert_eq!(counts.conflicted, 0);
    }

    #[test]
    fn render_snapshot_text_includes_empty_diff_sections() {
        let text = render_snapshot_text(Some("main"), true, "## main\n", Some(""), Some(""));

        assert!(text.contains("branch: main"));
        assert!(text.contains("clean: true"));
        assert!(text.contains("unstaged diff stat:\n  <none>"));
        assert!(text.contains("staged diff stat:\n  <none>"));
    }

    #[test]
    fn build_diff_stat_args_preserves_pathspec_and_cached_order() {
        let paths = vec!["src/lib.rs".to_string(), "README.md".to_string()];

        assert_eq!(
            build_diff_stat_args(true, &paths),
            vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--stat",
                "--cached",
                "--",
                "src/lib.rs",
                "README.md"
            ]
        );
    }

    #[test]
    fn empty_diff_stat_exec_matches_diff_command_summary_contract() {
        let paths = vec!["src/lib.rs".to_string()];
        let status_exec = GitExecResult {
            git_bin: "git".to_string(),
            args: Vec::new(),
            working_dir: Some("C:/repo".to_string()),
            exit_code: Some(0),
            success: true,
            stdout: "## main\n".to_string(),
            stderr: String::new(),
            stdout_bytes: b"## main\n".to_vec(),
            stderr_bytes: Vec::new(),
            truncated_stdout: false,
            truncated_stderr: false,
            timed_out: false,
            stdin: GitStdinSummary::none(),
        };

        let diff_exec = empty_diff_stat_exec(&status_exec, true, &paths);
        let summary = command_summary(&diff_exec);

        assert_eq!(
            diff_exec.args,
            build_git_args(build_diff_stat_args(true, &paths))
        );
        assert_eq!(diff_exec.working_dir, status_exec.working_dir);
        assert_eq!(summary["args"], json!(diff_exec.args));
        assert_eq!(summary["stdout"], "");
        assert_eq!(summary["stderr"], "");
        assert_eq!(summary["exit_code"], 0);
        assert_eq!(summary["success"], true);
        assert_eq!(summary["timed_out"], false);
        assert_eq!(summary["truncated_stdout"], false);
        assert_eq!(summary["truncated_stderr"], false);
    }
}
