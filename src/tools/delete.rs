use crate::tool_registry::McpTool;
use crate::validation;
use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

pub struct DeleteTool;

#[derive(Deserialize)]
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
                "cannot delete directory: {}. Only files can be deleted.",
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

impl McpTool for DeleteTool {
    const NAME: &'static str = "Delete";
    const ALIASES: &'static [&'static str] = &["delete"];
    const DESCRIPTION: &'static str = "Delete a file";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to delete"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(handle_delete(id, args))
    }
}
