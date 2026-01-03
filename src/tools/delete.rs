use crate::RpcResponse;
use crate::define_mcp_tool;
use crate::validation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    path: String,
}

async fn handle_delete(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<DeleteRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.path, "path", id.clone()) {
        return resp;
    }

    let path = Path::new(&req.path);

    if !path.exists() {
        return RpcResponse::err(id, format!("file not found: {}", path.display()));
    }

    if path.is_dir() {
        return RpcResponse::err(
            id,
            format!(
                "cannot delete directory: {}. This tool only deletes files. Remediation: delete files within the directory first, or use a shell tool carefully if you intend to remove a directory.",
                path.display()
            ),
        );
    }

    if let Err(err) = tokio::fs::remove_file(path).await {
        return RpcResponse::err(id, format!("failed to delete {}: {err}", path.display()));
    }

    RpcResponse::ok_text_with(
        id,
        format!("Deleted {}", path.display()),
        [("path", json!(path.display().to_string()))],
    )
}

define_mcp_tool! {
    DeleteTool,
    name: "Delete",
    aliases: ["delete"],
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
