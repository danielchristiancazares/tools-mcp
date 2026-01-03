//! # CodeQuery Module
//!
//! Semantic code search orchestration for the MCP server. This module centralizes all
//! vector-store coordination so `main.rs` stays focused on MCP protocol wiring.
//!
//! ## Overview
//!
//! CodeQuery provides intelligent code search by combining:
//! - **Automatic file discovery**: Walks the repository respecting `.gitignore` rules
//! - **Hash-based change detection**: Only re-uploads files whose content has changed
//! - **Vector store management**: Creates, caches, and resolves OpenAI vector stores
//! - **Semantic search**: Queries indexed code using natural language
//!
//! ## Architecture
//!
//! ```text
//! +---------------------------------------------------------------------+
//! |                     handle_code_query                               |
//! |  (MCP tool handler - validates input, coordinates workflow)         |
//! +---------------------------------------------------------------------+
//!                                |
//!         +----------------------+----------------------+
//!         v                      v                      v
//! +---------------+    +-----------------+    +---------------------+
//! | File Discovery|    | Vector Store    |    | core::code_query    |
//! |               |    | Resolution      |    |                     |
//! | - Walk repo   |    | - Cache lookup  |    | - Reindex files     |
//! | - .gitignore  |    | - API fallback  |    | - Wait for indexing |
//! | - Filter dirs |    | - Auto-create   |    | - Execute search    |
//! +---------------+    +-----------------+    +---------------------+
//!                                |
//!                                v
//!                      +-----------------+
//!                      |  cache module   |
//!                      |                 |
//!                      | ~/.codex/mcp/   |
//!                      |   stores.json   |
//!                      +-----------------+
//! ```
//!
//! ## File Discovery Strategy
//!
//! When `file_paths` is not provided, CodeQuery auto-discovers indexable files:
//!
//! 1. Walks from current directory using the `ignore` crate
//! 2. Respects `.gitignore`, `.git/info/exclude`, and global git ignores
//! 3. Skips common non-code directories (`.git`, `node_modules`, `target`, etc.)
//! 4. Filters to indexable code extensions (`.rs`, `.py`, `.js`, `.ts`, etc.)
//! 5. Excludes binary files, images, archives, and documentation
//!
//! ## Vector Store Lifecycle
//!
//! 1. **Name derivation**: Defaults to current directory name if not specified
//! 2. **Cache lookup**: Checks `~/.codex/mcp/stores.json` for known store ID
//! 3. **API fallback**: Lists stores via OpenAI API if cache misses
//! 4. **Auto-creation**: Creates new store if none exists with the given name
//! 5. **Cache update**: Persists newly discovered/created store IDs
//!
//! ## Hash-Based Reindexing
//!
//! CodeQuery uses SHA-256 content hashes to minimize API calls:
//!
//! - Each uploaded file has `path` and `hash` attributes in the vector store
//! - On reindex, local hashes are compared against stored hashes
//! - Only changed files are re-uploaded; unchanged files are skipped
//! - Deleted local files cause their vector store entries to be removed
//! - Moved files (same hash, different path) are detected and updated
//!
//! ## Error Handling
//!
//! The module implements retry logic with exponential backoff for transient errors:
//! - Network timeouts and connection resets
//! - Rate limiting (HTTP 429)
//! - Server errors (HTTP 5xx)
//!
//! Non-transient errors (validation failures, missing files) fail immediately.
//!
//! ## Usage Example
//!
//! ```json
//! {
//!   "method": "mcp/tools/call",
//!   "params": {
//!     "name": "CodeQuery",
//!     "arguments": {
//!       "query": "How does the authentication middleware work?",
//!       "vector_store_name": "my-project"
//!     }
//!   }
//! }
//! ```

use anyhow::{Result, anyhow};
use ignore::{DirEntry, WalkBuilder};
use reqwest::Client;
use serde_json::Value;
use std::path::Path;

use crate::validation;

pub mod cache;

pub use cache::{cache_store_id, load_store_id_from_cache};

