//! # Vector Store ID Cache
//!
//! Persistent disk cache for mapping vector store names to `OpenAI` IDs.
//!
//! ## Purpose
//!
//! `OpenAI` vector stores are identified by opaque IDs (e.g., `vs_abc123def456`), but
//! users typically reference them by human-readable names. This cache avoids repeated
//! API calls to resolve names to IDs by persisting the mapping to disk.
//!
//! ## Storage Location
//!
//! The cache is stored at `~/.codex/mcp/stores.json` as a simple JSON object:
//!
//! ```json
//! {
//!   "my-project": "vs_abc123def456",
//!   "another-repo": "vs_xyz789ghi012"
//! }
//! ```
//!
//! ## Cache Behavior
//!
//! - **Read**: Returns cached ID if present; returns `None` on cache miss or error
//! - **Write**: Updates cache atomically; logs warning on write failure but does not propagate error
//! - **Invalidation**: No automatic invalidation; cache entries persist until manually cleared
//!
//! ## Error Handling
//!
//! The cache is designed to be resilient:
//! - Missing cache file: Treated as empty cache (not an error)
//! - Corrupt JSON: Logged and treated as empty cache
//! - Missing `HOME` env var: Cache operations become no-ops
//! - Write failures: Logged but do not fail the calling operation
//!
//! This ensures cache issues never block the primary `CodeQuery` workflow.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tracing::warn;

/// In-memory cache of the store-name -> store-id mapping.
///
/// This avoids reading/parsing `stores.json` on every `CodeQuery` invocation in long-running
/// MCP server processes. Disk persistence is still performed on updates via
/// [`write_store_cache`].
static STORE_CACHE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn store_cache() -> &'static RwLock<HashMap<String, String>> {
    STORE_CACHE.get_or_init(|| RwLock::new(load_store_cache()))
}

/// Retrieves a cached vector store ID by name.
///
/// Performs a synchronous disk read of the cache file and looks up the
/// requested name in the stored mappings.
///
/// # Arguments
///
/// * `name` - The human-readable vector store name to look up
///
/// # Returns
///
/// - `Some(id)` - The cached `OpenAI` vector store ID
/// - `None` - Cache miss, or cache could not be read
///
/// # Performance
///
/// Each call reads the entire cache file from disk. For high-frequency lookups,
/// consider caching the result in memory within a single request.
///
/// # Example
///
/// ```ignore
/// if let Some(id) = load_store_id_from_cache("my-project") {
///     // Use cached ID directly
///     query_vector_store(&id).await?;
/// } else {
///     // Fall back to API lookup
///     let id = list_and_find_store("my-project").await?;
/// }
/// ```
pub fn load_store_id_from_cache(name: &str) -> Option<String> {
    store_cache()
        .read()
        .ok()
        .and_then(|cache| cache.get(name).cloned())
}

/// Persists a vector store name-to-ID mapping in the cache.
///
/// Updates the disk cache with the new mapping. If the mapping already exists
/// with the same ID, the write is skipped to avoid unnecessary I/O.
///
/// # Arguments
///
/// * `name` - The human-readable vector store name
/// * `id` - The `OpenAI` vector store ID (e.g., `vs_abc123def456`)
///
/// # Error Handling
///
/// Write failures are logged as warnings but do not propagate errors.
/// This ensures caching issues never fail the primary operation.
///
/// # Atomicity
///
/// The cache file is overwritten atomically via `fs::write`. Concurrent writes
/// from multiple processes may result in lost updates, but the cache will
/// remain valid (one writer wins).
///
/// # Example
///
/// ```ignore
/// // After creating or discovering a vector store
/// let store_id = create_vector_store(&client, &cfg, "my-project").await?;
/// cache_store_id("my-project", &store_id);
/// ```
pub fn cache_store_id(name: &str, id: &str) {
    let snapshot = {
        let mut cache = match store_cache().write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if cache.get(name).is_some_and(|existing| existing == id) {
            return;
        }

        cache.insert(name.to_string(), id.to_string());
        cache.clone()
    };

    if let Err(err) = write_store_cache(&snapshot) {
        warn!("Failed to persist CodeQuery store cache: {}", err);
    }
}

/// Loads the entire cache from disk.
///
/// Reads and parses `~/.codex/mcp/stores.json`. Returns an empty map on any
/// error condition (missing file, parse error, missing HOME).
///
/// # Returns
///
/// A `HashMap` mapping store names to `OpenAI` IDs. Empty if cache cannot be read.
fn load_store_cache() -> HashMap<String, String> {
    let Some(path) = stores_cache_path() else {
        return HashMap::new();
    };

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "Failed to read CodeQuery store cache at {}: {}",
                    path.display(),
                    err
                );
            }
            return HashMap::new();
        }
    };

    match serde_json::from_str::<HashMap<String, String>>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                "Ignoring invalid CodeQuery store cache at {}: {}",
                path.display(),
                err
            );
            HashMap::new()
        }
    }
}

/// Writes the cache to disk.
///
/// Serializes the cache as pretty-printed JSON and writes to `~/.codex/mcp/stores.json`.
/// Creates parent directories if they do not exist.
///
/// # Arguments
///
/// * `cache` - The complete cache contents to write
///
/// # Errors
///
/// Returns an error if:
/// - Parent directory creation fails
/// - JSON serialization fails
/// - File write fails
///
/// Note: Missing HOME environment variable is handled gracefully (logs warning, returns Ok).
fn write_store_cache(cache: &HashMap<String, String>) -> Result<()> {
    let Some(path) = stores_cache_path() else {
        warn!("Skipping CodeQuery store cache write because HOME is unset");
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create CodeQuery store cache directory {}",
                parent.display()
            )
        })?;
    }

    let payload =
        serde_json::to_string_pretty(cache).context("failed to serialize CodeQuery store cache")?;
    fs::write(&path, payload).with_context(|| {
        format!(
            "failed to write CodeQuery store cache at {}",
            path.display()
        )
    })?;
    Ok(())
}

/// Returns the path to the cache file.
///
/// The cache is stored at `$HOME/.codex/mcp/stores.json`.
///
/// # Returns
///
/// - `Some(path)` - The absolute path to the cache file
/// - `None` - If the HOME environment variable is not set
fn stores_cache_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".codex");
        path.push("mcp");
        path.push("stores.json");
        path
    })
}
