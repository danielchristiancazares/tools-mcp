use super::super::path_policy;
use super::super::run_git;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{
    DEFAULT_GIT_STDERR_BYTES, DEFAULT_GIT_STDOUT_BYTES, DEFAULT_GIT_TIMEOUT_MS, MAX_GIT_ARG_BYTES,
    MAX_GIT_DIFF_FILES, MAX_GIT_DIFF_HUNKS, MAX_GIT_HUNK_BODY_BYTES, MAX_GIT_PATHSPEC_BYTES,
    MAX_GIT_PATHSPECS, MAX_GIT_STRUCTURED_RESPONSE_BYTES, MAX_OUTPUT_BYTES,
};
use tools_mcp_core::validation;

#[derive(Debug, Clone)]
pub(crate) struct RepoContext {
    pub(crate) working_dir: String,
    pub(crate) toplevel: PathBuf,
    pub(crate) identity: String,
    pub(crate) stable_identity: RepoIdentitySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoIdentitySnapshot {
    pub(crate) anchors: BTreeMap<String, RepoAnchorSnapshot>,
}

impl RepoIdentitySnapshot {
    fn baseline_matches(&self, current: &Self) -> bool {
        self.anchors
            .iter()
            .all(|(key, expected)| current.anchors.get(key) == Some(expected))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoAnchorSnapshot {
    Absent,
    Present {
        kind: &'static str,
        fs_identity: String,
        content_sha256: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HunkRequestScope {
    pub(crate) staged: bool,
    pub(crate) paths: Vec<String>,
    pub(crate) context: u32,
    pub(crate) max_bytes: usize,
    pub(crate) working_dir_arg: Option<String>,
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedDiff {
    pub(crate) diff_id: String,
    pub(crate) diff_bytes: usize,
    pub(crate) files: Vec<FileHunks>,
    pub(crate) total_hunks: usize,
    pub(crate) hunk_body_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct FileHunks {
    pub(crate) file_index: usize,
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) status: ChangeStatus,
    pub(crate) change_kinds: Vec<String>,
    pub(crate) binary: bool,
    pub(crate) supported_for_stage_hunks: bool,
    pub(crate) unsupported_reason: Option<String>,
    pub(crate) diff_header: String,
    pub(crate) old_file_header: Option<String>,
    pub(crate) new_file_header: Option<String>,
    pub(crate) extended_headers: Vec<String>,
    pub(crate) hunks: Vec<ParsedHunk>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedHunk {
    pub(crate) id: String,
    pub(crate) file_index: usize,
    pub(crate) hunk_index: usize,
    pub(crate) header: String,
    pub(crate) old_start: i64,
    pub(crate) old_lines: i64,
    pub(crate) new_start: i64,
    pub(crate) new_lines: i64,
    pub(crate) body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    ModeChanged,
    TypeChanged,
    Submodule,
    Unmerged,
}

impl ChangeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::ModeChanged => "mode_changed",
            Self::TypeChanged => "type_changed",
            Self::Submodule => "submodule",
            Self::Unmerged => "unmerged",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHunksRequest {
    #[serde(default)]
    staged: Option<bool>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    include_advanced_templates: Option<bool>,
}

pub(crate) fn structured_error(
    error_type: &'static str,
    message: impl Into<String>,
    mut extra: Vec<(&'static str, Value)>,
) -> ToolCallOutcome {
    extra.push(("error_type", json!(error_type)));
    ToolCallOutcome::err_with(message, extra)
}

pub(crate) fn invalid_request(
    message: impl Into<String>,
    offender: &'static str,
) -> ToolCallOutcome {
    structured_error(
        "invalid_request",
        message,
        vec![("offender", json!(offender))],
    )
}

pub(crate) fn parse_request<T: serde::de::DeserializeOwned>(
    args: &Value,
) -> Result<T, ToolCallOutcome> {
    serde_json::from_value(args.clone()).map_err(|err| {
        let offender = serde_error_offender(&err);
        structured_error(
            "invalid_request",
            format!("invalid arguments: {err}"),
            vec![
                ("offender", json!(offender)),
                (
                    "remediation",
                    json!("Check arguments against the tool schema."),
                ),
            ],
        )
    })
}

fn serde_error_offender(err: &serde_json::Error) -> String {
    let message = err.to_string();
    for prefix in ["unknown field `", "missing field `", "unknown variant `"] {
        if let Some(rest) = message.strip_prefix(prefix)
            && let Some((field, _)) = rest.split_once('`')
        {
            return field.to_string();
        }
    }
    "arguments".to_string()
}

pub async fn handle_git_hunks(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match parse_request::<GitHunksRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    let context = req.context.unwrap_or(3);
    let timeout_ms = match validate_timeout(req.timeout_ms) {
        Ok(timeout) => timeout,
        Err(outcome) => return outcome,
    };
    let max_bytes =
        validation::clamp_bytes(req.max_bytes, DEFAULT_GIT_STDOUT_BYTES, MAX_OUTPUT_BYTES);
    let paths = match validate_literal_paths(req.paths.unwrap_or_default()) {
        Ok(paths) => paths,
        Err(outcome) => return outcome,
    };
    let scope = HunkRequestScope {
        staged: req.staged.unwrap_or(false),
        paths,
        context,
        max_bytes,
        working_dir_arg: req.working_dir.clone(),
        timeout_ms,
    };
    let repo = match resolve_repo_context(req.working_dir.as_deref(), timeout_ms).await {
        Ok(repo) => repo,
        Err(outcome) => return outcome,
    };
    let parsed = match enumerate_diff(&repo, &scope).await {
        Ok(parsed) => parsed,
        Err(outcome) => return outcome,
    };

    let response = hunk_response(
        &parsed,
        &scope,
        req.working_dir.as_deref(),
        req.max_bytes.is_some(),
        req.include_advanced_templates.unwrap_or(false),
    );
    ToolCallOutcome::ok(response)
}

pub(crate) async fn resolve_repo_context(
    working_dir: Option<&str>,
    timeout_ms: u64,
) -> Result<RepoContext, ToolCallOutcome> {
    let resolved_working_dir = match working_dir {
        Some(value) => match path_policy::resolve_working_dir(Some(value)) {
            Ok(Some(path)) => path,
            Ok(None) => unreachable!("Some working_dir must resolve to Some path"),
            Err(err) => {
                let error_type = if err.contains("outside") {
                    "working_dir_outside_authority"
                } else {
                    "working_dir_invalid"
                };
                return Err(structured_error(
                    error_type,
                    err,
                    vec![(
                        "remediation",
                        json!("Pass an existing repository root under the server authority."),
                    )],
                ));
            }
        },
        None => path_policy::authority_root_path().map_err(|err| {
            structured_error(
                "working_dir_invalid",
                format!("failed to resolve server authority root: {err}"),
                vec![],
            )
        })?,
    };

    let working_dir_string = display_path(&resolved_working_dir);
    let discovered_toplevel = match discover_worktree_toplevel_within_authority(
        &resolved_working_dir,
    )? {
        Some(toplevel) => toplevel,
        None => {
            return Err(structured_error(
                "repo_not_found_within_authority",
                "no Git repository root was found within the server authority",
                vec![(
                    "remediation",
                    json!(
                        "Pass the repository root as working_dir, or restart the server from the repository root or a parent directory."
                    ),
                )],
            ));
        }
    };

    if discovered_toplevel != resolved_working_dir {
        return Err(structured_error(
            "working_dir_not_worktree_root",
            "working_dir must be the Git worktree root for hunk/apply tools",
            vec![
                ("resolved_working_dir", json!(working_dir_string)),
                ("git_toplevel", json!(display_path(&discovered_toplevel))),
                (
                    "remediation",
                    json!(
                        "Pass the repository root as working_dir when it is inside the server authority."
                    ),
                ),
            ],
        ));
    }

    validate_basic_git_metadata(&discovered_toplevel)?;
    let stable_identity = build_repo_identity_snapshot(&discovered_toplevel)?;

    let show_toplevel = run_git(
        Some(working_dir_string.clone()),
        vec!["rev-parse".into(), "--show-toplevel".into()],
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "working_dir_probe_unavailable",
            format!("failed to run git repository probe: {err:#}"),
            vec![],
        )
    })?;

    if show_toplevel.timed_out {
        return Err(structured_error(
            "working_dir_probe_timeout",
            "git repository probe timed out",
            vec![],
        ));
    }
    if !show_toplevel.success {
        return Err(structured_error(
            "repo_not_found_within_authority",
            "no Git repository root was found within the server authority",
            vec![(
                "remediation",
                json!(
                    "Pass the repository root as working_dir, or restart the server from the repository root or a parent directory."
                ),
            )],
        ));
    }

    let toplevel_text = show_toplevel.stdout.trim_end_matches(['\r', '\n']);
    let git_toplevel = PathBuf::from(toplevel_text).canonicalize().map_err(|err| {
        structured_error(
            "working_dir_probe_failed",
            format!("failed to canonicalize git toplevel {toplevel_text:?}: {err}"),
            vec![],
        )
    })?;

    if git_toplevel != discovered_toplevel {
        return Err(structured_error(
            "working_dir_probe_failed",
            "git repository probe did not match bounded manual discovery",
            vec![
                ("resolved_working_dir", json!(working_dir_string)),
                ("manual_toplevel", json!(display_path(&discovered_toplevel))),
                ("git_toplevel", json!(display_path(&git_toplevel))),
                (
                    "remediation",
                    json!(
                        "Inspect repository metadata and re-run from an authority-contained repository root."
                    ),
                ),
            ],
        ));
    }

    let object_format = probe_object_format(&working_dir_string, timeout_ms).await?;
    validate_unsupported_repository_feature_config(&working_dir_string, timeout_ms).await?;

    let git_dir = discovered_toplevel
        .join(".git")
        .canonicalize()
        .map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to canonicalize .git directory: {err}"),
                vec![],
            )
        })?;
    let index = git_dir.join("index");
    let identity = format!(
        "worktree={};gitdir={};index={};object_format={}",
        display_path(&discovered_toplevel),
        display_path(&git_dir),
        display_path(&index),
        object_format
    );

    Ok(RepoContext {
        working_dir: working_dir_string,
        toplevel: discovered_toplevel,
        identity,
        stable_identity,
    })
}

fn discover_worktree_toplevel_within_authority(
    start: &Path,
) -> Result<Option<PathBuf>, ToolCallOutcome> {
    let authority = path_policy::authority_root_path().map_err(|err| {
        structured_error(
            "working_dir_invalid",
            format!("failed to resolve server authority root: {err}"),
            vec![],
        )
    })?;
    if start != authority && !start.starts_with(&authority) {
        return Err(structured_error(
            "working_dir_outside_authority",
            "working_dir must resolve inside the server authority",
            vec![
                ("resolved_working_dir", json!(display_path(start))),
                ("authority_root", json!(display_path(&authority))),
            ],
        ));
    }

    for ancestor in start.ancestors() {
        if ancestor != authority && !ancestor.starts_with(&authority) {
            break;
        }
        let dot_git = ancestor.join(".git");
        match std::fs::symlink_metadata(&dot_git) {
            Ok(_) => return Ok(Some(ancestor.to_path_buf())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(structured_error(
                    "unsupported_repository_metadata",
                    format!("failed to inspect {}: {err}", dot_git.display()),
                    vec![("path", json!(display_path(&dot_git)))],
                ));
            }
        }
        if ancestor == authority {
            break;
        }
    }

    Ok(None)
}

pub(crate) fn revalidate_repo_identity(repo: &RepoContext) -> Result<(), ToolCallOutcome> {
    validate_basic_git_metadata(&repo.toplevel).map_err(repo_identity_revalidation_error)?;
    let current =
        build_repo_identity_snapshot(&repo.toplevel).map_err(repo_identity_revalidation_error)?;
    if !repo.stable_identity.baseline_matches(&current) {
        return Err(structured_error(
            "repo_identity_changed",
            "repository metadata identity changed after initial validation",
            vec![(
                "remediation",
                json!("Re-run GitHunks or inspect GitStatus/GitDiff before further mutation."),
            )],
        ));
    }
    Ok(())
}

fn repo_identity_revalidation_error(outcome: ToolCallOutcome) -> ToolCallOutcome {
    if outcome.0["error_type"].as_str() == Some("git_metadata_outside_authority") {
        return outcome;
    }
    let cause_error_type = outcome.0["error_type"].clone();
    structured_error(
        "repo_identity_changed",
        "repository metadata could not be revalidated against the initial identity",
        vec![
            ("cause_error_type", cause_error_type),
            ("cause", outcome.0),
            (
                "remediation",
                json!("Re-run GitHunks or inspect GitStatus/GitDiff before further mutation."),
            ),
        ],
    )
}

fn build_repo_identity_snapshot(toplevel: &Path) -> Result<RepoIdentitySnapshot, ToolCallOutcome> {
    let dot_git = toplevel.join(".git");
    let mut anchors = BTreeMap::new();
    snapshot_anchor(&mut anchors, "worktree", toplevel, true, false)?;
    snapshot_anchor(&mut anchors, ".git", &dot_git, true, false)?;
    snapshot_anchor(
        &mut anchors,
        ".git/objects",
        &dot_git.join("objects"),
        true,
        false,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/refs",
        &dot_git.join("refs"),
        true,
        false,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/HEAD",
        &dot_git.join("HEAD"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/config",
        &dot_git.join("config"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/packed-refs",
        &dot_git.join("packed-refs"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/info",
        &dot_git.join("info"),
        false,
        false,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/info/attributes",
        &dot_git.join("info").join("attributes"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/logs",
        &dot_git.join("logs"),
        false,
        false,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/logs/HEAD",
        &dot_git.join("logs").join("HEAD"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/objects/info",
        &dot_git.join("objects").join("info"),
        false,
        false,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/objects/info/alternates",
        &dot_git.join("objects").join("info").join("alternates"),
        false,
        true,
    )?;
    snapshot_anchor(
        &mut anchors,
        ".git/objects/pack",
        &dot_git.join("objects").join("pack"),
        false,
        false,
    )?;

    let objects = dot_git.join("objects");
    for entry in std::fs::read_dir(&objects).map_err(|err| {
        structured_error(
            "unsupported_repository_metadata",
            format!("failed to list object store {}: {err}", objects.display()),
            vec![],
        )
    })? {
        let entry = entry.map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to read object store entry: {err}"),
                vec![],
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let key = format!(".git/objects/{name}");
            snapshot_anchor(&mut anchors, &key, &entry.path(), true, false)?;
        }
    }

    Ok(RepoIdentitySnapshot { anchors })
}

fn snapshot_anchor(
    anchors: &mut BTreeMap<String, RepoAnchorSnapshot>,
    key: &str,
    path: &Path,
    required: bool,
    hash_contents: bool,
) -> Result<(), ToolCallOutcome> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(metadata_symlink_error(
            path,
            "git metadata symlink is outside the v1 support matrix",
        )),
        Ok(metadata) => {
            let kind = if metadata.is_dir() {
                "dir"
            } else if metadata.is_file() {
                "file"
            } else {
                return Err(structured_error(
                    "unsupported_repository_metadata",
                    "git metadata anchor has an unsupported filesystem type",
                    vec![("path", json!(display_path(path)))],
                ));
            };
            let fs_identity = filesystem_identity(path, &metadata).map_err(|err| {
                structured_error(
                    "unsupported_repository_metadata",
                    format!(
                        "failed to inspect git metadata identity {}: {err}",
                        path.display()
                    ),
                    vec![("path", json!(display_path(path)))],
                )
            })?;
            let content_sha256 = if hash_contents {
                if !metadata.is_file() {
                    return Err(structured_error(
                        "unsupported_repository_metadata",
                        "git metadata anchor expected a regular file",
                        vec![("path", json!(display_path(path)))],
                    ));
                }
                let bytes = std::fs::read(path).map_err(|err| {
                    structured_error(
                        "unsupported_repository_metadata",
                        format!(
                            "failed to read git metadata anchor {}: {err}",
                            path.display()
                        ),
                        vec![("path", json!(display_path(path)))],
                    )
                })?;
                Some(hex_sha256(&bytes))
            } else {
                None
            };
            anchors.insert(
                key.to_string(),
                RepoAnchorSnapshot::Present {
                    kind,
                    fs_identity,
                    content_sha256,
                },
            );
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => {
            anchors.insert(key.to_string(), RepoAnchorSnapshot::Absent);
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(structured_error(
            "unsupported_repository_metadata",
            "required git metadata anchor is missing",
            vec![("path", json!(display_path(path)))],
        )),
        Err(err) => Err(structured_error(
            "unsupported_repository_metadata",
            format!(
                "failed to inspect git metadata anchor {}: {err}",
                path.display()
            ),
            vec![("path", json!(display_path(path)))],
        )),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[cfg(unix)]
fn filesystem_identity(_path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;

    Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn filesystem_identity(path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<String> {
    let created = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(format!(
        "windows-portable:{}:{}:{}",
        display_path(path),
        metadata.len(),
        created
    ))
}

#[cfg(not(any(unix, windows)))]
fn filesystem_identity(path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<String> {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(format!(
        "portable:{}:{}:{}",
        display_path(path),
        metadata.len(),
        modified
    ))
}

async fn probe_object_format(
    working_dir: &str,
    timeout_ms: u64,
) -> Result<String, ToolCallOutcome> {
    let exec = run_git(
        Some(working_dir.to_string()),
        vec!["rev-parse".into(), "--show-object-format".into()],
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "object_format_probe_unavailable",
            format!("failed to probe git object format: {err:#}"),
            vec![],
        )
    })?;

    if exec.timed_out {
        return Err(structured_error(
            "object_format_probe_timeout",
            "git object-format probe timed out",
            vec![],
        ));
    }
    if !exec.success {
        return Err(structured_error(
            "object_format_probe_failed",
            "git object-format probe failed",
            vec![("stderr", json!(exec.stderr))],
        ));
    }

    let object_format = exec.stdout.trim_end_matches(['\r', '\n']).to_string();
    if object_format != "sha1" {
        return Err(structured_error(
            "unsupported_object_format",
            "GitApply, GitHunks, and GitStageHunks v1 support only SHA-1 repositories",
            vec![
                ("object_format", json!(object_format)),
                (
                    "remediation",
                    json!(
                        "Use the existing whole-file git tools or a SHA-1 repository for v1 hunk workflows."
                    ),
                ),
            ],
        ));
    }

    Ok(object_format)
}

async fn validate_unsupported_repository_feature_config(
    working_dir: &str,
    timeout_ms: u64,
) -> Result<(), ToolCallOutcome> {
    for (key, feature) in [
        ("core.sparseCheckout", "sparse checkout"),
        ("core.splitIndex", "split index"),
        ("index.sparse", "sparse index"),
    ] {
        if repo_bool_config_enabled(working_dir, timeout_ms, key).await? {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "unsupported repository metadata is enabled for v1 hunk/apply tools",
                vec![
                    ("config_key", json!(key)),
                    ("feature", json!(feature)),
                    (
                        "remediation",
                        json!(
                            "Disable sparse checkout/sparse-index/split-index before using GitApply, GitHunks, or GitStageHunks v1."
                        ),
                    ),
                ],
            ));
        }
    }

    Ok(())
}

async fn repo_bool_config_enabled(
    working_dir: &str,
    timeout_ms: u64,
    key: &'static str,
) -> Result<bool, ToolCallOutcome> {
    let exec = run_git(
        Some(working_dir.to_string()),
        vec!["config".into(), "--bool".into(), "--get".into(), key.into()],
        timeout_ms,
        DEFAULT_GIT_STDOUT_BYTES,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "working_dir_probe_unavailable",
            format!("failed to probe git repository config: {err:#}"),
            vec![("config_key", json!(key))],
        )
    })?;

    if exec.timed_out {
        return Err(structured_error(
            "working_dir_probe_timeout",
            "git repository config probe timed out",
            vec![("config_key", json!(key))],
        ));
    }

    if exec.success {
        return match exec.stdout.trim_end_matches(['\r', '\n']) {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(structured_error(
                "unsupported_repository_metadata",
                "git boolean config probe returned an unsupported value",
                vec![("config_key", json!(key)), ("value", json!(other))],
            )),
        };
    }

    if exec.exit_code == Some(1) && exec.stdout.trim().is_empty() && exec.stderr.trim().is_empty() {
        return Ok(false);
    }

    Err(structured_error(
        "unsupported_repository_metadata",
        "failed to prove unsupported repository metadata config is disabled",
        vec![
            ("config_key", json!(key)),
            ("exit_code", json!(exec.exit_code)),
            ("stderr", json!(exec.stderr)),
        ],
    ))
}

fn validate_basic_git_metadata(toplevel: &Path) -> Result<(), ToolCallOutcome> {
    let dot_git = toplevel.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).map_err(|err| {
        structured_error(
            "unsupported_repository_metadata",
            format!("failed to inspect .git metadata: {err}"),
            vec![],
        )
    })?;

    if metadata.file_type().is_symlink() {
        return Err(metadata_symlink_error(
            &dot_git,
            "v1 git hunk/apply tools require a real <worktree>/.git directory",
        ));
    }
    if !metadata.is_dir() {
        return Err(structured_error(
            "unsupported_repository_metadata",
            "v1 git hunk/apply tools require a real <worktree>/.git directory",
            vec![(
                "remediation",
                json!(
                    "Linked worktrees and .git file indirection are outside the v1 support matrix."
                ),
            )],
        ));
    }

    for relative in ["objects", "refs"] {
        let path = dot_git.join(relative);
        let child = std::fs::symlink_metadata(&path).map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to inspect {}: {err}", path.display()),
                vec![],
            )
        })?;
        if child.file_type().is_symlink() {
            return Err(metadata_symlink_error(
                &path,
                "git metadata symlink is outside the v1 support matrix",
            ));
        }
        if !child.is_dir() {
            return Err(structured_error(
                "unsupported_repository_metadata",
                format!("{} must be a real directory in v1", path.display()),
                vec![],
            ));
        }
    }
    validate_object_store_metadata(&dot_git.join("objects"))?;
    validate_repo_config_metadata(&dot_git)?;
    validate_unsupported_metadata_markers(&dot_git)?;

    let sparse_checkout = dot_git.join("info").join("sparse-checkout");
    match std::fs::symlink_metadata(&sparse_checkout) {
        Ok(_) => {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "sparse checkout metadata is outside the v1 support matrix",
                vec![("path", json!(display_path(&sparse_checkout)))],
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(structured_error(
                "unsupported_repository_metadata",
                format!("failed to inspect {}: {err}", sparse_checkout.display()),
                vec![],
            ));
        }
    }

    let entries = std::fs::read_dir(&dot_git).map_err(|err| {
        structured_error(
            "unsupported_repository_metadata",
            format!("failed to list {}: {err}", dot_git.display()),
            vec![],
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to read {} entry: {err}", dot_git.display()),
                vec![],
            )
        })?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("sharedindex.") {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "split-index shared index metadata is outside the v1 support matrix",
                vec![("path", json!(display_path(&entry.path())))],
            ));
        }
    }

    validate_index_file_extensions(&dot_git)?;

    let alternates = dot_git.join("objects").join("info").join("alternates");
    if alternates.exists()
        && std::fs::read_to_string(&alternates)
            .map(|text| !text.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(structured_error(
            "unsupported_repository_metadata",
            "non-empty objects/info/alternates is outside the v1 support matrix",
            vec![],
        ));
    }

    Ok(())
}

