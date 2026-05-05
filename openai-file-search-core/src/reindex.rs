use anyhow::{Context, Result, anyhow};
use reqwest::{Client, StatusCode};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::time::{Duration, sleep};

use crate::files::upload_file;
use crate::openai::hash::looks_binary_by_content;
use crate::responses::responses_with_file_search;
use crate::vector_stores::{
    add_file_to_vector_store_with, delete_vector_store_file, list_vector_store_files,
    wait_for_vector_file_ready, wait_for_vector_store_ready,
};
use crate::{ApiConfig, CodeQueryOptions, compute_file_hash, is_codequery_indexable_path};

fn normalize_indexed_path_with_base(path: &str, base: Option<&Path>) -> String {
    let path_buf = PathBuf::from(path);
    if !path_buf.is_absolute() {
        return path.to_string();
    }

    if let Some(base) = base
        && let Ok(relative) = path_buf.strip_prefix(base)
    {
        return relative.to_string_lossy().to_string();
    }

    if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = path_buf.strip_prefix(cwd)
    {
        return relative.to_string_lossy().to_string();
    }

    path_buf.file_name().map_or_else(
        || path.to_string(),
        |name| name.to_string_lossy().to_string(),
    )
}

fn compute_indexed_path_base(file_paths: &[String]) -> Option<PathBuf> {
    let absolute_paths: Vec<PathBuf> = file_paths
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect();

    if absolute_paths.is_empty() {
        return None;
    }

    let mut parents = absolute_paths
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf));
    let mut common = parents.next()?;

    for parent in parents {
        while !parent.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }

    if let Some(git_root) = find_git_root_from(&common) {
        return Some(git_root);
    }

    if let Ok(cwd) = std::env::current_dir()
        && absolute_paths.iter().all(|path| path.starts_with(&cwd))
    {
        return Some(cwd);
    }

    Some(common)
}