/// Handles the CodeQuery MCP tool invocation.
///
/// This is the main entry point for semantic code search. It orchestrates:
/// 1. Input validation and parameter extraction
/// 2. Vector store resolution (by ID or name)
/// 3. File discovery (if paths not provided)
/// 4. Reindexing changed files
/// 5. Executing the semantic search query
///
/// # Parameters (from `args` JSON object)
///
/// | Parameter | Type | Required | Default | Description |
/// |-----------|------|----------|---------|-------------|
/// | `query` | string | **Yes** | - | Natural language search query |
/// | `vector_store_id` | string | No* | - | OpenAI vector store ID |
/// | `vector_store_name` | string | No* | cwd name | Human-readable store name |
/// | `file_paths` | string[] | No | auto-discover | Files to index |
/// | `concurrent_limit` | integer | No | 5 | Max concurrent uploads (1-20) |
/// | `timeout_ms` | integer | No | 60000 | Indexing timeout in milliseconds |
/// | `model` | string | No | gpt-4o | Model for semantic search |
/// | `max_num_results` | integer | No | - | Limit search results |
/// | `include_results` | boolean | No | false | Include raw search results |
///
/// *At least one of `vector_store_id` or `vector_store_name` should be provided,
/// otherwise the current directory name is used as `vector_store_name`.
///
/// # Response Format
///
/// On success, returns MCP content with:
/// - Primary text: The semantic search response
/// - Secondary text (optional): Reindex summary JSON
///
/// On error, returns an MCP error response with a descriptive message.
///
/// # Error Conditions
///
/// - `OPENAI_API_KEY` environment variable not set
/// - Empty or missing `query` parameter
/// - `concurrent_limit` outside valid range (1-20)
/// - `timeout_ms` less than 1000
/// - Vector store resolution failure
/// - File discovery failure (no indexable files found)
/// - Reindex failure after 3 retry attempts
///
/// # Example
///
/// ```ignore
/// let args = serde_json::json!({
///     "query": "How does error handling work?",
///     "vector_store_name": "my-project",
///     "concurrent_limit": 10
/// });
/// let response = handle_code_query(Some(json!(1)), args).await;
/// ```
pub async fn handle_code_query(id: Option<Value>, args: Value) -> crate::RpcResponse<'static> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return crate::RpcResponse::err_with(
            id,
            "OPENAI_API_KEY is not set. CodeQuery uses the OpenAI API (vector stores) and requires an API key.",
            [
                ("error_type", serde_json::json!("missing_env")),
                ("env_var", serde_json::json!("OPENAI_API_KEY")),
                (
                    "remediation",
                    serde_json::json!([
                        "Set OPENAI_API_KEY in the environment before starting the MCP server, then retry CodeQuery.",
                        "If you cannot provide an API key, use Search/Read/Glob for local-only code navigation.",
                    ]),
                ),
            ],
        );
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

    if let Err(resp) = validation::validate_non_empty(query, "query", id.clone()) {
        return resp;
    }

    if vector_store_id_arg.is_none() && vector_store_name.is_none() {
        // We default the store name to the repository directory so every checkout gets a stable
        // vector store without extra MCP arguments. This keeps agent UX simple while still letting
        // advanced callers override via vector_store_name when needed.
        vector_store_name = default_vector_store_name();
        if vector_store_name.is_none() {
            return crate::RpcResponse::err(
                id,
                "CodeQuery could not infer a vector store name. Provide vector_store_name explicitly.",
            );
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
                let message = format!(
                    "CodeQuery could not discover local files: {err}. Remediation: run the server from the repo root or pass file_paths explicitly."
                );
                tracing::error!(error = %message);
                return crate::RpcResponse::err(id, message);
            }
        }
    }

    let concurrent_limit = args
        .get("concurrent_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    if !(1..=20).contains(&concurrent_limit) {
        return crate::RpcResponse::err(
            id,
            format!(
                "concurrent_limit must be between 1 and 20 (got {concurrent_limit}). Use a smaller value to reduce API concurrency."
            ),
        );
    }

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    if timeout_ms < 1_000 {
        return crate::RpcResponse::err(
            id,
            format!(
                "timeout_ms must be at least 1000 milliseconds (got {timeout_ms}). Increase timeout_ms for large repos or slow networks."
            ),
        );
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
                return crate::RpcResponse::err(
                    id,
                    "CodeQuery could not determine a vector store name.",
                );
            };

            match resolve_vector_store_id(&client, &cfg, name).await {
                Ok(id) => id,
                Err(e) => {
                    return crate::RpcResponse::err(
                        id,
                        format!("failed to resolve vector store name '{}': {}", name, e),
                    );
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
                    serde_json::to_string(&summary).unwrap_or_else(|_| summary.to_string());
                content.push(serde_json::json!({
                    "type": "text",
                    "text": summary_text
                }));
            }

            crate::RpcResponse::ok(
                id,
                serde_json::json!({
                    "content": content,
                    "isError": false
                }),
            )
        }
        Err(e) => {
            let error_message = e.to_string();
            let lower = error_message.to_ascii_lowercase();

            // Avoid dumping huge server responses into the primary message; keep a bounded
            // `details` field for debugging while still giving the model actionable hints.
            const MAX_DETAILS_CHARS: usize = 1200;
            let details = if error_message.len() > MAX_DETAILS_CHARS {
                format!("{}…", &error_message[..MAX_DETAILS_CHARS])
            } else {
                error_message.clone()
            };

            let mut remediation: Vec<String> = Vec::new();
            if lower.contains("http 401")
                || lower.contains("unauthorized")
                || lower.contains("invalid api key")
            {
                remediation.push(
                    "Authentication failed. Verify OPENAI_API_KEY is valid, then restart the MCP server and retry."
                        .to_string(),
                );
            }
            if lower.contains("http 429") || lower.contains("rate limit") {
                remediation.push(
                    "You may be rate-limited. Retry later and/or reduce concurrent_limit (e.g., 1-3)."
                        .to_string(),
                );
            }
            if lower.contains("timeout") {
                remediation.push(
                    "Indexing/search timed out. Increase timeout_ms (especially for large repos) and retry."
                        .to_string(),
                );
            }
            if lower.contains("dns") || lower.contains("connection") || lower.contains("network") {
                remediation.push("Network/DNS error. Check connectivity and retry.".to_string());
            }
            if remediation.is_empty() {
                remediation.push(
                    "Retry CodeQuery; transient OpenAI/network errors often resolve.".to_string(),
                );
                remediation.push(
                    "If the repo is large, pass file_paths to limit indexing scope and reduce work."
                        .to_string(),
                );
            }
            remediation
                .push("Fallback: use Search/Read/Glob for local-only code navigation.".to_string());

            let headline = if lower.contains("code_query reindex failed") {
                "CodeQuery indexing failed after multiple attempts."
            } else {
                "CodeQuery failed."
            };

            tracing::error!("CodeQuery error: {}", error_message);

            crate::RpcResponse::err_with(
                id,
                headline,
                [
                    ("error_type", serde_json::json!("codequery_failure")),
                    ("details", serde_json::json!(details)),
                    ("remediation", serde_json::json!(remediation)),
                ],
            )
        }
    }
}

