use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::Value;
use std::path::Path;
use walkdir::{DirEntry, WalkDir};

// The CodeQuery module centralizes all vector-store orchestration so `main.rs` stays focused on
// MCP protocol wiring. Keeping this logic together avoids mixing transport concerns with OpenAI
// API coordination, which will make future tool additions less coupled.

pub mod cache;

pub use cache::{cache_store_id, load_store_id_from_cache};

pub async fn handle_code_query(id: Option<Value>, args: Value) -> crate::RpcResponse<'static> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return crate::RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(crate::err_text("OPENAI_API_KEY not set")),
            error: None,
        };
    }

    let vector_store_id_arg = args
        .get("vector_store_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let mut vector_store_name = args
        .get("vector_store_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

    if query.trim().is_empty() {
        return crate::RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(crate::err_text("query is required for CodeQuery")),
            error: None,
        };
    }

    if vector_store_id_arg.is_none() && vector_store_name.is_none() {
        // We default the store name to the repository directory so every checkout gets a stable
        // vector store without extra MCP arguments. This keeps agent UX simple while still letting
        // advanced callers override via vector_store_name when needed.
        vector_store_name = default_vector_store_name();
        if vector_store_name.is_none() {
            return crate::RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(crate::err_text(
                    "CodeQuery could not infer a vector store name. Provide vector_store_name explicitly.",
                )),
                error: None,
            };
        }
    }

    let mut file_paths: Vec<String> = args
        .get("file_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|val| val.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if file_paths.is_empty() {
        match discover_default_file_paths() {
            Ok(mut discovered) => {
                tracing::info!(
                    "CodeQuery auto-discovered {} file(s) for indexing",
                    discovered.len()
                );
                file_paths.append(&mut discovered);
            }
            Err(err) => {
                let message = format!("CodeQuery could not discover local files: {}", err);
                tracing::error!(error = %message);
                return crate::RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(crate::err_text(&message)),
                    error: None,
                };
            }
        }
    }

    let concurrent_limit = args
        .get("concurrent_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    if !(1..=20).contains(&concurrent_limit) {
        return crate::RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(crate::err_text("concurrent_limit must be between 1 and 20")),
            error: None,
        };
    }

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    if timeout_ms < 1_000 {
        return crate::RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(crate::err_text(
                "timeout_ms must be at least 1000 milliseconds",
            )),
            error: None,
        };
    }

    let include_results = args
        .get("include_results")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_num_results = args
        .get("max_num_results")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let model_override = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let client = reqwest::Client::new();
    let cfg = crate::core::ApiConfig::new(api_key, model_override.as_deref().unwrap_or("gpt-4o"));

    let vector_store_id = match vector_store_id_arg {
        Some(id) => id,
        None => {
            let Some(name) = vector_store_name.as_deref() else {
                return crate::RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(crate::err_text(
                        "CodeQuery could not determine a vector store name.",
                    )),
                    error: None,
                };
            };

            match resolve_vector_store_id(&client, &cfg, name).await {
                Ok(id) => id,
                Err(e) => {
                    return crate::RpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: Some(crate::err_text(&format!(
                            "failed to resolve vector store name '{}': {}",
                            name, e
                        ))),
                        error: None,
                    };
                }
            }
        }
    };

    match crate::core::code_query(
        &client,
        &cfg,
        &vector_store_id,
        &file_paths,
        query,
        crate::core::CodeQueryOptions {
            concurrent_limit,
            timeout_ms,
            model: model_override.as_deref(),
            max_num_results,
            include_results,
        },
    )
    .await
    {
        Ok((text, reindex_summary)) => {
            let mut content = vec![serde_json::json!({
                "type": "text",
                "text": text
            })];

            if let Some(summary) = reindex_summary {
                let summary_text =
                    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| summary.to_string());
                content.push(serde_json::json!({
                    "type": "text",
                    "text": summary_text
                }));
            }

            crate::RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(serde_json::json!({
                    "content": content,
                    "isError": false
                })),
                error: None,
            }
        }
        Err(e) => {
            let error_message = e.to_string();
            let (client_message, log_message) = if error_message
                .contains("code_query reindex failed")
            {
                (
                    "Codebase indexing failed after 3 attempts. Please try manual searching heuristics."
                        .to_string(),
                    format!("CodeQuery reindex failed: {}", error_message),
                )
            } else {
                (
                    format!("CodeQuery failed: {}", error_message),
                    format!("CodeQuery error: {}", error_message),
                )
            };

            tracing::error!(%log_message);

            crate::RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(crate::err_text(&client_message)),
                error: None,
            }
        }
    }
}

async fn resolve_vector_store_id(
    client: &Client,
    cfg: &crate::core::ApiConfig,
    name: &str,
) -> Result<String> {
    if let Some(id) = load_store_id_from_cache(name) {
        return Ok(id);
    }

    // We fall back to the API when the cache misses so the happy-path stays fast after the
    // first lookup without requiring manual list-stores calls.
    let stores = crate::core::list_vector_stores(client, cfg).await?;
    if let Some(entry) = stores
        .into_iter()
        .find(|entry| entry.name.as_deref() == Some(name))
    {
        cache_store_id(name, &entry.id);
        return Ok(entry.id);
    }

    // Absent a matching store we create one automatically so new clones come online without
    // manual setup. This favors seamless agent startup over requiring explicit provisioning.
    let new_id = crate::core::create_vector_store(client, cfg, name).await?;
    cache_store_id(name, &new_id);
    Ok(new_id)
}

fn default_vector_store_name() -> Option<String> {
    std::env::current_dir().ok().and_then(|path| {
        path.file_name()
            .and_then(|os| os.to_str())
            .map(|s| s.to_string())
    })
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    ".venv",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    "tmp",
];

const EXTRA_ALLOWED_EXTS: &[&str] = &[
    "rs", "toml", "lock", "yaml", "yml", "ini", "cfg", "conf", "sh", "bash", "zsh", "c", "cpp",
    "h", "hpp", "tsx", "jsx", "ts", "js", "css", "scss", "less", "xml", "sql", "proto", "env",
    "gradle", "swift", "kt", "kts",
];

const ALWAYS_INCLUDE_FILENAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "justfile",
    "cargo.toml",
    "cargo.lock",
    "license",
    "readme",
];

fn discover_default_file_paths() -> Result<Vec<String>> {
    let root = std::env::current_dir()?;
    let mut results = Vec::new();

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_visit(entry))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if should_index_file(path) {
            results.push(path.to_string_lossy().to_string());
        }
    }

    if results.is_empty() {
        return Err(anyhow!("No indexable files found under {}", root.display()));
    }

    results.sort();
    Ok(results)
}

fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if let Some(name) = entry.file_name().to_str() {
        if entry.file_type().is_dir() {
            let lower = name.to_ascii_lowercase();
            if SKIP_DIRS.contains(&lower.as_str()) {
                return false;
            }
            if lower.starts_with('.') {
                return false;
            }
        }
    }

    true
}

fn should_index_file(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(name) => name,
        None => return false,
    };

    if file_name.starts_with('.') && !file_name.eq_ignore_ascii_case(".env") {
        return false;
    }

    if ALWAYS_INCLUDE_FILENAMES
        .iter()
        .any(|candidate| file_name.eq_ignore_ascii_case(candidate))
    {
        return true;
    }

    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    match extension.as_deref() {
        Some(ext) => crate::core::is_allowed_upload_ext(ext) || EXTRA_ALLOWED_EXTS.contains(&ext),
        None => false,
    }
}
