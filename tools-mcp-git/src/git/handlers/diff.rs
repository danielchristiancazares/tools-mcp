use super::super::path_policy;
use super::super::run_git;
use super::super::types::build_git_response;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use tokio::fs;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS, MAX_OUTPUT_BYTES,
};
use tools_mcp_core::validation;

/// Sanitize a file path for use as a filename (replace path separators).
fn sanitize_path_for_filename(path: &str) -> String {
    path.replace(['/', '\\'], "__")
}

fn unique_patch_filename(
    base: &str,
    used_filenames: &mut HashSet<String>,
    next_suffix_by_base: &mut HashMap<String, usize>,
) -> String {
    let first = format!("{base}.patch");
    if used_filenames.insert(first.clone()) {
        return first;
    }

    let next_suffix = next_suffix_by_base.entry(base.to_string()).or_insert(2);
    for suffix in *next_suffix.. {
        let candidate = format!("{base}.{suffix}.patch");
        if used_filenames.insert(candidate.clone()) {
            *next_suffix = suffix + 1;
            return candidate;
        }
    }

    unreachable!("unbounded suffix search must find an unused patch filename")
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
    insertions: u64,
    deletions: u64,
}

#[derive(Debug, Clone)]
struct DiffManifestEntry {
    path: String,
    status: String,
    old_path: Option<String>,
    insertions: u32,
    deletions: u32,
    binary: bool,
}

fn diff_manifest_key(path: &str, old_path: Option<&str>) -> String {
    match old_path {
        Some(old_path) => format!("{old_path}\0{path}"),
        None => path.to_string(),
    }
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

fn build_ref_diff_args(
    from_ref: &str,
    to_ref: &str,
    mode_args: &[&str],
    paths: &[String],
) -> Vec<String> {
    let mut args = vec![
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--find-renames".into(),
    ];
    args.extend(mode_args.iter().map(|arg| (*arg).to_string()));
    args.push("--end-of-options".into());
    args.push(format!("{from_ref}..{to_ref}"));
    if !paths.is_empty() {
        args.push("--".into());
        args.extend(paths.iter().cloned());
    }
    args
}

fn parse_name_status_z(stdout: &str) -> Result<Vec<DiffManifestEntry>, String> {
    let mut tokens = stdout.split('\0').filter(|token| !token.is_empty());
    let mut entries = Vec::new();

    while let Some(status_token) = tokens.next() {
        let Some(status_code) = status_token.chars().next() else {
            return Err("git diff --name-status returned an empty status token".to_string());
        };

        match status_code {
            'A' | 'D' | 'M' | 'T' => {
                let path = tokens.next().ok_or_else(|| {
                    format!("git diff --name-status missing path for status {status_token}")
                })?;
                let status = match status_code {
                    'A' => "added",
                    'D' => "deleted",
                    _ => "modified",
                };
                entries.push(DiffManifestEntry {
                    path: (*path).to_string(),
                    status: status.to_string(),
                    old_path: None,
                    insertions: 0,
                    deletions: 0,
                    binary: false,
                });
            }
            'R' => {
                let old_path = tokens.next().ok_or_else(|| {
                    format!("git diff --name-status missing old path for status {status_token}")
                })?;
                let new_path = tokens.next().ok_or_else(|| {
                    format!("git diff --name-status missing new path for status {status_token}")
                })?;
                entries.push(DiffManifestEntry {
                    path: (*new_path).to_string(),
                    status: "renamed".to_string(),
                    old_path: Some((*old_path).to_string()),
                    insertions: 0,
                    deletions: 0,
                    binary: false,
                });
            }
            'C' => {
                let old_path = tokens.next().ok_or_else(|| {
                    format!("git diff --name-status missing old path for status {status_token}")
                })?;
                let new_path = tokens.next().ok_or_else(|| {
                    format!("git diff --name-status missing new path for status {status_token}")
                })?;
                entries.push(DiffManifestEntry {
                    path: (*new_path).to_string(),
                    status: "copied".to_string(),
                    old_path: Some((*old_path).to_string()),
                    insertions: 0,
                    deletions: 0,
                    binary: false,
                });
            }
            _ => {
                return Err(format!(
                    "git diff --name-status returned unsupported status token {status_token}"
                ));
            }
        }
    }

    Ok(entries)
}

fn apply_numstat_z(entries: &mut [DiffManifestEntry], stdout: &str) -> Result<(), String> {
    let mut entry_index: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            (
                diff_manifest_key(&entry.path, entry.old_path.as_deref()),
                idx,
            )
        })
        .collect();
    let mut tokens = stdout.split('\0').filter(|token| !token.is_empty());

    while let Some(stats) = tokens.next() {
        let mut parts = stats.splitn(3, '\t');
        let insertions_text = parts.next().unwrap_or_default();
        let Some(deletions_text) = parts.next() else {
            return Err(format!(
                "git diff --numstat returned malformed record: {stats:?}"
            ));
        };
        let Some(raw_path) = parts.next() else {
            return Err(format!(
                "git diff --numstat returned malformed record: {stats:?}"
            ));
        };

        let binary = insertions_text == "-" && deletions_text == "-";
        let insertions = if binary {
            0
        } else {
            insertions_text.parse::<u32>().map_err(|err| {
                format!(
                    "git diff --numstat invalid insertions value {:?}: {err}",
                    insertions_text
                )
            })?
        };
        let deletions = if binary {
            0
        } else {
            deletions_text.parse::<u32>().map_err(|err| {
                format!(
                    "git diff --numstat invalid deletions value {:?}: {err}",
                    deletions_text
                )
            })?
        };

        let (path, old_path) = if raw_path.is_empty() {
            let old_path = tokens.next().ok_or_else(|| {
                "git diff --numstat missing old path for rename record".to_string()
            })?;
            let new_path = tokens.next().ok_or_else(|| {
                "git diff --numstat missing new path for rename record".to_string()
            })?;
            ((*new_path).to_string(), Some((*old_path).to_string()))
        } else {
            (raw_path.to_string(), None)
        };

        let key = diff_manifest_key(&path, old_path.as_deref());
        let Some(entry_pos) = entry_index.remove(&key) else {
            return Err(format!(
                "git diff --numstat returned a path not present in --name-status: {path}"
            ));
        };
        let entry = &mut entries[entry_pos];
        entry.insertions = insertions;
        entry.deletions = deletions;
        entry.binary = binary;
    }

    Ok(())
}

