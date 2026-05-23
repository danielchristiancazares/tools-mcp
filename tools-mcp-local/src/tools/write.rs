use crate::path_policy;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::validation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    path: String,
    content: String,
}

async fn handle_write(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<WriteRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let path = match path_policy::resolve_mutation_path(&req.path, "path") {
        Ok(path) => path,
        Err(err) => return ToolCallOutcome::err(err.to_string()),
    };

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        return ToolCallOutcome::err(format!(
            "failed to create parent directories for {}: {err}",
            path.display()
        ));
    }

    // Write the file
    let bytes = req.content.as_bytes();
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
    {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return ToolCallOutcome::err(format!(
                "file already exists: {}. Use Edit to modify existing files.",
                path.display()
            ));
        }
        Err(err) => {
            return ToolCallOutcome::err(format!("failed to create {}: {err}", path.display()));
        }
    };

    if let Err(err) = file.write_all(bytes).await {
        return ToolCallOutcome::err(format!("failed to write {}: {err}", path.display()));
    }

    if let Err(err) = file.flush().await {
        return ToolCallOutcome::err(format!("failed to flush {}: {err}", path.display()));
    }

    let path_display = path.display().to_string();
    ToolCallOutcome::ok_text_with(
        format!("Created {path_display} ({} bytes)", bytes.len()),
        [("path", json!(path_display)), ("bytes", json!(bytes.len()))],
    )
}

#[cfg(test)]
mod tests {
    use super::handle_write;
    use crate::path_policy::tempdir_in_workspace;
    use serde_json::json;

    #[tokio::test]
    async fn write_rejects_parent_traversal_outside_workspace() {
        let outcome = handle_write(
            None,
            json!({
                "path": "../outside-write-policy.txt",
                "content": "blocked"
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], true);
        let msg = outcome.0["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("outside the server working directory"));
    }

    #[tokio::test]
    async fn write_preserves_in_workspace_create_behavior() {
        let dir = tempdir_in_workspace("write-in-scope-");
        let path = dir.path().join("nested").join("created.txt");

        let outcome = handle_write(
            None,
            json!({
                "path": path.display().to_string(),
                "content": "created"
            }),
        )
        .await;

        assert_eq!(
            outcome.0["isError"], false,
            "write should succeed: {outcome:?}"
        );
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read file"),
            "created"
        );
    }

    #[tokio::test]
    async fn write_rejects_existing_files_without_overwriting() {
        let dir = tempdir_in_workspace("write-existing-");
        let path = dir.path().join("existing.txt");
        tokio::fs::write(&path, "original")
            .await
            .expect("seed existing file");

        let outcome = handle_write(
            None,
            json!({
                "path": path.display().to_string(),
                "content": "replacement"
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], true);
        let msg = outcome.0["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("file already exists"));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read file"),
            "original"
        );
    }
}

define_mcp_tool! {
    WriteTool,
    name: "Write",
    description: "Write content to a new file, creating directories as needed",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to write"
            },
            "content": {
                "type": "string",
                "description": "Content to write to the file"
            }
        },
        "required": ["path", "content"],
        "additionalProperties": false
    },
    handler: handle_write
}
