use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::validation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    path: String,
}

async fn handle_delete(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<DeleteRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let path = Path::new(&req.path);

    // Reject symlinks to prevent TOCTOU races and deletion of files outside the workspace.
    if path.is_symlink() {
        return ToolCallOutcome::err(format!(
            "cannot delete symlink: {}. Remediation: delete the target file directly instead.",
            path.display()
        ));
    }

    if !path.exists() {
        return ToolCallOutcome::err(format!("file not found: {}", path.display()));
    }

    if path.is_dir() {
        return ToolCallOutcome::err(format!(
            "cannot delete directory: {}. This tool only deletes files. Remediation: delete files within the directory first, or use a shell tool carefully if you intend to remove a directory.",
            path.display()
        ));
    }

    if let Err(err) = tokio::fs::remove_file(path).await {
        return ToolCallOutcome::err(format!("failed to delete {}: {err}", path.display()));
    }

    ToolCallOutcome::ok_text_with(
        format!("Deleted {}", path.display()),
        [("path", json!(path.display().to_string()))],
    )
}

define_mcp_tool! {
    DeleteTool,
    name: "Delete",
    description: "Delete a file",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to delete"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_delete
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn delete_rejects_symlinks() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link");

        fs::write(&target, "important data").expect("write target");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        if link.is_symlink() {
            let args = serde_json::json!({
                "path": link.to_str().unwrap(),
            });

            let outcome = super::handle_delete(None, args).await;
            let is_error = outcome.0["isError"].as_bool().unwrap();

            assert!(is_error, "deleting a symlink should be an error");
            assert!(target.exists(), "symlink target should not be deleted");
        }

        // Cleanup
        let _ = fs::remove_file(&link);
        let _ = fs::remove_file(&target);
    }

    #[tokio::test]
    async fn delete_still_deletes_regular_files() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("regular.txt");
        fs::write(&path, "content").expect("write file");

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
        });

        let outcome = super::handle_delete(None, args).await;
        let is_error = outcome.0["isError"].as_bool().unwrap();

        assert!(!is_error, "deleting a regular file should succeed");
        assert!(!path.exists(), "file should be deleted");
    }
}
