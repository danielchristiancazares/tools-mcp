use super::super::run_git;
use super::super::types::{
    PorcelainStatusSummary, build_git_response_with_extra_fields, git_response_text,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GitStatusOptions {
    porcelain: bool,
    branch: bool,
    untracked: bool,
}

impl GitStatusOptions {
    fn from_optional(
        porcelain: Option<bool>,
        branch: Option<bool>,
        untracked: Option<bool>,
    ) -> Self {
        Self {
            porcelain: porcelain.unwrap_or(true),
            branch: branch.unwrap_or(true),
            untracked: untracked.unwrap_or(true),
        }
    }
}

fn build_status_args(options: GitStatusOptions) -> Vec<String> {
    let mut cmd_args: Vec<String> = vec!["status".into()];
    if options.porcelain {
        cmd_args.push("--porcelain=1".into());
        if options.branch {
            cmd_args.push("-b".into());
        }
        if !options.untracked {
            cmd_args.push("-uno".into());
        }
    }
    cmd_args
}

fn build_porcelain_clean_probe_args() -> Vec<String> {
    vec!["status".into(), "--porcelain=1".into()]
}

fn porcelain_status_is_clean(stdout: &str) -> bool {
    PorcelainStatusSummary::parse_v1(stdout).is_clean()
}

fn clean_from_primary_status_output(porcelain: bool, stdout: &str) -> Option<bool> {
    if porcelain {
        Some(porcelain_status_is_clean(stdout))
    } else {
        None
    }
}

fn clean_from_status_outputs(
    porcelain: bool,
    stdout: &str,
    probe_clean: Result<bool, String>,
) -> bool {
    match clean_from_primary_status_output(porcelain, stdout) {
        Some(clean) => clean,
        None => probe_clean.unwrap_or(false),
    }
}

async fn probe_porcelain_status_clean(
    working_dir: Option<String>,
    timeout_ms: u64,
) -> Result<bool, String> {
    let exec = run_git(
        working_dir,
        build_porcelain_clean_probe_args(),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|e| format!("git status --porcelain probe error: {e:#}"))?;

    if !exec.success {
        let detail = if !exec.stderr.trim().is_empty() {
            exec.stderr.trim()
        } else {
            exec.stdout.trim()
        };
        return Err(format!("git status --porcelain probe failed: {detail}"));
    }

    Ok(porcelain_status_is_clean(&exec.stdout))
}

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
    let status_options = GitStatusOptions::from_optional(req.porcelain, req.branch, req.untracked);

    let exec = match run_git(
        req.working_dir.clone(),
        build_status_args(status_options),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolCallOutcome::err(format!("git error: {e:#}")),
    };

    let clean = if !exec.success {
        false
    } else if status_options.porcelain {
        clean_from_status_outputs(status_options.porcelain, &exec.stdout, Ok(false))
    } else {
        clean_from_status_outputs(
            status_options.porcelain,
            &exec.stdout,
            probe_porcelain_status_clean(exec.working_dir.clone(), timeout_ms).await,
        )
    };
    let text = git_response_text(&exec);

    let payload = build_git_response_with_extra_fields(&exec, &text, [("clean", json!(clean))]);
    ToolCallOutcome::ok(payload)
}

#[cfg(test)]
mod tests {
    use super::{
        GitStatusOptions, build_status_args, clean_from_primary_status_output,
        clean_from_status_outputs, handle_git_status, porcelain_status_is_clean,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_bin() -> &'static str {
        if cfg!(target_os = "windows") {
            "git.exe"
        } else {
            "git"
        }
    }

