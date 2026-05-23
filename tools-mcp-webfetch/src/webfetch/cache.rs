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
//! ## Cache Policy
//!
//! - Entries expire after a conservative default TTL of 24 hours. Override with
//!   `WEBFETCH_CACHE_TTL_SECONDS`; `0` makes entries expire immediately.
//! - Total cache storage is capped at 100 MiB by default. Override with
//!   `WEBFETCH_CACHE_MAX_BYTES`; `0` prunes all cache entries after writes.
//! - No cache invalidation API (delete files manually to clear)

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Maximum size for a cache entry (25 MiB).
///
/// Entries exceeding this size are not written to cache to prevent
/// unbounded disk usage from large responses.
const MAX_CACHE_ENTRY_BYTES: usize = 25 * 1024 * 1024;

/// Cache entries are short-lived by default because fetched web content may
/// contain sensitive or fast-changing data. Operators can raise or lower this
/// via `WEBFETCH_CACHE_TTL_SECONDS`; invalid values fall back to this default.
const DEFAULT_CACHE_TTL_SECONDS: u64 = 24 * 60 * 60;

/// Total cache quota. This bounds persistent disk growth independently of the
/// per-entry cap above. Operators can override via `WEBFETCH_CACHE_MAX_BYTES`;
/// invalid values fall back to this default.
const DEFAULT_CACHE_MAX_BYTES: u64 = 100 * 1024 * 1024;

const CACHE_TTL_SECONDS_ENV: &str = "WEBFETCH_CACHE_TTL_SECONDS";
const CACHE_MAX_BYTES_ENV: &str = "WEBFETCH_CACHE_MAX_BYTES";
const CACHE_PRUNE_WRITE_INTERVAL: u64 = 8;
const CACHE_PRUNE_PENDING_BYTES_DIVISOR: u64 = 4;
const CACHE_PRUNE_MIN_PENDING_BYTES: u64 = 1024 * 1024;

static CACHE_PRUNE_STATE: OnceLock<Mutex<HashMap<PathBuf, CachePruneState>>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
struct CachePolicy {
    ttl: Duration,
    max_total_bytes: u64,
}

#[derive(Debug, Default)]
struct CachePruneState {
    writes_since_prune: u64,
    bytes_since_prune: u64,
}

impl CachePolicy {
    #[cfg(test)]
    fn defaults() -> Self {
        Self {
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            max_total_bytes: DEFAULT_CACHE_MAX_BYTES,
        }
    }

    fn from_env() -> Self {
        Self {
            ttl: Duration::from_secs(env_u64_or_default(
                CACHE_TTL_SECONDS_ENV,
                DEFAULT_CACHE_TTL_SECONDS,
            )),
            max_total_bytes: env_u64_or_default(CACHE_MAX_BYTES_ENV, DEFAULT_CACHE_MAX_BYTES),
        }
    }
}

fn env_u64_or_default(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => match value.parse::<u64>() {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(
                    env = name,
                    value = %value,
                    error = %err,
                    default,
                    "Ignoring invalid webfetch cache policy override"
                );
                default
            }
        },
        Err(std::env::VarError::NotPresent) => default,
        Err(err) => {
            tracing::warn!(
                env = name,
                error = %err,
                default,
                "Ignoring unreadable webfetch cache policy override"
            );
            default
        }
    }
}

fn cache_policy() -> CachePolicy {
    CachePolicy::from_env()
}

// ============================================================================
// Cache Directory Management
// ============================================================================

/// Returns the root directory for `WebFetch` cache files.
///
/// Uses the platform's temp directory for cross-platform compatibility
/// and to ensure the cache is cleaned up on system restart (on some systems).
fn cache_root() -> PathBuf {
    #[cfg(test)]
    {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent()
            .unwrap_or(&manifest_dir)
            .join("target")
            .join("webfetch-cache-tests")
            .join("default")
    }

    #[cfg(not(test))]
    {
        let mut dir = std::env::temp_dir();
        dir.push("tools-webfetch");
        dir
    }
}

