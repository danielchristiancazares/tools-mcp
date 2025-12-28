//! Disk-based caching for WebFetch responses.
//!
//! This module provides persistent caching of fetched web content to avoid
//! redundant network requests. Cached entries include the response body,
//! content type, and fetch timestamp.
//!
//! ## Cache Location
//!
//! Cache files are stored in the system temp directory:
//! - **Unix**: `/tmp/tools-mcp-webfetch/`
//! - **Windows**: `%TEMP%\tools-mcp-webfetch\`
//!
//! ## Cache Key Strategy
//!
//! Cache keys are SHA-256 hashes of the URL plus rendering method suffix.
//! This ensures:
//! - Unique filenames regardless of URL characters
//! - Separate cache entries for HTTP vs browser-rendered content
//! - No filename length issues on any filesystem
//!
//! Example keys:
//! - `https://example.com_http` -> `a1b2c3...` (HTTP-fetched)
//! - `https://example.com_browser` -> `d4e5f6...` (browser-rendered)
//!
//! ## Cache Format
//!
//! Entries are stored as JSON files containing:
//! - `content_type`: Original Content-Type header
//! - `body`: Raw response bytes (base64 in JSON)
//! - `fetched_at`: ISO 8601 timestamp
//!
//! ## Limitations
//!
//! - No automatic expiration (entries persist indefinitely)
//! - No size limits (cache can grow unbounded)
//! - No cache invalidation API (delete files manually to clear)

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

// ============================================================================
// Cache Directory Management
// ============================================================================

/// Returns the root directory for WebFetch cache files.
///
/// Uses the platform's temp directory for cross-platform compatibility
/// and to ensure the cache is cleaned up on system restart (on some systems).
fn cache_root() -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push("tools-mcp-webfetch");
    dir
}

/// Ensures the cache directory exists, creating it if necessary.
fn ensure_cache_dir() -> Result<PathBuf> {
    let dir = cache_root();
    if !dir.exists() {
        fs::create_dir_all(&dir).context("create webfetch cache dir")?;
    }
    Ok(dir)
}

/// Computes the SHA-256 hash of a cache key for use as a filename.
///
/// Using a hash ensures:
/// - Valid filenames (no special characters from URLs)
/// - Consistent length (64 hex characters)
/// - Collision resistance
fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Returns the filesystem path for a cached URL.
///
/// Note: The `url` parameter should include the method suffix
/// (e.g., `https://example.com_http`) to differentiate cache entries.
pub fn cache_path_for(url: &str) -> Result<PathBuf> {
    let root = ensure_cache_dir()?;
    Ok(root.join(hash_key(url)))
}

// ============================================================================
// Cache Entry Structure
// ============================================================================

/// A cached HTTP response with metadata.
///
/// This structure is serialized to JSON for disk storage. The body is stored
/// as raw bytes, which serde_json encodes as a byte array.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFetch {
    /// The Content-Type header from the original response.
    /// Used to determine extraction strategy when reading from cache.
    pub content_type: Option<String>,

    /// Raw response body bytes.
    /// For browser-rendered content, this is the final HTML after JS execution.
    pub body: Vec<u8>,

    /// Timestamp when the content was originally fetched.
    /// Preserved in response to indicate content freshness.
    pub fetched_at: DateTime<Utc>,
}

// ============================================================================
// Cache Operations
// ============================================================================

/// Reads a cached response for the given URL key.
///
/// # Arguments
///
/// * `url` - The cache key (URL with method suffix, e.g., `https://example.com_http`)
///
/// # Returns
///
/// - `Ok(Some(entry))` - Cache hit, returns the cached data
/// - `Ok(None)` - Cache miss, file does not exist
/// - `Err(...)` - I/O or deserialization error
pub fn read_cache(url: &str) -> Result<Option<CachedFetch>> {
    let path = cache_path_for(url)?;
    if path.exists() {
        let mut file = fs::File::open(path).context("open cache file")?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).context("read cache file")?;
        let entry: CachedFetch = serde_json::from_slice(&buf).context("deserialize cache entry")?;
        Ok(Some(entry))
    } else {
        Ok(None)
    }
}

/// Writes a response to the cache.
///
/// Creates the cache directory if it doesn't exist. Overwrites any existing
/// cache entry for the same URL key.
///
/// # Arguments
///
/// * `url` - The cache key (URL with method suffix)
/// * `data` - The response data to cache
///
/// # Errors
///
/// Returns an error if directory creation, file creation, or serialization fails.
pub fn write_cache(url: &str, data: &CachedFetch) -> Result<()> {
    let path = cache_path_for(url)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("ensure cache directory")?;
    }
    let mut file = fs::File::create(path).context("create cache file")?;
    let bytes = serde_json::to_vec(data).context("serialize cache entry")?;
    file.write_all(&bytes).context("write cache bytes")?;
    Ok(())
}
