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