fn validate_unsupported_metadata_markers(dot_git: &Path) -> Result<(), ToolCallOutcome> {
    for (relative, feature) in [
        ("commondir", "common-dir indirection"),
        ("config.worktree", "per-worktree config"),
        ("shallow", "shallow repository"),
        ("info/grafts", "grafts metadata"),
        ("refs/replace", "replace refs metadata"),
    ] {
        let path = dot_git.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(metadata_symlink_error(
                    &path,
                    "git metadata feature symlink is outside the v1 support matrix",
                ));
            }
            Ok(_) => {
                return Err(structured_error(
                    "unsupported_repository_metadata",
                    "git metadata feature is outside the v1 support matrix",
                    vec![
                        ("path", json!(display_path(&path))),
                        ("feature", json!(feature)),
                        (
                            "remediation",
                            json!(
                                "Use a repository without common-dir indirection, per-worktree config, shallow state, grafts, or replace refs for GitApply, GitHunks, or GitStageHunks v1."
                            ),
                        ),
                    ],
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(structured_error(
                    "unsupported_repository_metadata",
                    format!("failed to inspect git metadata {}: {err}", path.display()),
                    vec![("path", json!(display_path(&path)))],
                ));
            }
        }
    }

    Ok(())
}

fn validate_repo_config_metadata(dot_git: &Path) -> Result<(), ToolCallOutcome> {
    let config = dot_git.join("config");
    let text = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(structured_error(
                "unsupported_repository_metadata",
                format!(
                    "failed to read repository config {}: {err}",
                    config.display()
                ),
                vec![("path", json!(display_path(&config)))],
            ));
        }
    };

    let mut section = String::new();
    for (line_index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let config_key = if section == "include" && key == "path" {
            Some("include.path".to_string())
        } else if section.starts_with("includeif ") && key == "path" {
            Some(format!("{section}.path"))
        } else if section == "core"
            && matches!(
                key.as_str(),
                "worktree" | "attributesfile" | "excludesfile" | "hookspath"
            )
        {
            Some(
                match key.as_str() {
                    "attributesfile" => "core.attributesFile",
                    "excludesfile" => "core.excludesFile",
                    "hookspath" => "core.hooksPath",
                    _ => "core.worktree",
                }
                .to_string(),
            )
        } else {
            None
        };

        if let Some(config_key) = config_key {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "repository config path-valued metadata is outside the v1 support matrix",
                vec![
                    ("path", json!(display_path(&config))),
                    ("config_key", json!(config_key)),
                    ("line", json!(line_index + 1)),
                    (
                        "remediation",
                        json!(
                            "Remove repository config includes and path-valued metadata settings before using GitApply, GitHunks, or GitStageHunks v1."
                        ),
                    ),
                ],
            ));
        }
    }

    Ok(())
}

