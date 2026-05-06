use super::super::run_git;
use super::super::types::build_git_response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS,
};

fn porcelain_status_is_clean(stdout: &str) -> bool {
    stdout
        .lines()
        .all(|line| line.trim().is_empty() || line.starts_with("##"))
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

    let clean = if exec.success && porcelain {
        porcelain_status_is_clean(&exec.stdout)
    } else if exec.success {
        let mut clean_args: Vec<String> = vec!["status".into(), "--porcelain=1".into()];
        if branch {
            clean_args.push("-b".into());
        }
        if !untracked {
            clean_args.push("-uno".into());
        }
        match run_git(
            req.working_dir.clone(),
            clean_args,
            timeout_ms,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        {
            Ok(clean_exec) if clean_exec.success => porcelain_status_is_clean(&clean_exec.stdout),
            _ => false,
        }
    } else {
        false
    };
    let text = if exec.success {
        exec.stdout.trim_end_matches(&['\r', '\n'][..]).to_string()
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

#[cfg(test)]
mod tests {
    use super::{handle_git_status, porcelain_status_is_clean};
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
            .join("../target/tools-mcp-git-tests")
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

    #[test]
    fn porcelain_clean_ignores_branch_header() {
        assert!(porcelain_status_is_clean("## main\n"));
        assert!(porcelain_status_is_clean("## main...origin/main\n\n"));
        assert!(!porcelain_status_is_clean("## main\n M src/lib.rs\n"));
        assert!(!porcelain_status_is_clean("?? new.txt\n"));
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
}