/// Ensures the cache directory exists, creating it if necessary.
fn ensure_cache_dir() -> Result<PathBuf> {
    let dir = cache_root();
    ensure_cache_dir_at(&dir)?;
    Ok(dir)
}

fn ensure_cache_dir_at(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("refusing to use symlinked webfetch cache directory")
        }
        Ok(metadata) if !metadata.file_type().is_dir() => {
            anyhow::bail!("refusing to use non-directory webfetch cache path")
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).context("create webfetch cache dir")?;
            let metadata = fs::symlink_metadata(dir)
                .with_context(|| format!("inspect webfetch cache dir {}", dir.display()))?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!("refusing to use symlinked webfetch cache directory")
            }
            if !metadata.file_type().is_dir() {
                anyhow::bail!("refusing to use non-directory webfetch cache path")
            }
        }
        Err(err) => return Err(err).context("inspect webfetch cache dir"),
    }

    harden_cache_dir_permissions(dir)
}

#[cfg(unix)]
fn harden_cache_dir_permissions(dir: &Path) -> Result<()> {
    let mut open_options = fs::OpenOptions::new();
    open_options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let dir_file = open_options
        .open(dir)
        .with_context(|| format!("open webfetch cache dir {}", dir.display()))?;
    let permissions = dir_file
        .metadata()
        .with_context(|| format!("inspect webfetch cache dir {}", dir.display()))?
        .permissions();
    if permissions.mode() & 0o777 != 0o700 {
        dir_file
            .set_permissions(fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict webfetch cache dir {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_cache_dir_permissions(_dir: &Path) -> Result<()> {
    Ok(())
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
#[cfg(test)]
pub fn cache_path_for(url: &str) -> Result<PathBuf> {
    let root = ensure_cache_dir()?;
    Ok(cache_path_in_root(&root, url))
}

fn cache_path_in_root(root: &Path, url: &str) -> PathBuf {
    root.join(hash_key(url))
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

#[derive(Debug, Deserialize)]
struct CachedFetchMetadata {
    fetched_at: DateTime<Utc>,
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
            let mut out =
                Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_CACHE_ENTRY_BYTES));
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
/// - `Ok(None)` - Cache miss, expired entry, corrupt entry, or unsafe cache path
/// - `Err(...)` - I/O error while reading a valid cache file
pub fn read_cache(url: &str) -> Result<Option<CachedFetch>> {
    let root = ensure_cache_dir()?;
    read_cache_from_root(&root, url, cache_policy())
}

fn read_cache_from_root(
    root: &Path,
    url: &str,
    policy: CachePolicy,
) -> Result<Option<CachedFetch>> {
    let path = cache_path_in_root(root, url);
    if is_readable_cache_leaf(&path)? {
        let file = match open_cache_file_for_read(&path) {
            Ok(file) => file,
            Err(err) if is_symlink_open_error(&err) => {
                remove_cache_file_best_effort(&path, "unsafe");
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Ignoring symlinked webfetch cache entry"
                );
                return Ok(None);
            }
            Err(err) => return Err(err).context("open cache file"),
        };
        let file_len = file
            .metadata()
            .with_context(|| format!("inspect webfetch cache entry {}", path.display()))?
            .len();
        if file_len > MAX_CACHE_ENTRY_BYTES as u64 {
            remove_cache_file_best_effort(&path, "oversized");
            tracing::warn!(
                path = %path.display(),
                size = file_len,
                max_size = MAX_CACHE_ENTRY_BYTES,
                "Ignoring oversized webfetch cache entry"
            );
            return Ok(None);
        }

        match serde_json::from_reader::<_, CachedFetch>(BufReader::new(file)) {
            Ok(entry) => {
                if is_expired(entry.fetched_at, policy.ttl, Utc::now()) {
                    remove_cache_file_best_effort(&path, "expired");
                    Ok(None)
                } else {
                    Ok(Some(entry))
                }
            }
            Err(err) if err.is_io() => Err(err).context("read cache file"),
            Err(err) => {
                remove_cache_file_best_effort(&path, "corrupted");
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

fn is_readable_cache_leaf(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            remove_cache_file_best_effort(path, "unsafe");
            tracing::warn!(
                path = %path.display(),
                "Ignoring symlinked webfetch cache entry"
            );
            Ok(false)
        }
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => {
            remove_cache_file_best_effort(path, "unsafe");
            tracing::warn!(
                path = %path.display(),
                "Ignoring non-file webfetch cache entry"
            );
            Ok(false)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).context("inspect cache file"),
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
/// Returns an error if directory creation, path safety checks, file creation,
/// serialization, or pruning fails.
pub fn write_cache(url: &str, data: &CachedFetch) -> Result<()> {
    let root = ensure_cache_dir()?;
    write_cache_to_root(&root, url, data, cache_policy())
}

fn write_cache_to_root(
    root: &Path,
    url: &str,
    data: &CachedFetch,
    policy: CachePolicy,
) -> Result<()> {
    ensure_cache_dir_at(root)?;
    let path = cache_path_in_root(root, url);
    ensure_regular_cache_leaf(&path)?;
    let bytes = serde_json::to_vec(data).context("serialize cache entry")?;
    if bytes.len() > MAX_CACHE_ENTRY_BYTES {
        anyhow::bail!(
            "cache entry for {} exceeds maximum size ({} bytes > {} bytes)",
            url,
            bytes.len(),
            MAX_CACHE_ENTRY_BYTES
        );
    }
    let mut open_options = fs::OpenOptions::new();
    open_options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        open_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = open_options.open(&path).context("create cache file")?;
    harden_cache_file_permissions(&file, &path)?;
    file.write_all(&bytes).context("write cache bytes")?;
    if should_prune_cache_after_write(root, bytes.len() as u64, policy)? {
        prune_cache_root(root, policy).context("prune webfetch cache")?;
        mark_cache_pruned(root)?;
    }
    Ok(())
}

fn open_cache_file_for_read(path: &Path) -> io::Result<fs::File> {
    let mut open_options = fs::OpenOptions::new();
    open_options.read(true);
    #[cfg(unix)]
    {
        open_options.custom_flags(libc::O_NOFOLLOW);
    }
    open_options.open(path)
}

#[cfg(unix)]
fn is_symlink_open_error(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_err: &io::Error) -> bool {
    false
}

#[cfg(unix)]
fn harden_cache_file_permissions(file: &fs::File, path: &Path) -> Result<()> {
    let permissions = file
        .metadata()
        .with_context(|| format!("inspect webfetch cache file {}", path.display()))?
        .permissions();
    if permissions.mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict webfetch cache file {}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_cache_file_permissions(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

fn ensure_regular_cache_leaf(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("refusing to write webfetch cache entry through symlink")
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            anyhow::bail!("refusing to overwrite non-file webfetch cache path")
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("inspect existing cache path"),
    }
}

fn is_expired(fetched_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> bool {
    if ttl.is_zero() {
        return true;
    }

    match now.signed_duration_since(fetched_at).to_std() {
        Ok(age) => age >= ttl,
        Err(_) => false,
    }
}

fn remove_cache_file_best_effort(path: &Path, reason: &str) {
    match fs::remove_file(path) {
        Ok(()) => {
            tracing::debug!(path = %path.display(), reason, "Removed webfetch cache entry");
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                reason,
                error = %err,
                "Failed to remove webfetch cache entry"
            );
        }
    }
}

fn remove_cache_file(path: &Path, reason: &str) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            tracing::debug!(path = %path.display(), reason, "Removed webfetch cache entry");
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {reason} cache entry")),
    }
}

fn should_prune_cache_after_write(
    root: &Path,
    written_bytes: u64,
    policy: CachePolicy,
) -> Result<bool> {
    if policy.ttl.is_zero() || policy.max_total_bytes == 0 {
        return Ok(true);
    }

    let threshold = prune_pending_byte_threshold(policy);
    if written_bytes >= threshold {
        return Ok(true);
    }

    let state = CACHE_PRUNE_STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "WebFetch cache prune state lock was poisoned; pruning conservatively"
            );
            return Ok(true);
        }
    };

    let entry = state.entry(root.to_path_buf()).or_default();
    entry.writes_since_prune = entry.writes_since_prune.saturating_add(1);
    entry.bytes_since_prune = entry.bytes_since_prune.saturating_add(written_bytes);

    Ok(entry.writes_since_prune >= CACHE_PRUNE_WRITE_INTERVAL
        || entry.bytes_since_prune > threshold)
}

fn mark_cache_pruned(root: &Path) -> Result<()> {
    let Some(state) = CACHE_PRUNE_STATE.get() else {
        return Ok(());
    };

    match state.lock() {
        Ok(mut state) => {
            state.remove(root);
            Ok(())
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "WebFetch cache prune state lock was poisoned after pruning"
            );
            Ok(())
        }
    }
}

fn prune_pending_byte_threshold(policy: CachePolicy) -> u64 {
    let quota_fraction = policy
        .max_total_bytes
        .checked_div(CACHE_PRUNE_PENDING_BYTES_DIVISOR)
        .unwrap_or(0)
        .max(1);
    quota_fraction
        .max(CACHE_PRUNE_MIN_PENDING_BYTES)
        .min(policy.max_total_bytes)
}

#[derive(Debug)]
struct CacheFileForPrune {
    path: PathBuf,
    size: u64,
    fetched_at: DateTime<Utc>,
}

fn prune_cache_root(root: &Path, policy: CachePolicy) -> Result<()> {
    fs::create_dir_all(root).context("ensure cache directory")?;

    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    let now = Utc::now();

    for entry in fs::read_dir(root).context("read webfetch cache dir")? {
        let entry = entry.context("read webfetch cache dir entry")?;
        let path = entry.path();
        if !is_direct_cache_entry_path(root, &path) {
            continue;
        }

        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).context("inspect webfetch cache entry"),
        };
        if !metadata.file_type().is_file() {
            continue;
        }

        let size = metadata.len();
        if size > MAX_CACHE_ENTRY_BYTES as u64 {
            remove_cache_file(&path, "oversized")?;
            continue;
        }

        let fetched_at = match read_cache_metadata(&path) {
            Ok(metadata) => metadata.fetched_at,
            Err(err) => {
                remove_cache_file(&path, "corrupted")?;
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Ignoring corrupted webfetch cache entry during pruning"
                );
                continue;
            }
        };

        if is_expired(fetched_at, policy.ttl, now) {
            remove_cache_file(&path, "expired")?;
            continue;
        }

        total_bytes = total_bytes.saturating_add(size);
        files.push(CacheFileForPrune {
            path,
            size,
            fetched_at,
        });
    }

    if total_bytes <= policy.max_total_bytes {
        return Ok(());
    }

    files.sort_by(|left, right| {
        left.fetched_at
            .cmp(&right.fetched_at)
            .then_with(|| left.path.cmp(&right.path))
    });

    for file in files {
        if total_bytes <= policy.max_total_bytes {
            break;
        }

        remove_cache_file(&file.path, "quota")?;
        total_bytes = total_bytes.saturating_sub(file.size);
    }

    Ok(())
}