fn metadata_symlink_error(path: &Path, unsupported_message: &'static str) -> ToolCallOutcome {
    match symlink_target_outside_authority(path) {
        Ok(Some(target)) => structured_error(
            "git_metadata_outside_authority",
            "git metadata symlink resolves outside the server authority",
            vec![
                ("path", json!(display_path(path))),
                ("target", json!(display_path(&target))),
                (
                    "remediation",
                    json!(
                        "Use a repository whose git metadata resolves inside the server authority."
                    ),
                ),
            ],
        ),
        Ok(None) => structured_error(
            "unsupported_repository_metadata",
            unsupported_message,
            vec![("path", json!(display_path(path)))],
        ),
        Err(err) => structured_error(
            "unsupported_repository_metadata",
            format!(
                "failed to inspect git metadata symlink {}: {err}",
                path.display()
            ),
            vec![("path", json!(display_path(path)))],
        ),
    }
}

fn symlink_target_outside_authority(path: &Path) -> Result<Option<PathBuf>, String> {
    let target = path
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize symlink target: {err}"))?;
    let authority = path_policy::authority_root_path()?;
    if target == authority || target.starts_with(&authority) {
        Ok(None)
    } else {
        Ok(Some(target))
    }
}

fn validate_object_store_metadata(objects: &Path) -> Result<(), ToolCallOutcome> {
    for relative in ["info", "pack"] {
        let path = objects.join(relative);
        validate_object_store_directory(&path)?;
        validate_no_symlink_children(&path)?;
    }

    let entries = std::fs::read_dir(objects).map_err(|err| {
        structured_error(
            "unsupported_repository_metadata",
            format!("failed to list object store {}: {err}", objects.display()),
            vec![],
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to read object store entry: {err}"),
                vec![],
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            validate_object_store_directory(&entry.path())?;
            validate_no_symlink_children(&entry.path())?;
        }
    }

    Ok(())
}

fn validate_object_store_directory(path: &Path) -> Result<(), ToolCallOutcome> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(metadata_symlink_error(
            path,
            "object-store metadata symlink is outside the v1 support matrix",
        )),
        Ok(metadata) if !metadata.is_dir() => Err(structured_error(
            "unsupported_repository_metadata",
            "object-store metadata non-directory is outside the v1 support matrix",
            vec![("path", json!(display_path(path)))],
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(structured_error(
            "unsupported_repository_metadata",
            format!(
                "failed to inspect object-store metadata {}: {err}",
                path.display()
            ),
            vec![],
        )),
    }
}

fn validate_no_symlink_children(path: &Path) -> Result<(), ToolCallOutcome> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(structured_error(
                "unsupported_repository_metadata",
                format!(
                    "failed to list object-store metadata {}: {err}",
                    path.display()
                ),
                vec![],
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to read object-store metadata entry: {err}"),
                vec![],
            )
        })?;
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|err| {
            structured_error(
                "unsupported_repository_metadata",
                format!("failed to inspect object-store metadata entry: {err}"),
                vec![],
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(metadata_symlink_error(
                &entry.path(),
                "object-store metadata symlink is outside the v1 support matrix",
            ));
        }
    }
    Ok(())
}

fn validate_index_file_extensions(dot_git: &Path) -> Result<(), ToolCallOutcome> {
    let index_path = dot_git.join("index");
    let bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(structured_error(
                "unsupported_repository_metadata",
                format!("failed to read {}: {err}", index_path.display()),
                vec![],
            ));
        }
    };

    let signatures = parse_index_extension_signatures(&bytes).map_err(|reason| {
        structured_error(
            "unsupported_repository_metadata",
            "failed to parse git index metadata for v1 hunk/apply tools",
            vec![
                ("path", json!(display_path(&index_path))),
                ("reason", json!(reason)),
                (
                    "remediation",
                    json!("Rewrite the index with supported metadata disabled before using v1 hunk/apply tools."),
                ),
            ],
        )
    })?;
    for signature in signatures {
        let extension = String::from_utf8_lossy(&signature).to_string();
        if signature == *b"link" {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "split-index link extension is outside the v1 support matrix",
                vec![
                    ("path", json!(display_path(&index_path))),
                    ("index_extension", json!(extension)),
                    (
                        "remediation",
                        json!(
                            "Disable split-index before using GitApply, GitHunks, or GitStageHunks v1."
                        ),
                    ),
                ],
            ));
        }
        if signature == *b"sdir" {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "sparse-index extension is outside the v1 support matrix",
                vec![
                    ("path", json!(display_path(&index_path))),
                    ("index_extension", json!(extension)),
                    (
                        "remediation",
                        json!(
                            "Disable sparse-index before using GitApply, GitHunks, or GitStageHunks v1."
                        ),
                    ),
                ],
            ));
        }
        if signature[0].is_ascii_lowercase() {
            return Err(structured_error(
                "unsupported_repository_metadata",
                "required git index extension is outside the v1 support matrix",
                vec![
                    ("path", json!(display_path(&index_path))),
                    ("index_extension", json!(extension)),
                    (
                        "remediation",
                        json!(
                            "Rewrite the index without required lowercase extensions before using GitApply, GitHunks, or GitStageHunks v1."
                        ),
                    ),
                ],
            ));
        }
    }

    Ok(())
}

fn parse_index_extension_signatures(index: &[u8]) -> Result<Vec<[u8; 4]>, &'static str> {
    parse_index_extension_signatures_with_hash_len(index, 20)
        .or_else(|_| parse_index_extension_signatures_with_hash_len(index, 32))
}

fn parse_index_extension_signatures_with_hash_len(
    index: &[u8],
    hash_len: usize,
) -> Result<Vec<[u8; 4]>, &'static str> {
    if index.len() < 12 + hash_len {
        return Err("index is too short");
    }
    if &index[..4] != b"DIRC" {
        return Err("index signature is not DIRC");
    }

    let version = read_be_u32(index, 4).ok_or("missing index version")?;
    if !(2..=4).contains(&version) {
        return Err("unsupported index version");
    }
    let entries = read_be_u32(index, 8).ok_or("missing index entry count")? as usize;
    let mut offset = 12usize;
    let extension_end = index
        .len()
        .checked_sub(hash_len)
        .ok_or("index checksum is missing")?;

    for _ in 0..entries {
        offset = skip_index_entry(index, offset, extension_end, version, hash_len)?;
    }
    if offset > extension_end {
        return Err("index entries exceed index payload");
    }

    let mut signatures = Vec::new();
    while offset < extension_end {
        if extension_end - offset < 8 {
            return Err("truncated index extension header");
        }
        let signature = [
            index[offset],
            index[offset + 1],
            index[offset + 2],
            index[offset + 3],
        ];
        let size = read_be_u32(index, offset + 4).ok_or("missing index extension size")? as usize;
        offset += 8;
        let next = offset
            .checked_add(size)
            .ok_or("index extension size overflow")?;
        if next > extension_end {
            return Err("index extension exceeds index payload");
        }
        signatures.push(signature);
        offset = next;
    }

    Ok(signatures)
}

fn skip_index_entry(
    index: &[u8],
    entry_start: usize,
    extension_end: usize,
    version: u32,
    hash_len: usize,
) -> Result<usize, &'static str> {
    let entry_header_len = 40usize
        .checked_add(hash_len)
        .and_then(|len| len.checked_add(2))
        .ok_or("index entry header length overflow")?;
    let flags_offset = entry_start
        .checked_add(40)
        .and_then(|value| value.checked_add(hash_len))
        .ok_or("index entry flags offset overflow")?;
    let mut offset = entry_start
        .checked_add(entry_header_len)
        .ok_or("index entry offset overflow")?;
    if offset > extension_end {
        return Err("truncated index entry header");
    }

    let flags = read_be_u16(index, flags_offset).ok_or("missing index entry flags")?;
    if version >= 3 && flags & 0x4000 != 0 {
        offset = offset
            .checked_add(2)
            .ok_or("index extended flags offset overflow")?;
        if offset > extension_end {
            return Err("truncated index extended flags");
        }
    }

    if version == 4 {
        offset = skip_index_v4_varint(index, offset, extension_end)?;
        let nul = find_nul(index, offset, extension_end).ok_or("missing v4 index pathname NUL")?;
        return Ok(nul + 1);
    }

    let name_len = (flags & 0x0fff) as usize;
    if name_len < 0x0fff {
        let path_end = offset
            .checked_add(name_len)
            .ok_or("index pathname length overflow")?;
        if path_end >= extension_end {
            return Err("truncated index pathname");
        }
        if index[path_end] != 0 {
            return Err("index pathname missing NUL terminator");
        }
        offset = path_end + 1;
    } else {
        let nul = find_nul(index, offset, extension_end).ok_or("missing index pathname NUL")?;
        offset = nul + 1;
    }

    let entry_len = offset
        .checked_sub(entry_start)
        .ok_or("index entry length underflow")?;
    let padding = (8 - (entry_len % 8)) % 8;
    offset = offset
        .checked_add(padding)
        .ok_or("index entry padding overflow")?;
    if offset > extension_end {
        return Err("index entry padding exceeds payload");
    }
    Ok(offset)
}

fn skip_index_v4_varint(
    index: &[u8],
    mut offset: usize,
    extension_end: usize,
) -> Result<usize, &'static str> {
    let mut bytes = 0usize;
    loop {
        if offset >= extension_end {
            return Err("truncated v4 pathname prefix varint");
        }
        let byte = index[offset];
        offset += 1;
        bytes += 1;
        if bytes > 10 {
            return Err("v4 pathname prefix varint is too long");
        }
        if byte & 0x80 == 0 {
            return Ok(offset);
        }
    }
}

fn find_nul(index: &[u8], start: usize, end: usize) -> Option<usize> {
    index[start..end]
        .iter()
        .position(|byte| *byte == 0)
        .map(|relative| start + relative)
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

pub(crate) fn validate_timeout(timeout_ms: Option<u64>) -> Result<u64, ToolCallOutcome> {
    if timeout_ms.is_some_and(|timeout| timeout < 100) {
        return Err(invalid_request(
            "timeout_ms must be an integer >= 100",
            "timeout_ms",
        ));
    }
    Ok(timeout_ms
        .unwrap_or(DEFAULT_GIT_TIMEOUT_MS)
        .clamp(100, tools_mcp_core::config::MAX_GIT_TIMEOUT_MS))
}

pub(crate) fn validate_literal_paths(paths: Vec<String>) -> Result<Vec<String>, ToolCallOutcome> {
    if paths.len() > MAX_GIT_PATHSPECS {
        return Err(structured_error(
            "path_complexity_limit",
            "too many literal path filters",
            vec![("max_paths", json!(MAX_GIT_PATHSPECS))],
        ));
    }

    let mut total_bytes = 0usize;
    let mut argv_bytes = 0usize;
    for path in &paths {
        total_bytes = total_bytes.saturating_add(path.len());
        argv_bytes = argv_bytes.saturating_add(path.len() + 1);
        validate_repo_relative_path(path).map_err(|reason| {
            structured_error(
                "invalid_pathspec",
                format!("invalid literal path filter {path:?}: {reason}"),
                vec![("path", json!(path))],
            )
        })?;
    }

    if total_bytes > MAX_GIT_PATHSPEC_BYTES || argv_bytes > MAX_GIT_ARG_BYTES {
        return Err(structured_error(
            "path_complexity_limit",
            "literal path filters exceed byte limits",
            vec![
                ("path_bytes", json!(total_bytes)),
                ("argv_bytes", json!(argv_bytes)),
                ("max_path_bytes", json!(MAX_GIT_PATHSPEC_BYTES)),
                ("max_argv_bytes", json!(MAX_GIT_ARG_BYTES)),
            ],
        ));
    }

    Ok(paths)
}

pub(crate) fn validate_repo_relative_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.chars().all(char::is_whitespace) {
        return Err("path must be non-empty and not whitespace-only");
    }
    if path.as_bytes().contains(&0) {
        return Err("path must not contain NUL");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err("path must be relative");
    }
    if path.contains('\\') {
        return Err("path must use POSIX '/' separators");
    }
    if path.contains(':') && !looks_like_literal_pathspec_magic_filename(path) {
        return Err("Windows drive, UNC, and ADS-style colon syntax are not supported");
    }
    if path.ends_with('/') {
        return Err("path must not end with a slash");
    }

    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err("path must not contain empty, '.', or '..' components");
        }
        if component.eq_ignore_ascii_case(".git") {
            return Err("path must not target .git metadata");
        }
        if is_protected_8dot3_alias(component) {
            return Err("path must not target protected metadata via 8.3 alias");
        }
        if component.ends_with(' ') || component.ends_with('.') {
            return Err("path components must not end with space or dot");
        }
        if is_reserved_windows_name(component) {
            return Err("reserved Windows device names are not supported");
        }
    }

    Ok(())
}

