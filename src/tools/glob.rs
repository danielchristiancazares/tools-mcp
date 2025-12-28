use crate::config::{DEFAULT_GLOB_LIMIT, MAX_GLOB_LIMIT};
use crate::define_mcp_tool;
use crate::validation;
use crate::RpcResponse;
use glob::{MatchOptions, Pattern};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
struct GlobRequest {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn handle_glob(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<GlobRequest>(id.clone(), args) {
        Ok(req) => req,
        Err(resp) => return resp,
    };

    if let Err(resp) = validation::validate_non_empty(&req.pattern, "pattern", id.clone()) {
        return resp;
    }

    let base_path = req.path.as_deref().unwrap_or(".");
    let include_hidden = req.hidden.unwrap_or(false);
    let limit = validation::clamp_limit(req.limit, DEFAULT_GLOB_LIMIT, 1, MAX_GLOB_LIMIT);

    let base = Path::new(base_path);
    if !base.exists() {
        return RpcResponse::err(id, format!("base path does not exist: {}", base.display()));
    }
    if !base.is_dir() {
        return RpcResponse::err(
            id,
            format!("base path is not a directory: {}", base.display()),
        );
    }

    // Parse the glob pattern
    let pattern = match Pattern::new(&req.pattern) {
        Ok(p) => p,
        Err(err) => {
            return RpcResponse::err(id, format!("invalid glob pattern: {err}"));
        }
    };

    let match_options = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: !include_hidden,
    };

    // Walk directory tree respecting .gitignore
    let walker = WalkBuilder::new(base_path)
        .hidden(!include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let mut files: Vec<String> = Vec::new();
    let mut truncated = false;

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                return RpcResponse::err(id, format!("glob walk error: {err}"));
            }
        };
        // Skip directories
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }

        let path = entry.path();
        let rel_path = path.strip_prefix(base).unwrap_or(path);

        if !pattern.matches_path_with(rel_path, match_options) {
            continue;
        }

        files.push(path.display().to_string());

        if files.len() >= limit {
            truncated = true;
            break;
        }
    }

    // Sort for consistent output
    files.sort();

    let text_output = if files.is_empty() {
        format!("No files match pattern: {}", req.pattern)
    } else {
        files.join("\n")
    };

    let mut payload = json!({
        "content": [{"type": "text", "text": text_output}],
        "isError": false,
        "pattern": req.pattern,
        "base_path": base_path,
        "count": files.len(),
        "files": files
    });

    if truncated
        && let Some(obj) = payload.as_object_mut() {
            obj.insert("truncated".to_string(), Value::Bool(true));
        }

    RpcResponse::ok(id, payload)
}

define_mcp_tool! {
    GlobTool,
    name: "Glob",
    aliases: ["glob"],
    description: "Find files matching a glob pattern",
    schema: {
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Glob pattern (e.g., '**/*.rs', 'src/*.ts')"
            },
            "path": {
                "type": "string",
                "description": "Base directory to search from"
            },
            "hidden": {
                "type": "boolean",
                "description": "Include hidden files"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of matches to return"
            }
        },
        "required": ["pattern"],
        "additionalProperties": false
    },
    handler: handle_glob
}