/// Resolves a vector store name to its OpenAI ID using a tiered lookup strategy.
///
/// This function implements a three-tier resolution strategy to minimize API calls
/// while ensuring new projects get automatically provisioned:
///
/// 1. **Cache lookup**: Checks `~/.codex/mcp/stores.json` for a cached mapping
/// 2. **API search**: Lists all vector stores and searches for a name match
/// 3. **Auto-creation**: Creates a new vector store if none exists
///
/// Each successful resolution (from API or creation) updates the cache for
/// subsequent fast lookups.
///
/// # Arguments
///
/// * `client` - HTTP client for OpenAI API requests
/// * `cfg` - API configuration with authentication credentials
/// * `name` - Human-readable vector store name to resolve
///
/// # Returns
///
/// The OpenAI vector store ID (e.g., `vs_abc123def456`).
///
/// # Errors
///
/// Returns an error if:
/// - The API call to list vector stores fails
/// - Vector store creation fails (rate limit, auth error, etc.)
///
/// # Performance
///
/// - Cache hit: No network calls, instant return
/// - Cache miss with existing store: 1 API call (list stores)
/// - New store: 2 API calls (list stores + create)
///
/// The cache is persisted to disk, so the fast path applies across process restarts.
async fn resolve_vector_store_id(
    client: &Client,
    cfg: &crate::core::ApiConfig,
    name: &str,
) -> Result<String> {
    if let Some(id) = load_store_id_from_cache(name) {
        return Ok(id);
    }

    // Fall back to the API when the cache misses so the happy-path stays fast after the
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

/// Derives a default vector store name from the current working directory.
///
/// Uses the final component of the current directory path as the store name.
/// This provides stable, predictable naming across sessions without requiring
/// explicit configuration.
///
/// # Returns
///
/// - `Some(name)` - The directory name as a string
/// - `None` - If the current directory cannot be determined or has no name
///   (e.g., the root directory `/`)
///
/// # Example
///
/// If the current directory is `/home/user/my-project`, returns `Some("my-project")`.
fn default_vector_store_name() -> Option<String> {
    std::env::current_dir().ok().and_then(|path| {
        path.file_name()
            .and_then(|os| os.to_str())
            .map(|s| s.to_string())
    })
}

/// Directories to skip during file discovery.
///
/// These are common directories that contain non-source files:
/// - Version control: `.git`, `.hg`, `.svn`
/// - IDE/editor: `.idea`, `.vscode`
/// - Virtual environments: `.venv`
/// - Build artifacts: `target`, `dist`, `build`, `out`
/// - Dependencies: `node_modules`, `__pycache__`
/// - Other: `coverage`, `tmp`
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

/// Discovers indexable source files in the current directory tree.
///
/// Performs a recursive walk from the current working directory, respecting
/// gitignore rules and filtering to code files suitable for semantic search.
///
/// # Discovery Pipeline
///
/// ```text
/// Current Directory
///        |
///        v
/// +------------------+
/// | WalkBuilder      |  - Respects .gitignore, .git/info/exclude, global ignores
/// | (ignore crate)   |  - Does NOT follow symlinks
/// +------------------+
///        |
///        v
/// +------------------+
/// | should_visit()   |  - Skips SKIP_DIRS (node_modules, target, etc.)
/// |                  |  - Skips hidden directories (starting with .)
/// +------------------+
///        |
///        v
/// +------------------+
/// | should_index_file|  - Filters to indexable extensions (.rs, .py, .js, etc.)
/// |                  |  - Excludes binary files, images, docs
/// +------------------+
///        |
///        v
///   Sorted file list
/// ```
///
/// # Returns
///
/// A sorted vector of absolute file paths suitable for indexing.
///
/// # Errors
///
/// - Current directory cannot be determined
/// - File system traversal error
/// - No indexable files found (empty repository or all files filtered)
///
/// # Gitignore Handling
///
/// The function respects multiple ignore sources:
/// - `.gitignore` files at any level
/// - `.git/info/exclude`
/// - Global gitignore (`~/.config/git/ignore`)
/// - Parent directory ignore files
///
/// Gitignore rules are applied even in non-git directories (e.g., exported archives).
fn discover_default_file_paths() -> Result<Vec<String>> {
    let root = std::env::current_dir()?;
    let mut results = Vec::new();

    // Use `ignore`'s walker so `.gitignore` (plus global/exclude rules) are respected by default.
    // We keep our existing skip rules layered on top via `should_visit`/`should_index_file`.
    for entry in WalkBuilder::new(&root)
        .follow_links(false)
        // We apply our own "dotfile/dotdir" policy rather than `ignore`'s hidden-file default.
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        // Treat `.gitignore` as authoritative even when the checkout isn't a full git repo
        // (e.g. vendored source tree, exported zip, CI artifact).
        .require_git(false)
        .parents(true)
        .filter_entry(should_visit)
        .build()
    {
        let entry = entry?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
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

/// Filter predicate for directory traversal.
///
/// Determines whether a directory entry should be visited during the file walk.
/// This is called by the `ignore` crate's `WalkBuilder` for each entry.
///
/// # Filtering Rules
///
/// - Root directory (depth 0): Always visited
/// - Directories in [`SKIP_DIRS`]: Skipped (e.g., `node_modules`, `target`)
/// - Hidden directories (starting with `.`): Skipped
/// - All other entries: Visited
///
/// # Arguments
///
/// * `entry` - Directory entry from the walk iterator
///
/// # Returns
///
/// `true` if the entry should be visited, `false` to skip it and its descendants.
fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if let Some(name) = entry.file_name().to_str()
        && entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir())
    {
        let lower = name.to_ascii_lowercase();
        if SKIP_DIRS.contains(&lower.as_str()) {
            return false;
        }
        if lower.starts_with('.') {
            return false;
        }
    }

    true
}

/// Determines whether a file should be indexed for semantic search.
///
/// Delegates to [`crate::core::is_codequery_indexable_path`] which checks:
/// - File extension is a known code type (`.rs`, `.py`, `.js`, `.ts`, etc.)
/// - File is not a dotfile
/// - File is not a binary format (images, archives, executables)
/// - File is not documentation (`.md` files excluded)
///
/// # Arguments
///
/// * `path` - Path to the file being considered
///
/// # Returns
///
/// `true` if the file should be uploaded to the vector store for indexing.
fn should_index_file(path: &Path) -> bool {
    crate::core::is_codequery_indexable_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discover_default_file_paths_respects_gitignore() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Cargo.toml"), b"[package]\nname = \"x\"\n").unwrap();
        fs::write(temp.path().join("ignored.rs"), b"fn ignored() {}\n").unwrap();
        fs::write(temp.path().join("kept.rs"), b"fn kept() {}\n").unwrap();
        fs::write(temp.path().join("README.md"), b"# docs\n").unwrap();
        fs::write(temp.path().join("logo.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        fs::write(temp.path().join(".gitignore"), b"ignored.rs\n").unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        let discovered = discover_default_file_paths().unwrap();

        std::env::set_current_dir(original).unwrap();

        let discovered_joined = discovered.join("\n");
        assert!(discovered_joined.contains("kept.rs"));
        assert!(!discovered_joined.contains("ignored.rs"));
        assert!(!discovered_joined.contains("README.md"));
        assert!(!discovered_joined.contains("logo.png"));
    }
}