fn is_reserved_windows_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    let upper = base.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn is_protected_8dot3_alias(name: &str) -> bool {
    let component = name.strip_prefix('.').unwrap_or(name);
    let Some((stem, ordinal)) = component.rsplit_once('~') else {
        return false;
    };
    if !stem.eq_ignore_ascii_case("git") {
        return false;
    }
    !ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit())
}

fn looks_like_literal_pathspec_magic_filename(path: &str) -> bool {
    path.starts_with(":(") && path.contains(')') && path.matches(':').count() == 1
}

pub(crate) async fn enumerate_diff(
    repo: &RepoContext,
    scope: &HunkRequestScope,
) -> Result<ParsedDiff, ToolCallOutcome> {
    revalidate_repo_identity(repo)?;
    let args = build_hunk_diff_args(scope);
    let exec = run_git(
        Some(repo.working_dir.clone()),
        args,
        scope.timeout_ms,
        scope.max_bytes,
        DEFAULT_GIT_STDERR_BYTES,
    )
    .await
    .map_err(|err| {
        structured_error(
            "git_diff_unavailable",
            format!("failed to run git diff: {err:#}"),
            vec![],
        )
    })?;

    if exec.timed_out {
        return Err(structured_error(
            "git_diff_timeout",
            "git diff timed out",
            vec![("timed_out", json!(true))],
        ));
    }
    if !exec.success {
        return Err(structured_error(
            "git_diff_failed",
            "git diff failed",
            vec![
                ("exit_code", json!(exec.exit_code)),
                ("stderr", json!(exec.stderr)),
            ],
        ));
    }
    if exec.truncated_stdout {
        return Err(structured_error(
            "diff_output_too_large",
            "git diff output exceeded max_bytes",
            vec![
                ("truncated_stdout", json!(true)),
                ("max_bytes", json!(scope.max_bytes)),
            ],
        ));
    }

    parse_unified_diff(
        &exec.stdout_bytes,
        repo,
        scope.staged,
        scope.context,
        &scope.paths,
    )
}

fn build_hunk_diff_args(scope: &HunkRequestScope) -> Vec<String> {
    let mut args = vec![
        "--no-optional-locks".to_string(),
        "-c".to_string(),
        "core.quotePath=true".to_string(),
        "-c".to_string(),
        "core.abbrev=40".to_string(),
        "-c".to_string(),
        "core.protectNTFS=true".to_string(),
        "-c".to_string(),
        "core.protectHFS=true".to_string(),
        "-c".to_string(),
        "diff.noprefix=false".to_string(),
        "-c".to_string(),
        "diff.mnemonicPrefix=false".to_string(),
        "-c".to_string(),
        "diff.renames=false".to_string(),
        "-c".to_string(),
        "diff.renameLimit=32".to_string(),
        "-c".to_string(),
        "diff.relative=false".to_string(),
        "-c".to_string(),
        "diff.algorithm=default".to_string(),
        "-c".to_string(),
        "diff.indentHeuristic=false".to_string(),
        "-c".to_string(),
        "diff.suppressBlankEmpty=false".to_string(),
        "-c".to_string(),
        format!("diff.orderFile={}", git_null_device()),
        "--literal-pathspecs".to_string(),
        "diff".to_string(),
        "--no-color".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--src-prefix=a/".to_string(),
        "--dst-prefix=b/".to_string(),
        "--no-renames".to_string(),
        "--no-relative".to_string(),
        "--inter-hunk-context=0".to_string(),
        "--line-prefix=".to_string(),
        "--submodule=short".to_string(),
        "--ignore-submodules=all".to_string(),
        "--full-index".to_string(),
        "--abbrev=40".to_string(),
        "--diff-algorithm=default".to_string(),
        "--no-indent-heuristic".to_string(),
        format!("-U{}", scope.context),
    ];
    if scope.staged {
        args.push("--cached".to_string());
    }
    if !scope.paths.is_empty() {
        args.push("--".to_string());
        args.extend(scope.paths.iter().cloned());
    }
    args
}

#[cfg(windows)]
fn git_null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn git_null_device() -> &'static str {
    "/dev/null"
}

pub(crate) fn parse_unified_diff(
    diff: &[u8],
    repo: &RepoContext,
    staged: bool,
    context: u32,
    paths: &[String],
) -> Result<ParsedDiff, ToolCallOutcome> {
    let text = std::str::from_utf8(diff).map_err(|_| {
        structured_error(
            "non_utf8_diff",
            "git diff output contained non-UTF-8 bytes; v1 hunk responses fail closed",
            vec![],
        )
    })?;

    let diff_id = diff_id(diff, repo, staged, context, paths);
    if diff.is_empty() {
        return Ok(ParsedDiff {
            diff_id,
            diff_bytes: 0,
            files: Vec::new(),
            total_hunks: 0,
            hunk_body_bytes: 0,
        });
    }

    let lines = split_lines_keep_endings(text);
    let mut record_starts = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ")
        {
            record_starts.push(idx);
        }
    }
    if record_starts.is_empty() {
        return Err(structured_error(
            "diff_parse_error",
            "git diff output did not start with a supported file record",
            vec![],
        ));
    }
    if record_starts[0] != 0 {
        return Err(structured_error(
            "diff_parse_error",
            "git diff output contained data before the first file record",
            vec![],
        ));
    }
    record_starts.push(lines.len());

    let mut files = Vec::new();
    let mut total_hunks = 0usize;
    let mut total_body_bytes = 0usize;
    let mut seen_hunk_ids = HashSet::new();
    for window in record_starts.windows(2) {
        let start = window[0];
        let end = window[1];
        if files.len() >= MAX_GIT_DIFF_FILES {
            return Err(diff_complexity("too many files in diff"));
        }
        let file_index = files.len();
        let mut file =
            parse_file_record(&lines[start..end], file_index, repo, staged, context, paths)?;
        total_hunks = total_hunks.saturating_add(file.hunks.len());
        if total_hunks > MAX_GIT_DIFF_HUNKS {
            return Err(diff_complexity("too many hunks in diff"));
        }
        for hunk in &file.hunks {
            total_body_bytes = total_body_bytes.saturating_add(hunk.body.len());
            if total_body_bytes > MAX_GIT_HUNK_BODY_BYTES {
                return Err(diff_complexity("hunk bodies exceed byte limit"));
            }
            if !seen_hunk_ids.insert(hunk.id.clone()) {
                return Err(structured_error(
                    "hunk_id_collision",
                    "two hunks produced the same hunk ID",
                    vec![("hunk_id", json!(hunk.id))],
                ));
            }
        }
        file.file_index = file_index;
        files.push(file);
    }

    let estimated_response_bytes = diff.len().saturating_add(total_hunks.saturating_mul(256));
    if estimated_response_bytes > MAX_GIT_STRUCTURED_RESPONSE_BYTES {
        return Err(diff_complexity(
            "structured hunk response would exceed byte limit",
        ));
    }

    Ok(ParsedDiff {
        diff_id,
        diff_bytes: diff.len(),
        files,
        total_hunks,
        hunk_body_bytes: total_body_bytes,
    })
}

fn parse_file_record(
    lines: &[&str],
    file_index: usize,
    repo: &RepoContext,
    staged: bool,
    context: u32,
    paths: &[String],
) -> Result<FileHunks, ToolCallOutcome> {
    let Some(first) = lines.first() else {
        return Err(structured_error(
            "diff_parse_error",
            "empty file record",
            vec![],
        ));
    };

    if first.starts_with("diff --cc ") || first.starts_with("diff --combined ") {
        return Ok(FileHunks {
            file_index,
            path: combined_path(first),
            old_path: None,
            status: ChangeStatus::Unmerged,
            change_kinds: vec!["unmerged".to_string()],
            binary: false,
            supported_for_stage_hunks: false,
            unsupported_reason: Some("combined_or_unmerged_diff".to_string()),
            diff_header: (*first).to_string(),
            old_file_header: None,
            new_file_header: None,
            extended_headers: lines
                .iter()
                .skip(1)
                .map(|line| (*line).to_string())
                .collect(),
            hunks: Vec::new(),
        });
    }

    let (old_path, new_path) = parse_diff_git_header(first)?;
    let mut old_file_header = None;
    let mut new_file_header = None;
    let mut extended_headers = Vec::new();
    let mut hunk_ranges = Vec::new();
    let mut binary = false;
    let mut kinds = BTreeSet::new();
    let mut unsupported_metadata = false;
    kinds.insert("modified".to_string());

    let mut idx = 1usize;
    while idx < lines.len() {
        let line = lines[idx];
        if line.starts_with("@@ ") {
            let hunk_start = idx;
            idx += 1;
            while idx < lines.len() && !lines[idx].starts_with("@@ ") {
                idx += 1;
            }
            hunk_ranges.push((hunk_start, idx));
            continue;
        }

        if line.starts_with("--- ") {
            old_file_header = Some(line.to_string());
        } else if line.starts_with("+++ ") {
            new_file_header = Some(line.to_string());
        } else {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let mut recognized_metadata = false;
            if trimmed == "GIT binary patch" || trimmed.starts_with("Binary files ") {
                binary = true;
                recognized_metadata = true;
            }
            if trimmed.starts_with("new file mode") {
                kinds.insert("added".to_string());
                recognized_metadata = true;
            } else if trimmed.starts_with("deleted file mode") {
                kinds.insert("deleted".to_string());
                recognized_metadata = true;
            } else if trimmed.starts_with("rename from") || trimmed.starts_with("rename to") {
                kinds.insert("renamed".to_string());
                recognized_metadata = true;
            } else if trimmed.starts_with("copy from") || trimmed.starts_with("copy to") {
                kinds.insert("copied".to_string());
                recognized_metadata = true;
            } else if trimmed.starts_with("similarity index")
                || trimmed.starts_with("dissimilarity index")
            {
                recognized_metadata = true;
            } else if trimmed.starts_with("old mode") || trimmed.starts_with("new mode") {
                kinds.insert("mode_changed".to_string());
                recognized_metadata = true;
            } else if (trimmed.starts_with("index ") && trimmed.contains(" 160000"))
                || trimmed.starts_with("Subproject commit ")
            {
                kinds.insert("submodule".to_string());
                recognized_metadata = true;
            } else if trimmed.starts_with("index ") {
                recognized_metadata = true;
                if !kinds.contains("added")
                    && !kinds.contains("deleted")
                    && !valid_index_header(trimmed)
                {
                    unsupported_metadata = true;
                }
            }
            if !trimmed.is_empty() && !recognized_metadata && !binary {
                return Err(structured_error(
                    "diff_parse_error",
                    "unrecognized extended diff metadata",
                    vec![],
                ));
            }
            if !trimmed.is_empty() {
                extended_headers.push(line.to_string());
            }
        }
        idx += 1;
    }

    let mut change_kinds: Vec<String> = kinds.into_iter().collect();
    let status = status_from_kinds(&change_kinds, binary);
    if binary && !change_kinds.iter().any(|kind| kind == "modified") {
        change_kinds.push("modified".to_string());
    }

    let unsupported_reason = unsupported_reason(UnsupportedReasonInput {
        old_path: &old_path,
        new_path: &new_path,
        binary,
        change_kinds: &change_kinds,
        old_file_header: old_file_header.as_deref(),
        new_file_header: new_file_header.as_deref(),
        hunkless: hunk_ranges.is_empty(),
        unsupported_metadata,
    });
    let supported_for_stage_hunks = unsupported_reason.is_none();

    let mut hunks = Vec::new();
    for (hunk_index, (start, end)) in hunk_ranges.into_iter().enumerate() {
        let header = lines[start].to_string();
        let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(&header)?;
        validate_hunk_body_counts(&lines[start + 1..end], old_lines, new_lines)?;
        let body = lines[start + 1..end].concat();
        let id = hunk_id(HunkIdInput {
            repo,
            staged,
            context,
            paths,
            file_index,
            hunk_index,
            old_path: &old_path,
            new_path: &new_path,
            header: header.as_bytes(),
            body: body.as_bytes(),
        });
        hunks.push(ParsedHunk {
            id,
            file_index,
            hunk_index,
            header,
            old_start,
            old_lines,
            new_start,
            new_lines,
            body,
        });
    }

    Ok(FileHunks {
        file_index,
        path: new_path.clone(),
        old_path: Some(old_path),
        status,
        change_kinds,
        binary,
        supported_for_stage_hunks,
        unsupported_reason,
        diff_header: (*first).to_string(),
        old_file_header,
        new_file_header,
        extended_headers,
        hunks,
    })
}

struct UnsupportedReasonInput<'a> {
    old_path: &'a str,
    new_path: &'a str,
    binary: bool,
    change_kinds: &'a [String],
    old_file_header: Option<&'a str>,
    new_file_header: Option<&'a str>,
    hunkless: bool,
    unsupported_metadata: bool,
}