fn find_git_root_from(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn is_hash_match_move_candidate(old_key: &str, desired_indexed_paths: &HashSet<String>) -> bool {
    !desired_indexed_paths.contains(old_key)
}

/// Synchronizes local files into a vector store using path and hash metadata.
pub async fn reindex_files(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    concurrent_limit: usize,
    skip_per_file_wait: bool,
) -> Result<serde_json::Value> {
    use futures::stream::{self, StreamExt};

    let store_files_list = list_vector_store_files(client, cfg, vector_store_id).await?;
    let indexed_path_base = compute_indexed_path_base(file_paths);
    let desired_indexed_paths: HashSet<String> = file_paths
        .iter()
        .map(|path| normalize_indexed_path_with_base(path, indexed_path_base.as_deref()))
        .collect();

    let mut path_map: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut hash_map: HashMap<String, (String, String)> = HashMap::new();
    let mut filename_map: HashMap<String, (String, Option<String>)> = HashMap::new();

    for file in store_files_list.data {
        let path_attr = file
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("path"))
            .and_then(|path| path.as_str())
            .map(String::from);

        let filename = file
            .filename
            .clone()
            .or_else(|| file.file.as_ref().and_then(|inner| inner.filename.clone()));

        let hash = file
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("hash"))
            .and_then(|hash| hash.as_str())
            .map(String::from);

        if let Some(path) = &path_attr {
            path_map.insert(
                normalize_indexed_path_with_base(path, indexed_path_base.as_deref()),
                (file.id.clone(), hash.clone()),
            );
        }

        if let Some(name) = &filename {
            filename_map.insert(name.clone(), (file.id.clone(), hash.clone()));
        }

        let key = path_attr.or(filename);
        if let (Some(hash), Some(key)) = (hash, key) {
            hash_map.insert(hash, (key, file.id));
        }
    }

    let mut to_upload = Vec::new();
    let mut to_skip = Vec::new();
    let mut to_delete: HashMap<String, String> = HashMap::new();
    let mut errors = Vec::new();

    type HashedPathOk = (String, String, Option<String>);
    type HashedPathErr = (String, String);
    type HashedPathResult = std::result::Result<HashedPathOk, HashedPathErr>;

    let mut hash_results: Vec<(usize, HashedPathResult)> =
        stream::iter(file_paths.iter().cloned().enumerate())
            .map(|(idx, path)| async move {
                let filename = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(String::from);

                match compute_file_hash(&path).await {
                    Ok(hash) => (idx, Ok((path, hash, filename))),
                    Err(err) => (idx, Err((path, format!("Failed to hash: {err}")))),
                }
            })
            .buffer_unordered(concurrent_limit)
            .collect()
            .await;

    hash_results.sort_by_key(|(idx, _)| *idx);

    for (_, result) in hash_results {
        let (path, local_hash, filename) = match result {
            Ok(ok) => ok,
            Err((path, err)) => {
                errors.push((path, err));
                continue;
            }
        };
        let indexed_path = normalize_indexed_path_with_base(&path, indexed_path_base.as_deref());

        if let Some((file_id, store_hash)) = path_map.get(&indexed_path).cloned() {
            if store_hash.as_ref() == Some(&local_hash) {
                path_map.remove(&indexed_path);
                hash_map.remove(&local_hash);
                if let Some(name) = &filename {
                    filename_map.remove(name);
                }
                to_skip.push(path);
            } else {
                to_delete.insert(file_id.clone(), format!("content changed: {path}"));
                path_map.remove(&indexed_path);
                if let Some(old_hash) = store_hash {
                    hash_map.remove(&old_hash);
                }
                if let Some(name) = &filename {
                    filename_map.remove(name);
                }
                to_upload.push((path, local_hash));
            }
        } else if let Some((old_key, file_id)) = hash_map.get(&local_hash).cloned() {
            if !is_hash_match_move_candidate(&old_key, &desired_indexed_paths) {
                to_upload.push((path, local_hash));
                continue;
            }
            to_delete.insert(file_id.clone(), format!("moved from {old_key} to {path}"));
            path_map.remove(&old_key);
            hash_map.remove(&local_hash);
            if let Some(name) = &filename {
                filename_map.remove(name);
            }
            to_upload.push((path, local_hash));
        } else if let Some(name) = &filename {
            if let Some((file_id, store_hash)) = filename_map.get(name).cloned() {
                if store_hash.as_ref() == Some(&local_hash) {
                    to_skip.push(path);
                } else {
                    to_delete.insert(file_id.clone(), format!("content changed (legacy): {name}"));
                    to_upload.push((path, local_hash));
                }
                filename_map.remove(name);
            } else {
                to_upload.push((path, local_hash));
            }
        } else {
            to_upload.push((path, local_hash));
        }
    }

    for (file_id, reason) in &to_delete {
        tracing::debug!("Deleting file {}: {}", file_id, reason);
        if let Err(err) = delete_vector_store_file(client, cfg, vector_store_id, file_id).await {
            tracing::warn!("Failed to delete {}: {}", file_id, err);
        }
    }

    let mut uploaded = Vec::new();
    let mut upload_errors = Vec::new();

    let chunks: Vec<_> = to_upload
        .chunks(concurrent_limit)
        .map(<[(String, String)]>::to_vec)
        .collect();
    for chunk in chunks {
        let chunk_len = chunk.len();
        let results: Vec<_> = stream::iter(chunk)
            .map(|(path, hash)| {
                let indexed_path_base = indexed_path_base.clone();
                async move {
                    let file_id = match upload_file(client, cfg, &path).await {
                        Ok(id) => id,
                        Err(err) => return Err((path.clone(), format!("Upload failed: {err}"))),
                    };

                    let mut attributes = serde_json::Map::new();
                    attributes.insert(
                        "path".to_string(),
                        serde_json::Value::String(normalize_indexed_path_with_base(
                            &path,
                            indexed_path_base.as_deref(),
                        )),
                    );
                    attributes.insert("hash".to_string(), serde_json::Value::String(hash.clone()));
                    attributes.insert(
                        "indexed_at".to_string(),
                        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                    );

                    match add_file_to_vector_store_with(
                        client,
                        cfg,
                        vector_store_id,
                        &file_id,
                        Some(attributes),
                        None,
                    )
                    .await
                    {
                        Ok(()) => {
                            if !skip_per_file_wait
                                && let Err(err) = wait_for_vector_file_ready(
                                    client,
                                    cfg,
                                    vector_store_id,
                                    1000,
                                    30_000,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "File {} uploaded but processing incomplete: {}",
                                    path,
                                    err
                                );
                            }
                            Ok((path, file_id, hash))
                        }
                        Err(err) => Err((path.clone(), format!("Attach failed: {err}"))),
                    }
                }
            })
            .buffer_unordered(concurrent_limit)
            .collect()
            .await;

        for result in results {
            match result {
                Ok((path, file_id, hash)) => uploaded.push(serde_json::json!({
                    "path": path,
                    "file_id": file_id,
                    "hash": hash,
                    "action": "uploaded"
                })),
                Err((path, error)) => upload_errors.push((path, error)),
            }
        }

        if chunk_len > 0 {
            sleep(Duration::from_millis(500)).await;
        }
    }

    let mut deleted = Vec::new();
    let mut delete_errors = Vec::new();
    let mut orphan_files: HashMap<String, String> = HashMap::new();

    for (path, (file_id, _)) in path_map {
        orphan_files.insert(file_id, path);
    }
    for (filename, (file_id, _)) in filename_map {
        orphan_files.entry(file_id).or_insert(filename);
    }

    for (file_id, key) in orphan_files {
        match delete_vector_store_file(client, cfg, vector_store_id, &file_id).await {
            Ok(()) => deleted.push(serde_json::json!({
                "path": key,
                "file_id": file_id,
                "action": "deleted"
            })),
            Err(err) => delete_errors.push((key, err.to_string())),
        }
    }

    let all_errors: Vec<_> = errors
        .into_iter()
        .chain(upload_errors)
        .chain(delete_errors)
        .map(|(path, error)| serde_json::json!({ "path": path, "error": error }))
        .collect();

    let total_deleted = to_delete.len() + deleted.len();

    Ok(serde_json::json!({
        "summary": {
            "total_files": file_paths.len(),
            "uploaded": uploaded.len(),
            "skipped": to_skip.len(),
            "deleted": total_deleted,
            "errors": all_errors.len()
        },
        "uploaded": uploaded,
        "skipped": to_skip,
        "deleted": deleted,
        "errors": all_errors
    }))
}

