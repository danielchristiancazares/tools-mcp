//! File operation tools: Move, Copy, ListDir.

use crate::RpcResponse;
use crate::define_mcp_tool;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

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

async fn handle_move(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<MoveRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.source, "source", id.clone()) {
        return resp;
    }
    if let Err(resp) = validation::validate_non_empty(&req.destination, "destination", id.clone()) {
        return resp;
    }

    let source = Path::new(&req.source);
    let destination = Path::new(&req.destination);

    if !source.exists() {
        return RpcResponse::err(id, format!("source not found: {}", source.display()));
    }

    // If destination is a directory, move into it with same filename
    let final_dest = if destination.is_dir() {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return RpcResponse::err(id, "source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

    if final_dest.exists() && !req.overwrite.unwrap_or(false) {
        return RpcResponse::err(
            id,
            format!(
                "destination already exists: {}. Use overwrite: true to replace.",
                final_dest.display()
            ),
        );
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent() {
        if !parent.exists() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return RpcResponse::err(id, format!("failed to create parent directory: {err}"));
            }
        }
    }

    if let Err(err) = tokio::fs::rename(source, &final_dest).await {
        // rename() may fail across filesystems, try copy+delete
        if source.is_file() {
            if let Err(copy_err) = tokio::fs::copy(source, &final_dest).await {
                return RpcResponse::err(
                    id,
                    format!(
                        "failed to move {}: {err}, copy fallback failed: {copy_err}",
                        source.display()
                    ),
                );
            }
            if let Err(del_err) = tokio::fs::remove_file(source).await {
                return RpcResponse::err(
                    id,
                    format!("moved file but failed to remove source: {del_err}"),
                );
            }
        } else {
            return RpcResponse::err(id, format!("failed to move {}: {err}", source.display()));
        }
    }

    RpcResponse::ok_text_with(
        id,
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
    aliases: ["move", "rename", "mv"],
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

async fn handle_copy(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<CopyRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.source, "source", id.clone()) {
        return resp;
    }
    if let Err(resp) = validation::validate_non_empty(&req.destination, "destination", id.clone()) {
        return resp;
    }

    let source = Path::new(&req.source);
    let destination = Path::new(&req.destination);

    if !source.exists() {
        return RpcResponse::err(id, format!("source not found: {}", source.display()));
    }

    // If destination is a directory, copy into it with same filename
    let final_dest = if destination.is_dir() {
        if let Some(filename) = source.file_name() {
            destination.join(filename)
        } else {
            return RpcResponse::err(id, "source has no filename");
        }
    } else {
        destination.to_path_buf()
    };

    if final_dest.exists() && !req.overwrite.unwrap_or(false) {
        return RpcResponse::err(
            id,
            format!(
                "destination already exists: {}. Use overwrite: true to replace.",
                final_dest.display()
            ),
        );
    }

    // Create parent directories if needed
    if let Some(parent) = final_dest.parent() {
        if !parent.exists() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return RpcResponse::err(id, format!("failed to create parent directory: {err}"));
            }
        }
    }

    if source.is_file() {
        if let Err(err) = tokio::fs::copy(source, &final_dest).await {
            return RpcResponse::err(id, format!("failed to copy {}: {err}", source.display()));
        }
    } else if source.is_dir() {
        if !req.recursive.unwrap_or(false) {
            return RpcResponse::err(
                id,
                format!(
                    "{} is a directory. Use recursive: true to copy directories.",
                    source.display()
                ),
            );
        }
        if let Err(err) = copy_dir_recursive(source, &final_dest).await {
            return RpcResponse::err(
                id,
                format!("failed to copy directory {}: {err}", source.display()),
            );
        }
    }

    RpcResponse::ok_text_with(
        id,
        format!("Copied {} to {}", source.display(), final_dest.display()),
        [
            ("source", json!(source.display().to_string())),
            ("destination", json!(final_dest.display().to_string())),
        ],
    )
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
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
    aliases: ["copy", "cp"],
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

async fn handle_listdir(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<ListDirRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.path, "path", id.clone()) {
        return resp;
    }

    let path = Path::new(&req.path);
    let show_hidden = req.all.unwrap_or(false);
    let long_format = req.long.unwrap_or(false);

    if !path.exists() {
        return RpcResponse::err(
            id,
            format!(
                "path not found: {}. Remediation: check the path (relative to the server working directory) or use '.' for the current directory.",
                path.display()
            ),
        );
    }

    if !path.is_dir() {
        return RpcResponse::err(
            id,
            format!(
                "not a directory: {}. Remediation: pass a directory path (use Read for files).",
                path.display()
            ),
        );
    }

    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(e) => e,
        Err(err) => {
            return RpcResponse::err(
                id,
                format!(
                    "failed to read directory {}: {err}. Remediation: check permissions and that the path is a directory.",
                    path.display()
                ),
            );
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

    RpcResponse::ok_text_with(
        id,
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
    aliases: ["listdir", "ls", "dir"],
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