async fn collect_ref_diff_manifest(
    working_dir: Option<&str>,
    from_ref: &str,
    to_ref: &str,
    paths: &[String],
    timeout_ms: u64,
) -> Result<Vec<DiffManifestEntry>, String> {
    let name_status_args = build_ref_diff_args(from_ref, to_ref, &["--name-status", "-z"], paths);
    let name_status_exec = run_git(
        working_dir.map(std::string::ToString::to_string),
        name_status_args,
        timeout_ms,
        MAX_OUTPUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|e| format!("git diff --name-status error: {e:#}"))?;
    if !name_status_exec.success {
        return Err(format!(
            "git diff --name-status failed: {}",
            name_status_exec.stderr.trim()
        ));
    }

    let mut entries = parse_name_status_z(&name_status_exec.stdout)?;

    let numstat_args = build_ref_diff_args(from_ref, to_ref, &["--numstat", "-z"], paths);
    let numstat_exec = run_git(
        working_dir.map(std::string::ToString::to_string),
        numstat_args,
        timeout_ms,
        MAX_OUTPUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|e| format!("git diff --numstat error: {e:#}"))?;
    if !numstat_exec.success {
        return Err(format!(
            "git diff --numstat failed: {}",
            numstat_exec.stderr.trim()
        ));
    }

    apply_numstat_z(&mut entries, &numstat_exec.stdout)?;
    Ok(entries)
}

