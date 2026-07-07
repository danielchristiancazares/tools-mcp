use super::super::run_git;
use super::super::run_git_with_stdin;
use super::super::types::{build_git_response_with_is_error, git_response_text};
use super::hunks::{
    ChangeStatus, FileHunks, HunkRequestScope, ParsedDiff, ParsedHunk, RepoContext, display_path,
    enumerate_diff, hunk_body_key, hunk_lookup, invalid_request, parse_request, parse_unified_diff,
    resolve_repo_context, revalidate_repo_identity, structured_error, validate_literal_paths,
    validate_repo_relative_path, validate_timeout,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, MAX_GIT_ARG_BYTES, MAX_GIT_PATCH_PATHS,
    MAX_GIT_SELECTED_HUNKS, MAX_GIT_STDIN_BYTES, MAX_OUTPUT_BYTES,
};
use tools_mcp_core::validation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitApplyRequest {
    patch: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    check_only: Option<bool>,
    #[serde(default)]
    reverse: Option<bool>,
    #[serde(default)]
    three_way: Option<bool>,
    #[serde(default)]
    recount: Option<bool>,
    #[serde(default)]
    unidiff_zero: Option<bool>,
    #[serde(default)]
    whitespace: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyTarget {
    Cached,
    IndexWorktree,
    Worktree,
}

impl ApplyTarget {
    fn parse(value: Option<&str>) -> Result<Self, ToolCallOutcome> {
        match value.unwrap_or("cached") {
            "cached" => Ok(Self::Cached),
            "index_worktree" => Ok(Self::IndexWorktree),
            "worktree" => Ok(Self::Worktree),
            _ => Err(invalid_request(
                "target must be one of cached, index_worktree, or worktree",
                "target",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::IndexWorktree => "index_worktree",
            Self::Worktree => "worktree",
        }
    }

    fn writes_index(self) -> bool {
        matches!(self, Self::Cached | Self::IndexWorktree)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageAction {
    PrepareCommit,
    StageOnly,
    Unstage,
}

impl StageAction {
    fn parse(value: Option<&str>) -> Result<Self, ToolCallOutcome> {
        match value.unwrap_or("prepare_commit") {
            "prepare_commit" => Ok(Self::PrepareCommit),
            "stage_only" => Ok(Self::StageOnly),
            "unstage" => Ok(Self::Unstage),
            _ => Err(invalid_request(
                "action must be one of prepare_commit, stage_only, or unstage",
                "action",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::PrepareCommit => "prepare_commit",
            Self::StageOnly => "stage_only",
            Self::Unstage => "unstage",
        }
    }

    fn source_staged(self) -> bool {
        self == Self::Unstage
    }

    fn target_staged(self) -> bool {
        !self.source_staged()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitStageHunksRequest {
    diff_id: String,
    hunk_ids: Vec<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    commit_type: Option<String>,
    #[serde(default)]
    commit_scope: Option<String>,
    #[serde(default)]
    commit_message: Option<String>,
}

pub async fn handle_git_apply(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match parse_request::<GitApplyRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    let target = match ApplyTarget::parse(req.target.as_deref()) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let whitespace = match req.whitespace.as_deref().unwrap_or("nowarn") {
        "nowarn" | "warn" | "fix" | "error" | "error-all" => {
            req.whitespace.as_deref().unwrap_or("nowarn").to_string()
        }
        _ => return invalid_request("invalid whitespace mode", "whitespace"),
    };
    let timeout_ms = match validate_timeout(req.timeout_ms) {
        Ok(timeout) => timeout,
        Err(outcome) => return outcome,
    };
    if req.three_way.unwrap_or(false) && target == ApplyTarget::Worktree {
        return structured_error(
            "incompatible_options",
            "three_way=true is valid only for cached and index_worktree targets",
            vec![(
                "remediation",
                json!("Use target cached or index_worktree, or set three_way=false."),
            )],
        );
    }

    if req.patch.len() > MAX_GIT_STDIN_BYTES {
        return structured_error(
            "stdin_too_large",
            "patch exceeds MAX_GIT_STDIN_BYTES",
            vec![("max_bytes", json!(MAX_GIT_STDIN_BYTES))],
        );
    }
    if req.patch.trim().is_empty() {
        return structured_error("empty_patch", "patch must be non-empty", vec![]);
    }
    if req.patch.as_bytes().contains(&0) {
        return structured_error(
            "invalid_patch_path",
            "patch must not contain NUL bytes",
            vec![],
        );
    }

    let repo = match resolve_repo_context(req.working_dir.as_deref(), timeout_ms).await {
        Ok(repo) => repo,
        Err(outcome) => return outcome,
    };
    let patch_paths = match validate_supported_patch(&req.patch, &repo).await {
        Ok(paths) => paths,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = ensure_unmerged_index_absent(&repo, timeout_ms).await {
        return outcome;
    }
    if let Err(outcome) = ensure_tracked_regular_files(&repo, &patch_paths, timeout_ms).await {
        return outcome;
    }
    if target != ApplyTarget::Cached {
        for path in &patch_paths {
            if let Err(outcome) = validate_worktree_regular_file(&repo.toplevel, path) {
                return outcome;
            }
        }
    }

    let check_only = req.check_only.unwrap_or(false);
    let reverse = req.reverse.unwrap_or(false);
    let three_way = req.three_way.unwrap_or(false);
    let apply_args = build_git_apply_args(GitApplyArgs {
        target,
        check_only,
        reverse,
        three_way,
        recount: req.recount.unwrap_or(true),
        unidiff_zero: req.unidiff_zero.unwrap_or(false),
        whitespace: &whitespace,
    });
    if let Err(outcome) = revalidate_repo_identity(&repo) {
        return outcome;
    }

    let exec = match run_git_with_stdin(
        Some(repo.working_dir.clone()),
        apply_args,
        Some(req.patch.into_bytes()),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    {
        Ok(exec) => exec,
        Err(err) => {
            return structured_error(
                "git_apply_unavailable",
                format!("failed to run git apply: {err:#}"),
                vec![],
            );
        }
    };
    if exec.success
        && exec.stdin.fully_delivered != Some(false)
        && let Err(outcome) = revalidate_repo_identity(&repo)
    {
        let error_type = outcome.0["error_type"]
            .as_str()
            .unwrap_or("repo_identity_changed");
        let payload = build_git_response_with_is_error(
            &exec,
            "git apply completed but repository identity could not be verified",
            true,
            vec![
                ("state", json!("state_unknown")),
                ("applied", json!(false)),
                ("checked", json!(false)),
                ("target", json!(target.as_str())),
                ("reverse", json!(reverse)),
                ("three_way", json!(three_way)),
                ("state_unknown_reason", json!(error_type)),
                ("error_type", json!(error_type)),
                ("repo_identity_error", outcome.0),
                ("stdin_write_error", json!(exec.stdin.write_error)),
                ("stdin_write_broken_pipe", json!(exec.stdin.broken_pipe)),
                (
                    "remediation",
                    json!("Inspect GitStatus/GitDiff before further mutation."),
                ),
            ],
        );
        return ToolCallOutcome::ok(payload);
    }

    let index_locked = !exec.success
        && !exec.timed_out
        && target.writes_index()
        && apply_failed_on_index_lock(&repo, &exec, timeout_ms).await;
    let classification = if index_locked {
        ApplyClassification {
            state: "failed",
            is_error: true,
            state_unknown_reason: None,
            conflicted: None,
            conflict_probe_error: None,
        }
    } else {
        classify_apply_result(&repo, &exec, check_only, three_way, timeout_ms).await
    };
    let text = git_response_text(&exec);
    let checked = classification.state == "checked";
    let applied = classification.state == "applied";
    let mut extras = vec![
        ("state", json!(classification.state)),
        ("applied", json!(applied)),
        ("checked", json!(checked)),
        ("target", json!(target.as_str())),
        ("reverse", json!(reverse)),
        ("three_way", json!(three_way)),
        ("stdin_write_error", json!(exec.stdin.write_error)),
        ("stdin_write_broken_pipe", json!(exec.stdin.broken_pipe)),
    ];
    if let Some(reason) = classification.state_unknown_reason {
        extras.push(("state_unknown_reason", json!(reason)));
        extras.push(("error_type", json!(reason)));
    }
    if index_locked {
        extras.push(("error_type", json!("index_locked")));
    }
    if let Some(conflicted) = classification.conflicted {
        extras.push(("conflicted", json!(conflicted)));
    }
    if let Some(conflict_probe_error) = classification.conflict_probe_error {
        extras.push(("conflict_probe_error", json!(conflict_probe_error)));
    }
    let payload = build_git_response_with_is_error(&exec, &text, classification.is_error, extras);
    ToolCallOutcome::ok(payload)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplyClassification {
    state: &'static str,
    is_error: bool,
    state_unknown_reason: Option<&'static str>,
    conflicted: Option<bool>,
    conflict_probe_error: Option<&'static str>,
}

struct GitApplyArgs<'a> {
    target: ApplyTarget,
    check_only: bool,
    reverse: bool,
    three_way: bool,
    recount: bool,
    unidiff_zero: bool,
    whitespace: &'a str,
}

fn build_git_apply_args(options: GitApplyArgs<'_>) -> Vec<String> {
    let mut args = vec![
        "--no-optional-locks".to_string(),
        "-c".to_string(),
        "core.protectNTFS=true".to_string(),
        "-c".to_string(),
        "core.protectHFS=true".to_string(),
        "-c".to_string(),
        "apply.ignoreWhitespace=false".to_string(),
        "apply".to_string(),
    ];
    match options.target {
        ApplyTarget::Cached => args.push("--cached".to_string()),
        ApplyTarget::IndexWorktree => args.push("--index".to_string()),
        ApplyTarget::Worktree => {}
    }
    if options.check_only {
        args.push("--check".to_string());
    }
    if options.reverse {
        args.push("-R".to_string());
    }
    if options.three_way {
        args.push("--3way".to_string());
    }
    if options.recount {
        args.push("--recount".to_string());
    }
    if options.unidiff_zero {
        args.push("--unidiff-zero".to_string());
    }
    args.push(format!("--whitespace={}", options.whitespace));
    args
}

async fn classify_apply_result(
    repo: &RepoContext,
    exec: &super::super::types::GitExecResult,
    check_only: bool,
    three_way: bool,
    timeout_ms: u64,
) -> ApplyClassification {
    if exec.timed_out {
        return ApplyClassification {
            state: "state_unknown",
            is_error: true,
            state_unknown_reason: Some("timeout"),
            conflicted: None,
            conflict_probe_error: None,
        };
    }

    if !check_only && three_way && !exec.success {
        return match index_has_unmerged_entries(repo, timeout_ms).await {
            Ok(true) => ApplyClassification {
                state: "state_unknown",
                is_error: true,
                state_unknown_reason: Some("three_way_conflict"),
                conflicted: Some(true),
                conflict_probe_error: None,
            },
            Ok(false) => ApplyClassification {
                state: "state_unknown",
                is_error: true,
                state_unknown_reason: Some("three_way_indeterminate"),
                conflicted: None,
                conflict_probe_error: None,
            },
            Err(error_type) => ApplyClassification {
                state: "state_unknown",
                is_error: true,
                state_unknown_reason: Some("three_way_indeterminate"),
                conflicted: None,
                conflict_probe_error: Some(error_type),
            },
        };
    }

    if exec.success && exec.stdin.fully_delivered == Some(false) {
        return ApplyClassification {
            state: "state_unknown",
            is_error: true,
            state_unknown_reason: Some("stdin_write"),
            conflicted: None,
            conflict_probe_error: None,
        };
    }
    if check_only && exec.success {
        return ApplyClassification {
            state: "checked",
            is_error: false,
            state_unknown_reason: None,
            conflicted: None,
            conflict_probe_error: None,
        };
    }
    if !check_only && exec.success {
        return ApplyClassification {
            state: "applied",
            is_error: false,
            state_unknown_reason: None,
            conflicted: None,
            conflict_probe_error: None,
        };
    }

    if check_only {
        return ApplyClassification {
            state: "failed",
            is_error: true,
            state_unknown_reason: None,
            conflicted: None,
            conflict_probe_error: None,
        };
    }

    ApplyClassification {
        state: "state_unknown",
        is_error: true,
        state_unknown_reason: Some("unproved_git_nonzero"),
        conflicted: None,
        conflict_probe_error: None,
    }
}

async fn validate_supported_patch(
    patch: &str,
    repo: &RepoContext,
) -> Result<Vec<String>, ToolCallOutcome> {
    let parsed = match parse_unified_diff(patch.as_bytes(), repo, false, 3, &[]) {
        Ok(parsed) => parsed,
        Err(outcome) => {
            let parser_error_type = outcome.0["error_type"].as_str();
            let error_type = if parser_error_type == Some("diff_complexity_limit") {
                "patch_complexity_limit"
            } else {
                "unsupported_patch_record"
            };
            let message = if error_type == "patch_complexity_limit" {
                "patch exceeds supported complexity limits"
            } else {
                "patch is not a supported tracked textual modification diff"
            };
            return Err(structured_error(
                error_type,
                message,
                vec![("parser_error", outcome.0)],
            ));
        }
    };
    if parsed.files.is_empty() {
        return Err(structured_error(
            "empty_patch",
            "patch has no file records",
            vec![],
        ));
    }
    if parsed.files.len() > MAX_GIT_PATCH_PATHS {
        return Err(structured_error(
            "patch_complexity_limit",
            "patch contains too many file paths",
            vec![("max_paths", json!(MAX_GIT_PATCH_PATHS))],
        ));
    }

    let mut paths = Vec::new();
    let mut argv_bytes = 0usize;
    for file in parsed.files {
        if !file.supported_for_stage_hunks {
            if file.unsupported_reason.as_deref() == Some("invalid_path") {
                return Err(structured_error(
                    "invalid_patch_path",
                    "patch path is not a safe repo-relative POSIX path",
                    vec![("path", json!(file.path))],
                ));
            }
            return Err(structured_error(
                "unsupported_patch_record",
                "GitApply v1 accepts only tracked textual modified-file records",
                vec![
                    ("path", json!(file.path)),
                    ("unsupported_reason", json!(file.unsupported_reason)),
                ],
            ));
        }
        validate_repo_relative_path(&file.path).map_err(|reason| {
            structured_error(
                "invalid_patch_path",
                format!("invalid patch path {}: {reason}", file.path),
                vec![("path", json!(file.path))],
            )
        })?;
        if file
            .hunks
            .iter()
            .any(|hunk| !hunk_body_has_content_change(&hunk.body))
        {
            return Err(structured_error(
                "unsupported_patch_record",
                "GitApply v1 rejects no-op hunks with no added or deleted content lines",
                vec![
                    ("path", json!(file.path)),
                    ("unsupported_reason", json!("no_content_changes")),
                ],
            ));
        }
        argv_bytes = argv_bytes.saturating_add(file.path.len() + 1);
        paths.push(file.path);
    }
    if argv_bytes > MAX_GIT_ARG_BYTES {
        return Err(structured_error(
            "patch_complexity_limit",
            "patch target path arguments exceed byte limits",
            vec![
                ("argv_bytes", json!(argv_bytes)),
                ("max_argv_bytes", json!(MAX_GIT_ARG_BYTES)),
            ],
        ));
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn hunk_body_has_content_change(body: &str) -> bool {
    let mut line_start = true;
    for byte in body.as_bytes() {
        if line_start && (*byte == b'+' || *byte == b'-') {
            return true;
        }
        line_start = *byte == b'\n' || *byte == b'\r';
    }
    false
}

async fn ensure_unmerged_index_absent(
    repo: &RepoContext,
    timeout_ms: u64,
) -> Result<(), ToolCallOutcome> {
    let has_unmerged =
        index_has_unmerged_entries(repo, timeout_ms)
            .await
            .map_err(|error_type| {
                structured_error(
                    error_type,
                    "failed to prove the index has no unmerged entries",
                    vec![],
                )
            })?;
    if has_unmerged {
        return Err(structured_error(
            "unmerged_index",
            "GitApply and GitStageHunks v1 reject pre-existing unmerged index entries",
            vec![],
        ));
    }
    Ok(())
}

async fn index_has_unmerged_entries(
    repo: &RepoContext,
    timeout_ms: u64,
) -> Result<bool, &'static str> {
    if let Err(outcome) = revalidate_repo_identity(repo) {
        return Err(repo_identity_error_type(&outcome));
    }
    let exec = run_git(
        Some(repo.working_dir.clone()),
        vec!["ls-files".into(), "-u".into()],
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|_| "unmerged_index_probe_unavailable")?;
    if exec.timed_out {
        return Err("unmerged_index_probe_timeout");
    }
    if !exec.success {
        return Err("unmerged_index_probe_failed");
    }
    Ok(!exec.stdout.trim().is_empty())
}

fn repo_identity_error_type(outcome: &ToolCallOutcome) -> &'static str {
    match outcome.0["error_type"].as_str() {
        Some("git_metadata_outside_authority") => "git_metadata_outside_authority",
        _ => "repo_identity_changed",
    }
}

async fn ensure_tracked_regular_files(
    repo: &RepoContext,
    paths: &[String],
    timeout_ms: u64,
) -> Result<(), ToolCallOutcome> {
    revalidate_repo_identity(repo)?;
    let mut args = vec![
        "--literal-pathspecs".to_string(),
        "ls-files".to_string(),
        "-s".to_string(),
        "-v".to_string(),
        "--debug".to_string(),
        "-z".to_string(),
        "--".to_string(),
    ];
    args.extend(paths.iter().cloned());
    let exec = run_git(
        Some(repo.working_dir.clone()),
        args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "trackedness_preflight_unavailable",
            format!("failed to run trackedness preflight: {err:#}"),
            vec![],
        )
    })?;
    if exec.timed_out {
        return Err(structured_error(
            "trackedness_preflight_timeout",
            "trackedness preflight timed out",
            vec![],
        ));
    }
    if !exec.success {
        return Err(structured_error(
            "trackedness_preflight_failed",
            "trackedness preflight failed",
            vec![("stderr", json!(exec.stderr))],
        ));
    }

    let found = parse_ls_files_preflight_stdout(&exec.stdout_bytes);
    if found.len() != paths.len() {
        return Err(structured_error(
            "unsupported_patch_record",
            "trackedness preflight returned a path set different from the requested patch paths",
            vec![
                ("requested_paths", json!(paths)),
                (
                    "returned_paths",
                    json!(found.keys().cloned().collect::<Vec<_>>()),
                ),
            ],
        ));
    }
    for path in paths {
        match found.get(path) {
            Some(entry) if entry.intent_to_add => {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "patch path has the intent-to-add index flag, which is outside the v1 support matrix",
                    vec![("path", json!(path))],
                ));
            }
            Some(entry) if entry.skip_worktree => {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "patch path has the skip-worktree index flag, which is outside the v1 support matrix",
                    vec![("path", json!(path))],
                ));
            }
            Some(entry) if entry.assume_unchanged => {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "patch path has the assume-unchanged index flag, which is outside the v1 support matrix",
                    vec![("path", json!(path))],
                ));
            }
            Some(entry)
                if entry.stage == "0" && (entry.mode == "100644" || entry.mode == "100755") => {}
            Some(_) | None => {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "patch path is not a tracked regular file in the index",
                    vec![("path", json!(path))],
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexEntryPreflight {
    mode: String,
    object_id: String,
    stage: String,
    skip_worktree: bool,
    assume_unchanged: bool,
    intent_to_add: bool,
}

fn parse_ls_files_preflight_stdout(stdout: &[u8]) -> HashMap<String, IndexEntryPreflight> {
    let mut found = HashMap::new();
    let mut chunks = stdout.split(|byte| *byte == 0);
    let Some(first) = chunks.next() else {
        return found;
    };
    let mut current = parse_ls_files_preflight_entry_record(first);
    for chunk in chunks {
        let next_header = find_ls_files_entry_header(chunk);
        let debug_bytes = next_header.map_or(chunk, |offset| &chunk[..offset]);
        if let Some((path, mut entry)) = current.take() {
            if let Some(flags) = parse_git_debug_flags_bytes(debug_bytes) {
                entry.skip_worktree |= flags & 0x4000_0000 != 0;
                entry.intent_to_add |= flags & 0x2000_0000 != 0;
                entry.assume_unchanged |= flags & 0x0000_8000 != 0;
            }
            found.insert(path, entry);
        }
        if let Some(offset) = next_header {
            current = parse_ls_files_preflight_entry_record(&chunk[offset..]);
        }
    }
    found
}

fn find_ls_files_entry_header(chunk: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    while offset < chunk.len() {
        if parse_ls_files_preflight_entry_record(&chunk[offset..]).is_some() {
            return Some(offset);
        }
        let relative = chunk[offset..].iter().position(|byte| *byte == b'\n')?;
        offset = offset.saturating_add(relative + 1);
    }
    None
}

fn parse_ls_files_preflight_entry_record(record: &[u8]) -> Option<(String, IndexEntryPreflight)> {
    if record.first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let tab = record.iter().position(|byte| *byte == b'\t')?;
    let meta = std::str::from_utf8(&record[..tab]).ok()?;
    let path = String::from_utf8(record[tab + 1..].to_vec()).ok()?;
    if path.is_empty() {
        return None;
    }
    let mut fields = meta.split_whitespace();
    let tag = fields.next()?;
    let mode = fields.next()?.to_string();
    let object_id = fields.next()?.to_string();
    let stage = fields.next()?.to_string();
    if fields.next().is_some() {
        return None;
    }
    if tag.len() != 1 || !matches!(mode.as_str(), "100644" | "100755" | "120000" | "160000") {
        return None;
    }
    let tag = tag.chars().next()?;
    Some((
        path,
        IndexEntryPreflight {
            mode,
            object_id,
            stage,
            skip_worktree: tag == 'S' || tag == 's',
            assume_unchanged: tag.is_ascii_lowercase(),
            intent_to_add: false,
        },
    ))
}

fn parse_git_debug_flags(flags: &str) -> Option<u32> {
    let token = flags.split_whitespace().next()?;
    if token == "0" {
        return Some(0);
    }
    u32::from_str_radix(token, 16).ok()
}

fn parse_git_debug_flags_bytes(debug: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(debug).ok()?;
    for line in text.lines() {
        let Some((_, flags)) = line.split_once("flags:") else {
            continue;
        };
        if let Some(parsed) = parse_git_debug_flags(flags.trim_start()) {
            return Some(parsed);
        }
    }
    None
}

fn validate_worktree_regular_file(root: &Path, path: &str) -> Result<(), ToolCallOutcome> {
    let mut current = PathBuf::from(root);
    let components: Vec<&str> = path.split('/').collect();
    for (idx, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(|err| {
            structured_error(
                "unsupported_patch_record",
                format!(
                    "failed to inspect worktree path {}: {err}",
                    current.display()
                ),
                vec![("path", json!(path))],
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(structured_error(
                "unsupported_patch_record",
                "worktree-writing apply rejects symlink path components",
                vec![("path", json!(path))],
            ));
        }
        if is_reparse_point(&metadata) {
            return Err(structured_error(
                "unsupported_patch_record",
                "worktree-writing apply rejects reparse point path components",
                vec![("path", json!(path))],
            ));
        }
        if idx + 1 == components.len() {
            if !metadata.is_file() {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "worktree-writing apply target must be a regular file",
                    vec![("path", json!(path))],
                ));
            }
            let link_count = regular_file_link_count(&current, &metadata).map_err(|err| {
                structured_error(
                    "unsupported_patch_record",
                    format!(
                        "failed to inspect worktree file link count {}: {err}",
                        current.display()
                    ),
                    vec![("path", json!(path))],
                )
            })?;
            if link_count > 1 {
                return Err(structured_error(
                    "unsupported_patch_record",
                    "worktree-writing apply rejects hardlinked target files",
                    vec![("path", json!(path)), ("link_count", json!(link_count))],
                ));
            }
        } else if !metadata.is_dir() {
            return Err(structured_error(
                "unsupported_patch_record",
                "worktree-writing apply path ancestor must be a directory",
                vec![("path", json!(path))],
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn regular_file_link_count(_path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;

    Ok(metadata.nlink())
}

#[cfg(windows)]
fn regular_file_link_count(path: &Path, _metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    use std::fs::File;
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = File::open(path)?;
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let ok =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, info.as_mut_ptr()) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    Ok(u64::from(info.nNumberOfLinks))
}

#[cfg(not(any(unix, windows)))]
fn regular_file_link_count(_path: &Path, _metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    Ok(1)
}

pub async fn handle_git_stage_hunks(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match parse_request::<GitStageHunksRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    if !valid_diff_id(&req.diff_id) {
        return invalid_request("diff_id must match sha256:<64 lowercase hex>", "diff_id");
    }
    let hunk_ids = match validate_hunk_ids(req.hunk_ids.clone()) {
        Ok(ids) => ids,
        Err(outcome) => return outcome,
    };
    let action = match StageAction::parse(req.action.as_deref()) {
        Ok(action) => action,
        Err(outcome) => return outcome,
    };
    let timeout_ms = match validate_timeout(req.timeout_ms) {
        Ok(timeout) => timeout,
        Err(outcome) => return outcome,
    };
    let context = req.context.unwrap_or(3);
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);
    let paths = match validate_literal_paths(req.paths.clone().unwrap_or_default()) {
        Ok(paths) => paths,
        Err(outcome) => return outcome,
    };
    let repo = match resolve_repo_context(req.working_dir.as_deref(), timeout_ms).await {
        Ok(repo) => repo,
        Err(outcome) => return outcome,
    };

    if let Err(outcome) = ensure_unmerged_index_absent(&repo, timeout_ms).await {
        return outcome;
    }

    let source_scope = HunkRequestScope {
        staged: action.source_staged(),
        paths: paths.clone(),
        context,
        max_bytes,
        working_dir_arg: req.working_dir.clone(),
        timeout_ms,
    };
    let source = match enumerate_diff(&repo, &source_scope).await {
        Ok(diff) => diff,
        Err(outcome) => return outcome,
    };
    if source.diff_id != req.diff_id {
        let opposite_scope = HunkRequestScope {
            staged: !action.source_staged(),
            ..source_scope.clone()
        };
        let direction_check = enumerate_diff(&repo, &opposite_scope).await;
        match direction_check {
            Ok(opposite) if opposite.diff_id == req.diff_id => {
                return structured_error(
                    "direction_mismatch",
                    "diff_id matches the opposite staged/unstaged direction",
                    vec![("expected_staged", json!(source_scope.staged))],
                );
            }
            Ok(_) => {}
            Err(outcome) => {
                return structured_error(
                    "stale_diff",
                    "diff_id no longer matches the current scoped diff; re-run GitHunks",
                    vec![
                        ("source_diff_id", json!(req.diff_id)),
                        ("current_diff_id", json!(source.diff_id)),
                        ("direction_check_unavailable", json!(true)),
                        ("cause_error_type", outcome.0["error_type"].clone()),
                        ("cause", outcome.0),
                    ],
                );
            }
        }
        return structured_error(
            "stale_diff",
            "diff_id no longer matches the current scoped diff; re-run GitHunks",
            vec![
                ("source_diff_id", json!(req.diff_id)),
                ("current_diff_id", json!(source.diff_id)),
            ],
        );
    }

    let selected = match selected_hunks(&source, &hunk_ids) {
        Ok(selected) => selected,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = reject_ambiguous_subset(&source, &selected) {
        return outcome;
    }
    let selected_paths = selected_patch_paths(&selected);
    if let Err(outcome) = ensure_tracked_regular_files(&repo, &selected_paths, timeout_ms).await {
        return outcome;
    }
    if action == StageAction::PrepareCommit {
        let full_staged_scope = HunkRequestScope {
            staged: true,
            paths: Vec::new(),
            context,
            max_bytes,
            working_dir_arg: req.working_dir.clone(),
            timeout_ms,
        };
        match enumerate_diff(&repo, &full_staged_scope).await {
            Ok(full_staged) if full_staged.files.is_empty() => {}
            Ok(_) => {
                return structured_error(
                    "index_not_clean",
                    "prepare_commit requires the full index to be clean before staging",
                    vec![
                        ("commit_ready", json!(false)),
                        (
                            "remediation",
                            json!(
                                "Commit, unstage, or inspect existing staged changes before using prepare_commit."
                            ),
                        ),
                    ],
                );
            }
            Err(outcome) => return outcome,
        }
    }

    if let Err(outcome) = reject_mixed_direction_files(&repo, &source_scope, &selected).await {
        return outcome;
    }

    let target_scope = HunkRequestScope {
        staged: action.target_staged(),
        ..source_scope.clone()
    };
    let pre_target = match enumerate_diff(&repo, &target_scope).await {
        Ok(diff) => diff,
        Err(outcome) => return outcome,
    };
    let pre_full_unstaged = if action == StageAction::PrepareCommit {
        if !source_scope.staged && source_scope.paths.is_empty() {
            Some(source.clone())
        } else {
            let pre_full_unstaged_scope = HunkRequestScope {
                staged: false,
                paths: Vec::new(),
                ..source_scope.clone()
            };
            match enumerate_diff(&repo, &pre_full_unstaged_scope).await {
                Ok(diff) => Some(diff),
                Err(outcome) => return commit_group_verification_unavailable_before_apply(outcome),
            }
        }
    } else {
        None
    };

    let patch = match reconstruct_patch(&source, &selected) {
        Ok(patch) => patch,
        Err(outcome) => return outcome,
    };
    if patch.len() > MAX_GIT_STDIN_BYTES {
        return structured_error(
            "diff_complexity_limit",
            "reconstructed patch exceeds stdin byte limit",
            vec![("max_bytes", json!(MAX_GIT_STDIN_BYTES))],
        );
    }

    let unidiff_zero = context == 0;
    let preflight = match run_cached_apply(
        &repo,
        &patch,
        action == StageAction::Unstage,
        true,
        unidiff_zero,
        timeout_ms,
    )
    .await
    {
        Ok(exec) => exec,
        Err(outcome) => return outcome,
    };
    if preflight.timed_out {
        return stage_failure_from_exec(
            "preflight_timeout",
            "failed",
            &preflight,
            "git apply --check timed out",
        );
    }
    if preflight.stdin.fully_delivered == Some(false) {
        return stage_failure_from_exec(
            "preflight_stdin_write_failed",
            "failed",
            &preflight,
            "git apply --check stdin delivery failed",
        );
    }
    if !preflight.success {
        let error_type = if apply_failed_on_index_lock(&repo, &preflight, timeout_ms).await {
            "index_locked"
        } else {
            "preflight_failed"
        };
        return stage_failure_from_exec(
            error_type,
            "failed",
            &preflight,
            "git apply --check failed",
        );
    }

    let pre_index_blobs = match index_blobs_for_paths(&repo, &selected_paths, timeout_ms).await {
        Ok(blobs) => blobs,
        Err(outcome)
            if matches!(
                outcome.0["error_type"].as_str(),
                Some("repo_identity_changed" | "git_metadata_outside_authority")
            ) =>
        {
            return outcome;
        }
        Err(_) => {
            return structured_error(
                "preflight_unavailable",
                "pre-apply index blobs could not be captured for blob-level verification",
                vec![
                    ("commit_ready", json!(false)),
                    ("verification_state", json!("verification_unavailable")),
                    (
                        "remediation",
                        json!("Inspect GitStatus/GitDiff before further staging or committing."),
                    ),
                ],
            );
        }
    };

    let apply_exec = match run_cached_apply(
        &repo,
        &patch,
        action == StageAction::Unstage,
        false,
        unidiff_zero,
        timeout_ms,
    )
    .await
    {
        Ok(exec) => exec,
        Err(outcome) => return outcome,
    };

    if apply_exec.timed_out {
        return stage_failure_from_exec(
            "apply_timeout",
            "state_unknown",
            &apply_exec,
            "git apply timed out",
        );
    }
    if apply_exec.stdin.fully_delivered == Some(false) && apply_exec.success {
        return stage_failure_from_exec(
            "stdin_write_failed",
            "state_unknown",
            &apply_exec,
            "git apply stdin delivery failed",
        );
    }
    if !apply_exec.success {
        let error_type = if apply_failed_on_index_lock(&repo, &apply_exec, timeout_ms).await {
            "index_locked"
        } else {
            "git_apply_failed"
        };
        let state = if error_type == "index_locked" {
            "failed"
        } else {
            "state_unknown"
        };
        return stage_failure_from_exec(error_type, state, &apply_exec, "git apply failed");
    }

    let verification = verify_after_apply(
        &repo,
        VerificationInputs {
            source_scope: &source_scope,
            action,
            pre_source: &source,
            pre_target: &pre_target,
            pre_full_unstaged: pre_full_unstaged.as_ref(),
            selected: &selected,
            pre_index_blobs: &pre_index_blobs,
        },
    )
    .await;
    let (verification_state, commit_ready, is_error, error_type, verification_success) =
        match verification {
            Ok(success) => (
                "verified",
                action == StageAction::PrepareCommit,
                false,
                None,
                Some(success),
            ),
            Err(VerificationFailure::ScopedUnavailable) => (
                "verification_unavailable",
                false,
                true,
                Some("verification_unavailable"),
                None,
            ),
            Err(VerificationFailure::ScopedMismatch) => (
                "verification_mismatch",
                false,
                true,
                Some("verification_mismatch"),
                None,
            ),
            Err(VerificationFailure::CommitGroupUnavailable) => (
                "verification_unavailable",
                false,
                true,
                Some("commit_group_verification_unavailable"),
                None,
            ),
            Err(VerificationFailure::CommitGroupMismatch) => (
                "verification_mismatch",
                false,
                true,
                Some("commit_group_verification_mismatch"),
                None,
            ),
            Err(VerificationFailure::RepoIdentityChanged) => (
                "verification_unavailable",
                false,
                true,
                Some("repo_identity_changed"),
                None,
            ),
            Err(VerificationFailure::MetadataOutsideAuthority) => (
                "verification_unavailable",
                false,
                true,
                Some("git_metadata_outside_authority"),
                None,
            ),
        };

    let mut extras = vec![
        (
            "state",
            json!(if is_error { "state_unknown" } else { "applied" }),
        ),
        ("applied", json!(!is_error)),
        ("checked", json!(false)),
        ("action", json!(action.as_str())),
        ("source_diff_id", json!(req.diff_id)),
        ("pre_apply_diff_id", json!(source.diff_id)),
        ("requested_hunk_ids", json!(hunk_ids)),
        (
            "applied_hunk_ids",
            json!(
                selected
                    .iter()
                    .map(|(_, h)| h.id.clone())
                    .collect::<Vec<_>>()
            ),
        ),
        ("applied_patch_bytes", json!(patch.len())),
        ("verification_state", json!(verification_state)),
        ("commit_ready", json!(commit_ready)),
    ];
    if let Some(success) = &verification_success {
        extras.push((
            "post_apply_source_diff_id",
            json!(success.post_source_diff_id),
        ));
        extras.push((
            "post_apply_target_diff_id",
            json!(success.post_target_diff_id),
        ));
        let (post_staged_diff_id, post_unstaged_diff_id) = if source_scope.staged {
            (&success.post_source_diff_id, &success.post_target_diff_id)
        } else {
            (&success.post_target_diff_id, &success.post_source_diff_id)
        };
        extras.push(("post_apply_staged_diff_id", json!(post_staged_diff_id)));
        extras.push(("post_apply_unstaged_diff_id", json!(post_unstaged_diff_id)));
        if let Some(diff_id) = &success.post_full_staged_diff_id {
            extras.push(("post_apply_full_staged_diff_id", json!(diff_id)));
        }
        if let Some(diff_id) = &success.post_full_unstaged_diff_id {
            extras.push(("post_apply_full_unstaged_diff_id", json!(diff_id)));
        }
    }
    if let Some(error_type) = error_type {
        extras.push(("error_type", json!(error_type)));
        extras.push((
            "remediation",
            json!("Inspect GitStatus/GitDiff before further staging or committing."),
        ));
    }
    if commit_ready {
        extras.push(("full_index_clean_before", json!(true)));
        extras.push(("full_index_verified_after", json!(true)));
        let post_apply_full_staged_diff_id = verification_success
            .as_ref()
            .and_then(|success| success.post_full_staged_diff_id.clone());
        let post_apply_full_unstaged_diff_id = verification_success
            .as_ref()
            .and_then(|success| success.post_full_unstaged_diff_id.clone());
        extras.push((
            "pre_commit_verification",
            json!({
                "full_index_clean_before": true,
                "full_index_verified_after": true,
                "post_apply_full_staged_diff_id": post_apply_full_staged_diff_id,
                "post_apply_full_unstaged_diff_id": post_apply_full_unstaged_diff_id,
            }),
        ));
        extras.push((
            "commit_call_template",
            commit_call_template(req.working_dir.as_deref(), &req),
        ));
        extras.push((
            "next_actions",
            json!([
                "Fill any placeholder commit fields.",
                "Run GitCommit if hooks are trusted or controlled.",
                "Re-run GitStatus/GitDiff/GitHunks because hunk IDs are expired."
            ]),
        ));
    }
    let text = if commit_ready {
        "selected hunks staged and ready for GitCommit".to_string()
    } else if is_error {
        "selected hunks were applied but verification did not prove the requested final state"
            .to_string()
    } else {
        "selected hunks applied".to_string()
    };
    let payload = build_git_response_with_is_error(&apply_exec, &text, is_error, extras);
    ToolCallOutcome::ok(payload)
}

fn valid_diff_id(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn validate_hunk_ids(ids: Vec<String>) -> Result<Vec<String>, ToolCallOutcome> {
    if ids.is_empty() {
        return Err(invalid_request("hunk_ids must be non-empty", "hunk_ids"));
    }
    if ids.len() > MAX_GIT_SELECTED_HUNKS {
        return Err(structured_error(
            "malformed_hunk_ids",
            "too many hunk IDs selected",
            vec![("max_hunk_ids", json!(MAX_GIT_SELECTED_HUNKS))],
        ));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    for id in &ids {
        if id.len() > 96 || !valid_hunk_id(id) {
            return Err(structured_error(
                "malformed_hunk_ids",
                "hunk ID has invalid syntax",
                vec![("hunk_id", json!(id))],
            ));
        }
        if !seen.insert(id.clone()) {
            return Err(structured_error(
                "malformed_hunk_ids",
                "duplicate hunk ID",
                vec![("hunk_id", json!(id))],
            ));
        }
    }
    Ok(ids)
}

fn valid_hunk_id(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(file_idx) = parts.next() else {
        return false;
    };
    let Some(hunk_idx) = parts.next() else {
        return false;
    };
    let Some(hash) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || hash.len() != 64 {
        return false;
    }
    parse_canonical_usize(file_idx).is_some()
        && parse_canonical_usize(hunk_idx).is_some()
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn parse_canonical_usize(value: &str) -> Option<usize> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    value.parse::<usize>().ok()
}

fn selected_hunks<'a>(
    source: &'a ParsedDiff,
    hunk_ids: &[String],
) -> Result<Vec<(&'a FileHunks, &'a ParsedHunk)>, ToolCallOutcome> {
    let lookup = hunk_lookup(source);
    let mut selected = Vec::with_capacity(hunk_ids.len());
    let mut unsupported = Vec::new();
    let mut unknown = Vec::new();
    for id in hunk_ids {
        match lookup.get(id) {
            Some((file, hunk)) if file.supported_for_stage_hunks => selected.push((*file, *hunk)),
            Some((file, _)) => unsupported.push(json!({
                "hunk_id": id,
                "path": file.path,
                "unsupported_reason": file.unsupported_reason,
            })),
            None => unknown.push(id.clone()),
        }
    }
    if !unsupported.is_empty() {
        return Err(structured_error(
            "unsupported_hunk_ids",
            "one or more selected hunk IDs belong to unsupported records",
            vec![("unsupported_hunk_ids", json!(unsupported))],
        ));
    }
    if !unknown.is_empty() {
        return Err(structured_error(
            "unknown_hunk_ids",
            "one or more selected hunk IDs were not found in the current diff",
            vec![("unknown_hunk_ids", json!(unknown))],
        ));
    }
    Ok(selected)
}

fn selected_patch_paths(selected: &[(&FileHunks, &ParsedHunk)]) -> Vec<String> {
    let mut paths: Vec<String> = selected.iter().map(|(file, _)| file.path.clone()).collect();
    paths.sort();
    paths.dedup();
    paths
}

fn reject_ambiguous_subset(
    source: &ParsedDiff,
    selected: &[(&FileHunks, &ParsedHunk)],
) -> Result<(), ToolCallOutcome> {
    let mut total: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for file in &source.files {
        if file.supported_for_stage_hunks {
            for hunk in &file.hunks {
                *total.entry(hunk_body_key(file, hunk)).or_default() += 1;
            }
        }
    }
    let mut selected_counts: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for (file, hunk) in selected {
        *selected_counts
            .entry(hunk_body_key(file, hunk))
            .or_default() += 1;
    }
    for (key, selected_count) in selected_counts {
        let total_count = total.get(&key).copied().unwrap_or(0);
        if total_count > selected_count && selected_count != total_count {
            return Err(structured_error(
                "ambiguous_hunk_ids",
                "selected hunk subset contains duplicate same-path hunk bodies that v1 cannot disambiguate",
                vec![],
            ));
        }
    }
    Ok(())
}

async fn reject_mixed_direction_files(
    repo: &RepoContext,
    source_scope: &HunkRequestScope,
    selected: &[(&FileHunks, &ParsedHunk)],
) -> Result<(), ToolCallOutcome> {
    let selected_paths: BTreeSet<String> =
        selected.iter().map(|(file, _)| file.path.clone()).collect();
    let opposite_scope = HunkRequestScope {
        staged: !source_scope.staged,
        ..source_scope.clone()
    };
    let opposite = enumerate_diff(repo, &opposite_scope)
        .await
        .map_err(direction_check_unavailable)?;
    for file in opposite.files {
        if selected_paths.contains(&file.path)
            || file
                .old_path
                .as_ref()
                .is_some_and(|path| selected_paths.contains(path))
        {
            return Err(structured_error(
                "mixed_direction_file",
                "selected path also has changes in the opposite staged/unstaged direction",
                vec![("path", json!(file.path))],
            ));
        }
    }
    Ok(())
}

fn direction_check_unavailable(outcome: ToolCallOutcome) -> ToolCallOutcome {
    structured_error(
        "direction_check_unavailable",
        "opposite staged/unstaged diff could not be enumerated",
        vec![
            ("cause_error_type", outcome.0["error_type"].clone()),
            ("cause", outcome.0),
            (
                "remediation",
                json!("Inspect GitStatus/GitDiff and re-run GitHunks before staging hunks."),
            ),
        ],
    )
}

fn reconstruct_patch(
    source: &ParsedDiff,
    selected: &[(&FileHunks, &ParsedHunk)],
) -> Result<Vec<u8>, ToolCallOutcome> {
    let selected_ids: HashSet<&str> = selected.iter().map(|(_, hunk)| hunk.id.as_str()).collect();
    let mut patch = String::new();
    for file in &source.files {
        let file_hunks: Vec<&ParsedHunk> = file
            .hunks
            .iter()
            .filter(|hunk| selected_ids.contains(hunk.id.as_str()))
            .collect();
        if file_hunks.is_empty() {
            continue;
        }
        if !file.supported_for_stage_hunks {
            return Err(structured_error(
                "unsupported_hunk_ids",
                "selected hunk belongs to unsupported file record",
                vec![("path", json!(file.path))],
            ));
        }
        patch.push_str(&file.diff_header);
        if let Some(old) = &file.old_file_header {
            patch.push_str(old);
        } else {
            patch.push_str(&format!("--- a/{}\n", file.path));
        }
        if let Some(new) = &file.new_file_header {
            patch.push_str(new);
        } else {
            patch.push_str(&format!("+++ b/{}\n", file.path));
        }
        for hunk in file_hunks {
            patch.push_str(&hunk.header);
            patch.push_str(&hunk.body);
        }
    }
    if patch.is_empty() {
        return Err(structured_error(
            "unknown_hunk_ids",
            "no selected hunks were reconstructable",
            vec![],
        ));
    }
    Ok(patch.into_bytes())
}

async fn run_cached_apply(
    repo: &RepoContext,
    patch: &[u8],
    reverse: bool,
    check_only: bool,
    unidiff_zero: bool,
    timeout_ms: u64,
) -> Result<super::super::types::GitExecResult, ToolCallOutcome> {
    revalidate_repo_identity(repo)?;
    let args = build_git_apply_args(GitApplyArgs {
        target: ApplyTarget::Cached,
        check_only,
        reverse,
        three_way: false,
        recount: true,
        unidiff_zero,
        whitespace: "nowarn",
    });
    run_git_with_stdin(
        Some(repo.working_dir.clone()),
        args,
        Some(patch.to_vec()),
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            if check_only {
                "preflight_unavailable"
            } else {
                "git_apply_unavailable"
            },
            format!("failed to run git apply: {err:#}"),
            vec![],
        )
    })
}

async fn apply_failed_on_index_lock(
    repo: &RepoContext,
    exec: &super::super::types::GitExecResult,
    timeout_ms: u64,
) -> bool {
    let Some(lock_path) = resolve_index_lock_path(repo, timeout_ms).await else {
        return false;
    };
    lock_path.exists() || stderr_mentions_path(&exec.stderr, &lock_path)
}

async fn resolve_index_lock_path(repo: &RepoContext, timeout_ms: u64) -> Option<PathBuf> {
    let exec = run_git(
        Some(repo.working_dir.clone()),
        vec![
            "rev-parse".into(),
            "--path-format=absolute".into(),
            "--git-path".into(),
            "index.lock".into(),
        ],
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .ok()?;
    if exec.timed_out || !exec.success {
        return None;
    }
    let path = exec.stdout.trim_end_matches(['\r', '\n']);
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn stderr_mentions_path(stderr: &str, path: &Path) -> bool {
    let display = display_path(path);
    let display_slash = display.replace('\\', "/");
    let stderr_slash = stderr.replace('\\', "/");
    stderr.contains(&display) || stderr_slash.contains(&display_slash)
}

fn stage_failure_from_exec(
    error_type: &'static str,
    state: &'static str,
    exec: &super::super::types::GitExecResult,
    message: &'static str,
) -> ToolCallOutcome {
    let payload = build_git_response_with_is_error(
        exec,
        message,
        true,
        vec![
            ("error_type", json!(error_type)),
            ("state", json!(state)),
            ("applied", json!(false)),
            ("checked", json!(false)),
            ("commit_ready", json!(false)),
            ("verification_state", json!("verification_unavailable")),
            (
                "remediation",
                json!("Inspect GitStatus/GitDiff before further staging or committing."),
            ),
        ],
    );
    ToolCallOutcome::ok(payload)
}

#[derive(Debug, Clone, Copy)]
enum VerificationFailure {
    ScopedUnavailable,
    ScopedMismatch,
    CommitGroupUnavailable,
    CommitGroupMismatch,
    RepoIdentityChanged,
    MetadataOutsideAuthority,
}

#[derive(Debug, Clone)]
struct VerificationSuccess {
    post_source_diff_id: String,
    post_target_diff_id: String,
    post_full_staged_diff_id: Option<String>,
    post_full_unstaged_diff_id: Option<String>,
}

type BlobLines = Vec<Vec<u8>>;

struct VerificationInputs<'a> {
    source_scope: &'a HunkRequestScope,
    action: StageAction,
    pre_source: &'a ParsedDiff,
    pre_target: &'a ParsedDiff,
    pre_full_unstaged: Option<&'a ParsedDiff>,
    selected: &'a [(&'a FileHunks, &'a ParsedHunk)],
    pre_index_blobs: &'a BTreeMap<String, Vec<u8>>,
}

async fn verify_after_apply(
    repo: &RepoContext,
    inputs: VerificationInputs<'_>,
) -> Result<VerificationSuccess, VerificationFailure> {
    let VerificationInputs {
        source_scope,
        action,
        pre_source,
        pre_target,
        pre_full_unstaged,
        selected,
        pre_index_blobs,
    } = inputs;
    let target_scope = HunkRequestScope {
        staged: action.target_staged(),
        ..source_scope.clone()
    };
    let post_source = enumerate_diff(repo, source_scope)
        .await
        .map_err(verification_failure_from_outcome)?;
    let post_target = enumerate_diff(repo, &target_scope)
        .await
        .map_err(verification_failure_from_outcome)?;

    let selected_counts = body_counts_from_selected(selected);
    let pre_source_counts = body_counts_from_diff(pre_source);
    let pre_target_counts = body_counts_from_diff(pre_target);
    let post_source_counts = body_counts_from_diff(&post_source);
    let post_target_counts = body_counts_from_diff(&post_target);

    if !verify_scoped_count_delta(
        action,
        &selected_counts,
        &pre_source_counts,
        &pre_target_counts,
        &post_source_counts,
        &post_target_counts,
    ) {
        return Err(VerificationFailure::ScopedMismatch);
    }

    if !verify_index_blob_delta(
        repo,
        pre_index_blobs,
        selected,
        action,
        source_scope.timeout_ms,
    )
    .await?
    {
        return Err(VerificationFailure::ScopedMismatch);
    }

    let mut success = VerificationSuccess {
        post_source_diff_id: post_source.diff_id.clone(),
        post_target_diff_id: post_target.diff_id.clone(),
        post_full_staged_diff_id: None,
        post_full_unstaged_diff_id: None,
    };

    if action == StageAction::PrepareCommit {
        let full_staged_scope = HunkRequestScope {
            staged: true,
            paths: Vec::new(),
            ..source_scope.clone()
        };
        let full_staged = enumerate_diff(repo, &full_staged_scope)
            .await
            .map_err(|outcome| {
                let failure = verification_failure_from_outcome(outcome);
                if matches!(failure, VerificationFailure::ScopedUnavailable) {
                    VerificationFailure::CommitGroupUnavailable
                } else {
                    failure
                }
            })?;
        let full_unstaged_scope = HunkRequestScope {
            staged: false,
            paths: Vec::new(),
            ..source_scope.clone()
        };
        let full_unstaged =
            enumerate_diff(repo, &full_unstaged_scope)
                .await
                .map_err(|outcome| {
                    let failure = verification_failure_from_outcome(outcome);
                    if matches!(failure, VerificationFailure::ScopedUnavailable) {
                        VerificationFailure::CommitGroupUnavailable
                    } else {
                        failure
                    }
                })?;
        let selected_counts = body_counts_from_selected(selected);
        let selected_paths = selected
            .iter()
            .map(|(file, _)| file.path.clone())
            .collect::<BTreeSet<_>>();
        if !full_staged_diff_matches_selected_group(&full_staged, &selected_counts) {
            return Err(VerificationFailure::CommitGroupMismatch);
        }
        let Some(pre_full_unstaged) = pre_full_unstaged else {
            return Err(VerificationFailure::CommitGroupUnavailable);
        };
        if !full_unstaged_diff_matches_prepare_commit_delta(
            pre_full_unstaged,
            &full_unstaged,
            &selected_counts,
            &selected_paths,
        ) {
            return Err(VerificationFailure::CommitGroupMismatch);
        }
        success.post_full_staged_diff_id = Some(full_staged.diff_id);
        success.post_full_unstaged_diff_id = Some(full_unstaged.diff_id);
    }

    Ok(success)
}

async fn verify_index_blob_delta(
    repo: &RepoContext,
    pre_index_blobs: &BTreeMap<String, Vec<u8>>,
    selected: &[(&FileHunks, &ParsedHunk)],
    action: StageAction,
    timeout_ms: u64,
) -> Result<bool, VerificationFailure> {
    let expected = expected_index_blobs_after_selected(pre_index_blobs, selected, action)
        .map_err(|_| VerificationFailure::ScopedMismatch)?;
    let paths = expected.keys().cloned().collect::<Vec<_>>();
    let post_index_blobs = index_blobs_for_paths(repo, &paths, timeout_ms)
        .await
        .map_err(verification_failure_from_outcome)?;
    Ok(post_index_blobs == expected)
}

fn verification_failure_from_outcome(outcome: ToolCallOutcome) -> VerificationFailure {
    match outcome.0["error_type"].as_str() {
        Some("git_metadata_outside_authority") => VerificationFailure::MetadataOutsideAuthority,
        Some("repo_identity_changed") => VerificationFailure::RepoIdentityChanged,
        _ => VerificationFailure::ScopedUnavailable,
    }
}

fn expected_index_blobs_after_selected(
    pre_index_blobs: &BTreeMap<String, Vec<u8>>,
    selected: &[(&FileHunks, &ParsedHunk)],
    action: StageAction,
) -> Result<BTreeMap<String, Vec<u8>>, ()> {
    let mut grouped: BTreeMap<&str, Vec<(&FileHunks, &ParsedHunk)>> = BTreeMap::new();
    for (file, hunk) in selected {
        grouped.entry(&file.path).or_default().push((*file, *hunk));
    }

    let mut expected = BTreeMap::new();
    for (path, mut hunks) in grouped {
        hunks.sort_by_key(|(file, hunk)| (file.file_index, hunk.hunk_index));
        let pre_blob = pre_index_blobs.get(path).ok_or(())?;
        let path_expected = apply_selected_hunks_to_blob(
            pre_blob,
            &hunks.iter().map(|(_, hunk)| *hunk).collect::<Vec<_>>(),
            action == StageAction::Unstage,
        )?;
        expected.insert(path.to_string(), path_expected);
    }

    Ok(expected)
}

fn apply_selected_hunks_to_blob(
    blob: &[u8],
    hunks: &[&ParsedHunk],
    reverse: bool,
) -> Result<Vec<u8>, ()> {
    let lines = split_blob_lines_keep_endings(blob);
    let mut output: Vec<Vec<u8>> = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let start_line = if reverse {
            hunk.new_start
        } else {
            hunk.old_start
        };
        if start_line < 0 {
            return Err(());
        }
        let start = if start_line == 0 {
            0
        } else {
            usize::try_from(start_line - 1).map_err(|_| ())?
        };
        if start < cursor || start > lines.len() {
            return Err(());
        }

        output.extend(lines[cursor..start].iter().cloned());
        let (source_lines, target_lines) = hunk_source_target_lines(hunk, reverse)?;
        let source_end = start.checked_add(source_lines.len()).ok_or(())?;
        if source_end > lines.len() || lines[start..source_end] != source_lines[..] {
            return Err(());
        }

        output.extend(target_lines);
        cursor = source_end;
    }

    output.extend(lines[cursor..].iter().cloned());
    Ok(output.into_iter().flatten().collect())
}

fn hunk_source_target_lines(
    hunk: &ParsedHunk,
    reverse: bool,
) -> Result<(BlobLines, BlobLines), ()> {
    let mut old_lines = Vec::new();
    let mut new_lines = Vec::new();
    let mut last_sides = None;

    for line in split_blob_lines_keep_endings(hunk.body.as_bytes()) {
        if line.starts_with(b"\\ No newline at end of file") {
            let Some((old_side, new_side)) = last_sides else {
                return Err(());
            };
            if old_side {
                strip_diff_line_separator(old_lines.last_mut().ok_or(())?);
            }
            if new_side {
                strip_diff_line_separator(new_lines.last_mut().ok_or(())?);
            }
            continue;
        }

        let Some((&prefix, content)) = line.split_first() else {
            return Err(());
        };
        match prefix {
            b' ' => {
                old_lines.push(content.to_vec());
                new_lines.push(content.to_vec());
                last_sides = Some((true, true));
            }
            b'-' => {
                old_lines.push(content.to_vec());
                last_sides = Some((true, false));
            }
            b'+' => {
                new_lines.push(content.to_vec());
                last_sides = Some((false, true));
            }
            _ => return Err(()),
        }
    }

    if reverse {
        Ok((new_lines, old_lines))
    } else {
        Ok((old_lines, new_lines))
    }
}

fn strip_diff_line_separator(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
}

fn split_blob_lines_keep_endings(bytes: &[u8]) -> BlobLines {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(bytes[start..=idx].to_vec());
            start = idx + 1;
        }
    }
    if start < bytes.len() {
        lines.push(bytes[start..].to_vec());
    }
    lines
}

async fn index_blobs_for_paths(
    repo: &RepoContext,
    paths: &[String],
    timeout_ms: u64,
) -> Result<BTreeMap<String, Vec<u8>>, ToolCallOutcome> {
    let entries = index_entries_for_paths(repo, paths, timeout_ms).await?;
    let mut blobs = BTreeMap::new();
    for path in paths {
        let entry = entries.get(path).ok_or_else(|| {
            structured_error(
                "verification_unavailable",
                "index entry disappeared during blob verification",
                vec![("path", json!(path))],
            )
        })?;
        if entry.stage != "0" || !(entry.mode == "100644" || entry.mode == "100755") {
            return Err(structured_error(
                "verification_unavailable",
                "index entry was not a stage-0 regular file during blob verification",
                vec![("path", json!(path))],
            ));
        }
        revalidate_repo_identity(repo)?;
        let exec = run_git(
            Some(repo.working_dir.clone()),
            vec![
                "cat-file".to_string(),
                "blob".to_string(),
                entry.object_id.clone(),
            ],
            timeout_ms,
            MAX_OUTPUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .map_err(|err| {
            structured_error(
                "verification_unavailable",
                format!("failed to read index blob for verification: {err:#}"),
                vec![("path", json!(path))],
            )
        })?;
        if exec.timed_out || !exec.success || exec.truncated_stdout {
            return Err(structured_error(
                "verification_unavailable",
                "index blob could not be read for verification",
                vec![
                    ("path", json!(path)),
                    ("timed_out", json!(exec.timed_out)),
                    ("exit_code", json!(exec.exit_code)),
                    ("truncated_stdout", json!(exec.truncated_stdout)),
                ],
            ));
        }
        blobs.insert(path.clone(), exec.stdout_bytes);
    }
    Ok(blobs)
}

async fn index_entries_for_paths(
    repo: &RepoContext,
    paths: &[String],
    timeout_ms: u64,
) -> Result<HashMap<String, IndexEntryPreflight>, ToolCallOutcome> {
    revalidate_repo_identity(repo)?;
    let mut args = vec![
        "--literal-pathspecs".to_string(),
        "ls-files".to_string(),
        "-s".to_string(),
        "-v".to_string(),
        "--debug".to_string(),
        "-z".to_string(),
        "--".to_string(),
    ];
    args.extend(paths.iter().cloned());
    let exec = run_git(
        Some(repo.working_dir.clone()),
        args,
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "verification_unavailable",
            format!("failed to run index-entry verification probe: {err:#}"),
            vec![],
        )
    })?;
    if exec.timed_out || !exec.success {
        return Err(structured_error(
            "verification_unavailable",
            "index-entry verification probe failed",
            vec![
                ("timed_out", json!(exec.timed_out)),
                ("exit_code", json!(exec.exit_code)),
                ("stderr", json!(exec.stderr)),
            ],
        ));
    }

    let found = parse_ls_files_preflight_stdout(&exec.stdout_bytes);
    if found.len() != paths.len() || paths.iter().any(|path| !found.contains_key(path)) {
        return Err(structured_error(
            "verification_unavailable",
            "index-entry verification returned a path set different from requested paths",
            vec![
                ("requested_paths", json!(paths)),
                (
                    "returned_paths",
                    json!(found.keys().cloned().collect::<Vec<_>>()),
                ),
            ],
        ));
    }
    Ok(found)
}

fn verify_scoped_count_delta(
    action: StageAction,
    selected_counts: &BTreeMap<Vec<u8>, usize>,
    pre_source: &BTreeMap<Vec<u8>, usize>,
    pre_target: &BTreeMap<Vec<u8>, usize>,
    post_source: &BTreeMap<Vec<u8>, usize>,
    post_target: &BTreeMap<Vec<u8>, usize>,
) -> bool {
    let mut keys = BTreeSet::new();
    keys.extend(selected_counts.keys().cloned());
    keys.extend(pre_source.keys().cloned());
    keys.extend(pre_target.keys().cloned());
    keys.extend(post_source.keys().cloned());
    keys.extend(post_target.keys().cloned());

    for key in keys {
        let selected = selected_counts.get(&key).copied().unwrap_or(0);
        let pre_source_count = pre_source.get(&key).copied().unwrap_or(0);
        let pre_target_count = pre_target.get(&key).copied().unwrap_or(0);
        let post_source_count = post_source.get(&key).copied().unwrap_or(0);
        let post_target_count = post_target.get(&key).copied().unwrap_or(0);

        let Some(expected_source) = pre_source_count.checked_sub(selected) else {
            return false;
        };
        let expected_target = pre_target_count.saturating_add(selected);

        if post_source_count != expected_source || post_target_count != expected_target {
            return false;
        }

        if action == StageAction::Unstage {
            let Some(expected_staged_source) = pre_source_count.checked_sub(selected) else {
                return false;
            };
            let expected_unstaged_target = pre_target_count.saturating_add(selected);
            if post_source_count != expected_staged_source
                || post_target_count != expected_unstaged_target
            {
                return false;
            }
        }
    }

    true
}

fn body_counts_from_selected(selected: &[(&FileHunks, &ParsedHunk)]) -> BTreeMap<Vec<u8>, usize> {
    let mut counts = BTreeMap::new();
    for (file, hunk) in selected {
        *counts.entry(hunk_body_key(file, hunk)).or_default() += 1;
    }
    counts
}

fn body_counts_from_diff(diff: &ParsedDiff) -> BTreeMap<Vec<u8>, usize> {
    let mut counts = BTreeMap::new();
    for file in &diff.files {
        for hunk in &file.hunks {
            *counts.entry(hunk_body_key(file, hunk)).or_default() += 1;
        }
    }
    counts
}

fn full_staged_diff_matches_selected_group(
    diff: &ParsedDiff,
    selected_counts: &BTreeMap<Vec<u8>, usize>,
) -> bool {
    for file in &diff.files {
        if !file.supported_for_stage_hunks || file.hunks.is_empty() {
            return false;
        }
    }
    body_counts_from_diff(diff) == *selected_counts
}

fn full_unstaged_diff_matches_prepare_commit_delta(
    pre_full_unstaged: &ParsedDiff,
    post_full_unstaged: &ParsedDiff,
    selected_counts: &BTreeMap<Vec<u8>, usize>,
    selected_paths: &BTreeSet<String>,
) -> bool {
    if unselected_file_inventory_counts(pre_full_unstaged, selected_paths)
        != unselected_file_inventory_counts(post_full_unstaged, selected_paths)
    {
        return false;
    }

    if unsupported_or_hunkless_file_counts(pre_full_unstaged)
        != unsupported_or_hunkless_file_counts(post_full_unstaged)
    {
        return false;
    }

    let pre_counts = body_counts_from_diff(pre_full_unstaged);
    let post_counts = body_counts_from_diff(post_full_unstaged);
    let mut keys = BTreeSet::new();
    keys.extend(selected_counts.keys().cloned());
    keys.extend(pre_counts.keys().cloned());
    keys.extend(post_counts.keys().cloned());

    for key in keys {
        let selected = selected_counts.get(&key).copied().unwrap_or(0);
        let pre_count = pre_counts.get(&key).copied().unwrap_or(0);
        let post_count = post_counts.get(&key).copied().unwrap_or(0);
        let Some(expected_post_count) = pre_count.checked_sub(selected) else {
            return false;
        };
        if post_count != expected_post_count {
            return false;
        }
    }

    true
}

fn unselected_file_inventory_counts(
    diff: &ParsedDiff,
    selected_paths: &BTreeSet<String>,
) -> BTreeMap<Vec<u8>, usize> {
    let mut counts = BTreeMap::new();
    for file in &diff.files {
        if !selected_paths.contains(&file.path) {
            *counts.entry(file_inventory_signature(file)).or_default() += 1;
        }
    }
    counts
}

fn unsupported_or_hunkless_file_counts(diff: &ParsedDiff) -> BTreeMap<Vec<u8>, usize> {
    let mut counts = BTreeMap::new();
    for file in &diff.files {
        if !file.supported_for_stage_hunks || file.hunks.is_empty() {
            *counts.entry(file_inventory_signature(file)).or_default() += 1;
        }
    }
    counts
}

fn file_inventory_signature(file: &FileHunks) -> Vec<u8> {
    fn push_field(out: &mut Vec<u8>, label: &str, value: &[u8]) {
        out.extend_from_slice(label.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(value);
        out.push(b'\0');
    }

    let mut signature = Vec::new();
    push_field(&mut signature, "path", file.path.as_bytes());
    push_field(
        &mut signature,
        "old_path",
        file.old_path.as_deref().unwrap_or("").as_bytes(),
    );
    push_field(
        &mut signature,
        "status",
        change_status_name(file.status).as_bytes(),
    );
    push_field(
        &mut signature,
        "change_kinds",
        file.change_kinds.join("\n").as_bytes(),
    );
    push_field(
        &mut signature,
        "binary",
        if file.binary { b"true" } else { b"false" },
    );
    push_field(
        &mut signature,
        "supported",
        if file.supported_for_stage_hunks {
            b"true"
        } else {
            b"false"
        },
    );
    push_field(
        &mut signature,
        "unsupported_reason",
        file.unsupported_reason.as_deref().unwrap_or("").as_bytes(),
    );
    push_field(&mut signature, "diff_header", file.diff_header.as_bytes());
    push_field(
        &mut signature,
        "old_file_header",
        file.old_file_header.as_deref().unwrap_or("").as_bytes(),
    );
    push_field(
        &mut signature,
        "new_file_header",
        file.new_file_header.as_deref().unwrap_or("").as_bytes(),
    );
    push_field(
        &mut signature,
        "extended_headers",
        file.extended_headers.join("\n").as_bytes(),
    );
    for hunk in &file.hunks {
        push_field(&mut signature, "hunk_id", hunk.id.as_bytes());
        push_field(&mut signature, "hunk_header", hunk.header.as_bytes());
        push_field(&mut signature, "hunk_body", hunk.body.as_bytes());
    }
    signature
}

fn change_status_name(status: ChangeStatus) -> &'static str {
    match status {
        ChangeStatus::Modified => "modified",
        ChangeStatus::Added => "added",
        ChangeStatus::Deleted => "deleted",
        ChangeStatus::Renamed => "renamed",
        ChangeStatus::Copied => "copied",
        ChangeStatus::ModeChanged => "mode_changed",
        ChangeStatus::TypeChanged => "type_changed",
        ChangeStatus::Submodule => "submodule",
        ChangeStatus::Unmerged => "unmerged",
    }
}

fn commit_group_verification_unavailable_before_apply(cause: ToolCallOutcome) -> ToolCallOutcome {
    structured_error(
        "commit_group_verification_unavailable",
        "full unstaged diff could not be captured before staging",
        vec![
            ("state", json!("failed")),
            ("applied", json!(false)),
            ("checked", json!(false)),
            ("commit_ready", json!(false)),
            ("verification_state", json!("verification_unavailable")),
            ("cause_error_type", cause.0["error_type"].clone()),
            ("cause", cause.0),
            (
                "remediation",
                json!("Inspect GitStatus/GitDiff before further staging or committing."),
            ),
        ],
    )
}

fn commit_call_template(working_dir: Option<&str>, req: &GitStageHunksRequest) -> Value {
    let commit_type = req
        .commit_type
        .clone()
        .unwrap_or_else(|| "<fill commit type>".to_string());
    let message = req
        .commit_message
        .clone()
        .unwrap_or_else(|| "<fill commit message>".to_string());
    let mut args = json!({
        "type": commit_type,
        "message": message,
    });
    if let Some(scope) = &req.commit_scope {
        args["scope"] = json!(scope);
    }
    if let Some(working_dir) = working_dir {
        args["working_dir"] = json!(working_dir);
    }
    json!({
        "name": "GitCommit",
        "arguments": args,
        "placeholders": {
            "type": req.commit_type.is_none(),
            "message": req.commit_message.is_none(),
            "scope": req.commit_scope.is_none(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::types::{GitExecResult, GitStdinSummary};
    use super::super::hunks::ChangeStatus;
    use super::*;
    use crate::git::path_policy;

    fn repo() -> RepoContext {
        RepoContext {
            working_dir: ".".to_string(),
            toplevel: PathBuf::from("."),
            identity: "repo-id".to_string(),
            stable_identity: crate::git::handlers::hunks::RepoIdentitySnapshot {
                anchors: BTreeMap::new(),
            },
        }
    }

    fn tempdir_under_authority(prefix: &str) -> tempfile::TempDir {
        let root = path_policy::authority_root_path()
            .expect("authority root")
            .join("target")
            .join("tools-mcp-git-apply-tests");
        std::fs::create_dir_all(&root).expect("apply test root");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(root)
            .expect("tempdir under authority")
    }

    async fn git_available() -> bool {
        run_git(
            None,
            vec!["--version".to_string()],
            30_000,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .is_ok()
    }

    async fn run_fixture_git(repo: &Path, args: &[&str]) -> GitExecResult {
        let exec = run_git(
            Some(repo.to_string_lossy().to_string()),
            args.iter().map(|arg| (*arg).to_string()).collect(),
            30_000,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("git command should spawn");
        assert!(
            exec.success,
            "git {:?} failed: stdout={} stderr={}",
            args, exec.stdout, exec.stderr
        );
        exec
    }

    async fn three_way_conflict_apply_fixture(prefix: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir_under_authority(prefix);
        run_fixture_git(dir.path(), &["init", "-q"]).await;
        run_fixture_git(dir.path(), &["config", "user.email", "test@example.com"]).await;
        run_fixture_git(dir.path(), &["config", "user.name", "Test User"]).await;
        run_fixture_git(dir.path(), &["config", "core.autocrlf", "false"]).await;
        std::fs::write(dir.path().join("story.txt"), "base\n").expect("write base");
        run_fixture_git(dir.path(), &["add", "story.txt"]).await;
        run_fixture_git(dir.path(), &["commit", "-q", "-m", "initial"]).await;

        std::fs::write(dir.path().join("story.txt"), "patch\n").expect("write patch version");
        let patch = run_fixture_git(dir.path(), &["diff", "--no-ext-diff", "--full-index"])
            .await
            .stdout;
        assert!(patch.contains("-base"), "{patch}");
        assert!(patch.contains("+patch"), "{patch}");

        std::fs::write(dir.path().join("story.txt"), "current\n").expect("write current version");
        run_fixture_git(dir.path(), &["add", "story.txt"]).await;

        (dir, patch)
    }

    fn exec(success: bool, timed_out: bool, stdin_delivered: Option<bool>) -> GitExecResult {
        GitExecResult {
            git_bin: "git".to_string(),
            args: vec!["apply".to_string()],
            working_dir: Some(".".to_string()),
            exit_code: if timed_out {
                None
            } else if success {
                Some(0)
            } else {
                Some(1)
            },
            success,
            stdout: String::new(),
            stderr: String::new(),
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            truncated_stdout: false,
            truncated_stderr: false,
            timed_out,
            stdin: GitStdinSummary {
                requested_bytes: Some(5),
                written_bytes: Some(if stdin_delivered == Some(false) { 3 } else { 5 }),
                fully_delivered: stdin_delivered,
                write_error: None,
                broken_pipe: false,
            },
        }
    }

    fn counts(items: &[(&[u8], usize)]) -> BTreeMap<Vec<u8>, usize> {
        items
            .iter()
            .map(|(key, count)| ((*key).to_vec(), *count))
            .collect()
    }

    fn test_file(path: &str) -> FileHunks {
        FileHunks {
            file_index: 0,
            path: path.to_string(),
            old_path: Some(path.to_string()),
            status: ChangeStatus::Modified,
            change_kinds: vec!["modified".to_string()],
            binary: false,
            supported_for_stage_hunks: true,
            unsupported_reason: None,
            diff_header: format!("diff --git a/{path} b/{path}\n"),
            old_file_header: Some(format!("--- a/{path}\n")),
            new_file_header: Some(format!("+++ b/{path}\n")),
            extended_headers: Vec::new(),
            hunks: Vec::new(),
        }
    }

    fn test_hunk(hunk_index: usize, old_start: i64, new_start: i64, body: &str) -> ParsedHunk {
        ParsedHunk {
            id: format!(
                "0.{hunk_index}.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ),
            file_index: 0,
            hunk_index,
            header: format!("@@ -{old_start},1 +{new_start},1 @@\n"),
            old_start,
            old_lines: 1,
            new_start,
            new_lines: 1,
            body: body.to_string(),
        }
    }

    fn parsed_diff(files: Vec<FileHunks>) -> ParsedDiff {
        let total_hunks = files.iter().map(|file| file.hunks.len()).sum();
        let hunk_body_bytes = files
            .iter()
            .flat_map(|file| &file.hunks)
            .map(|hunk| hunk.body.len())
            .sum();
        ParsedDiff {
            diff_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            diff_bytes: 0,
            files,
            total_hunks,
            hunk_body_bytes,
        }
    }

    fn supported_patch_record(path: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\n\
             index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
             --- a/{path}\n\
             +++ b/{path}\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n"
        )
    }

    fn assert_apply_hardening_prefix(args: &[String]) {
        let expected: Vec<String> = [
            "--no-optional-locks",
            "-c",
            "core.protectNTFS=true",
            "-c",
            "core.protectHFS=true",
            "-c",
            "apply.ignoreWhitespace=false",
            "apply",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(
            args.len() >= expected.len(),
            "GitApply args should include the hardening prefix: {args:?}"
        );
        assert_eq!(
            &args[..expected.len()],
            expected.as_slice(),
            "GitApply hardening pins must precede the apply subcommand"
        );
        assert_eq!(
            args.iter().position(|arg| arg == "apply"),
            Some(expected.len() - 1),
            "apply subcommand should appear exactly after hardening pins"
        );
    }

    #[test]
    fn git_apply_args_map_targets_and_flags() {
        let args = build_git_apply_args(GitApplyArgs {
            target: ApplyTarget::Cached,
            check_only: true,
            reverse: true,
            three_way: false,
            recount: true,
            unidiff_zero: true,
            whitespace: "nowarn",
        });

        assert_apply_hardening_prefix(&args);
        assert!(args.contains(&"--cached".to_string()));
        assert!(args.contains(&"--check".to_string()));
        assert!(args.contains(&"-R".to_string()));
        assert!(args.contains(&"--recount".to_string()));
        assert!(args.contains(&"--unidiff-zero".to_string()));
        assert!(args.contains(&"--whitespace=nowarn".to_string()));
    }

    #[test]
    fn git_apply_args_pin_hardening_for_every_target_and_check_mode() {
        for target in [
            ApplyTarget::Cached,
            ApplyTarget::IndexWorktree,
            ApplyTarget::Worktree,
        ] {
            for check_only in [false, true] {
                let args = build_git_apply_args(GitApplyArgs {
                    target,
                    check_only,
                    reverse: false,
                    three_way: false,
                    recount: true,
                    unidiff_zero: false,
                    whitespace: "error-all",
                });

                assert_apply_hardening_prefix(&args);
                assert_eq!(args.contains(&"--check".to_string()), check_only);
                assert_eq!(
                    args.contains(&"--cached".to_string()),
                    target == ApplyTarget::Cached
                );
                assert_eq!(
                    args.contains(&"--index".to_string()),
                    target == ApplyTarget::IndexWorktree
                );
                assert!(args.contains(&"--whitespace=error-all".to_string()));
                assert!(!args.contains(&"--unsafe-paths".to_string()));
            }
        }
    }

    #[tokio::test]
    async fn git_apply_patch_file_cap_reports_patch_complexity_limit() {
        let mut patch = String::new();
        for index in 0..=MAX_GIT_PATCH_PATHS {
            patch.push_str(&supported_patch_record(&format!("file_{index:04}.txt")));
        }

        let err = validate_supported_patch(&patch, &repo())
            .await
            .expect_err("too many patch records should be rejected by the GitApply cap");

        assert_eq!(err.0["error_type"], "patch_complexity_limit");
        assert_eq!(err.0["parser_error"]["error_type"], "diff_complexity_limit");
    }

    #[tokio::test]
    async fn git_apply_patch_argv_cap_reports_patch_complexity_limit() {
        let mut patch = String::new();
        let mut argv_bytes = 0usize;
        let mut record_count = 0usize;
        while argv_bytes <= MAX_GIT_ARG_BYTES {
            let path = format!("dir/file_{record_count:04}_{}.txt", "a".repeat(64));
            argv_bytes += path.len() + 1;
            patch.push_str(&supported_patch_record(&path));
            record_count += 1;
        }
        assert!(
            record_count < MAX_GIT_PATCH_PATHS,
            "fixture should isolate argv bytes from file-count caps"
        );

        let err = validate_supported_patch(&patch, &repo())
            .await
            .expect_err("oversized patch path argv should be rejected");

        assert_eq!(err.0["error_type"], "patch_complexity_limit");
        assert!(
            err.0["argv_bytes"]
                .as_u64()
                .is_some_and(|bytes| { bytes as usize > MAX_GIT_ARG_BYTES })
        );
    }

    #[tokio::test]
    async fn git_apply_validation_rejects_closed_support_matrix_records() {
        let cases = [
            (
                "added file",
                "diff --git a/new.txt b/new.txt\n\
                 new file mode 100644\n\
                 index 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222\n\
                 --- /dev/null\n\
                 +++ b/new.txt\n\
                 @@ -0,0 +1 @@\n\
                 +new\n",
                "unsupported_patch_record",
                Some("unsupported_change_kind"),
            ),
            (
                "default binary marker",
                "diff --git a/blob.bin b/blob.bin\n\
                 Binary files a/blob.bin and b/blob.bin differ\n",
                "unsupported_patch_record",
                Some("binary"),
            ),
            (
                "content plus mode change",
                "diff --git a/run.sh b/run.sh\n\
                 old mode 100644\n\
                 new mode 100755\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222\n\
                 --- a/run.sh\n\
                 +++ b/run.sh\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n",
                "unsupported_patch_record",
                Some("unsupported_change_kind"),
            ),
            (
                "hunkless modified record",
                "diff --git a/story.txt b/story.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/story.txt\n\
                 +++ b/story.txt\n",
                "unsupported_patch_record",
                Some("hunkless"),
            ),
            (
                "context-only no-op hunk",
                "diff --git a/story.txt b/story.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/story.txt\n\
                 +++ b/story.txt\n\
                 @@ -1 +1 @@\n\
                 \x20old\n",
                "unsupported_patch_record",
                Some("no_content_changes"),
            ),
            (
                "mixed no-op and changing hunks",
                "diff --git a/story.txt b/story.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/story.txt\n\
                 +++ b/story.txt\n\
                 @@ -1 +1 @@\n\
                \x20same\n\
                 @@ -3 +3 @@\n\
                 -old\n\
                 +new\n",
                "unsupported_patch_record",
                Some("no_content_changes"),
            ),
            (
                "all-zero old index id",
                "diff --git a/story.txt b/story.txt\n\
                 index 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222 100644\n\
                 --- a/story.txt\n\
                 +++ b/story.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n",
                "unsupported_patch_record",
                Some("unsupported_index_header"),
            ),
            (
                "diff old/new path mismatch",
                "diff --git a/old.txt b/new.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/old.txt\n\
                 +++ b/new.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n",
                "unsupported_patch_record",
                Some("old_new_path_mismatch"),
            ),
            (
                "file header path mismatch",
                "diff --git a/story.txt b/story.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/other.txt\n\
                 +++ b/story.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n",
                "unsupported_patch_record",
                Some("unsupported_path"),
            ),
            (
                "path escape",
                "diff --git a/../escape.txt b/../escape.txt\n\
                 --- a/../escape.txt\n\
                 +++ b/../escape.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n",
                "invalid_patch_path",
                None,
            ),
        ];

        for (name, patch, expected_error, expected_reason) in cases {
            let err = match validate_supported_patch(patch, &repo()).await {
                Ok(_) => panic!("{name} should be rejected before git apply"),
                Err(err) => err,
            };

            assert_eq!(err.0["error_type"], expected_error, "{name}");
            if let Some(expected_reason) = expected_reason {
                assert_eq!(
                    err.0["unsupported_reason"], expected_reason,
                    "{name}: {:?}",
                    err.0
                );
            }
        }
    }

    #[tokio::test]
    async fn git_apply_patch_byte_cap_rejects_just_over_cap_before_repo_probe() {
        let response = handle_git_apply(
            None,
            json!({
                "patch": "x".repeat(MAX_GIT_STDIN_BYTES + 1)
            }),
        )
        .await;

        assert_eq!(response.0["isError"], true);
        assert_eq!(response.0["error_type"], "stdin_too_large");
        assert_eq!(response.0["max_bytes"], MAX_GIT_STDIN_BYTES);
    }

    #[tokio::test]
    async fn git_apply_patch_byte_cap_uses_bytes_for_multibyte_payloads() {
        let multibyte = "é".repeat((MAX_GIT_STDIN_BYTES / "é".len()) + 1);
        assert!(
            multibyte.chars().count() < MAX_GIT_STDIN_BYTES,
            "fixture must be below the byte cap if measured as characters"
        );
        assert!(multibyte.len() > MAX_GIT_STDIN_BYTES);

        let response = handle_git_apply(
            None,
            json!({
                "patch": multibyte
            }),
        )
        .await;

        assert_eq!(response.0["isError"], true);
        assert_eq!(response.0["error_type"], "stdin_too_large");
    }

    #[tokio::test]
    async fn git_apply_patch_exact_byte_cap_is_not_size_rejected() {
        let response = handle_git_apply(
            None,
            json!({
                "patch": " ".repeat(MAX_GIT_STDIN_BYTES)
            }),
        )
        .await;

        assert_eq!(response.0["isError"], true);
        assert_eq!(response.0["error_type"], "empty_patch");
    }

    #[test]
    fn index_blob_expected_result_applies_selected_forward_hunk() {
        let file = test_file("story.txt");
        let hunk = test_hunk(0, 1, 1, " line 1\n-line 2\n+line 2 edited\n line 3\n");
        let mut pre_blobs = BTreeMap::new();
        pre_blobs.insert(
            "story.txt".to_string(),
            b"line 1\nline 2\nline 3\n".to_vec(),
        );

        let expected = expected_index_blobs_after_selected(
            &pre_blobs,
            &[(&file, &hunk)],
            StageAction::PrepareCommit,
        )
        .expect("expected blob");

        assert_eq!(
            expected.get("story.txt").expect("story blob"),
            b"line 1\nline 2 edited\nline 3\n"
        );
    }

    #[test]
    fn index_blob_expected_result_handles_reverse_unstage() {
        let file = test_file("story.txt");
        let hunk = test_hunk(0, 1, 1, " line 1\n-old\n+new\n line 3\n");
        let mut pre_blobs = BTreeMap::new();
        pre_blobs.insert("story.txt".to_string(), b"line 1\nnew\nline 3\n".to_vec());

        let expected = expected_index_blobs_after_selected(
            &pre_blobs,
            &[(&file, &hunk)],
            StageAction::Unstage,
        )
        .expect("expected reverse blob");

        assert_eq!(
            expected.get("story.txt").expect("story blob"),
            b"line 1\nold\nline 3\n"
        );
    }

    #[test]
    fn index_blob_expected_result_preserves_no_final_newline_markers() {
        let file = test_file("story.txt");
        let hunk = test_hunk(
            0,
            1,
            1,
            " line 1\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        );
        let mut pre_blobs = BTreeMap::new();
        pre_blobs.insert("story.txt".to_string(), b"line 1\nold".to_vec());

        let expected = expected_index_blobs_after_selected(
            &pre_blobs,
            &[(&file, &hunk)],
            StageAction::PrepareCommit,
        )
        .expect("expected no-final-newline blob");

        assert_eq!(
            expected.get("story.txt").expect("story blob"),
            b"line 1\nnew"
        );
    }

    #[test]
    fn index_blob_expected_result_rejects_wrong_location_result() {
        let file = test_file("story.txt");
        let hunk = test_hunk(0, 1, 1, " alpha\n-target\n+changed\n gamma\n");
        let mut pre_blobs = BTreeMap::new();
        pre_blobs.insert(
            "story.txt".to_string(),
            b"alpha\ntarget\ngamma\ntarget\n".to_vec(),
        );

        let expected = expected_index_blobs_after_selected(
            &pre_blobs,
            &[(&file, &hunk)],
            StageAction::PrepareCommit,
        )
        .expect("expected blob");
        let wrong_location = b"alpha\ntarget\ngamma\nchanged\n".to_vec();

        assert_ne!(
            expected.get("story.txt").expect("story blob"),
            &wrong_location
        );
    }

    #[test]
    fn hunk_id_validation_rejects_leading_zeroes_and_uppercase_hash() {
        assert!(valid_hunk_id(
            "0.0.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!valid_hunk_id(
            "01.0.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!valid_hunk_id(
            "0.0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
    }

    #[test]
    fn diff_id_validation_accepts_only_canonical_lowercase_sha256() {
        assert!(valid_diff_id(&format!("sha256:{}", "a".repeat(64))));

        for diff_id in [
            format!("SHA256:{}", "a".repeat(64)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!(" sha256:{}", "a".repeat(64)),
        ] {
            assert!(!valid_diff_id(&diff_id), "{diff_id} should be invalid");
        }
    }

    #[test]
    fn validate_hunk_ids_rejects_empty_duplicate_overflow_too_long_and_cap() {
        let hash = "a".repeat(64);
        let valid = format!("0.0.{hash}");

        let empty = validate_hunk_ids(Vec::new()).expect_err("empty IDs should be rejected");
        assert_eq!(empty.0["error_type"], "invalid_request");
        assert_eq!(empty.0["offender"], "hunk_ids");

        let duplicate =
            validate_hunk_ids(vec![valid.clone(), valid.clone()]).expect_err("duplicate IDs");
        assert_eq!(duplicate.0["error_type"], "malformed_hunk_ids");
        assert_eq!(duplicate.0["hunk_id"], valid);

        let overflowing = validate_hunk_ids(vec![format!("{}0.0.{hash}", usize::MAX)])
            .expect_err("overflowing file index");
        assert_eq!(overflowing.0["error_type"], "malformed_hunk_ids");

        let too_long = validate_hunk_ids(vec![format!("{}.0.{}", "1".repeat(32), hash)])
            .expect_err("overlong hunk ID");
        assert_eq!(too_long.0["error_type"], "malformed_hunk_ids");

        let over_cap: Vec<String> = (0..=MAX_GIT_SELECTED_HUNKS)
            .map(|idx| format!("0.{idx}.{hash}"))
            .collect();
        let capped = validate_hunk_ids(over_cap).expect_err("selection cap should be enforced");
        assert_eq!(capped.0["error_type"], "malformed_hunk_ids");
        assert_eq!(capped.0["max_hunk_ids"], MAX_GIT_SELECTED_HUNKS);
    }

    #[test]
    fn ls_files_preflight_parses_skip_worktree_assume_unchanged_and_intent_flags() {
        let entries = parse_ls_files_preflight_stdout(
            concat!(
                "H 100644 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tstory.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 0\n",
                "S 100644 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tskip.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 40004000\n",
                "h 100755 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tassume.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 8000\n",
                "H 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tintent.txt",
                "\0  ctime: 0:0\n",
                "  size: 0\tflags: 20004000\n",
                "H 100644 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tcafé.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 0\n",
                "H 100644 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tdir/file\tname.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 0\n",
                "H 100644 2e65efe2a145dda7ee51d1741299f848e5bf752e 0\tdir/line\nname.txt",
                "\0  ctime: 1:2\n",
                "  size: 1\tflags: 0\n",
            )
            .as_bytes(),
        );

        let normal = entries.get("story.txt").expect("normal entry");
        assert_eq!(normal.mode, "100644");
        assert!(!normal.skip_worktree);
        assert!(!normal.assume_unchanged);
        assert!(!normal.intent_to_add);

        let skip = entries.get("skip.txt").expect("skip-worktree entry");
        assert!(skip.skip_worktree);
        assert!(!skip.assume_unchanged);
        assert!(!skip.intent_to_add);

        let assume = entries.get("assume.txt").expect("assume-unchanged entry");
        assert_eq!(assume.mode, "100755");
        assert!(!assume.skip_worktree);
        assert!(assume.assume_unchanged);
        assert!(!assume.intent_to_add);

        let intent = entries.get("intent.txt").expect("intent-to-add entry");
        assert_eq!(intent.mode, "100644");
        assert!(!intent.skip_worktree);
        assert!(!intent.assume_unchanged);
        assert!(intent.intent_to_add);

        let utf8 = entries.get("café.txt").expect("quoted UTF-8 entry");
        assert_eq!(utf8.mode, "100644");
        assert!(!utf8.skip_worktree);
        assert!(!utf8.assume_unchanged);
        assert!(!utf8.intent_to_add);

        assert!(
            entries.contains_key("dir/file\tname.txt"),
            "NUL-delimited parser should preserve tabs in pathnames"
        );
        assert!(
            entries.contains_key("dir/line\nname.txt"),
            "NUL-delimited parser should preserve newlines in pathnames"
        );
    }

    #[test]
    fn validate_worktree_regular_file_accepts_single_link_regular_leaf() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("story.txt"), "before\n").expect("write fixture");

        validate_worktree_regular_file(temp.path(), "story.txt").expect("single-link file");
    }

    #[test]
    fn validate_worktree_regular_file_rejects_directory_leaf() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(temp.path().join("story.txt")).expect("create directory leaf");

        let err = validate_worktree_regular_file(temp.path(), "story.txt")
            .expect_err("directory leaf should be rejected");

        assert_eq!(err.0["error_type"], "unsupported_patch_record");
        assert!(
            err.0["content"][0]["text"]
                .as_str()
                .is_some_and(|message| message.contains("target must be a regular file"))
        );
    }

    #[test]
    fn validate_worktree_regular_file_rejects_non_directory_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("dir"), "not a directory\n")
            .expect("write non-directory ancestor");

        let err = validate_worktree_regular_file(temp.path(), "dir/story.txt")
            .expect_err("non-directory ancestor should be rejected");

        assert_eq!(err.0["error_type"], "unsupported_patch_record");
        assert!(
            err.0["content"][0]["text"]
                .as_str()
                .is_some_and(|message| message.contains("path ancestor must be a directory"))
        );
    }

    #[test]
    fn validate_worktree_regular_file_rejects_symlink_leaf_when_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("story.txt");
        let link = temp.path().join("story-link.txt");
        std::fs::write(&target, "before\n").expect("write fixture");
        if let Err(err) = create_file_symlink(&target, &link) {
            eprintln!("skipping symlink validator test because symlinks are unavailable: {err}");
            return;
        }

        let err = validate_worktree_regular_file(temp.path(), "story-link.txt")
            .expect_err("symlink leaf should be rejected");

        assert_eq!(err.0["error_type"], "unsupported_patch_record");
        assert!(
            err.0["content"][0]["text"]
                .as_str()
                .is_some_and(|message| message.contains("symlink path components"))
        );
    }

    #[test]
    fn validate_worktree_regular_file_rejects_symlink_ancestor_when_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        let link = temp.path().join("dir");
        std::fs::create_dir(&target).expect("create target dir");
        if let Err(err) = create_dir_symlink(&target, &link) {
            eprintln!(
                "skipping symlink ancestor validator test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = validate_worktree_regular_file(temp.path(), "dir/story.txt")
            .expect_err("symlink ancestor should be rejected");

        assert_eq!(err.0["error_type"], "unsupported_patch_record");
        assert!(
            err.0["content"][0]["text"]
                .as_str()
                .is_some_and(|message| message.contains("symlink path components"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_point_detector_flags_symlink_metadata_when_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("story.txt");
        let link = temp.path().join("story-link.txt");
        std::fs::write(&target, "before\n").expect("write fixture");
        if let Err(err) = std::os::windows::fs::symlink_file(&target, &link) {
            eprintln!(
                "skipping reparse-point detector test because symlinks are unavailable: {err}"
            );
            return;
        }

        let metadata = std::fs::symlink_metadata(&link).expect("symlink metadata");

        assert!(is_reparse_point(&metadata));
    }

    #[test]
    fn validate_worktree_regular_file_rejects_hardlinked_leaf() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("story.txt");
        let hardlink = temp.path().join("story-copy.txt");
        std::fs::write(&original, "before\n").expect("write fixture");
        if let Err(err) = std::fs::hard_link(&original, &hardlink) {
            eprintln!("skipping hardlink validator test because hard links are unavailable: {err}");
            return;
        }

        let err = validate_worktree_regular_file(temp.path(), "story.txt")
            .expect_err("hardlinked leaf should be rejected");

        assert_eq!(err.0["error_type"], "unsupported_patch_record");
        assert_eq!(err.0["link_count"], 2);
        assert!(
            err.0["content"][0]["text"]
                .as_str()
                .is_some_and(|message| message.contains("hardlinked target files"))
        );
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn create_file_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "file symlinks are not supported on this platform",
        ))
    }

    #[cfg(unix)]
    fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn create_dir_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "directory symlinks are not supported on this platform",
        ))
    }

    #[tokio::test]
    async fn git_apply_non_check_nonzero_without_proof_is_state_unknown() {
        let classification =
            classify_apply_result(&repo(), &exec(false, false, Some(true)), false, false, 100)
                .await;

        assert_eq!(classification.state, "state_unknown");
        assert!(classification.is_error);
        assert_eq!(
            classification.state_unknown_reason,
            Some("unproved_git_nonzero")
        );
    }

    #[tokio::test]
    async fn git_apply_check_nonzero_is_failed_without_state_unknown_reason() {
        let classification =
            classify_apply_result(&repo(), &exec(false, false, Some(true)), true, false, 100).await;

        assert_eq!(classification.state, "failed");
        assert!(classification.is_error);
        assert_eq!(classification.state_unknown_reason, None);
    }

    #[tokio::test]
    async fn git_apply_success_classifies_checked_and_applied() {
        let checked =
            classify_apply_result(&repo(), &exec(true, false, Some(true)), true, false, 100).await;
        let applied =
            classify_apply_result(&repo(), &exec(true, false, Some(true)), false, false, 100).await;

        assert_eq!(checked.state, "checked");
        assert!(!checked.is_error);
        assert_eq!(checked.state_unknown_reason, None);

        assert_eq!(applied.state, "applied");
        assert!(!applied.is_error);
        assert_eq!(applied.state_unknown_reason, None);
    }

    #[tokio::test]
    async fn git_apply_success_with_incomplete_stdin_is_state_unknown() {
        let classification =
            classify_apply_result(&repo(), &exec(true, false, Some(false)), true, false, 100).await;

        assert_eq!(classification.state, "state_unknown");
        assert!(classification.is_error);
        assert_eq!(classification.state_unknown_reason, Some("stdin_write"));
    }

    #[tokio::test]
    async fn git_apply_non_check_success_with_incomplete_stdin_is_state_unknown() {
        let classification =
            classify_apply_result(&repo(), &exec(true, false, Some(false)), false, false, 100)
                .await;

        assert_eq!(classification.state, "state_unknown");
        assert!(classification.is_error);
        assert_eq!(classification.state_unknown_reason, Some("stdin_write"));
    }

    #[tokio::test]
    async fn git_apply_timeout_precedes_stdin_delivery_diagnostics() {
        let classification =
            classify_apply_result(&repo(), &exec(false, true, Some(false)), true, false, 100).await;

        assert_eq!(classification.state, "state_unknown");
        assert!(classification.is_error);
        assert_eq!(classification.state_unknown_reason, Some("timeout"));
        assert_eq!(classification.conflicted, None);
        assert_eq!(classification.conflict_probe_error, None);
    }

    #[tokio::test]
    async fn git_apply_three_way_nonzero_reports_conflict_when_index_has_unmerged_entries() {
        if !git_available().await {
            eprintln!("skipping three-way conflict classifier test because git is unavailable");
            return;
        }

        let dir = tempdir_under_authority("three-way-conflict-");
        run_fixture_git(dir.path(), &["init", "-q"]).await;
        run_fixture_git(dir.path(), &["checkout", "-q", "-b", "main"]).await;
        run_fixture_git(dir.path(), &["config", "user.email", "test@example.com"]).await;
        run_fixture_git(dir.path(), &["config", "user.name", "Test User"]).await;
        std::fs::write(dir.path().join("story.txt"), "base\n").expect("write base");
        run_fixture_git(dir.path(), &["add", "story.txt"]).await;
        run_fixture_git(dir.path(), &["commit", "-q", "-m", "initial"]).await;

        run_fixture_git(dir.path(), &["checkout", "-q", "-b", "other"]).await;
        std::fs::write(dir.path().join("story.txt"), "other\n").expect("write other");
        run_fixture_git(dir.path(), &["commit", "-am", "other", "-q"]).await;

        run_fixture_git(dir.path(), &["checkout", "-q", "main"]).await;
        std::fs::write(dir.path().join("story.txt"), "main\n").expect("write main");
        run_fixture_git(dir.path(), &["commit", "-am", "main", "-q"]).await;

        let merge = run_git(
            Some(dir.path().to_string_lossy().to_string()),
            vec!["merge".to_string(), "other".to_string()],
            30_000,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("merge should spawn");
        assert!(
            !merge.success,
            "fixture merge should leave unmerged entries: stdout={} stderr={}",
            merge.stdout, merge.stderr
        );

        let working_dir = dir.path().to_string_lossy().to_string();
        let repo = resolve_repo_context(Some(&working_dir), 30_000)
            .await
            .expect("repo context");
        let classification =
            classify_apply_result(&repo, &exec(false, false, Some(true)), false, true, 30_000)
                .await;

        assert_eq!(classification.state, "state_unknown");
        assert!(classification.is_error);
        assert_eq!(
            classification.state_unknown_reason,
            Some("three_way_conflict")
        );
        assert_eq!(classification.conflicted, Some(true));
        assert_eq!(classification.conflict_probe_error, None);
    }

    #[tokio::test]
    async fn git_apply_cached_three_way_conflict_returns_state_unknown_conflicted_response() {
        if !git_available().await {
            eprintln!("skipping three-way conflict response test because git is unavailable");
            return;
        }

        let (dir, patch) = three_way_conflict_apply_fixture("three-way-response-").await;

        let response = handle_git_apply(
            None,
            json!({
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached",
                "three_way": true
            }),
        )
        .await;

        assert_eq!(response.0["isError"], true, "{:?}", response.0);
        assert_eq!(response.0["state"], "state_unknown", "{:?}", response.0);
        assert_eq!(response.0["applied"], false, "{:?}", response.0);
        assert_eq!(response.0["checked"], false, "{:?}", response.0);
        assert_eq!(
            response.0["state_unknown_reason"], "three_way_conflict",
            "{:?}",
            response.0
        );
        assert_eq!(
            response.0["error_type"], "three_way_conflict",
            "{:?}",
            response.0
        );
        assert_eq!(response.0["conflicted"], true, "{:?}", response.0);

        let unmerged = run_git(
            Some(dir.path().to_string_lossy().to_string()),
            vec!["ls-files".to_string(), "-u".to_string()],
            30_000,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("ls-files -u should spawn");
        assert!(
            !unmerged.stdout.trim().is_empty(),
            "three-way conflict should leave unmerged index entries"
        );
    }

    #[tokio::test]
    async fn git_apply_index_worktree_three_way_conflict_returns_state_unknown_conflicted_response()
    {
        if !git_available().await {
            eprintln!(
                "skipping index-worktree three-way conflict response test because git is unavailable"
            );
            return;
        }

        let (dir, patch) = three_way_conflict_apply_fixture("three-way-index-worktree-").await;

        let response = handle_git_apply(
            None,
            json!({
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "index_worktree",
                "three_way": true
            }),
        )
        .await;

        assert_eq!(response.0["isError"], true, "{:?}", response.0);
        assert_eq!(response.0["state"], "state_unknown", "{:?}", response.0);
        assert_eq!(response.0["applied"], false, "{:?}", response.0);
        assert_eq!(response.0["checked"], false, "{:?}", response.0);
        assert_eq!(
            response.0["state_unknown_reason"], "three_way_conflict",
            "{:?}",
            response.0
        );
        assert_eq!(
            response.0["error_type"], "three_way_conflict",
            "{:?}",
            response.0
        );
        assert_eq!(response.0["conflicted"], true, "{:?}", response.0);

        let unmerged = run_git(
            Some(dir.path().to_string_lossy().to_string()),
            vec!["ls-files".to_string(), "-u".to_string()],
            30_000,
            DEFAULT_GIT_STDOUT_BYTES,
            DEFAULT_GIT_STDERR_BYTES,
        )
        .await
        .expect("ls-files -u should spawn");
        assert!(
            !unmerged.stdout.trim().is_empty(),
            "index_worktree three-way conflict should leave unmerged index entries"
        );
    }

    #[test]
    fn scoped_delta_verifier_requires_unrequested_counts_to_remain_unchanged() {
        let selected = counts(&[(b"a\0-body", 1)]);
        let pre_source = counts(&[(b"a\0-body", 1), (b"b\0-body", 1)]);
        let pre_target = counts(&[(b"c\0-body", 1)]);
        let post_source = counts(&[(b"b\0-body", 1)]);
        let post_target = counts(&[(b"a\0-body", 1), (b"c\0-body", 1)]);

        assert!(verify_scoped_count_delta(
            StageAction::PrepareCommit,
            &selected,
            &pre_source,
            &pre_target,
            &post_source,
            &post_target,
        ));

        let post_target_with_extra = counts(&[
            (b"a\0-body", 1),
            (b"c\0-body", 1),
            (b"unrequested\0-body", 1),
        ]);
        assert!(!verify_scoped_count_delta(
            StageAction::PrepareCommit,
            &selected,
            &pre_source,
            &pre_target,
            &post_source,
            &post_target_with_extra,
        ));
    }

    #[test]
    fn scoped_delta_verifier_handles_reverse_unstage_direction() {
        let selected = counts(&[(b"a\0-body", 1)]);
        let pre_source = counts(&[(b"a\0-body", 1), (b"b\0-body", 1)]);
        let pre_target = counts(&[(b"c\0-body", 1)]);
        let post_source = counts(&[(b"b\0-body", 1)]);
        let post_target = counts(&[(b"a\0-body", 1), (b"c\0-body", 1)]);

        assert!(verify_scoped_count_delta(
            StageAction::Unstage,
            &selected,
            &pre_source,
            &pre_target,
            &post_source,
            &post_target,
        ));

        let post_source_with_extra = counts(&[(b"b\0-body", 1), (b"unrequested\0-body", 1)]);
        assert!(!verify_scoped_count_delta(
            StageAction::Unstage,
            &selected,
            &pre_source,
            &pre_target,
            &post_source_with_extra,
            &post_target,
        ));
    }

    #[test]
    fn scoped_delta_verifier_rejects_missing_selected_source_count() {
        let selected = counts(&[(b"a\0-body", 2)]);
        let pre_source = counts(&[(b"a\0-body", 1)]);
        let pre_target = counts(&[]);
        let post_source = counts(&[]);
        let post_target = counts(&[(b"a\0-body", 2)]);

        assert!(!verify_scoped_count_delta(
            StageAction::PrepareCommit,
            &selected,
            &pre_source,
            &pre_target,
            &post_source,
            &post_target,
        ));
        assert!(!verify_scoped_count_delta(
            StageAction::Unstage,
            &selected,
            &pre_source,
            &pre_target,
            &post_source,
            &post_target,
        ));
    }

    #[test]
    fn full_staged_group_verifier_rejects_hunkless_metadata_records() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let mut hunkless_metadata = test_file("mode-only.txt");
        hunkless_metadata.status = ChangeStatus::ModeChanged;
        hunkless_metadata.change_kinds = vec!["mode_changed".to_string()];
        hunkless_metadata.supported_for_stage_hunks = false;
        hunkless_metadata.unsupported_reason = Some("mode_changed".to_string());

        let full_staged = parsed_diff(vec![selected_file, hunkless_metadata]);

        assert!(!full_staged_diff_matches_selected_group(
            &full_staged,
            &selected_counts,
        ));
    }

    #[test]
    fn full_staged_group_verifier_rejects_unsupported_hunked_records() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let mut unsupported_selected_body = selected_file.clone();
        unsupported_selected_body.status = ChangeStatus::ModeChanged;
        unsupported_selected_body.change_kinds =
            vec!["modified".to_string(), "mode_changed".to_string()];
        unsupported_selected_body.supported_for_stage_hunks = false;
        unsupported_selected_body.unsupported_reason = Some("mode_changed".to_string());

        let full_staged = parsed_diff(vec![unsupported_selected_body]);

        assert_eq!(body_counts_from_diff(&full_staged), selected_counts);
        assert!(!full_staged_diff_matches_selected_group(
            &full_staged,
            &selected_counts,
        ));
    }

    #[test]
    fn full_staged_group_verifier_accepts_exact_supported_selected_group() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let full_staged = parsed_diff(vec![selected_file]);

        assert!(full_staged_diff_matches_selected_group(
            &full_staged,
            &selected_counts,
        ));
    }

    #[test]
    fn full_unstaged_group_verifier_accepts_selected_delta_with_unrelated_records_unchanged() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let mut unrelated_file = test_file("other.txt");
        unrelated_file.hunks = vec![test_hunk(0, 3, 3, "-before\n+after\n")];
        let mut metadata_file = test_file("mode-only.txt");
        metadata_file.status = ChangeStatus::ModeChanged;
        metadata_file.change_kinds = vec!["mode_changed".to_string()];
        metadata_file.supported_for_stage_hunks = false;
        metadata_file.unsupported_reason = Some("mode_changed".to_string());
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let selected_paths = BTreeSet::from(["story.txt".to_string()]);
        let pre_full_unstaged = parsed_diff(vec![
            selected_file,
            unrelated_file.clone(),
            metadata_file.clone(),
        ]);
        let post_full_unstaged = parsed_diff(vec![unrelated_file, metadata_file]);

        assert!(full_unstaged_diff_matches_prepare_commit_delta(
            &pre_full_unstaged,
            &post_full_unstaged,
            &selected_counts,
            &selected_paths,
        ));
    }

    #[test]
    fn full_unstaged_group_verifier_rejects_unexpected_unrelated_hunk_change() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let mut unrelated_file = test_file("other.txt");
        unrelated_file.hunks = vec![test_hunk(0, 3, 3, "-before\n+after\n")];
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let selected_paths = BTreeSet::from(["story.txt".to_string()]);
        let pre_full_unstaged = parsed_diff(vec![selected_file, unrelated_file]);
        let post_full_unstaged = parsed_diff(Vec::new());

        assert!(!full_unstaged_diff_matches_prepare_commit_delta(
            &pre_full_unstaged,
            &post_full_unstaged,
            &selected_counts,
            &selected_paths,
        ));
    }

    #[test]
    fn full_unstaged_group_verifier_rejects_unselected_same_body_relocation() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let mut unrelated_before = test_file("other.txt");
        unrelated_before.hunks = vec![test_hunk(0, 1, 1, "-target\n+changed\n")];
        let mut unrelated_after = test_file("other.txt");
        unrelated_after.hunks = vec![test_hunk(0, 3, 3, "-target\n+changed\n")];
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let selected_paths = BTreeSet::from(["story.txt".to_string()]);
        let pre_full_unstaged = parsed_diff(vec![selected_file, unrelated_before]);
        let post_full_unstaged = parsed_diff(vec![unrelated_after]);

        assert_eq!(
            body_counts_from_diff(&pre_full_unstaged),
            BTreeMap::from([
                (b"other.txt\0-target\n+changed\n".to_vec(), 1),
                (b"story.txt\0-old\n+new\n".to_vec(), 1),
            ])
        );
        assert_eq!(
            body_counts_from_diff(&post_full_unstaged),
            BTreeMap::from([(b"other.txt\0-target\n+changed\n".to_vec(), 1)])
        );
        assert!(!full_unstaged_diff_matches_prepare_commit_delta(
            &pre_full_unstaged,
            &post_full_unstaged,
            &selected_counts,
            &selected_paths,
        ));
    }

    #[test]
    fn full_unstaged_group_verifier_rejects_hunkless_metadata_change() {
        let mut selected_file = test_file("story.txt");
        selected_file.hunks = vec![test_hunk(0, 1, 1, "-old\n+new\n")];
        let mut metadata_before = test_file("mode-only.txt");
        metadata_before.status = ChangeStatus::ModeChanged;
        metadata_before.change_kinds = vec!["mode_changed".to_string()];
        metadata_before.supported_for_stage_hunks = false;
        metadata_before.unsupported_reason = Some("mode_changed".to_string());
        let mut metadata_after = metadata_before.clone();
        metadata_after.unsupported_reason = Some("type_changed".to_string());
        let selected_counts = body_counts_from_diff(&parsed_diff(vec![selected_file.clone()]));
        let selected_paths = BTreeSet::from(["story.txt".to_string()]);
        let pre_full_unstaged = parsed_diff(vec![selected_file, metadata_before]);
        let post_full_unstaged = parsed_diff(vec![metadata_after]);

        assert!(!full_unstaged_diff_matches_prepare_commit_delta(
            &pre_full_unstaged,
            &post_full_unstaged,
            &selected_counts,
            &selected_paths,
        ));
    }

    #[test]
    fn stage_failure_from_exec_returns_failure_shaped_commit_not_ready_response() {
        let response = stage_failure_from_exec(
            "apply_timeout",
            "state_unknown",
            &exec(false, true, Some(true)),
            "git apply timed out",
        );

        assert_eq!(response.0["isError"], true);
        assert_eq!(response.0["error_type"], "apply_timeout");
        assert_eq!(response.0["state"], "state_unknown");
        assert_eq!(response.0["applied"], false);
        assert_eq!(response.0["checked"], false);
        assert_eq!(response.0["commit_ready"], false);
        assert_eq!(response.0["verification_state"], "verification_unavailable");
        assert_eq!(response.0["timed_out"], true);
        assert!(
            response.0["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("timed out"))
        );
    }

    #[test]
    fn index_lock_detection_requires_the_resolved_lock_path() {
        let lock_path = Path::new("/tmp/repo/.git/index.lock");

        assert!(stderr_mentions_path(
            "fatal: Unable to create '/tmp/repo/.git/index.lock': File exists.",
            lock_path,
        ));
        assert!(!stderr_mentions_path(
            "error: tracked file named index.lock does not apply",
            lock_path,
        ));
    }
}
