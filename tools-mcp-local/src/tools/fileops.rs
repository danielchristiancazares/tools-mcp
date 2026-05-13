//! File operation tools: Move, Copy, `ListDir`.

use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::validation;

// ============================================================================
// Move / Rename
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveRequest {
    source: String,
    destination: String,
    #[serde(default)]
    overwrite: Option<bool>,
}

async fn handle_move(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<MoveRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.source, "source", None) {
        return o;
    }
    if let Err(o) = validation::validate_non_empty(&req.destination, "destination", None) {
        return o;
    }

    let source = Path::new(&req.source);
    let destination = Path::new(&req.destination);

    if !source.exists() {
        return ToolCallOutcome::err(format!("source not found: {}", source.display()));
    }

    // Existing *real* directories act as container targets in mv-like mode.
    // Use symlink_metadata so symlink destinations are treated as the
    // destination path itself instead of being followed into targets.
    let destination_is_real_dir = tokio::fs::symlink_metadata(destination)
        .await
        .map(|meta| meta.file_type().is_dir())
        .unwrap_or(false);

    let final_dest = if destination_is_real_dir {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return ToolCallOutcome::err("source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

    if source.is_dir() {
        let source_norm = normalize_absolute_or_cwd(source);
        let dest_norm = normalize_absolute_or_cwd(&final_dest);
        if dest_norm != source_norm && dest_norm.starts_with(&source_norm) {
            return ToolCallOutcome::err(format!(
                "refusing move: destination {} is inside source {}",
                final_dest.display(),
                source.display()
            ));
        }
    }

    if final_dest.exists() && !req.overwrite.unwrap_or(false) {
        return ToolCallOutcome::err(format!(
            "destination already exists: {}. Use overwrite: true to replace.",
            final_dest.display()
        ));
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent()
        && !parent.exists()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        return ToolCallOutcome::err(format!("failed to create parent directory: {err}"));
    }

    if let Err(err) = tokio::fs::rename(source, &final_dest).await {
        // rename() may fail across filesystems, try copy+delete
        if source.is_file() {
            if let Err(copy_err) = tokio::fs::copy(source, &final_dest).await {
                return ToolCallOutcome::err(format!(
                    "failed to move {}: {err}, copy fallback failed: {copy_err}",
                    source.display()
                ));
            }
            if let Err(del_err) = tokio::fs::remove_file(source).await {
                return ToolCallOutcome::err(format!(
                    "moved file but failed to remove source: {del_err}"
                ));
            }
        } else {
            return ToolCallOutcome::err(format!("failed to move {}: {err}", source.display()));
        }
    }

    ToolCallOutcome::ok_text_with(
        format!("Moved {} to {}", source.display(), final_dest.display()),
        [
            ("source", json!(source.display().to_string())),
            ("destination", json!(final_dest.display().to_string())),
        ],
    )
}

define_mcp_tool! {
    MoveTool,
    name: "Move",
    description: "Move or rename a file or directory.",
    schema: {
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "description": "Source path to move"
            },
            "destination": {
                "type": "string",
                "description": "Destination path (or directory to move into)"
            },
            "overwrite": {
                "type": "boolean",
                "default": false,
                "description": "Overwrite destination if it exists"
            }
        },
        "required": ["source", "destination"],
        "additionalProperties": false
    },
    handler: handle_move
}

// ============================================================================
// Copy
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyRequest {
    source: String,
    destination: String,
    #[serde(default)]
    overwrite: Option<bool>,
    #[serde(default)]
    recursive: Option<bool>,
}

