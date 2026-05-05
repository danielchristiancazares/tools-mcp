//! Disk-based caching for `WebFetch` responses.
//!
//! This module provides persistent caching of fetched web content to avoid
//! redundant network requests. Cached entries include the response body,
//! content type, and fetch timestamp.
//!
//! ## Cache Location
//!
//! Cache files are stored in the system temp directory:
//! - **Unix**: `/tmp/tools-webfetch/`
//! - **Windows**: `%TEMP%\tools-webfetch\`
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
//! - `body`: Raw response bytes (base64 string in JSON)
//! - `fetched_at`: ISO 8601 timestamp
//!
//! ## Limitations
//!
//! - No automatic expiration (entries persist indefinitely)
//! - No cache invalidation API (delete files manually to clear)

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

/// Maximum size for a cache entry (25 MiB).
///
/// Entries exceeding this size are not written to cache to prevent
/// unbounded disk usage from large responses.
const MAX_CACHE_ENTRY_BYTES: usize = 25 * 1024 * 1024;

// ============================================================================
// Cache Directory Management
// ============================================================================

/// Returns the root directory for `WebFetch` cache files.
///
/// Uses the platform's temp directory for cross-platform compatibility
/// and to ensure the cache is cleaned up on system restart (on some systems).
fn cache_root() -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push("tools-webfetch");
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
/// as raw bytes, which `serde_json` encodes as a byte array.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFetch {
    /// The Content-Type header from the original response.
    /// Used to determine extraction strategy when reading from cache.
    pub content_type: Option<String>,

    /// Raw response body bytes.
    /// For browser-rendered content, this is the final HTML after JS execution.
    ///
    /// Serialized as base64 for compact JSON, but deserialization also accepts the
    /// legacy JSON `Vec<u8>` representation (an array of numbers) for backward
    /// compatibility with existing cache files.
    #[serde(
        serialize_with = "serialize_body_base64",
        deserialize_with = "deserialize_body_base64_or_array"
    )]
    pub body: Vec<u8>,

    /// Timestamp when the content was originally fetched.
    /// Preserved in response to indicate content freshness.
    pub fetched_at: DateTime<Utc>,
}

fn serialize_body_base64<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let encoded = general_purpose::STANDARD.encode(bytes);
    serializer.serialize_str(&encoded)
}