/// Retries reindexing when failures look transient.
pub async fn reindex_with_retry(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    concurrent_limit: usize,
    skip_per_file_wait: bool,
) -> Result<serde_json::Value> {
    const MAX_ATTEMPTS: usize = 3;
    const BACKOFF_MS: [u64; MAX_ATTEMPTS] = [200, 500, 1000];

    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..MAX_ATTEMPTS {
        let is_last_attempt = attempt + 1 == MAX_ATTEMPTS;

        match reindex_files(
            client,
            cfg,
            vector_store_id,
            file_paths,
            concurrent_limit,
            skip_per_file_wait,
        )
        .await
        {
            Ok(summary) => {
                let maybe_errors = summary.get("errors").and_then(|value| value.as_array());
                let Some(errors) = maybe_errors else {
                    return Ok(summary);
                };
                if errors.is_empty() {
                    return Ok(summary);
                }

                let mut should_retry = false;
                let mut first_error_message: Option<String> = None;

                for entry in errors {
                    if let Some(message) = entry.get("error").and_then(|value| value.as_str()) {
                        if first_error_message.is_none() {
                            first_error_message = Some(message.to_string());
                        }
                        if is_transient_error_message(message) {
                            should_retry = true;
                        }
                    }
                }

                let first_error =
                    first_error_message.unwrap_or_else(|| "unknown error".to_string());
                let summary_details =
                    serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
                let err = anyhow!(
                    "Reindex completed with {} error(s); sample: {}. Details: {}",
                    errors.len(),
                    first_error,
                    summary_details
                );

                if should_retry && !is_last_attempt {
                    tracing::warn!(
                        attempt = attempt + 1,
                        "Reindex attempt {} returned errors; retrying",
                        attempt + 1
                    );
                    last_error = Some(err);
                } else {
                    return Err(err);
                }
            }
            Err(err) => {
                if !is_transient_error(&err) || is_last_attempt {
                    return Err(err);
                }

                tracing::warn!(
                    attempt = attempt + 1,
                    "Reindex attempt {} failed: {}; retrying",
                    attempt + 1,
                    err
                );
                last_error = Some(err);
            }
        }

        if attempt + 1 < MAX_ATTEMPTS {
            let base_delay = BACKOFF_MS
                .get(attempt)
                .copied()
                .unwrap_or(*BACKOFF_MS.last().unwrap_or(&1000));
            let jitter_ms = 50 * (attempt as u64 + 1);
            sleep(Duration::from_millis(base_delay + jitter_ms)).await;
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("Reindex failed with unknown error")))
}