async fn handle_copy(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<CopyRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.source, "source", None) {
        return o;
    }
    if let Err(o) = validation::validate_non_empty(&req.destination, "destination", None) {
        return o;
    }

    let source = Path::new(&req.source);
    let destination = Path::new(&req.destination);

    if !source.exists() {
        return ToolCallOutcome::err(format!("source not found: {}", source.display()));
    }

    let source_metadata = match tokio::fs::symlink_metadata(source).await {
        Ok(metadata) => metadata,
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "failed to inspect source {}: {err}",
                source.display()
            ));
        }
    };

    let overwrite = req.overwrite.unwrap_or(false);

    // Existing directories always act as container targets in cp-like mode.
    // overwrite applies to the resolved child path, not the directory itself.
    let final_dest = if destination.is_dir() {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return ToolCallOutcome::err("source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

    // Refuse to recursively delete a real directory just because the caller
    // passed `overwrite: true` with a non-directory source. The historical
    // overwrite path runs `remove_dir_all(final_dest)` before renaming the
    // staged file into place, which silently wipes the entire directory
    // subtree (including unrelated nested files). GNU `cp` rejects this
    // even with `--force`; we do the same. The `dir → file` and `dir → dir`
    // overwrite cases remain supported by `copy_directory_with_overwrite`
    // (covered by existing tests) because the caller has explicitly opted
    // into directory semantics by passing a directory source.
    //
    // Use `symlink_metadata` so a symlink whose target is a directory is
    // NOT misclassified as a real directory: replacing such a symlink with
    // a file just unlinks the symlink and leaves the target dir untouched,
    // which is safe and remains permitted.
    if overwrite
        && source.is_file()
        && let Ok(dst_meta) = tokio::fs::symlink_metadata(&final_dest).await
        && dst_meta.file_type().is_dir()
    {
        return ToolCallOutcome::err(format!(
            "refusing to overwrite directory {} with non-directory source {}: \
             type-mismatch replacement would recursively delete the directory; \
             remove or rename the directory first if replacement is intended",
            final_dest.display(),
            source.display()
        ));
    }

    if source_metadata.file_type().is_symlink() && source.is_dir() && req.recursive.unwrap_or(false)
    {
        return ToolCallOutcome::err(format!(
            "refusing recursive copy from symlinked directory: {}",
            source.display()
        ));
    }

    if source.is_dir() && req.recursive.unwrap_or(false) {
        let source_norm = normalize_absolute_or_cwd(source);
        let dest_norm = normalize_absolute_or_cwd(&final_dest);
        if dest_norm.starts_with(&source_norm) {
            return ToolCallOutcome::err(format!(
                "refusing recursive copy: destination {} is inside source {} (would recurse indefinitely)",
                final_dest.display(),
                source.display()
            ));
        }
    }

    if final_dest.exists() && !overwrite {
        return ToolCallOutcome::err(format!(
            "destination already exists: {}. Use overwrite: true to replace.",
            final_dest.display()
        ));
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent()
        && !parent.exists()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        return ToolCallOutcome::err(format!("failed to create parent directory: {err}"));
    }

    if source.is_file() {
        if let Err(err) = copy_file_with_overwrite(source, &final_dest, overwrite).await {
            return ToolCallOutcome::err(format!("failed to copy {}: {err}", source.display()));
        }
    } else if source.is_dir() {
        if !req.recursive.unwrap_or(false) {
            return ToolCallOutcome::err(format!(
                "{} is a directory. Use recursive: true to copy directories.",
                source.display()
            ));
        }
        if let Err(err) = copy_directory_with_overwrite(source, &final_dest, overwrite).await {
            return ToolCallOutcome::err(format!(
                "failed to copy directory {}: {err}",
                source.display()
            ));
        }
    }

    ToolCallOutcome::ok_text_with(
        format!("Copied {} to {}", source.display(), final_dest.display()),
        [
            ("source", json!(source.display().to_string())),
            ("destination", json!(final_dest.display().to_string())),
        ],
    )
}

fn normalize_absolute_or_cwd(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize_path(&absolute)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().await?;

        // Avoid following symlinked directories during recursive copy.
        // Following links can cause unbounded recursion (e.g., a symlink
        // pointing back to an ancestor) or unintentionally copy data outside
        // the source subtree.
        if file_type.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to recurse through symlink while copying directory: {}",
                    src_path.display()
                ),
            ));
        }

        if file_type.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path).await?;
        }
    }
    Ok(())
}

fn replacement_stage_path(dst: &Path) -> PathBuf {
    let file_name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("replacement");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    dst.with_file_name(format!(
        ".{file_name}.codex-tmp-{}-{nanos}",
        std::process::id()
    ))
}

async fn remove_existing_path(path: &Path) -> std::io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
}

async fn copy_file_with_overwrite(src: &Path, dst: &Path, overwrite: bool) -> std::io::Result<u64> {
    if !overwrite || !dst.exists() {
        return tokio::fs::copy(src, dst).await;
    }

    // Defense in depth: this primitive is only meant to replace a regular
    // file with another regular file. The historical overwrite path below
    // calls `remove_existing_path(dst)` which dispatches to `remove_dir_all`
    // when `dst` is a real directory, recursively wiping unrelated state.
    // `handle_copy` already rejects this shape, but re-check here so any
    // future caller — and any TOCTOU race that turns `dst` into a directory
    // between the entry-point check and this point — fails closed before
    // any mutation. `symlink_metadata` keeps the safe symlink-to-dir case
    // working: only the symlink itself is unlinked; its target is preserved.
    if let Ok(meta) = tokio::fs::symlink_metadata(dst).await
        && meta.file_type().is_dir()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to overwrite directory {} with a non-directory source",
                dst.display()
            ),
        ));
    }

    let staged = replacement_stage_path(dst);
    if staged.exists() {
        remove_existing_path(&staged).await?;
    }

    let bytes = tokio::fs::copy(src, &staged).await?;
    if let Err(err) = remove_existing_path(dst).await {
        let _ = remove_existing_path(&staged).await;
        return Err(err);
    }
    if let Err(err) = tokio::fs::rename(&staged, dst).await {
        let _ = remove_existing_path(&staged).await;
        return Err(err);
    }

    Ok(bytes)
}