fn unsupported_reason(input: UnsupportedReasonInput<'_>) -> Option<String> {
    if validate_repo_relative_path(input.old_path).is_err()
        || validate_repo_relative_path(input.new_path).is_err()
    {
        return Some("invalid_path".to_string());
    }
    if input.binary {
        return Some("binary".to_string());
    }
    if input.unsupported_metadata {
        return Some("unsupported_index_header".to_string());
    }
    if input.hunkless {
        return Some("hunkless".to_string());
    }
    if input.old_path != input.new_path {
        return Some("old_new_path_mismatch".to_string());
    }
    let non_modified: Vec<_> = input
        .change_kinds
        .iter()
        .filter(|kind| kind.as_str() != "modified")
        .collect();
    if !non_modified.is_empty() {
        return Some("unsupported_change_kind".to_string());
    }
    let old_file_header_matches = input
        .old_file_header
        .map(|header| file_header_path_matches(header, "--- ", "a/", input.old_path))
        .unwrap_or(false);
    let new_file_header_matches = input
        .new_file_header
        .map(|header| file_header_path_matches(header, "+++ ", "b/", input.new_path))
        .unwrap_or(false);
    if !old_file_header_matches || !new_file_header_matches {
        return Some("unsupported_path".to_string());
    }
    None
}

fn file_header_path_matches(header: &str, marker: &str, prefix: &str, expected: &str) -> bool {
    let Some(rest) = header.trim_end_matches(['\r', '\n']).strip_prefix(marker) else {
        return false;
    };
    if rest == "/dev/null" {
        return false;
    }
    let path = if rest.starts_with('"') {
        let Ok((path, consumed)) = parse_c_quoted_path(rest) else {
            return false;
        };
        if !rest[consumed..].trim().is_empty() {
            return false;
        }
        path
    } else {
        rest.to_string()
    };
    path.strip_prefix(prefix) == Some(expected)
}

fn valid_index_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("index ") else {
        return false;
    };
    let mut parts = rest.split_whitespace();
    let Some(ids) = parts.next() else {
        return false;
    };
    let Some((old_id, new_id)) = ids.split_once("..") else {
        return false;
    };
    if !valid_sha1_prefix(old_id) || !valid_sha1_prefix(new_id) {
        return false;
    }
    match parts.next() {
        Some("100644" | "100755") => parts.next().is_none(),
        Some(_) => false,
        None => true,
    }
}

fn valid_sha1_prefix(value: &str) -> bool {
    (4..=40).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().any(|byte| byte != b'0')
}

fn status_from_kinds(change_kinds: &[String], binary: bool) -> ChangeStatus {
    let has = |name: &str| change_kinds.iter().any(|kind| kind == name);
    if has("unmerged") {
        ChangeStatus::Unmerged
    } else if has("added") {
        ChangeStatus::Added
    } else if has("deleted") {
        ChangeStatus::Deleted
    } else if has("renamed") {
        ChangeStatus::Renamed
    } else if has("copied") {
        ChangeStatus::Copied
    } else if has("type_changed") {
        ChangeStatus::TypeChanged
    } else if has("submodule") {
        ChangeStatus::Submodule
    } else if has("mode_changed") {
        ChangeStatus::ModeChanged
    } else {
        let _ = binary;
        ChangeStatus::Modified
    }
}

fn split_lines_keep_endings(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\n' => {
                lines.push(&text[start..idx + 1]);
                idx += 1;
                start = idx;
            }
            b'\r' => {
                let end = if bytes.get(idx + 1) == Some(&b'\n') {
                    idx + 2
                } else {
                    idx + 1
                };
                lines.push(&text[start..end]);
                idx = end;
                start = idx;
            }
            _ => {
                idx += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn parse_diff_git_header(line: &str) -> Result<(String, String), ToolCallOutcome> {
    let line = line.trim_end_matches(['\r', '\n']);
    let rest = line.strip_prefix("diff --git ").ok_or_else(|| {
        structured_error(
            "diff_parse_error",
            "file record missing diff --git header",
            vec![],
        )
    })?;
    let (old, new) = if rest.starts_with('"') {
        let (old, consumed) = parse_c_quoted_path(rest).map_err(|err| {
            structured_error(
                "diff_parse_error",
                format!("invalid quoted old path: {err}"),
                vec![],
            )
        })?;
        let remaining = rest[consumed..].trim_start();
        let (new, new_consumed) = parse_c_quoted_path(remaining).map_err(|err| {
            structured_error(
                "diff_parse_error",
                format!("invalid quoted new path: {err}"),
                vec![],
            )
        })?;
        if !remaining[new_consumed..].trim().is_empty() {
            return Err(structured_error(
                "diff_parse_error",
                "quoted diff --git header contains trailing data",
                vec![],
            ));
        }
        (old, new)
    } else {
        let marker = " b/";
        let mut matches = rest.match_indices(marker);
        let split = matches.next().map(|(idx, _)| idx).ok_or_else(|| {
            structured_error("diff_parse_error", "diff --git header is ambiguous", vec![])
        })?;
        if matches.next().is_some() {
            return Err(structured_error(
                "diff_parse_error",
                "diff --git header is ambiguous",
                vec![],
            ));
        }
        (rest[..split].to_string(), rest[split + 1..].to_string())
    };
    let old = strip_git_prefix(&old, "a/")?;
    let new = strip_git_prefix(&new, "b/")?;
    Ok((old, new))
}

fn strip_git_prefix(path: &str, prefix: &str) -> Result<String, ToolCallOutcome> {
    let Some(stripped) = path.strip_prefix(prefix) else {
        return Err(structured_error(
            "diff_parse_error",
            format!("path {path:?} did not use expected {prefix:?} prefix"),
            vec![],
        ));
    };
    if stripped.is_empty() {
        return Err(structured_error(
            "diff_parse_error",
            "stripped path was empty",
            vec![],
        ));
    }
    Ok(stripped.to_string())
}

pub(crate) fn parse_c_quoted_path(input: &str) -> Result<(String, usize), &'static str> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return Err("path does not start with a quote");
    }
    let mut out = Vec::new();
    let mut idx = 1usize;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' => {
                let path = String::from_utf8(out).map_err(|_| "quoted path is not valid UTF-8")?;
                return Ok((path, idx + 1));
            }
            b'\\' => {
                idx += 1;
                if idx >= bytes.len() {
                    return Err("unterminated quoted path escape");
                }
                let byte = bytes[idx];
                if byte.is_ascii_digit() && byte < b'8' {
                    let mut value = (byte - b'0') as u16;
                    for _ in 0..2 {
                        let Some(next) = bytes.get(idx + 1).copied() else {
                            break;
                        };
                        if !next.is_ascii_digit() || next >= b'8' {
                            break;
                        }
                        value = (value * 8) + u16::from(next - b'0');
                        idx += 1;
                    }
                    if value > u16::from(u8::MAX) {
                        return Err("octal escape exceeds byte range");
                    }
                    out.push(value as u8);
                } else {
                    out.push(match byte {
                        b'a' => 0x07,
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'v' => 0x0b,
                        b'"' => b'"',
                        b'\\' => b'\\',
                        other => other,
                    });
                }
            }
            byte => out.push(byte),
        }
        idx += 1;
    }
    Err("unterminated quoted path")
}

fn parse_hunk_header(header: &str) -> Result<(i64, i64, i64, i64), ToolCallOutcome> {
    let header = header.trim_end_matches(['\r', '\n']);
    let rest = header.strip_prefix("@@ ").ok_or_else(|| {
        structured_error(
            "diff_parse_error",
            format!("invalid hunk header {header:?}"),
            vec![],
        )
    })?;
    let end = rest.find(" @@").ok_or_else(|| {
        structured_error(
            "diff_parse_error",
            format!("invalid hunk header {header:?}"),
            vec![],
        )
    })?;
    let range_text = &rest[..end];
    let mut parts = range_text.split_whitespace();
    let old = parts.next().ok_or_else(|| {
        structured_error(
            "diff_parse_error",
            format!("invalid hunk header {header:?}"),
            vec![],
        )
    })?;
    let new = parts.next().ok_or_else(|| {
        structured_error(
            "diff_parse_error",
            format!("invalid hunk header {header:?}"),
            vec![],
        )
    })?;
    let (old_start, old_lines) = parse_range(old, '-')?;
    let (new_start, new_lines) = parse_range(new, '+')?;
    Ok((old_start, old_lines, new_start, new_lines))
}

fn validate_hunk_body_counts(
    body_lines: &[&str],
    expected_old: i64,
    expected_new: i64,
) -> Result<(), ToolCallOutcome> {
    let expected_old = usize::try_from(expected_old).map_err(|_| {
        structured_error(
            "diff_parse_error",
            "hunk old-line count exceeds supported range",
            vec![],
        )
    })?;
    let expected_new = usize::try_from(expected_new).map_err(|_| {
        structured_error(
            "diff_parse_error",
            "hunk new-line count exceeds supported range",
            vec![],
        )
    })?;

    let mut actual_old = 0usize;
    let mut actual_new = 0usize;
    for line in body_lines {
        match line.as_bytes().first().copied() {
            Some(b' ') => {
                actual_old = actual_old.saturating_add(1);
                actual_new = actual_new.saturating_add(1);
            }
            Some(b'-') => {
                actual_old = actual_old.saturating_add(1);
            }
            Some(b'+') => {
                actual_new = actual_new.saturating_add(1);
            }
            Some(b'\\')
                if line.trim_end_matches(['\r', '\n']) == "\\ No newline at end of file" => {}
            _ => {
                return Err(structured_error(
                    "diff_parse_error",
                    "hunk body contained a malformed line",
                    vec![],
                ));
            }
        }
    }

    if actual_old != expected_old || actual_new != expected_new {
        return Err(structured_error(
            "diff_parse_error",
            "hunk body line counts did not match hunk header",
            vec![
                ("expected_old_lines", json!(expected_old)),
                ("expected_new_lines", json!(expected_new)),
                ("actual_old_lines", json!(actual_old)),
                ("actual_new_lines", json!(actual_new)),
            ],
        ));
    }

    Ok(())
}

fn parse_range(text: &str, prefix: char) -> Result<(i64, i64), ToolCallOutcome> {
    let Some(text) = text.strip_prefix(prefix) else {
        return Err(structured_error(
            "diff_parse_error",
            "hunk range missing prefix",
            vec![],
        ));
    };
    let (start, lines) = match text.split_once(',') {
        Some((start, lines)) => (start, lines),
        None => (text, "1"),
    };
    let start = start
        .parse::<i64>()
        .map_err(|_| structured_error("diff_parse_error", "invalid hunk range start", vec![]))?;
    let lines = lines
        .parse::<i64>()
        .map_err(|_| structured_error("diff_parse_error", "invalid hunk range length", vec![]))?;
    if start < 0 || lines < 0 {
        return Err(structured_error(
            "diff_parse_error",
            "hunk range values must be non-negative",
            vec![],
        ));
    }
    Ok((start, lines))
}

fn diff_id(
    diff: &[u8],
    repo: &RepoContext,
    staged: bool,
    context: u32,
    paths: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hash_len_bytes(&mut hasher, b"tools-mcp-git-diff-v1");
    hash_len_bytes(&mut hasher, repo.identity.as_bytes());
    hasher.update([u8::from(staged)]);
    hasher.update(context.to_le_bytes());
    for path in paths {
        hash_len_bytes(&mut hasher, path.as_bytes());
    }
    hash_len_bytes(&mut hasher, diff);
    format!("sha256:{:x}", hasher.finalize())
}

struct HunkIdInput<'a> {
    repo: &'a RepoContext,
    staged: bool,
    context: u32,
    paths: &'a [String],
    file_index: usize,
    hunk_index: usize,
    old_path: &'a str,
    new_path: &'a str,
    header: &'a [u8],
    body: &'a [u8],
}

fn hunk_id(input: HunkIdInput<'_>) -> String {
    let mut hasher = Sha256::new();
    hash_len_bytes(&mut hasher, b"tools-mcp-git-hunk-v1");
    hash_len_bytes(&mut hasher, input.repo.identity.as_bytes());
    hasher.update([u8::from(input.staged)]);
    hasher.update(input.context.to_le_bytes());
    for path in input.paths {
        hash_len_bytes(&mut hasher, path.as_bytes());
    }
    hash_len_bytes(&mut hasher, input.old_path.as_bytes());
    hash_len_bytes(&mut hasher, input.new_path.as_bytes());
    hash_len_bytes(&mut hasher, input.header);
    hash_len_bytes(&mut hasher, input.body);
    let digest = format!("{:x}", hasher.finalize());
    format!("{}.{}.{}", input.file_index, input.hunk_index, digest)
}

fn hash_len_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn combined_path(line: &str) -> String {
    line.split_whitespace()
        .nth(2)
        .unwrap_or("<combined>")
        .to_string()
}