fn read_cache_metadata(path: &Path) -> Result<CachedFetchMetadata> {
    let file = open_cache_file_for_read(path).context("open cache file for metadata")?;
    serde_json::from_reader(BufReader::new(file)).context("read cache metadata")
}

fn is_direct_cache_entry_path(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root) && path.file_name().is_some_and(is_cache_entry_name)
}

fn is_cache_entry_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };

    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    struct TestCacheDir {
        root: PathBuf,
    }

    impl TestCacheDir {
        fn new(name: &str) -> Self {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let base = manifest_dir
                .parent()
                .unwrap_or(&manifest_dir)
                .join("target")
                .join("webfetch-cache-tests");
            let root = base.join(format!("{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).expect("create isolated cache test root");
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn path_for(&self, key: &str) -> PathBuf {
            cache_path_in_root(&self.root, key)
        }
    }

    impl Drop for TestCacheDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
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
        let cache = TestCacheDir::new("cache-roundtrip");
        let fetched_at = Utc::now();
        let key = format!("test://cache-roundtrip-{}_http", uuid::Uuid::new_v4());
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: b"<html><body>ok</body></html>".to_vec(),
            fetched_at,
        };

        write_cache_to_root(cache.root(), &key, &entry, CachePolicy::defaults())
            .expect("write cache");
        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("read cache")
            .expect("expected cache hit");
        assert_eq!(loaded.content_type, entry.content_type);
        assert_eq!(loaded.body, entry.body);
    }

    #[test]
    fn read_cache_treats_corrupt_entry_as_miss_and_removes_file() {
        let cache = TestCacheDir::new("cache-corrupt");
        let key = format!("test://cache-corrupt-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);

        fs::write(&path, b"{not-json").expect("write corrupt cache");

        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("read cache should not fail on corruption");
        assert!(loaded.is_none(), "corrupt cache should be treated as miss");
        assert!(
            !path.exists(),
            "corrupt cache file should be removed to avoid repeated failures"
        );
    }

    // Corrupt entries are treated as misses so WebFetch can recover by refetching.
    #[test]
    fn read_cache_silently_swallows_corruption_error() {
        let cache = TestCacheDir::new("cache-silent-corrupt");
        let key = format!("test://cache-silent-corrupt-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);

        fs::write(&path, b"{not-json").expect("write corrupt cache");

        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("should not error");

        assert!(loaded.is_none());
        assert!(!path.exists());
    }

    // The cache directory may be removed by external cleanup between path calculation and write.
    #[test]
    fn write_cache_creates_parent_directory_if_missing() {
        let cache = TestCacheDir::new("cache-dir-check");
        let key = format!("test://cache-dir-check-{}_http", uuid::Uuid::new_v4());
        let root = cache.root().to_path_buf();
        let path = cache_path_in_root(&root, &key);

        // Ensure directory exists, then remove it to test recreation.
        let _ = fs::remove_dir_all(&root);

        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: b"test".to_vec(),
            fetched_at: Utc::now(),
        };

        // This should succeed even if the directory was removed.
        let result = write_cache_to_root(&root, &key, &entry, CachePolicy::defaults());
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
        let cache = TestCacheDir::new("cache-size-limit");
        let unique_id = uuid::Uuid::new_v4();
        let key = format!("test://cache-size-limit-{}_http", unique_id);

        // Create an entry exceeding the limit (26 MB).
        let large_body = vec![0u8; 26 * 1024 * 1024];
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: large_body,
            fetched_at: Utc::now(),
        };

        let result = write_cache_to_root(cache.root(), &key, &entry, CachePolicy::defaults());
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

    #[test]
    fn read_cache_treats_expired_entry_as_miss_and_removes_file() {
        let cache = TestCacheDir::new("cache-expired");
        let key = format!("test://cache-expired-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);
        let policy = CachePolicy {
            ttl: Duration::from_secs(60),
            max_total_bytes: DEFAULT_CACHE_MAX_BYTES,
        };
        let entry = CachedFetch {
            content_type: Some("text/html".to_string()),
            body: b"<html><body>expired</body></html>".to_vec(),
            fetched_at: Utc::now() - chrono::Duration::seconds(61),
        };
        let bytes = serde_json::to_vec(&entry).expect("serialize expired cache entry");
        fs::write(&path, bytes).expect("write expired cache entry");

        let loaded = read_cache_from_root(cache.root(), &key, policy)
            .expect("expired cache read should not fail");

        assert!(loaded.is_none(), "expired cache entry should be a miss");
        assert!(
            !path.exists(),
            "expired cache file should be removed after a miss"
        );
    }

    #[test]
    fn read_cache_treats_oversized_entry_as_miss_and_removes_file() {
        let cache = TestCacheDir::new("cache-oversized-read");
        let key = format!("test://cache-oversized-read-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);
        let file = fs::File::create(&path).expect("create oversized cache entry");
        file.set_len(MAX_CACHE_ENTRY_BYTES as u64 + 1)
            .expect("size oversized cache entry");
        drop(file);

        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("oversized cache read should not fail");

        assert!(loaded.is_none(), "oversized cache entry should be a miss");
        assert!(
            !path.exists(),
            "oversized cache file should be removed after a miss"
        );
    }

    #[test]
    fn write_cache_prunes_old_entries_to_total_quota() {
        let cache = TestCacheDir::new("cache-quota");
        let base = Utc.timestamp_opt(Utc::now().timestamp() - 30, 0).unwrap();
        let old_key = format!("test://cache-quota-old-{}_http", uuid::Uuid::new_v4());
        let middle_key = format!("test://cache-quota-middle-{}_http", uuid::Uuid::new_v4());
        let newest_key = format!("test://cache-quota-newest-{}_http", uuid::Uuid::new_v4());
        let old = cache_entry_at(base);
        let middle = cache_entry_at(base + chrono::Duration::seconds(10));
        let newest = cache_entry_at(base + chrono::Duration::seconds(20));
        let policy = CachePolicy {
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            max_total_bytes: (serialized_len(&middle) + serialized_len(&newest)) as u64,
        };

        write_cache_to_root(cache.root(), &old_key, &old, policy).expect("write old cache entry");
        write_cache_to_root(cache.root(), &middle_key, &middle, policy)
            .expect("write middle cache entry");
        write_cache_to_root(cache.root(), &newest_key, &newest, policy)
            .expect("write newest cache entry and prune");

        assert!(
            !cache.path_for(&old_key).exists(),
            "oldest cache entry should be pruned first"
        );
        assert!(
            cache.path_for(&middle_key).exists(),
            "newer cache entry should be retained"
        );
        assert!(
            cache.path_for(&newest_key).exists(),
            "newest cache entry should be retained"
        );
    }

    #[test]
    fn write_cache_amortizes_pruning_until_write_cadence() {
        let cache = TestCacheDir::new("cache-prune-cadence");
        let expired_key = format!(
            "test://cache-prune-cadence-expired-{}_http",
            uuid::Uuid::new_v4()
        );
        let expired_path = cache.path_for(&expired_key);
        let policy = CachePolicy {
            ttl: Duration::from_secs(60),
            max_total_bytes: DEFAULT_CACHE_MAX_BYTES,
        };
        let expired = cache_entry_at(Utc::now() - chrono::Duration::seconds(61));
        let expired_bytes = serde_json::to_vec(&expired).expect("serialize expired cache entry");
        fs::write(&expired_path, expired_bytes).expect("write expired cache entry");

        for write_index in 0..(CACHE_PRUNE_WRITE_INTERVAL - 1) {
            let key = format!(
                "test://cache-prune-cadence-{}-{}_http",
                write_index,
                uuid::Uuid::new_v4()
            );
            write_cache_to_root(cache.root(), &key, &cache_entry_at(Utc::now()), policy)
                .expect("write cache entry before prune cadence");
        }

        assert!(
            expired_path.exists(),
            "expired entry should remain until amortized prune cadence is reached"
        );

        let cadence_key = format!(
            "test://cache-prune-cadence-final-{}_http",
            uuid::Uuid::new_v4()
        );
        write_cache_to_root(
            cache.root(),
            &cadence_key,
            &cache_entry_at(Utc::now()),
            policy,
        )
        .expect("write cache entry at prune cadence");

        assert!(
            !expired_path.exists(),
            "expired entry should be removed when prune cadence is reached"
        );
    }

    #[test]
    fn prune_cache_removes_oversized_entries_before_metadata_parse() {
        let cache = TestCacheDir::new("cache-oversized-prune");
        let key = format!("test://cache-oversized-prune-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);
        let file = fs::File::create(&path).expect("create oversized cache entry");
        file.set_len(MAX_CACHE_ENTRY_BYTES as u64 + 1)
            .expect("size oversized cache entry");
        drop(file);

        prune_cache_root(cache.root(), CachePolicy::defaults()).expect("prune cache");

        assert!(
            !path.exists(),
            "oversized cache entry should be pruned without parsing JSON metadata"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_cache_restricts_unix_cache_permissions() {
        let cache = TestCacheDir::new("cache-permissions");
        let key = format!("test://cache-permissions-{}_http", uuid::Uuid::new_v4());
        let entry = cache_entry_at(Utc::now());

        write_cache_to_root(cache.root(), &key, &entry, CachePolicy::defaults())
            .expect("write cache entry");

        let dir_mode = fs::metadata(cache.root())
            .expect("cache root metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(cache.path_for(&key))
            .expect("cache file metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(dir_mode, 0o700, "cache directory should be owner-only");
        assert_eq!(file_mode, 0o600, "cache file should be owner-only");
    }

    #[cfg(unix)]
    #[test]
    fn write_cache_rejects_symlinked_cache_root() {
        use std::os::unix::fs as unix_fs;

        let target = TestCacheDir::new("cache-root-target");
        let link = target
            .root()
            .with_file_name(format!("cache-root-link-{}", uuid::Uuid::new_v4()));
        unix_fs::symlink(target.root(), &link).expect("create cache root symlink");
        let entry = cache_entry_at(Utc::now());

        let err = write_cache_to_root(
            &link,
            &format!("test://cache-root-link-{}_http", uuid::Uuid::new_v4()),
            &entry,
            CachePolicy::defaults(),
        )
        .expect_err("symlinked cache root must be rejected");

        assert!(
            err.to_string()
                .contains("symlinked webfetch cache directory"),
            "expected symlink cache root rejection, got: {err:#}"
        );
        let _ = fs::remove_file(link);
    }

    #[cfg(unix)]
    #[test]
    fn read_cache_ignores_and_removes_symlinked_cache_entry() {
        use std::os::unix::fs as unix_fs;

        let cache = TestCacheDir::new("cache-entry-symlink");
        let key = format!("test://cache-entry-symlink-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);
        let target = cache.root().join("not-a-cache-entry");
        fs::write(&target, b"not cache json").expect("write symlink target");
        unix_fs::symlink(&target, &path).expect("create cache entry symlink");

        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("symlinked cache entry should be treated as cache miss");

        assert!(loaded.is_none());
        assert!(
            !path.exists(),
            "symlinked cache entry should be removed after being rejected"
        );
        assert!(
            target.exists(),
            "rejecting a symlinked cache entry must not remove its target"
        );
    }

    #[test]
    fn read_cache_treats_non_file_cache_entry_as_miss() {
        let cache = TestCacheDir::new("cache-entry-non-file");
        let key = format!("test://cache-entry-non-file-{}_http", uuid::Uuid::new_v4());
        let path = cache.path_for(&key);
        fs::create_dir(&path).expect("create non-file cache entry");

        let loaded = read_cache_from_root(cache.root(), &key, CachePolicy::defaults())
            .expect("non-file cache entry should be treated as cache miss");

        assert!(loaded.is_none());
        assert!(
            path.is_dir(),
            "non-file cache entry should not be read as cache data"
        );
    }

    fn cache_entry_at(fetched_at: DateTime<Utc>) -> CachedFetch {
        CachedFetch {
            content_type: Some("text/html".to_string()),
            body: vec![b'x'; 256],
            fetched_at,
        }
    }

    fn serialized_len(entry: &CachedFetch) -> usize {
        serde_json::to_vec(entry)
            .expect("serialize cache entry")
            .len()
    }
}