async fn copy_directory_with_overwrite(
    src: &Path,
    dst: &Path,
    overwrite: bool,
) -> std::io::Result<()> {
    if !overwrite || !dst.exists() {
        return copy_dir_recursive(src, dst).await;
    }

    let staged = replacement_stage_path(dst);
    if staged.exists() {
        remove_existing_path(&staged).await?;
    }

    if let Err(err) = copy_dir_recursive(src, &staged).await {
        let _ = remove_existing_path(&staged).await;
        return Err(err);
    }
    if let Err(err) = remove_existing_path(dst).await {
        let _ = remove_existing_path(&staged).await;
        return Err(err);
    }
    if let Err(err) = tokio::fs::rename(&staged, dst).await {
        let _ = remove_existing_path(&staged).await;
        return Err(err);
    }

    Ok(())
}

define_mcp_tool! {
    CopyTool,
    name: "Copy",
    description: "Copy a file or directory, replacing the destination when overwrite is true.",
    schema: {
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "description": "Source path to copy"
            },
            "destination": {
                "type": "string",
                "description": "Destination path (or directory to copy into)"
            },
            "overwrite": {
                "type": "boolean",
                "default": false,
                "description": "Overwrite destination if it exists"
            },
            "recursive": {
                "type": "boolean",
                "default": false,
                "description": "Copy directories recursively"
            }
        },
        "required": ["source", "destination"],
        "additionalProperties": false
    },
    handler: handle_copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn copy_rejects_recursive_copy_into_own_subdirectory() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let child = src.join("nested");
        tokio::fs::create_dir_all(&src).await.expect("create src");
        tokio::fs::write(src.join("file.txt"), "hello")
            .await
            .expect("write");

        let args = json!({
            "source": src.display().to_string(),
            "destination": child.display().to_string(),
            "recursive": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], true);
        let msg = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("destination"));
        assert!(msg.contains("inside source"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copy_rejects_symlink_inside_recursive_directory_copy() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let nested = src.join("nested");
        tokio::fs::create_dir_all(&nested)
            .await
            .expect("create nested");
        tokio::fs::write(src.join("file.txt"), "hello")
            .await
            .expect("write");

        // Create loop: src/nested/back -> src
        unix_fs::symlink(&src, nested.join("back")).expect("symlink");

        let args = json!({
            "source": src.display().to_string(),
            "destination": dst.display().to_string(),
            "recursive": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], true);
        let msg = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("symlink"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copy_rejects_symlinked_directory_as_recursive_source() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        let dst = dir.path().join("dst");
        tokio::fs::create_dir_all(&target)
            .await
            .expect("create target");
        tokio::fs::write(target.join("outside.txt"), "outside")
            .await
            .expect("write target file");
        unix_fs::symlink(&target, &link).expect("symlink");

        let args = json!({
            "source": link.display().to_string(),
            "destination": dst.display().to_string(),
            "recursive": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], true);
        let msg = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("symlinked directory"));
        assert!(!dst.exists());
    }

    #[tokio::test]
    async fn copy_overwrite_into_existing_directory_keeps_container() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        tokio::fs::create_dir_all(&src).await.expect("create src");
        tokio::fs::create_dir_all(&dst).await.expect("create dst");
        tokio::fs::write(src.join("fresh.txt"), "fresh")
            .await
            .expect("write fresh");
        tokio::fs::write(dst.join("stale.txt"), "stale")
            .await
            .expect("write stale");

        let args = json!({
            "source": src.display().to_string(),
            "destination": dst.display().to_string(),
            "recursive": true,
            "overwrite": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], false);
        assert!(dst.join("src").join("fresh.txt").exists());
        assert!(dst.join("stale.txt").exists());
    }

    #[tokio::test]
    async fn copy_overwrite_replaces_destination_type_mismatch() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        tokio::fs::create_dir_all(&src).await.expect("create src");
        tokio::fs::write(src.join("inside.txt"), "content")
            .await
            .expect("write src");
        tokio::fs::write(&dst, "old file").await.expect("write dst");

        let args = json!({
            "source": src.display().to_string(),
            "destination": dst.display().to_string(),
            "recursive": true,
            "overwrite": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], false);
        assert!(dst.is_dir());
        assert!(dst.join("inside.txt").exists());
    }

    /// Exploit-closure regression: replicates the original PoC that turned
    /// `Copy` with `overwrite: true` and a non-directory source on top of an
    /// existing directory destination into an unbounded `rm -rf` of the
    /// directory subtree. The fix must abort before any mutation, leaving
    /// the directory and its nested contents intact.
    #[tokio::test]
    async fn copy_refuses_overwriting_directory_with_file() {
        let dir = tempdir().expect("tempdir");
        let dst = dir.path().join("victim_dir");
        let nested = dst.join("nested");
        tokio::fs::create_dir_all(&nested)
            .await
            .expect("create nested");
        tokio::fs::write(nested.join("keep.txt"), "IMPORTANT_DO_NOT_DELETE")
            .await
            .expect("write keep.txt");
        let src = dir.path().join("source.txt");
        tokio::fs::write(&src, "ATTACKER_FILE")
            .await
            .expect("write src");

        let args = json!({
            "source": src.display().to_string(),
            "destination": dst.display().to_string(),
            "overwrite": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], true, "must refuse: {result}");
        let msg = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            msg.contains("refusing to overwrite directory"),
            "expected refusal diagnostic, got: {msg}"
        );

        // Filesystem must be untouched: directory still exists, nested file
        // still exists with the original content, and the destination did
        // not become a regular file.
        assert!(
            dst.is_dir(),
            "victim directory must still exist as a directory"
        );
        assert!(
            !dst.is_file(),
            "destination must not have been replaced with a regular file"
        );
        assert!(
            nested.join("keep.txt").exists(),
            "nested file must not have been recursively deleted"
        );
        let kept = tokio::fs::read_to_string(nested.join("keep.txt"))
            .await
            .expect("read keep.txt");
        assert_eq!(
            kept, "IMPORTANT_DO_NOT_DELETE",
            "nested file content must be preserved"
        );
    }

    /// Defense-in-depth: even if a future caller bypasses the entry-point
    /// check, the underlying primitive must refuse to recursively delete a
    /// directory.
    #[tokio::test]
    async fn copy_file_with_overwrite_refuses_directory_destination() {
        let dir = tempdir().expect("tempdir");
        let dst = dir.path().join("real_dir");
        let nested = dst.join("inside.txt");
        tokio::fs::create_dir_all(&dst).await.expect("create dst");
        tokio::fs::write(&nested, "preserved")
            .await
            .expect("write nested");
        let src = dir.path().join("source.txt");
        tokio::fs::write(&src, "data").await.expect("write src");

        let err = copy_file_with_overwrite(&src, &dst, true)
            .await
            .expect_err("primitive must refuse a directory destination");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("refusing to overwrite directory"),
            "expected refusal diagnostic, got: {err}"
        );
        assert!(dst.is_dir(), "destination directory must be preserved");
        assert!(nested.exists(), "nested file must be preserved");
    }

    /// Pins the safe-by-design behavior we explicitly want to keep: when
    /// the destination is a symlink whose target is a directory, the
    /// `Copy` operation only unlinks the symlink and replaces it with a
    /// regular file. The directory the symlink pointed to (and any data
    /// it contained) must be preserved, because the dangerous case is
    /// recursive deletion of a real directory's tree, not unlinking a
    /// single symlink.
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_overwrite_replaces_symlink_to_directory_without_destroying_target() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target_dir");
        let target_inside = target.join("preserved.txt");
        let link = dir.path().join("link");
        tokio::fs::create_dir_all(&target)
            .await
            .expect("create target dir");
        tokio::fs::write(&target_inside, "preserved")
            .await
            .expect("write target inside");
        unix_fs::symlink(&target, &link).expect("symlink");

        let src = dir.path().join("source.txt");
        tokio::fs::write(&src, "fresh").await.expect("write src");

        let args = json!({
            "source": src.display().to_string(),
            "destination": link.display().to_string(),
            "overwrite": true
        });

        let resp = handle_copy(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], false, "must succeed: {result}");

        // The symlink path is now a regular file.
        let link_meta = tokio::fs::symlink_metadata(&link)
            .await
            .expect("link metadata");
        assert!(
            link_meta.file_type().is_file(),
            "symlink path should now be a regular file"
        );
        // The directory the symlink pointed to and its contents survive.
        assert!(target.is_dir(), "target directory must survive");
        assert!(
            target_inside.exists(),
            "data behind the original symlink must survive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn move_overwrite_replaces_symlink_to_directory_path() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target_dir");
        let target_inside = target.join("preserved.txt");
        let link = dir.path().join("link");
        tokio::fs::create_dir_all(&target)
            .await
            .expect("create target dir");
        tokio::fs::write(&target_inside, "preserved")
            .await
            .expect("write target inside");
        unix_fs::symlink(&target, &link).expect("symlink");

        let src = dir.path().join("source.txt");
        tokio::fs::write(&src, "fresh").await.expect("write src");

        let args = json!({
            "source": src.display().to_string(),
            "destination": link.display().to_string(),
            "overwrite": true
        });

        let resp = handle_move(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], false, "must succeed: {result}");

        let link_meta = tokio::fs::symlink_metadata(&link)
            .await
            .expect("link metadata");
        assert!(
            link_meta.file_type().is_file(),
            "symlink path should now be a regular file"
        );
        assert!(target.is_dir(), "target directory must survive");
        assert!(
            target_inside.exists(),
            "data behind the original symlink must survive"
        );
    }

    #[tokio::test]
    async fn move_rejects_directory_into_own_descendant_without_creating_parent() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let nested_parent = src.join("nested");
        let dst = nested_parent.join("moved");
        tokio::fs::create_dir_all(&src).await.expect("create src");
        tokio::fs::write(src.join("file.txt"), "hello")
            .await
            .expect("write");

        let args = json!({
            "source": src.display().to_string(),
            "destination": dst.display().to_string()
        });

        let resp = handle_move(Some(json!(1)), args).await;
        let result = resp.0;
        assert_eq!(result["isError"], true);
        let msg = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("inside source"));
        assert!(!nested_parent.exists());
        assert!(src.join("file.txt").exists());
    }
}

