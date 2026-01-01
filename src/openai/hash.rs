//! SHA256 hashing utilities for file content and binary detection.
//!
//! This module provides functions for computing content hashes used in
//! change detection and reindexing workflows.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Computes the SHA256 hash of a file's contents.
///
/// Reads the file in chunks to handle large files efficiently without loading
/// the entire file into memory.
///
/// # Arguments
///
/// * `path` - Path to the file to hash
///
/// # Returns
///
/// The SHA256 hash as a lowercase hexadecimal string (64 characters).
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be opened (does not exist, permission denied)
/// - An I/O error occurs while reading
///
/// # Example
///
/// ```ignore
/// let hash = compute_file_hash("src/main.rs").await?;
/// println!("Hash: {}", hash);  // e.g., "a1b2c3d4..."
/// ```
///
/// # Performance
///
/// Uses 8KB chunks for reading, providing a good balance between memory usage
/// and system call overhead.
pub async fn compute_file_hash(path: &str) -> Result<String> {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    let mut file = File::open(path)
        .await
        .with_context(|| format!("Failed to open file: {}", path))?;

    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 8192];

    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Detects if a file appears to contain binary content.
///
/// Reads the first 8KB of the file and checks for NUL bytes, which are a strong
/// indicator of binary data. This is used by [`crate::code_query`] to skip binary files
/// that would produce noise in semantic code search.
///
/// # Arguments
///
/// * `path` - Path to the file to check
///
/// # Returns
///
/// `true` if the file contains NUL bytes in its first 8KB, indicating binary content.
///
/// # Design
///
/// Intentionally conservative: errs on the side of classifying files as binary
/// to avoid uploading inappropriate content to CodeQuery.
pub(crate) async fn looks_binary_by_content(path: &str) -> Result<bool> {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    let mut file = File::open(path)
        .await
        .with_context(|| format!("Failed to open file: {}", path))?;

    let mut buf = vec![0u8; 8192];
    let n = file.read(&mut buf).await?;
    Ok(buf[..n].contains(&0))
}

/// Computes the SHA256 hash of a byte slice.
///
/// Synchronous version for in-memory data. For files, prefer [`compute_file_hash`]
/// which reads in chunks.
///
/// # Arguments
///
/// * `bytes` - The byte slice to hash
///
/// # Returns
///
/// The SHA256 hash as a lowercase hexadecimal string (64 characters).
///
/// # Example
///
/// ```
/// use file_search_core::compute_bytes_hash;
///
/// let hash = compute_bytes_hash(b"hello world");
/// assert_eq!(hash.len(), 64);  // SHA256 produces 64 hex chars
/// ```
pub fn compute_bytes_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