fn diff_complexity(message: &'static str) -> ToolCallOutcome {
    structured_error("diff_complexity_limit", message, vec![])
}

fn hunk_response(
    parsed: &ParsedDiff,
    scope: &HunkRequestScope,
    working_dir: Option<&str>,
    max_bytes_supplied: bool,
    include_advanced_templates: bool,
) -> Value {
    let files_json: Vec<Value> = parsed.files.iter().map(file_json).collect();
    let recommended_next_action = if scope.staged {
        "unstage"
    } else {
        "prepare_commit"
    };
    let mut template = json!({
        "name": "GitStageHunks",
        "arguments": {
            "diff_id": parsed.diff_id,
            "hunk_ids": [],
            "action": recommended_next_action,
            "context": scope.context,
            "paths": scope.paths,
        }
    });
    if let Some(working_dir) = working_dir {
        template["arguments"]["working_dir"] = json!(working_dir);
    }
    if max_bytes_supplied {
        template["arguments"]["max_bytes"] = json!(scope.max_bytes);
    }

    let mut response = json!({
        "content": [{"type": "text", "text": hunk_summary_text(parsed, scope)}],
        "isError": false,
        "diff_id": parsed.diff_id,
        "staged": scope.staged,
        "context": scope.context,
        "paths": scope.paths,
        "max_bytes": scope.max_bytes,
        "diff_bytes": parsed.diff_bytes,
        "counts": {
            "files": parsed.files.len(),
            "hunks": parsed.total_hunks,
            "hunk_body_bytes": parsed.hunk_body_bytes,
        },
        "recommended_next_action": recommended_next_action,
        "recommended_next_action_template": template,
        "files": files_json,
    });

    if include_advanced_templates && !scope.staged {
        let mut advanced = response["recommended_next_action_template"].clone();
        advanced["arguments"]["action"] = json!("stage_only");
        response["advanced_stage_only_template"] = advanced;
    }

    response
}

fn hunk_summary_text(parsed: &ParsedDiff, scope: &HunkRequestScope) -> String {
    if parsed.files.is_empty() {
        if scope.staged {
            "no staged hunks".to_string()
        } else {
            "no unstaged hunks".to_string()
        }
    } else {
        format!(
            "{} file(s), {} hunk(s) in {} diff",
            parsed.files.len(),
            parsed.total_hunks,
            if scope.staged { "staged" } else { "unstaged" }
        )
    }
}

fn file_json(file: &FileHunks) -> Value {
    json!({
        "file_index": file.file_index,
        "path": file.path,
        "old_path": file.old_path,
        "status": file.status.as_str(),
        "change_kinds": file.change_kinds,
        "binary": file.binary,
        "supported_for_stage_hunks": file.supported_for_stage_hunks,
        "unsupported_reason": file.unsupported_reason,
        "diff_header": file.diff_header,
        "old_file_header": file.old_file_header,
        "new_file_header": file.new_file_header,
        "extended_headers": file.extended_headers,
        "hunks": file.hunks.iter().map(hunk_json).collect::<Vec<_>>(),
    })
}

fn hunk_json(hunk: &ParsedHunk) -> Value {
    json!({
        "id": hunk.id,
        "file_index": hunk.file_index,
        "hunk_index": hunk.hunk_index,
        "header": hunk.header,
        "old_start": hunk.old_start,
        "old_lines": hunk.old_lines,
        "new_start": hunk.new_start,
        "new_lines": hunk.new_lines,
        "body": hunk.body,
    })
}

pub(crate) fn hunk_lookup(parsed: &ParsedDiff) -> BTreeMap<String, (&FileHunks, &ParsedHunk)> {
    let mut lookup = BTreeMap::new();
    for file in &parsed.files {
        for hunk in &file.hunks {
            lookup.insert(hunk.id.clone(), (file, hunk));
        }
    }
    lookup
}

pub(crate) fn hunk_body_key(file: &FileHunks, hunk: &ParsedHunk) -> Vec<u8> {
    let mut key = Vec::new();
    key.extend_from_slice(file.path.as_bytes());
    key.push(0);
    key.extend_from_slice(hunk.body.as_bytes());
    key
}

pub(crate) fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    strip_windows_verbatim_prefix(text)
}

