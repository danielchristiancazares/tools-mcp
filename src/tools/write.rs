use crate::define_mcp_tool;
use crate::tool_outcome::ToolCallOutcome;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    path: String,
    content: String,
}

async fn handle_write(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<WriteRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    let path = Path::new(&req.path);

    // Create parent directories if needed
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
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
        .open(path)
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
            return ToolCallOutcome::err(format!(
                "failed to create {}: {err}",
                path.display()
            ));
        }
    };

    if let Err(err) = file.write_all(bytes).await {
        return ToolCallOutcome::err(format!(
            "failed to write {}: {err}",
            path.display()
        ));
    }

    ToolCallOutcome::ok_text_with(
        format!("Created {} ({} bytes)", path.display(), bytes.len()),
        [
            ("path", json!(path.display().to_string())),
            ("bytes", json!(bytes.len())),
        ],
    )
}

define_mcp_tool! {
    WriteTool,
    name: "Write",
    aliases: ["write"],
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