fn is_transient_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(req_err) = cause.downcast_ref::<reqwest::Error>() {
            if req_err.is_timeout() || req_err.is_connect() {
                return true;
            }
            if let Some(status) = req_err.status() {
                return status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            }
        }
        false
    })
}

fn is_transient_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("temporarily unavailable")
        || lower.contains("try again")
        || message.contains("429")
        || message.contains("500")
        || message.contains("502")
        || message.contains("503")
        || message.contains("504")
}

/// High-level semantic code search with optional local reindexing.
pub async fn code_query(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    query: &str,
    options: CodeQueryOptions<'_>,
) -> Result<(String, Option<serde_json::Value>)> {
    let CodeQueryOptions {
        concurrent_limit,
        timeout_ms,
        model,
        max_num_results,
        include_results,
    } = options;

    if query.trim().is_empty() {
        return Err(anyhow!("query must not be empty"));
    }

    for path in file_paths {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Err(anyhow!(
                "remote paths are not supported in CodeQuery: {path}"
            ));
        }
    }

    let mut reindex_summary: Option<serde_json::Value> = None;
    if !file_paths.is_empty() {
        let mut filtered_paths = Vec::new();
        let mut filtered_out = Vec::new();

        for path in file_paths {
            let metadata = tokio::fs::metadata(path)
                .await
                .with_context(|| format!("Failed to access file path: {path}"))?;
            if !metadata.is_file() {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "not a regular file"
                }));
                continue;
            }

            if !is_codequery_indexable_path(std::path::Path::new(path)) {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "not an indexable code/config file"
                }));
                continue;
            }

            if looks_binary_by_content(path).await? {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "binary content (NUL byte detected)"
                }));
                continue;
            }

            filtered_paths.push(path.clone());
        }

        if filtered_paths.is_empty() {
            return Err(anyhow!(
                "No indexable files provided for CodeQuery after filtering non-code files"
            ));
        }

        let summary = reindex_with_retry(
            client,
            cfg,
            vector_store_id,
            &filtered_paths,
            concurrent_limit,
            true,
        )
        .await
        .map_err(|err| anyhow!("code_query reindex failed: {err}"))?;

        let mut summary = summary;
        if let Some(root) = summary.as_object_mut() {
            if !filtered_out.is_empty() {
                root.insert(
                    "filtered_out".to_string(),
                    serde_json::Value::Array(filtered_out),
                );
            }
            if let Some(obj) = root
                .get_mut("summary")
                .and_then(|value| value.as_object_mut())
            {
                obj.insert(
                    "requested_files".to_string(),
                    serde_json::json!(file_paths.len()),
                );
                obj.insert(
                    "indexed_files".to_string(),
                    serde_json::json!(filtered_paths.len()),
                );
            }
        }

        wait_for_vector_store_ready(client, cfg, vector_store_id, 1000, timeout_ms).await?;
        reindex_summary = Some(summary);
    }

    let model_to_use = model.unwrap_or(&cfg.default_model);
    let raw_response = responses_with_file_search(
        client,
        cfg,
        model_to_use,
        query,
        vector_store_id,
        max_num_results,
        include_results,
    )
    .await?;

    let response_text =
        crate::openai::types::extract_text_from_response_value(&raw_response, include_results);

    Ok((response_text, reindex_summary))
}

#[cfg(test)]
mod tests {
    use super::{
        compute_indexed_path_base, is_hash_match_move_candidate, normalize_indexed_path_with_base,
    };
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn absolute_paths_normalize_against_common_base() {
        let base = Path::new("/workspace/repo");

        assert_eq!(
            normalize_indexed_path_with_base("/workspace/repo/src/lib.rs", Some(base)),
            "src/lib.rs"
        );
    }

    #[test]
    fn indexed_path_base_uses_common_parent_for_absolute_paths_outside_cwd() {
        let paths = vec![
            "/workspace/repo/src/lib.rs".to_string(),
            "/workspace/repo/tests/integration.rs".to_string(),
        ];

        assert_eq!(
            compute_indexed_path_base(&paths).as_deref(),
            Some(Path::new("/workspace/repo"))
        );
    }

    #[test]
    fn identical_hash_at_requested_old_path_is_copy_not_move() {
        let desired = HashSet::from(["src/original.rs".to_string(), "src/copy.rs".to_string()]);

        assert!(!is_hash_match_move_candidate("src/original.rs", &desired));
        assert!(is_hash_match_move_candidate("src/removed.rs", &desired));
    }
}