#[cfg(windows)]
fn strip_windows_verbatim_prefix(path: String) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_windows_verbatim_prefix(path: String) -> String {
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn repo() -> RepoContext {
        RepoContext {
            working_dir: ".".to_string(),
            toplevel: PathBuf::from("."),
            identity: "repo-id".to_string(),
            stable_identity: RepoIdentitySnapshot {
                anchors: BTreeMap::new(),
            },
        }
    }

    fn minimal_repo_metadata() -> TempDir {
        let dir = tempfile::Builder::new()
            .prefix("git-hunks-metadata-")
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git").join("objects")).expect("objects");
        std::fs::create_dir_all(dir.path().join(".git").join("refs")).expect("refs");
        std::fs::create_dir_all(dir.path().join(".git").join("info")).expect("info");
        dir
    }

    fn minimal_repo_metadata_under_authority() -> TempDir {
        let root = path_policy::authority_root_path()
            .expect("authority root")
            .join("target")
            .join("tools-mcp-git-metadata-tests");
        std::fs::create_dir_all(&root).expect("metadata test root");
        let dir = tempfile::Builder::new()
            .prefix("git-hunks-metadata-")
            .tempdir_in(root)
            .expect("tempdir under authority");
        std::fs::create_dir_all(dir.path().join(".git").join("objects")).expect("objects");
        std::fs::create_dir_all(dir.path().join(".git").join("refs")).expect("refs");
        std::fs::create_dir_all(dir.path().join(".git").join("info")).expect("info");
        dir
    }

    fn repo_context_with_stable_identity(dir: &TempDir) -> RepoContext {
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        }
    }

    #[test]
    fn parse_request_returns_structured_invalid_request_offender() {
        let outcome = match parse_request::<GitHunksRequest>(&json!({ "unexpected": true })) {
            Ok(_) => panic!("unknown fields should be rejected with structured context"),
            Err(outcome) => outcome,
        };

        assert_eq!(outcome.0["error_type"], "invalid_request");
        assert_eq!(outcome.0["offender"], "unexpected");
        assert!(
            outcome.0["remediation"].as_str().is_some(),
            "invalid request responses should carry remediation"
        );
    }

    #[test]
    fn bounded_discovery_finds_nearest_authority_contained_git_directory() {
        let dir = minimal_repo_metadata_under_authority();
        let nested = dir.path().join("src").join("nested");
        std::fs::create_dir_all(&nested).expect("nested directory");

        let discovered = discover_worktree_toplevel_within_authority(&nested)
            .expect("discovery should run")
            .expect("repo should be discovered");

        assert_eq!(discovered, dir.path());
    }

    fn index_with_extension(signature: &[u8; 4], payload_len: u32) -> Vec<u8> {
        let mut index = Vec::new();
        index.extend_from_slice(b"DIRC");
        index.extend_from_slice(&2u32.to_be_bytes());
        index.extend_from_slice(&0u32.to_be_bytes());
        index.extend_from_slice(signature);
        index.extend_from_slice(&payload_len.to_be_bytes());
        index.resize(index.len() + payload_len as usize, 0);
        index.resize(index.len() + 20, 0);
        index
    }

    #[test]
    fn basic_metadata_rejects_sparse_checkout_marker() {
        let dir = minimal_repo_metadata();
        std::fs::write(
            dir.path().join(".git").join("info").join("sparse-checkout"),
            "/*\n",
        )
        .expect("sparse checkout marker");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
    }

    #[test]
    fn basic_metadata_rejects_split_index_marker() {
        let dir = minimal_repo_metadata();
        std::fs::write(
            dir.path().join(".git").join("sharedindex.0123456789abcdef"),
            "fixture",
        )
        .expect("shared index marker");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
    }

    #[test]
    fn basic_metadata_rejects_git_file_indirection() {
        let dir = minimal_repo_metadata();
        let dot_git = dir.path().join(".git");
        std::fs::remove_dir_all(&dot_git).expect("remove .git directory");
        std::fs::write(&dot_git, "gitdir: ../outside\n").expect(".git file indirection");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
    }

    #[test]
    fn basic_metadata_rejects_repository_config_include_path() {
        let dir = minimal_repo_metadata();
        let config = dir.path().join(".git").join("config");
        std::fs::write(&config, "[include]\n\tpath = ../shared.gitconfig\n")
            .expect("repository config include");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["config_key"], "include.path");
    }

    #[test]
    fn basic_metadata_rejects_repository_config_include_if_path() {
        let dir = minimal_repo_metadata();
        let config = dir.path().join(".git").join("config");
        std::fs::write(
            &config,
            "[includeIf \"gitdir:../\"]\n\tpath = ../conditional.gitconfig\n",
        )
        .expect("repository conditional config include");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["config_key"], "includeif \"gitdir:../\".path");
    }

    #[test]
    fn basic_metadata_rejects_path_valued_core_config() {
        let dir = minimal_repo_metadata();
        let config = dir.path().join(".git").join("config");
        std::fs::write(&config, "[core]\n\tattributesFile = ../attrs\n")
            .expect("repository path-valued core config");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["config_key"], "core.attributesFile");
    }

    #[test]
    fn basic_metadata_rejects_unsupported_metadata_markers() {
        for (idx, (relative, feature)) in [
            ("commondir", "common-dir indirection"),
            ("config.worktree", "per-worktree config"),
            ("shallow", "shallow repository"),
            ("info/grafts", "grafts metadata"),
            ("refs/replace", "replace refs metadata"),
        ]
        .into_iter()
        .enumerate()
        {
            let dir = minimal_repo_metadata();
            let marker = dir.path().join(".git").join(relative);
            if let Some(parent) = marker.parent() {
                std::fs::create_dir_all(parent).expect("marker parent");
            }
            if relative == "refs/replace" {
                std::fs::create_dir(&marker).expect("replace refs directory");
            } else {
                std::fs::write(&marker, format!("fixture-{idx}\n")).expect("metadata marker");
            }

            let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

            assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
            assert_eq!(err.0["feature"], feature);
            assert_eq!(err.0["path"], display_path(&marker));
        }
    }

    #[test]
    fn basic_metadata_rejects_authority_escaping_metadata_marker_symlink_with_precedence() {
        let dir = minimal_repo_metadata_under_authority();
        let authority = path_policy::authority_root_path().expect("authority root");
        let Some(target) = authority.parent() else {
            eprintln!(
                "skipping metadata marker authority escape test because authority root has no parent"
            );
            return;
        };
        let marker = dir.path().join(".git").join("config.worktree");
        if let Err(err) = create_dir_symlink(target, &marker) {
            eprintln!(
                "skipping metadata marker authority-escape symlink test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "git_metadata_outside_authority");
        assert_eq!(err.0["path"], display_path(&marker));
    }

    #[test]
    fn basic_metadata_rejects_non_empty_object_alternates() {
        let dir = minimal_repo_metadata();
        let object_info = dir.path().join(".git").join("objects").join("info");
        std::fs::create_dir_all(&object_info).expect("objects/info directory");
        std::fs::write(object_info.join("alternates"), "../other-objects\n")
            .expect("non-empty alternates");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
    }

    #[test]
    fn basic_metadata_rejects_authority_contained_symlinked_refs_directory_as_unsupported() {
        let dir = minimal_repo_metadata_under_authority();
        let target = dir.path().join("inside-refs");
        let link = dir.path().join(".git").join("refs");
        std::fs::remove_dir_all(&link).expect("remove refs directory");
        std::fs::create_dir(&target).expect("target dir");
        if let Err(err) = create_dir_symlink(&target, &link) {
            eprintln!(
                "skipping refs symlink validator test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["path"], display_path(&link));
    }

    #[test]
    fn basic_metadata_rejects_authority_contained_symlinked_object_fanout_as_unsupported() {
        let dir = minimal_repo_metadata_under_authority();
        let target = dir.path().join("inside-objects");
        let link = dir.path().join(".git").join("objects").join("aa");
        std::fs::create_dir(&target).expect("target dir");
        if let Err(err) = create_dir_symlink(&target, &link) {
            eprintln!(
                "skipping object-store symlink validator test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["path"], display_path(&link));
    }

    #[test]
    fn basic_metadata_rejects_authority_escaping_symlinked_object_fanout() {
        let dir = minimal_repo_metadata_under_authority();
        let authority = path_policy::authority_root_path().expect("authority root");
        let Some(target) = authority.parent() else {
            eprintln!("skipping authority escape test because authority root has no parent");
            return;
        };
        let link = dir.path().join(".git").join("objects").join("aa");
        if let Err(err) = create_dir_symlink(target, &link) {
            eprintln!(
                "skipping object-store authority-escape symlink test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "git_metadata_outside_authority");
        assert_eq!(err.0["path"], display_path(&link));
        assert_eq!(
            err.0["target"],
            display_path(&target.canonicalize().expect("canonical target"))
        );
    }

    #[test]
    fn repo_identity_revalidation_detects_same_path_dot_git_replacement() {
        let dir = minimal_repo_metadata();
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        let repo = RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        };

        let original = dir.path().join(".git");
        let moved = dir.path().join(".git-original");
        std::fs::rename(&original, &moved).expect("move original .git aside");
        std::fs::create_dir_all(original.join("objects")).expect("replacement objects");
        std::fs::create_dir_all(original.join("refs")).expect("replacement refs");

        let err = revalidate_repo_identity(&repo).expect_err("identity should change");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_head_content_change() {
        let dir = minimal_repo_metadata();
        let head = dir.path().join(".git").join("HEAD");
        std::fs::write(&head, "ref: refs/heads/main\n").expect("initial HEAD");
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        let repo = RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        };

        std::fs::write(&head, "ref: refs/heads/other\n").expect("changed HEAD");

        let err = revalidate_repo_identity(&repo).expect_err("HEAD change should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_config_content_change() {
        let dir = minimal_repo_metadata();
        let config = dir.path().join(".git").join("config");
        std::fs::write(&config, "[core]\n\trepositoryformatversion = 0\n").expect("initial config");
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        let repo = RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        };

        std::fs::write(
            &config,
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )
        .expect("changed config");

        let err = revalidate_repo_identity(&repo).expect_err("config change should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_info_attributes_content_change() {
        let dir = minimal_repo_metadata();
        let attributes = dir.path().join(".git").join("info").join("attributes");
        std::fs::write(&attributes, "*.txt text\n").expect("initial attributes");
        let repo = repo_context_with_stable_identity(&dir);

        std::fs::write(&attributes, "*.txt -text\n").expect("changed attributes");

        let err =
            revalidate_repo_identity(&repo).expect_err("info attributes change should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_info_directory_replacement() {
        let dir = minimal_repo_metadata();
        let info = dir.path().join(".git").join("info");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir.path().join(".git").join("info-original");
        std::fs::rename(&info, &moved).expect("move info aside");
        std::fs::create_dir(&info).expect("replacement info");

        let err = revalidate_repo_identity(&repo).expect_err("info replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_refs_directory_replacement() {
        let dir = minimal_repo_metadata();
        let refs = dir.path().join(".git").join("refs");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir.path().join(".git").join("refs-original");
        std::fs::rename(&refs, &moved).expect("move refs aside");
        std::fs::create_dir(&refs).expect("replacement refs");

        let err = revalidate_repo_identity(&repo).expect_err("refs replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_packed_refs_content_change() {
        let dir = minimal_repo_metadata();
        let packed_refs = dir.path().join(".git").join("packed-refs");
        std::fs::write(
            &packed_refs,
            "# pack-refs with: peeled fully-peeled sorted\n",
        )
        .expect("initial packed-refs");
        let repo = repo_context_with_stable_identity(&dir);

        std::fs::write(
            &packed_refs,
            "# pack-refs with: peeled fully-peeled sorted\n0123456789012345678901234567890123456789 refs/heads/main\n",
        )
        .expect("changed packed-refs");

        let err = revalidate_repo_identity(&repo).expect_err("packed-refs change should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_logs_head_content_change() {
        let dir = minimal_repo_metadata();
        let logs = dir.path().join(".git").join("logs");
        std::fs::create_dir(&logs).expect("logs directory");
        let logs_head = logs.join("HEAD");
        std::fs::write(&logs_head, "old HEAD log\n").expect("initial logs/HEAD");
        let repo = repo_context_with_stable_identity(&dir);

        std::fs::write(&logs_head, "new HEAD log\n").expect("changed logs/HEAD");

        let err = revalidate_repo_identity(&repo).expect_err("logs/HEAD change should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_objects_info_directory_replacement() {
        let dir = minimal_repo_metadata();
        let objects_info = dir.path().join(".git").join("objects").join("info");
        std::fs::create_dir(&objects_info).expect("objects/info directory");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir
            .path()
            .join(".git")
            .join("objects")
            .join("info-original");
        std::fs::rename(&objects_info, &moved).expect("move objects/info aside");
        std::fs::create_dir(&objects_info).expect("replacement objects/info");

        let err =
            revalidate_repo_identity(&repo).expect_err("objects/info replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_objects_directory_replacement() {
        let dir = minimal_repo_metadata();
        let objects = dir.path().join(".git").join("objects");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir.path().join(".git").join("objects-original");
        std::fs::rename(&objects, &moved).expect("move objects aside");
        std::fs::create_dir(&objects).expect("replacement objects");

        let err = revalidate_repo_identity(&repo).expect_err("objects replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_empty_alternates_anchor_deletion() {
        let dir = minimal_repo_metadata();
        let objects_info = dir.path().join(".git").join("objects").join("info");
        std::fs::create_dir(&objects_info).expect("objects/info directory");
        let alternates = objects_info.join("alternates");
        std::fs::write(&alternates, "").expect("empty alternates");
        let repo = repo_context_with_stable_identity(&dir);

        std::fs::remove_file(&alternates).expect("delete alternates");

        let err = revalidate_repo_identity(&repo).expect_err("alternates deletion should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_objects_pack_directory_replacement() {
        let dir = minimal_repo_metadata();
        let objects_pack = dir.path().join(".git").join("objects").join("pack");
        std::fs::create_dir(&objects_pack).expect("objects/pack directory");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir
            .path()
            .join(".git")
            .join("objects")
            .join("pack-original");
        std::fs::rename(&objects_pack, &moved).expect("move objects/pack aside");
        std::fs::create_dir(&objects_pack).expect("replacement objects/pack");

        let err =
            revalidate_repo_identity(&repo).expect_err("objects/pack replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_detects_existing_object_fanout_directory_replacement() {
        let dir = minimal_repo_metadata();
        let fanout = dir.path().join(".git").join("objects").join("aa");
        std::fs::create_dir(&fanout).expect("object fanout directory");
        let repo = repo_context_with_stable_identity(&dir);

        let moved = dir.path().join(".git").join("objects").join("aa-original");
        std::fs::rename(&fanout, &moved).expect("move fanout aside");
        std::fs::create_dir(&fanout).expect("replacement fanout");

        let err = revalidate_repo_identity(&repo).expect_err("fanout replacement should reject");

        assert_eq!(err.0["error_type"], "repo_identity_changed");
    }

    #[test]
    fn repo_identity_revalidation_allows_new_object_fanout_directory() {
        let dir = minimal_repo_metadata();
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        let repo = RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        };

        std::fs::create_dir_all(dir.path().join(".git").join("objects").join("aa"))
            .expect("new fanout directory");

        revalidate_repo_identity(&repo).expect("new fanout directory should be allowed");
    }

    #[test]
    fn repo_identity_revalidation_preserves_authority_escape_precedence() {
        let dir = minimal_repo_metadata_under_authority();
        let stable_identity =
            build_repo_identity_snapshot(dir.path()).expect("initial identity snapshot");
        let repo = RepoContext {
            working_dir: display_path(dir.path()),
            toplevel: dir.path().to_path_buf(),
            identity: "repo-id".to_string(),
            stable_identity,
        };
        let authority = path_policy::authority_root_path().expect("authority root");
        let Some(target) = authority.parent() else {
            eprintln!(
                "skipping authority escape precedence test because authority root has no parent"
            );
            return;
        };
        let link = dir.path().join(".git").join("objects").join("aa");
        if let Err(err) = create_dir_symlink(target, &link) {
            eprintln!(
                "skipping authority escape precedence test because symlinks are unavailable: {err}"
            );
            return;
        }

        let err = revalidate_repo_identity(&repo).expect_err("authority escape should reject");

        assert_eq!(err.0["error_type"], "git_metadata_outside_authority");
        assert_eq!(err.0["path"], display_path(&link));
    }

    #[test]
    fn basic_metadata_rejects_split_index_link_extension() {
        let dir = minimal_repo_metadata();
        std::fs::write(
            dir.path().join(".git").join("index"),
            index_with_extension(b"link", 20),
        )
        .expect("index with link extension");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["index_extension"], "link");
    }

    #[test]
    fn basic_metadata_rejects_sparse_index_extension() {
        let dir = minimal_repo_metadata();
        std::fs::write(
            dir.path().join(".git").join("index"),
            index_with_extension(b"sdir", 0),
        )
        .expect("index with sparse-index extension");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["index_extension"], "sdir");
    }

    #[test]
    fn basic_metadata_rejects_required_lowercase_index_extension() {
        let dir = minimal_repo_metadata();
        std::fs::write(
            dir.path().join(".git").join("index"),
            index_with_extension(b"abcd", 0),
        )
        .expect("index with required lowercase extension");

        let err = validate_basic_git_metadata(dir.path()).expect_err("metadata should reject");

        assert_eq!(err.0["error_type"], "unsupported_repository_metadata");
        assert_eq!(err.0["index_extension"], "abcd");
    }

    #[test]
    fn index_extension_parser_skips_v2_entries() {
        let mut index = Vec::new();
        index.extend_from_slice(b"DIRC");
        index.extend_from_slice(&2u32.to_be_bytes());
        index.extend_from_slice(&1u32.to_be_bytes());
        index.extend_from_slice(&[0; 40]);
        index.extend_from_slice(&[1; 20]);
        index.extend_from_slice(&5u16.to_be_bytes());
        index.extend_from_slice(b"a.txt\0");
        let entry_len = 40 + 20 + 2 + 6;
        index.resize(index.len() + ((8 - (entry_len % 8)) % 8), 0);
        index.extend_from_slice(b"TREE");
        index.extend_from_slice(&0u32.to_be_bytes());
        index.resize(index.len() + 20, 0);

        let signatures =
            parse_index_extension_signatures(&index).expect("index extensions should parse");

        assert_eq!(signatures, vec![*b"TREE"]);
    }

    #[test]
    fn parse_unified_diff_mints_hunk_ids_for_modified_text() {
        let diff = b"diff --git a/a.txt b/a.txt\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[]).expect("diff parses");

        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.total_hunks, 1);
        assert!(parsed.files[0].supported_for_stage_hunks);
        assert_eq!(parsed.files[0].hunks[0].old_start, 1);
        assert!(parsed.files[0].hunks[0].id.starts_with("0.0."));
    }

    #[test]
    fn split_lines_keep_endings_preserves_lf_crlf_and_bare_cr() {
        assert_eq!(
            split_lines_keep_endings("one\ntwo\r\nthree\rfour"),
            vec!["one\n", "two\r\n", "three\r", "four"]
        );
    }

    #[test]
    fn parse_unified_diff_preserves_crlf_hunk_body_bytes() {
        let diff = b"diff --git a/a.txt b/a.txt\r\n--- a/a.txt\r\n+++ b/a.txt\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n";
        let parsed =
            parse_unified_diff(diff, &repo(), false, 3, &[]).expect("CRLF diff should parse");

        assert_eq!(parsed.total_hunks, 1);
        assert_eq!(parsed.files[0].hunks[0].header, "@@ -1 +1 @@\r\n");
        assert_eq!(parsed.files[0].hunks[0].body, "-old\r\n+new\r\n");
    }

    #[test]
    fn parse_unified_diff_preserves_bare_cr_hunk_body_bytes() {
        let diff =
            b"diff --git a/a.txt b/a.txt\r--- a/a.txt\r+++ b/a.txt\r@@ -1 +1 @@\r-old\r+new\r";
        let parsed =
            parse_unified_diff(diff, &repo(), false, 3, &[]).expect("bare-CR diff should parse");

        assert_eq!(parsed.total_hunks, 1);
        assert_eq!(parsed.files[0].hunks[0].header, "@@ -1 +1 @@\r");
        assert_eq!(parsed.files[0].hunks[0].body, "-old\r+new\r");
    }

    #[test]
    fn parse_unified_diff_marks_added_file_unsupported_but_keeps_hunk_id() {
        let diff = b"diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+new\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[]).expect("diff parses");

        assert_eq!(parsed.files[0].status, ChangeStatus::Added);
        assert!(!parsed.files[0].supported_for_stage_hunks);
        assert_eq!(
            parsed.files[0].unsupported_reason.as_deref(),
            Some("unsupported_change_kind")
        );
        assert_eq!(parsed.files[0].hunks.len(), 1);
    }

    #[test]
    fn parse_unified_diff_marks_all_zero_index_header_unsupported() {
        let diff = b"diff --git a/a.txt b/a.txt\nindex 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[]).expect("diff parses");

        assert!(!parsed.files[0].supported_for_stage_hunks);
        assert_eq!(
            parsed.files[0].unsupported_reason.as_deref(),
            Some("unsupported_index_header")
        );
        assert_eq!(parsed.files[0].hunks.len(), 1);
    }

    #[test]
    fn parse_unified_diff_rejects_unknown_extended_metadata() {
        let diff = b"diff --git a/a.txt b/a.txt\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\nx-unknown metadata\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("unknown metadata must fail closed");

        assert_eq!(
            err.0["error_type"], "diff_parse_error",
            "unknown metadata must not emit partial hunk IDs"
        );
    }

    #[test]
    fn parse_unified_diff_marks_unsafe_paths_invalid() {
        let diff = b"diff --git a/../escape.txt b/../escape.txt\n--- a/../escape.txt\n+++ b/../escape.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[]).expect("diff parses");

        assert!(!parsed.files[0].supported_for_stage_hunks);
        assert_eq!(
            parsed.files[0].unsupported_reason.as_deref(),
            Some("invalid_path")
        );
        assert_eq!(parsed.files[0].hunks.len(), 1);
    }

    #[test]
    fn parse_unified_diff_rejects_ambiguous_unquoted_diff_git_header() {
        let diff = b"diff --git a/old b/name b/old b/name\n--- a/old b/name\n+++ b/old b/name\n@@ -1 +1 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("ambiguous unquoted diff --git header should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
    }

    #[test]
    fn parse_unified_diff_rejects_quoted_diff_git_header_trailing_data() {
        let diff = b"diff --git \"a/story.txt\" \"b/story.txt\" trailing\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("quoted diff --git header with trailing data should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
    }

    #[test]
    fn parse_unified_diff_rejects_negative_hunk_ranges() {
        let diff = b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1,-1 +1,1 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("negative hunk ranges should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
    }

    #[test]
    fn parse_unified_diff_rejects_leading_data_before_first_record() {
        let diff = b"unexpected prelude\ndiff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("leading data before first record should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
    }

    #[test]
    fn parse_unified_diff_rejects_malformed_hunk_body_lines() {
        let diff = b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\nold without prefix\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("hunk body line without a diff prefix should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
    }

    #[test]
    fn parse_unified_diff_rejects_hunk_body_count_mismatch() {
        let diff = b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n";
        let err = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect_err("truncated hunk body should fail closed");

        assert_eq!(err.0["error_type"], "diff_parse_error");
        assert_eq!(err.0["expected_old_lines"], 2);
        assert_eq!(err.0["expected_new_lines"], 2);
        assert_eq!(err.0["actual_old_lines"], 1);
        assert_eq!(err.0["actual_new_lines"], 1);
    }

    #[test]
    fn parse_unified_diff_accepts_no_newline_markers_without_counting_them() {
        let diff = b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[])
            .expect("no-newline markers should not affect hunk counts");

        assert_eq!(parsed.total_hunks, 1);
        assert!(parsed.files[0].supported_for_stage_hunks);
    }

    #[test]
    fn parse_unified_diff_rejects_file_count_complexity_limit() {
        let mut diff = String::new();
        for idx in 0..=MAX_GIT_DIFF_FILES {
            diff.push_str(&format!("diff --git a/file-{idx}.txt b/file-{idx}.txt\n"));
        }

        let err = parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &[])
            .expect_err("too many file records should reject before returning IDs");

        assert_eq!(err.0["error_type"], "diff_complexity_limit");
    }

    #[test]
    fn parse_unified_diff_rejects_hunk_count_complexity_limit() {
        let mut diff = String::from("diff --git a/story.txt b/story.txt\n");
        for _ in 0..=MAX_GIT_DIFF_HUNKS {
            diff.push_str("@@ -1 +1 @@\n-old\n+new\n");
        }

        let err = parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &[])
            .expect_err("too many hunks should reject before returning IDs");

        assert_eq!(err.0["error_type"], "diff_complexity_limit");
    }

    #[test]
    fn parse_unified_diff_rejects_hunk_body_byte_complexity_limit() {
        let mut diff = String::from("diff --git a/story.txt b/story.txt\n@@ -1 +0,0 @@\n-");
        diff.push_str(&"x".repeat(MAX_GIT_HUNK_BODY_BYTES + 1));
        diff.push('\n');

        let err = parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &[])
            .expect_err("oversized hunk body should reject before returning IDs");

        assert_eq!(err.0["error_type"], "diff_complexity_limit");
    }

    #[test]
    fn parse_unified_diff_rejects_structured_response_complexity_limit() {
        let mut diff = String::from("diff --git a/story.txt b/story.txt\n");
        diff.push_str("old mode ");
        diff.push_str(&"x".repeat(MAX_GIT_STRUCTURED_RESPONSE_BYTES));
        diff.push('\n');

        let err = parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &[])
            .expect_err("oversized structured response should reject before returning IDs");

        assert_eq!(err.0["error_type"], "diff_complexity_limit");
    }

    #[test]
    fn parse_unified_diff_generated_malformed_corpus_fails_closed() {
        let cases: &[(&str, &[u8], &str)] = &[
            (
                "missing diff header",
                b"--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "leading blank line before record",
                b"\ndiff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "unterminated quoted old path",
                b"diff --git \"a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "missing quoted new path",
                b"diff --git \"a/story.txt\"\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "invalid hunk range length",
                b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1,nope +1 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "hunk header without close marker",
                b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "hunk body contains header-like line",
                b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\ndiff --git a/other.txt b/other.txt\n+new\n",
                "diff_parse_error",
            ),
            (
                "hunk body contains invalid no-newline marker text",
                b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1 @@\n-old\n\\ Different marker\n+new\n",
                "diff_parse_error",
            ),
            (
                "truncated first hunk before second header",
                b"diff --git a/story.txt b/story.txt\n--- a/story.txt\n+++ b/story.txt\n@@ -1,2 +1,2 @@\n-old\n@@ -9 +9 @@\n-old\n+new\n",
                "diff_parse_error",
            ),
            (
                "non utf8 diff bytes",
                b"diff --git a/\xff.txt b/\xff.txt\n",
                "non_utf8_diff",
            ),
        ];

        for (name, diff, expected_error) in cases {
            let err = match parse_unified_diff(diff, &repo(), false, 3, &[]) {
                Ok(_) => panic!("{name} should fail closed"),
                Err(err) => err,
            };

            assert_eq!(
                err.0["error_type"], *expected_error,
                "{name} should use a stable fail-closed error code"
            );
        }
    }

    #[test]
    fn parse_unified_diff_pseudorandom_corpus_fails_closed_or_keeps_unique_ids() {
        let mut seed = 0x5eed_5afe_cafe_f00d_u64;
        for case_index in 0..256 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = 1 + (seed as usize % 512);
            let mut diff = if case_index % 4 == 0 {
                b"diff --git a/random.txt b/random.txt\n--- a/random.txt\n+++ b/random.txt\n"
                    .to_vec()
            } else {
                Vec::new()
            };
            for _ in 0..len {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                diff.push((seed >> 24) as u8);
            }

            match parse_unified_diff(&diff, &repo(), false, 3, &[]) {
                Ok(parsed) => {
                    let mut ids = std::collections::HashSet::new();
                    for file in &parsed.files {
                        for hunk in &file.hunks {
                            assert!(
                                ids.insert(hunk.id.clone()),
                                "case {case_index} returned a duplicate hunk ID {}",
                                hunk.id
                            );
                        }
                    }
                    assert_eq!(ids.len(), parsed.total_hunks);
                }
                Err(err) => {
                    let error_type = err.0["error_type"]
                        .as_str()
                        .expect("parser errors expose stable error_type");
                    assert!(
                        matches!(
                            error_type,
                            "diff_parse_error" | "non_utf8_diff" | "diff_complexity_limit"
                        ),
                        "case {case_index} returned unexpected parser error {error_type}"
                    );
                }
            }
        }
    }

    #[test]
    fn parse_unified_diff_generated_valid_fixture_has_deterministic_unique_ids() {
        let mut diff = String::new();
        for file_index in 0..8 {
            diff.push_str(&format!(
                "diff --git a/file-{file_index}.txt b/file-{file_index}.txt\n"
            ));
            diff.push_str(&format!("--- a/file-{file_index}.txt\n"));
            diff.push_str(&format!("+++ b/file-{file_index}.txt\n"));
            for hunk_index in 0..3 {
                let line = hunk_index * 10 + 1;
                diff.push_str(&format!(
                    "@@ -{line} +{line} @@\n-old-{file_index}-{hunk_index}\n+new-{file_index}-{hunk_index}\n"
                ));
            }
        }
        let paths = vec!["scope-a.txt".to_string(), "scope-b.txt".to_string()];

        let first =
            parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &paths).expect("diff parses");
        let second =
            parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &paths).expect("diff parses");

        assert_eq!(first.diff_id, second.diff_id);
        assert_eq!(first.total_hunks, 24);
        let mut seen = std::collections::HashSet::new();
        for (first_file, second_file) in first.files.iter().zip(&second.files) {
            for (first_hunk, second_hunk) in first_file.hunks.iter().zip(&second_file.hunks) {
                assert_eq!(first_hunk.id, second_hunk.id);
                assert!(
                    seen.insert(first_hunk.id.clone()),
                    "generated hunk ID should be unique: {}",
                    first_hunk.id
                );

                let mut parts = first_hunk.id.split('.');
                assert!(parts.next().is_some(), "missing file index");
                assert!(parts.next().is_some(), "missing hunk index");
                let hash = parts.next().expect("missing hash");
                assert_eq!(hash.len(), 64);
                assert!(parts.next().is_none(), "unexpected extra hunk-id field");
                assert!(
                    hash.bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                );
            }
        }
        assert_eq!(seen.len(), first.total_hunks);

        let reversed_paths = vec!["scope-b.txt".to_string(), "scope-a.txt".to_string()];
        let reversed = parse_unified_diff(diff.as_bytes(), &repo(), false, 3, &reversed_paths)
            .expect("diff parses");

        assert_ne!(first.diff_id, reversed.diff_id);
        assert_ne!(first.files[0].hunks[0].id, reversed.files[0].hunks[0].id);
    }

    #[test]
    fn literal_path_validation_rejects_escape_and_git_metadata() {
        for path in [
            "../x",
            "a/../x",
            ".git/config",
            "CON",
            "a\\b",
            "a//b",
            "a/./b",
            "C:/x",
            "a:ads",
            "GIT~1/config",
            ".GIT~2/config",
        ] {
            assert!(validate_repo_relative_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn literal_path_validation_allows_pathspec_magic_as_literal() {
        validate_repo_relative_path(":(glob)name").expect("pathspec-looking name is literal");
    }

    #[test]
    fn c_quoted_paths_decode_octal_escapes_before_validation() {
        let (path, consumed) =
            parse_c_quoted_path("\"a/dir\\057file\\134name.txt\" rest").expect("quoted path");

        assert_eq!(path, "a/dir/file\\name.txt");
        assert_eq!(consumed, "\"a/dir\\057file\\134name.txt\"".len());
        assert!(
            validate_repo_relative_path(path.strip_prefix("a/").expect("prefix")).is_err(),
            "decoded backslash must be rejected by path validation"
        );
    }

    #[test]
    fn c_quoted_paths_decode_octal_utf8_bytes_before_validation() {
        let (path, _) = parse_c_quoted_path("\"a/caf\\303\\251.txt\"").expect("quoted path");

        assert_eq!(path, "a/café.txt");
        validate_repo_relative_path(path.strip_prefix("a/").expect("prefix"))
            .expect("decoded UTF-8 path should validate");
    }

    #[test]
    fn parse_unified_diff_supports_c_quoted_utf8_file_headers() {
        let diff = b"diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\n--- \"a/caf\\303\\251.txt\"\n+++ \"b/caf\\303\\251.txt\"\n@@ -1 +1 @@\n-old\n+new\n";
        let parsed = parse_unified_diff(diff, &repo(), false, 3, &[]).expect("diff parses");

        assert_eq!(parsed.files[0].path, "café.txt");
        assert!(parsed.files[0].supported_for_stage_hunks);
        assert_eq!(parsed.files[0].unsupported_reason, None);
        assert_eq!(parsed.files[0].hunks.len(), 1);
    }

    #[cfg(unix)]
    fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn create_dir_symlink(
        _target: &std::path::Path,
        _link: &std::path::Path,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "directory symlinks are not supported on this platform",
        ))
    }

    #[test]
    fn c_quoted_paths_reject_non_utf8_octal_bytes() {
        let err = parse_c_quoted_path("\"a/bad\\377.txt\"").expect_err("non-UTF-8 byte rejects");

        assert_eq!(err, "quoted path is not valid UTF-8");
    }

    #[test]
    fn hunk_diff_args_neutralize_orderfile_with_null_device() {
        let scope = HunkRequestScope {
            staged: false,
            paths: Vec::new(),
            context: 3,
            max_bytes: 1024,
            working_dir_arg: None,
            timeout_ms: DEFAULT_GIT_TIMEOUT_MS,
        };
        let args = build_hunk_diff_args(&scope);
        assert!(
            args.contains(&format!("diff.orderFile={}", git_null_device())),
            "diff args must not use an empty diff.orderFile path: {args:?}"
        );
        assert!(
            !args.contains(&"diff.orderFile=".to_string()),
            "empty diff.orderFile makes supported Git versions try to open an empty orderfile path"
        );
    }
}
