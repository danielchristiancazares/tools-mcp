use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

/// Root directory for WebFetch cache files.
/// Uses platform temp directory for cross-platform compatibility.
fn cache_root() -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push("tools-mcp-webfetch");
    dir
}

fn ensure_cache_dir() -> Result<PathBuf> {
    let dir = cache_root();
    if !dir.exists() {
        fs::create_dir_all(&dir).context("create webfetch cache dir")?;
    }
    Ok(dir)
}

fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn cache_path_for(url: &str) -> Result<PathBuf> {
    let root = ensure_cache_dir()?;
    Ok(root.join(hash_key(url)))
}

/// Persist both headers and body so tokenized summaries stay valid across runs.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFetch {
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub fetched_at: DateTime<Utc>,
}

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