    fn git_available() -> bool {
        Command::new(git_bin())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/tools-mcp-git-tests")
            .join(format!("{name}-{}-{nanos}", std::process::id()))
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new(git_bin())
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git command should start");
        assert!(status.success(), "git {args:?} failed");
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn status_args_default_to_porcelain_with_branch_header() {
        let args = build_status_args(GitStatusOptions::from_optional(None, None, None));

        assert_eq!(args, strings(&["status", "--porcelain=1", "-b"]));
    }

    #[test]
    fn status_args_preserve_untracked_suppression_in_porcelain_mode() {
        let args = build_status_args(GitStatusOptions::from_optional(
            Some(true),
            Some(false),
            Some(false),
        ));

        assert_eq!(args, strings(&["status", "--porcelain=1", "-uno"]));
    }

    #[test]
    fn status_args_ignore_porcelain_only_flags_in_human_mode() {
        let args = build_status_args(GitStatusOptions::from_optional(
            Some(false),
            Some(false),
            Some(false),
        ));

        assert_eq!(args, strings(&["status"]));
    }

    #[test]
    fn porcelain_clean_ignores_branch_header() {
        assert!(porcelain_status_is_clean("## main\n"));
        assert!(porcelain_status_is_clean("## main...origin/main\n\n"));
        assert!(!porcelain_status_is_clean("## main\n M src/lib.rs\n"));
        assert!(!porcelain_status_is_clean("?? new.txt\n"));
    }

    #[test]
    fn human_status_output_is_not_used_for_clean_detection() {
        let localized_clean_output =
            "En la rama main\nnada para confirmar, el arbol de trabajo esta limpio\n";

        assert_eq!(
            clean_from_primary_status_output(false, localized_clean_output),
            None
        );
        assert_eq!(clean_from_primary_status_output(true, ""), Some(true));
    }

    #[test]
    fn non_porcelain_clean_probe_failure_falls_back_to_dirty() {
        let human_clean_output = "On branch main\nnothing to commit, working tree clean\n";

        assert!(!clean_from_status_outputs(
            false,
            human_clean_output,
            Err("probe timed out".to_string())
        ));
        assert!(clean_from_status_outputs(
            false,
            human_clean_output,
            Ok(true)
        ));
        assert!(clean_from_status_outputs(
            true,
            "",
            Err("probe should not matter".to_string())
        ));
    }

    #[tokio::test]
    async fn git_status_default_branch_header_still_reports_clean() {
        if !git_available() {
            eprintln!("Skipping GitStatus clean test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("status-clean-with-branch");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("tracked.txt"), "tracked\n").expect("write tracked file");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);

        let outcome = handle_git_status(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string()
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(outcome.0["clean"], true);
        let stdout = outcome.0["stdout"].as_str().expect("stdout");
        let text = outcome.0["content"][0]["text"]
            .as_str()
            .expect("content text");
        assert_eq!(text, stdout.trim_end_matches(&['\r', '\n'][..]));
        assert!(
            stdout.starts_with("##"),
            "expected branch line in porcelain stdout"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_status_non_porcelain_reports_clean() {
        if !git_available() {
            eprintln!("Skipping GitStatus non-porcelain clean test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("status-clean-non-porcelain");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("tracked.txt"), "tracked\n").expect("write tracked file");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);

        let outcome = handle_git_status(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "porcelain": false
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(outcome.0["clean"], true);
        let stdout = outcome.0["stdout"].as_str().expect("stdout");
        let text = outcome.0["content"][0]["text"]
            .as_str()
            .expect("content text");
        assert_eq!(text, stdout.trim_end_matches(&['\r', '\n'][..]));
        assert!(
            !text.is_empty(),
            "expected human-readable git status output, got empty text"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_status_non_porcelain_reports_dirty_from_porcelain_probe() {
        if !git_available() {
            eprintln!("Skipping GitStatus non-porcelain dirty test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("status-dirty-non-porcelain");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("tracked.txt"), "tracked\n").expect("write tracked file");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);
        std::fs::write(repo.join("tracked.txt"), "modified\n").expect("modify tracked file");

        let outcome = handle_git_status(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "porcelain": false
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(outcome.0["clean"], false);
        let text = outcome.0["content"][0]["text"]
            .as_str()
            .expect("content text");
        assert!(
            text.contains("Changes not staged for commit") || text.contains("modified:"),
            "expected human-readable dirty status output, got {text:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_status_non_porcelain_clean_detection_is_locale_stable() {
        if !git_available() {
            eprintln!("Skipping GitStatus locale-stable clean test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("status-clean-non-porcelain-locale-stable");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("tracked.txt"), "tracked\n").expect("write tracked file");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);

        let outcome = handle_git_status(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "porcelain": false
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(outcome.0["clean"], true);
        let args = outcome.0["args"].as_array().expect("args");
        assert!(
            args.iter().all(|arg| arg.as_str() != Some("--porcelain=1")),
            "primary status output should remain human-readable: {args:?}"
        );
        let stdout = outcome.0["stdout"].as_str().expect("stdout");
        assert!(
            !stdout.is_empty(),
            "expected human-readable git status output, got empty stdout"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_status_reports_canonical_working_dir_for_symlinked_directory() {
        use std::os::unix::fs as unix_fs;

        if !git_available() {
            eprintln!("Skipping GitStatus symlink working_dir test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("status-symlink-working-dir");
        let repo = root.join("repo");
        let repo_link = root.join("repo-link");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        unix_fs::symlink(&repo, &repo_link).expect("symlink repo");

        let outcome = handle_git_status(
            None,
            json!({
                "working_dir": repo_link.to_string_lossy().to_string()
            }),
        )
        .await;

        let expected = repo.canonicalize().expect("canonical repo");
        let expected = expected.to_string_lossy().to_string();
        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(outcome.0["working_dir"].as_str(), Some(expected.as_str()));

        let _ = std::fs::remove_dir_all(root);
    }
}
