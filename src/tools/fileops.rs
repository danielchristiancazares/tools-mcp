//! File operation tools: Move, Copy, ListDir.

use crate::define_mcp_tool;
use crate::tool_outcome::ToolCallOutcome;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

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
    let req = match ToolCallOutcome::parse_args::<MoveRequest>(args) {
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

    // If destination is a directory, move into it with same filename
    let final_dest = if destination.is_dir() {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return ToolCallOutcome::err("source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

    if final_dest.exists() && !req.overwrite.unwrap_or(false) {
        return ToolCallOutcome::err(format!(
            "destination already exists: {}. Use overwrite: true to replace.",
            final_dest.display()
        ));
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent() {
        if !parent.exists() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return ToolCallOutcome::err(format!("failed to create parent directory: {err}"));
            }
        }
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
        "required": ["source", "destination"]
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
    let req = match ToolCallOutcome::parse_args::<CopyRequest>(args) {
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

    // If destination is a directory, copy into it with same filename
    let final_dest = if destination.is_dir() {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return ToolCallOutcome::err("source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

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

    if final_dest.exists() && !req.overwrite.unwrap_or(false) {
        return ToolCallOutcome::err(format!(
            "destination already exists: {}. Use overwrite: true to replace.",
            final_dest.display()
        ));
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent() {
        if !parent.exists() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return ToolCallOutcome::err(format!("failed to create parent directory: {err}"));
            }
        }
    }

    if source.is_file() {
        if let Err(err) = tokio::fs::copy(source, &final_dest).await {
            return ToolCallOutcome::err(format!("failed to copy {}: {err}", source.display()));
        }
    } else if source.is_dir() {
        if !req.recursive.unwrap_or(false) {
            return ToolCallOutcome::err(format!(
                "{} is a directory. Use recursive: true to copy directories.",
                source.display()
            ));
        }
        if let Err(err) = copy_dir_recursive(source, &final_dest).await {
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

define_mcp_tool! {
    CopyTool,
    name: "Copy",
    description: "Copy a file or directory.",
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
        "required": ["source", "destination"]
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
    let req = match ToolCallOutcome::parse_args::<ListDirRequest>(args) {
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
        let is_dir = file_type.as_ref().is_some_and(|ft| ft.is_dir());
        let is_symlink = file_type.as_ref().is_some_and(|ft| ft.is_symlink());

        if long_format {
            let metadata = entry.metadata().await.ok();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
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

            lines.push(format!("{} {:>10} {}", type_char, size, name));

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
            lines.push(format!("{}{}", name, suffix));
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
        "required": ["path"]
    },
    handler: handle_listdir
}