// ============================================================================
// ListDir
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirRequest {
    path: String,
    #[serde(default)]
    all: Option<bool>,
    #[serde(default)]
    long: Option<bool>,
}

async fn handle_listdir(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<ListDirRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let path = Path::new(&req.path);
    let show_hidden = req.all.unwrap_or(false);
    let long_format = req.long.unwrap_or(false);

    if !path.exists() {
        return ToolCallOutcome::err(format!(
            "path not found: {}. Remediation: check the path (relative to the server working directory) or use '.' for the current directory.",
            path.display()
        ));
    }

    if !path.is_dir() {
        return ToolCallOutcome::err(format!(
            "not a directory: {}. Remediation: pass a directory path (use Read for files).",
            path.display()
        ));
    }

    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(e) => e,
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "failed to read directory {}: {err}. Remediation: check permissions and that the path is a directory.",
                path.display()
            ));
        }
    };

    let mut items: Vec<Value> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files unless requested
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type().await.ok();
        let is_dir = file_type.as_ref().is_some_and(std::fs::FileType::is_dir);
        let is_symlink = file_type
            .as_ref()
            .is_some_and(std::fs::FileType::is_symlink);

        if long_format {
            let metadata = entry.metadata().await.ok();
            let size = metadata.as_ref().map_or(0, std::fs::Metadata::len);
            let modified = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                });

            let type_char = if is_symlink {
                'l'
            } else if is_dir {
                'd'
            } else {
                '-'
            };

            lines.push(format!("{type_char} {size:>10} {name}"));

            items.push(json!({
                "name": name,
                "type": if is_symlink { "symlink" } else if is_dir { "dir" } else { "file" },
                "size": size,
                "modified": modified,
            }));
        } else {
            let suffix = if is_dir {
                "/"
            } else if is_symlink {
                "@"
            } else {
                ""
            };
            lines.push(format!("{name}{suffix}"));
            items.push(json!({
                "name": name,
                "type": if is_symlink { "symlink" } else if is_dir { "dir" } else { "file" },
            }));
        }
    }

    // Sort alphabetically
    lines.sort();
    items.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });

    ToolCallOutcome::ok_text_with(
        lines.join("\n"),
        [
            ("path", json!(path.display().to_string())),
            ("count", json!(items.len())),
            ("entries", json!(items)),
        ],
    )
}

define_mcp_tool! {
    ListDirTool,
    name: "ListDir",
    description: "List directory contents.",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Directory path to list"
            },
            "all": {
                "type": "boolean",
                "default": false,
                "description": "Include hidden files (starting with .)"
            },
            "long": {
                "type": "boolean",
                "default": false,
                "description": "Show detailed information (size, modified time)"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_listdir
}