fn deserialize_body_base64_or_array<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BodyVisitor;

    impl<'de> de::Visitor<'de> for BodyVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a base64 string or an array of bytes")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            general_purpose::STANDARD
                .decode(v)
                .map_err(|e| de::Error::custom(format!("invalid base64 body: {e}")))
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&v)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(b) = seq.next_element::<u8>()? {
                out.push(b);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_any(BodyVisitor)
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
        let mut file = fs::File::open(&path).context("open cache file")?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).context("read cache file")?;
        match serde_json::from_slice(&buf) {
            Ok(entry) => Ok(Some(entry)),
            Err(err) => {
                let _ = fs::remove_file(&path);
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Ignoring corrupted webfetch cache entry"
                );
                Ok(None)
            }
        }
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
    let bytes = serde_json::to_vec(data).context("serialize cache entry")?;
    if bytes.len() > MAX_CACHE_ENTRY_BYTES {
        anyhow::bail!(
            "cache entry for {} exceeds maximum size ({} bytes > {} bytes)",
            url,
            bytes.len(),
            MAX_CACHE_ENTRY_BYTES
        );
    }
    let mut file = fs::File::create(path).context("create cache file")?;
    file.write_all(&bytes).context("write cache bytes")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use std::sync::{Mutex, MutexGuard};

    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn cache_test_guard() -> MutexGuard<'static, ()> {
        CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn cached_fetch_serializes_body_as_base64_string() {
        let fetched_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let entry = CachedFetch {
            content_type: Some("text/plain".to_string()),
            body: vec![0, 1, 2, 3],
            fetched_at,
        };

        let v: serde_json::Value = serde_json::to_value(&entry).expect("serialize");
        let body = v.get("body").expect("missing body");
        assert!(body.is_string(), "body should be base64 string");
        assert_eq!(body.as_str().unwrap(), "AAECAw==");
    }

    #[test]
    fn cached_fetch_deserializes_legacy_body_array() {
        let fetched_at = "2025-01-01T00:00:00Z";
        let v = serde_json::json!({
            "content_type": "text/plain",
            "body": [0, 1, 2, 3],
            "fetched_at": fetched_at
        });

        let entry: CachedFetch = serde_json::from_value(v).expect("deserialize");
        assert_eq!(entry.body, vec![0, 1, 2, 3]);
        assert_eq!(entry.content_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn cache_write_then_read_roundtrip() {
        let _guard = cache_test_guard();
        let fetched_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let key = format!("test://cache-roundtrip-{}_http", uuid::Uuid::new_v4());
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: b"<html><body>ok</body></html>".to_vec(),
            fetched_at,
        };

        // Ensure clean slate.
        let path = cache_path_for(&key).expect("cache path");
        let _ = fs::remove_file(&path);

        write_cache(&key, &entry).expect("write cache");
        let loaded = read_cache(&key)
            .expect("read cache")
            .expect("expected cache hit");
        assert_eq!(loaded.content_type, entry.content_type);
        assert_eq!(loaded.body, entry.body);

        // Cleanup so temp dir doesn't grow unbounded during test runs.
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_cache_treats_corrupt_entry_as_miss_and_removes_file() {
        let _guard = cache_test_guard();
        let key = format!("test://cache-corrupt-{}_http", uuid::Uuid::new_v4());
        let path = cache_path_for(&key).expect("cache path");

        fs::write(&path, b"{not-json").expect("write corrupt cache");

        let loaded = read_cache(&key).expect("read cache should not fail on corruption");
        assert!(loaded.is_none(), "corrupt cache should be treated as miss");
        assert!(
            !path.exists(),
            "corrupt cache file should be removed to avoid repeated failures"
        );
    }

    // BUG: read_cache removes the corrupt file but does not return Err — it returns Ok(None).
    // This means callers can't distinguish between "cache miss" and "corrupt entry removed".
    // The tracing::warn! is the only signal, which is lost in non-debug environments.
    #[test]
    fn read_cache_silently_swallows_corruption_error() {
        let _guard = cache_test_guard();
        let key = format!("test://cache-silent-corrupt-{}_http", uuid::Uuid::new_v4());
        let path = cache_path_for(&key).expect("cache path");

        fs::write(&path, b"{not-json").expect("write corrupt cache");

        let loaded = read_cache(&key).expect("should not error");

        // BUG: Returns Ok(None) — indistinguishable from a genuine cache miss.
        // A caller would retry the network fetch, unaware that a corrupt entry existed.
        assert!(loaded.is_none());
        assert!(!path.exists());
        // This test documents the bug: no way to tell corruption from miss.
    }

    // BUG: write_cache does not check if the parent directory exists before creating the file.
    // The cache_root() function creates the directory, but if someone deletes it between
    // cache_path_for() and write_cache(), the write will fail.
    #[test]
    fn write_cache_creates_parent_directory_if_missing() {
        let _guard = cache_test_guard();
        let key = format!("test://cache-dir-check-{}_http", uuid::Uuid::new_v4());
        let path = cache_path_for(&key).expect("cache path");

        // Ensure directory exists, then remove it to test recreation.
        let _ = fs::remove_dir_all(path.parent().unwrap());

        let fetched_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: b"test".to_vec(),
            fetched_at,
        };

        // This should succeed even if the directory was removed.
        let result = write_cache(&key, &entry);
        assert!(
            result.is_ok(),
            "write_cache should recreate missing directory"
        );

        // Cleanup.
        let _ = fs::remove_file(&path);
    }

    // REGRESSION: Cache entry size is now limited to MAX_CACHE_ENTRY_BYTES (25 MiB).
    // Entries exceeding this size are rejected.
    #[test]
    fn cache_rejects_entries_exceeding_size_limit() {
        let _guard = cache_test_guard();
        let unique_id = uuid::Uuid::new_v4();
        let key = format!("test://cache-size-limit-{}_http", unique_id);
        let _ = fs::remove_file(cache_path_for(&key).unwrap());

        // Create an entry exceeding the limit (26 MB).
        let large_body = vec![0u8; 26 * 1024 * 1024];
        let fetched_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: large_body,
            fetched_at,
        };

        let result = write_cache(&key, &entry);
        assert!(
            result.is_err(),
            "cache should reject entries exceeding 25 MiB"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds maximum size"),
            "error message should mention size limit"
        );
    }
}