/// Write per-file patches to a directory and generate _summary.json.
async fn write_patches_to_dir(
    working_dir: Option<&str>,
    from_ref: &str,
    to_ref: &str,
    output_dir: &str,
    paths: &[String],
    timeout_ms: u64,
) -> Result<(Value, String), String> {
    let out_path = path_policy::resolve_output_dir(output_dir)?;
    let effective_output_dir = out_path.display().to_string();
    fs::create_dir_all(&out_path)
        .await
        .map_err(|e| format!("Failed to create output directory: {e}"))?;

    let entries =
        collect_ref_diff_manifest(working_dir, from_ref, to_ref, paths, timeout_ms).await?;

    let mut files: Vec<FileDiffEntry> = Vec::new();
    let mut total_insertions: u64 = 0;
    let mut total_deletions: u64 = 0;
    let mut used_patch_filenames = HashSet::new();
    let mut next_patch_suffix_by_base = HashMap::new();

    for entry in entries {
        total_insertions += u64::from(entry.insertions);
        total_deletions += u64::from(entry.deletions);

        let patch_filename = unique_patch_filename(
            &sanitize_path_for_filename(&entry.path),
            &mut used_patch_filenames,
            &mut next_patch_suffix_by_base,
        );
        let patch_path = out_path.join(&patch_filename);

        let mut patch_args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--find-renames".into(),
            "--end-of-options".into(),
            format!("{from_ref}..{to_ref}"),
            "--".into(),
        ];
        if let Some(old_path) = &entry.old_path {
            patch_args.push(old_path.clone());
        }
        patch_args.push(entry.path.clone());
        let patch_exec = run_git(
            working_dir.map(std::string::ToString::to_string),
            patch_args,
            timeout_ms,
            MAX_OUTPUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .map_err(|e| format!("git diff error for {}: {e:#}", entry.path))?;
        if !patch_exec.success {
            return Err(format!(
                "git diff failed for {}: {}",
                entry.path,
                patch_exec.stderr.trim()
            ));
        }

        let patch_content = if patch_exec.stdout.is_empty() && entry.binary {
            format!("Binary file: {}\n", entry.path)
        } else {
            patch_exec.stdout
        };

        fs::write(&patch_path, &patch_content)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", patch_path.display()))?;

        files.push(FileDiffEntry {
            path: entry.path,
            status: entry.status,
            old_path: entry.old_path,
            insertions: entry.insertions,
            deletions: entry.deletions,
            patch_file: patch_filename,
            binary: entry.binary,
        });
    }

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

    Ok((json!(summary), effective_output_dir))
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
    #[serde(deny_unknown_fields)]
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

    let req = match ToolCallOutcome::parse_args::<GitDiffRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let paths = match requested_paths(req.paths) {
        Ok(paths) => paths,
        Err(o) => return o,
    };

    match (&req.from_ref, &req.to_ref, &req.output_dir) {
        (Some(from_ref), Some(to_ref), Some(output_dir)) => {
            for (field_name, value) in [
                ("from_ref", from_ref.as_str()),
                ("to_ref", to_ref.as_str()),
                ("output_dir", output_dir.as_str()),
            ] {
                if let Err(o) = validation::validate_non_empty(value, field_name, None) {
                    return o;
                }
            }

            match write_patches_to_dir(
                req.working_dir.as_deref(),
                from_ref,
                to_ref,
                output_dir,
                &paths,
                timeout_ms,
            )
            .await
            {
                Ok((summary, effective_output_dir)) => {
                    let files_changed = summary["summary"]["files_changed"].as_u64().unwrap_or(0);
                    let text = format!(
                        "Diff between {from_ref} and {to_ref}: {files_changed} files changed. Patches written to {effective_output_dir}"
                    );
                    let mut response = serde_json::Map::new();
                    response.insert(
                        "content".to_string(),
                        json!([{"type": "text", "text": text}]),
                    );
                    response.insert("isError".to_string(), json!(false));
                    response.insert("from_ref".to_string(), json!(from_ref));
                    response.insert("to_ref".to_string(), json!(to_ref));
                    response.insert("output_dir".to_string(), json!(effective_output_dir));
                    response.insert("summary".to_string(), summary["summary"].clone());
                    response.insert("files".to_string(), summary["files"].clone());
                    return ToolCallOutcome::ok(Value::Object(response));
                }
                Err(e) => return ToolCallOutcome::err(e),
            }
        }
        (None, None, None) => {}
        _ => {
            return ToolCallOutcome::err("from_ref, to_ref, and output_dir are required together");
        }
    }

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
    if let Some(u) = req.unified {
        if u < 0 {
            return ToolCallOutcome::err("unified must be >= 0");
        }
        cmd_args.push(format!("-U{u}"));
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

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_numstat_z, build_ref_diff_args, handle_git_diff, parse_name_status_z,
        unique_patch_filename,
    };
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
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

    fn create_two_commit_repo(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);

        std::fs::write(repo.join("a.txt"), "a1\n").expect("write a");
        std::fs::write(repo.join("b.txt"), "b1\n").expect("write b");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);

        std::fs::write(repo.join("a.txt"), "a2\n").expect("update a");
        std::fs::write(repo.join("b.txt"), "b2\n").expect("update b");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "second"]);
        repo
    }

    #[test]
    fn parse_name_status_z_handles_rename_entries() {
        let entries = parse_name_status_z("R100\0src/old.txt\0src/new.txt\0")
            .expect("rename entry should parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "renamed");
        assert_eq!(entries[0].old_path.as_deref(), Some("src/old.txt"));
        assert_eq!(entries[0].path, "src/new.txt");
    }

    #[test]
    fn apply_numstat_z_populates_rename_counts() {
        let mut entries = parse_name_status_z("R100\0src/old.txt\0src/new.txt\0")
            .expect("rename entry should parse");
        apply_numstat_z(&mut entries, "0\t0\t\0src/old.txt\0src/new.txt\0")
            .expect("rename numstat should parse");
        assert_eq!(entries[0].insertions, 0);
        assert_eq!(entries[0].deletions, 0);
        assert_eq!(entries[0].old_path.as_deref(), Some("src/old.txt"));
        assert_eq!(entries[0].path, "src/new.txt");
    }

    #[test]
    fn parse_name_status_z_handles_copy_entries() {
        let entries = parse_name_status_z("C100\0src/original.txt\0src/copy.txt\0")
            .expect("copy entry should parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "copied");
        assert_eq!(entries[0].old_path.as_deref(), Some("src/original.txt"));
        assert_eq!(entries[0].path, "src/copy.txt");
    }

    #[test]
    fn apply_numstat_z_populates_copy_counts() {
        let mut entries = parse_name_status_z("C100\0src/original.txt\0src/copy.txt\0")
            .expect("copy entry should parse");
        apply_numstat_z(&mut entries, "10\t5\t\0src/original.txt\0src/copy.txt\0")
            .expect("copy numstat should parse");
        assert_eq!(entries[0].insertions, 10);
        assert_eq!(entries[0].deletions, 5);
        assert_eq!(entries[0].old_path.as_deref(), Some("src/original.txt"));
        assert_eq!(entries[0].path, "src/copy.txt");
    }

    #[test]
    fn apply_numstat_z_preserves_tabs_in_paths() {
        let mut entries =
            parse_name_status_z("M\0src/file\tname.txt\0").expect("tabbed path entry should parse");
        apply_numstat_z(&mut entries, "3\t2\tsrc/file\tname.txt\0")
            .expect("tabbed path numstat should parse");
        assert_eq!(entries[0].insertions, 3);
        assert_eq!(entries[0].deletions, 2);
        assert_eq!(entries[0].path, "src/file\tname.txt");
    }

    #[test]
    fn ref_diff_args_place_modes_before_end_of_options() {
        let paths = vec!["src/lib.rs".to_string()];
        let args = build_ref_diff_args(
            "--output=target/side-effect",
            "HEAD",
            &["--name-status", "-z"],
            &paths,
        );
        assert_eq!(
            args,
            vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--name-status",
                "-z",
                "--end-of-options",
                "--output=target/side-effect..HEAD",
                "--",
                "src/lib.rs",
            ]
        );
    }

    #[test]
    fn unique_patch_filename_disambiguates_sanitized_path_collisions() {
        let mut used = HashSet::new();
        let mut next_suffix_by_base = HashMap::new();
        assert_eq!(
            unique_patch_filename("a__b.txt", &mut used, &mut next_suffix_by_base),
            "a__b.txt.patch"
        );
        assert_eq!(
            unique_patch_filename("a__b.txt", &mut used, &mut next_suffix_by_base),
            "a__b.txt.2.patch"
        );
        assert_eq!(
            unique_patch_filename("a__b.txt.2", &mut used, &mut next_suffix_by_base),
            "a__b.txt.2.2.patch"
        );
        assert_eq!(
            unique_patch_filename("a__b.txt", &mut used, &mut next_suffix_by_base),
            "a__b.txt.3.patch"
        );
    }

    #[tokio::test]
    async fn git_diff_rejects_whitespace_only_paths() {
        let outcome = handle_git_diff(None, json!({"paths": ["   ", "\t"]})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("paths must include")
        );
    }

    #[tokio::test]
    async fn git_diff_rejects_negative_unified_context() {
        let outcome = handle_git_diff(None, json!({"unified": -1})).await;
        assert_eq!(outcome.0["isError"], true);
        assert!(
            outcome.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unified must be >= 0")
        );
    }

    #[tokio::test]
    async fn git_diff_ref_export_requires_complete_non_empty_ref_tuple() {
        let incomplete = handle_git_diff(None, json!({"output_dir": "patches"})).await;
        assert_eq!(incomplete.0["isError"], true);
        assert!(
            incomplete.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("from_ref, to_ref, and output_dir are required together")
        );

        let empty_ref = handle_git_diff(
            None,
            json!({"from_ref": "   ", "to_ref": "HEAD", "output_dir": "patches"}),
        )
        .await;
        assert_eq!(empty_ref.0["isError"], true);
        assert!(
            empty_ref.0["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("from_ref is required")
        );
    }

    #[tokio::test]
    async fn git_diff_ref_export_honors_paths_filter() {
        if !git_available() {
            eprintln!("Skipping GitDiff path filter test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("diff-path-filter");
        let repo = create_two_commit_repo(&root);
        let patches = root.join("patches");

        let outcome = handle_git_diff(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "from_ref": "HEAD~1",
                "to_ref": "HEAD",
                "output_dir": patches.to_string_lossy().to_string(),
                "paths": ["a.txt"]
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        let files = outcome.0["files"].as_array().expect("files array");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "a.txt");
        assert!(patches.join("a.txt.patch").exists());
        assert!(!patches.join("b.txt.patch").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_diff_ref_export_reports_canonical_output_dir_for_symlinked_parent() {
        use std::os::unix::fs as unix_fs;

        if !git_available() {
            eprintln!("Skipping GitDiff symlink output_dir test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("diff-output-symlink");
        let repo = create_two_commit_repo(&root);
        let patch_target = root.join("patch-target");
        let patch_link = root.join("patch-link");
        std::fs::create_dir_all(&patch_target).expect("create patch target");
        unix_fs::symlink(&patch_target, &patch_link).expect("symlink patch target");

        let outcome = handle_git_diff(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "from_ref": "HEAD~1",
                "to_ref": "HEAD",
                "output_dir": patch_link.join("patches").to_string_lossy().to_string(),
                "paths": ["a.txt"]
            }),
        )
        .await;

        let expected = patch_target
            .canonicalize()
            .expect("canonical patch target")
            .join("patches");
        let expected_output_dir = expected.to_string_lossy().to_string();
        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        assert_eq!(
            outcome.0["output_dir"].as_str(),
            Some(expected_output_dir.as_str())
        );
        assert!(expected.join("_summary.json").exists());
        assert!(expected.join("a.txt.patch").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_diff_ref_export_uses_distinct_patch_files_for_sanitized_path_collisions() {
        if !git_available() {
            eprintln!("Skipping GitDiff patch collision test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("diff-patch-collision");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("a")).expect("create nested dir");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("a").join("b.txt"), "nested old\n").expect("write nested");
        std::fs::write(repo.join("a__b.txt"), "flat old\n").expect("write flat");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);

        std::fs::write(repo.join("a").join("b.txt"), "nested new\n").expect("update nested");
        std::fs::write(repo.join("a__b.txt"), "flat new\n").expect("update flat");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "second"]);

        let patches = root.join("patches");
        let outcome = handle_git_diff(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "from_ref": "HEAD~1",
                "to_ref": "HEAD",
                "output_dir": patches.to_string_lossy().to_string()
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], false, "{:?}", outcome.0);
        let files = outcome.0["files"].as_array().expect("files array");
        assert_eq!(files.len(), 2);
        let patch_files: HashSet<&str> = files
            .iter()
            .map(|file| file["patch_file"].as_str().expect("patch file"))
            .collect();
        assert_eq!(patch_files.len(), 2, "patch filenames must be unique");
        for patch_file in patch_files {
            assert!(
                patches.join(patch_file).exists(),
                "{patch_file} should exist"
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_diff_ref_export_does_not_treat_refs_as_options() {
        if !git_available() {
            eprintln!("Skipping GitDiff option-like ref test: git not found on PATH");
            return;
        }

        let root = unique_test_dir("diff-option-ref");
        let repo = create_two_commit_repo(&root);
        let patches = root.join("patches");
        let injected_output = root.join("side-effect..HEAD");

        let outcome = handle_git_diff(
            None,
            json!({
                "working_dir": repo.to_string_lossy().to_string(),
                "from_ref": "--output=../side-effect",
                "to_ref": "HEAD",
                "output_dir": patches.to_string_lossy().to_string()
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], true);
        assert!(
            !injected_output.exists(),
            "option-like ref must not create an output file"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
