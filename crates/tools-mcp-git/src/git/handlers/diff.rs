use super::super::run_git;
use super::super::types::build_git_response;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
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

fn parse_name_status_z(stdout: &str) -> Result<Vec<DiffManifestEntry>, String> {
    let tokens: Vec<&str> = stdout
        .split('\0')
        .filter(|token| !token.is_empty())
        .collect();
    let mut idx = 0usize;
    let mut entries = Vec::new();

    while idx < tokens.len() {
        let status_token = tokens[idx];
        idx += 1;
        let Some(status_code) = status_token.chars().next() else {
            return Err("git diff --name-status returned an empty status token".to_string());
        };

        match status_code {
            'A' | 'D' | 'M' | 'T' => {
                let path = tokens.get(idx).ok_or_else(|| {
                    format!("git diff --name-status missing path for status {status_token}")
                })?;
                idx += 1;
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
                let old_path = tokens.get(idx).ok_or_else(|| {
                    format!("git diff --name-status missing old path for status {status_token}")
                })?;
                let new_path = tokens.get(idx + 1).ok_or_else(|| {
                    format!("git diff --name-status missing new path for status {status_token}")
                })?;
                idx += 2;
                entries.push(DiffManifestEntry {
                    path: (*new_path).to_string(),
                    status: "renamed".to_string(),
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
    let tokens: Vec<&str> = stdout
        .split('\0')
        .filter(|token| !token.is_empty())
        .collect();
    let mut idx = 0usize;

    while idx < tokens.len() {
        let stats = tokens[idx];
        idx += 1;
        let parts: Vec<&str> = stats.split('\t').collect();
        if parts.len() < 3 {
            return Err(format!(
                "git diff --numstat returned malformed record: {stats:?}"
            ));
        }

        let binary = parts[0] == "-" && parts[1] == "-";
        let insertions = if binary {
            0
        } else {
            parts[0].parse::<u32>().map_err(|err| {
                format!(
                    "git diff --numstat invalid insertions value {:?}: {err}",
                    parts[0]
                )
            })?
        };
        let deletions = if binary {
            0
        } else {
            parts[1].parse::<u32>().map_err(|err| {
                format!(
                    "git diff --numstat invalid deletions value {:?}: {err}",
                    parts[1]
                )
            })?
        };

        let raw_path = parts[2..].join("\t");
        let (path, old_path) = if raw_path.is_empty() {
            let old_path = tokens.get(idx).ok_or_else(|| {
                "git diff --numstat missing old path for rename record".to_string()
            })?;
            let new_path = tokens.get(idx + 1).ok_or_else(|| {
                "git diff --numstat missing new path for rename record".to_string()
            })?;
            idx += 2;
            ((*new_path).to_string(), Some((*old_path).to_string()))
        } else {
            (raw_path, None)
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
    timeout_ms: u64,
) -> Result<Vec<DiffManifestEntry>, String> {
    let common_args = vec![
        "diff".into(),
        format!("{from_ref}..{to_ref}"),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--find-renames".into(),
    ];

    let mut name_status_args = common_args.clone();
    name_status_args.push("--name-status".into());
    name_status_args.push("-z".into());
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

    let mut numstat_args = common_args;
    numstat_args.push("--numstat".into());
    numstat_args.push("-z".into());
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
    timeout_ms: u64,
) -> Result<Value, String> {
    let out_path = Path::new(output_dir);
    fs::create_dir_all(out_path)
        .await
        .map_err(|e| format!("Failed to create output directory: {e}"))?;

    let entries = collect_ref_diff_manifest(working_dir, from_ref, to_ref, timeout_ms).await?;

    let mut files: Vec<FileDiffEntry> = Vec::new();
    let mut total_insertions: u32 = 0;
    let mut total_deletions: u32 = 0;

    for entry in entries {
        total_insertions += entry.insertions;
        total_deletions += entry.deletions;

        let patch_filename = format!("{}.patch", sanitize_path_for_filename(&entry.path));
        let patch_path = out_path.join(&patch_filename);

        let mut patch_args = vec![
            "diff".into(),
            format!("{from_ref}..{to_ref}"),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--find-renames".into(),
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

    Ok(json!(summary))
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
                    "Diff between {from_ref} and {to_ref}: {files_changed} files changed. Patches written to {output_dir}"
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

    if req.from_ref.is_some() || req.to_ref.is_some() {
        if req.output_dir.is_none() {
            return ToolCallOutcome::err("output_dir is required when using from_ref and to_ref");
        }
        if req.from_ref.is_none() || req.to_ref.is_none() {
            return ToolCallOutcome::err("both from_ref and to_ref are required together");
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

    let payload = build_git_response(&exec, &text, Some(extra_fields));
    ToolCallOutcome::ok(payload)
}

#[cfg(test)]
mod tests {
    use super::{apply_numstat_z, parse_name_status_z};

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
}
