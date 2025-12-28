use crate::tool_registry::McpTool;
use crate::validation;
use crate::RpcResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::io::AsyncWriteExt;

pub struct WriteTool;

#[derive(Deserialize)]
struct WriteRequest {
    path: String,
    content: String,
}

async fn handle_write(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<WriteRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.path, "path", id.clone()) {
        return resp;
    }

    let path = Path::new(&req.path);

    // Create parent directories if needed
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() && !parent.exists()
            && let Err(err) = tokio::fs::create_dir_all(parent).await {
                return RpcResponse::err(
                    id,
                    format!(
                        "failed to create parent directories for {}: {err}",
                        path.display()
                    ),
                );
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
            return RpcResponse::err(
                id,
                format!(
                    "file already exists: {}. Use Edit to modify existing files.",
                    path.display()
                ),
            );
        }
        Err(err) => {
            return RpcResponse::err(id, format!("failed to create {}: {err}", path.display()));
        }
    };

    if let Err(err) = file.write_all(bytes).await {
        return RpcResponse::err(id, format!("failed to write {}: {err}", path.display()));
    }

    RpcResponse::ok_text_with(
        id,
        format!("Created {} ({} bytes)", path.display(), bytes.len()),
        [
            ("path", json!(path.display().to_string())),
            ("bytes", json!(bytes.len())),
        ],
    )
}

impl McpTool for WriteTool {
    const NAME: &'static str = "Write";
    const ALIASES: &'static [&'static str] = &["write"];
    const DESCRIPTION: &'static str = "Write content to a new file, creating directories as needed";

    fn input_schema() -> Value {
        json!({
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
        })
    }

    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(handle_write(id, args))
    }
}
