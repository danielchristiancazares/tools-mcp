//! # File Search Core Library
//!
//! This library provides the core functionality for interacting with OpenAI's vector stores API,
//! enabling file uploads, vector store management, and semantic search capabilities.
//!
//! ## Main Components
//!
//! - **ApiConfig**: Configuration for OpenAI API authentication
//! - **File Operations**: Upload files with automatic format validation
//! - **Vector Store Management**: Create, list, and manage vector stores
//! - **Semantic Search**: Query vector stores using OpenAI's Responses API
//! - **Response Processing**: Type-safe deserialization of OpenAI responses
//!
//! ## File Format Support
//!
//! The library automatically validates and converts file formats:
//! - Supported formats: txt, pdf, doc, docx, py, js, json, md, and many more
//! - Unsupported formats are automatically converted to .txt for compatibility
//!
//! ## Example Usage
//!
//! ```no_run
//! use file_search_core::{ApiConfig, upload_file};
//! use reqwest::Client;
//!
//! async fn example() -> anyhow::Result<()> {
//!     let client = Client::new();
//!     let config = ApiConfig::new("your-api-key", "gpt-4");
//!     let file_id = upload_file(&client, &config, "document.pdf").await?;
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, StatusCode, multipart};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use tokio::time::{Duration, sleep};

pub const BASE_URL: &str = "https://api.openai.com/v1";

// OpenAI Responses API data model
#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseObject {
    pub id: String,
    pub object: String,
    pub created_at: i64,
    pub status: String,
    pub model: String,
    pub output: Vec<OutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum OutputItem {
    #[serde(rename = "message")]
    Message(MessageOutput),
    #[serde(rename = "file_search_call")]
    FileSearchCall(FileSearchOutput),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageOutput {
    pub id: String,
    pub role: String,
    pub status: String,
    #[serde(default)]
    pub content: Vec<ContentItem>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FileSearchOutput {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,
}

impl ResponseObject {
    /// Extract the main text content from the response
    pub fn extract_text(&self, include_results: bool) -> String {
        // Find the message output
        let message = self.output.iter().find_map(|item| {
            if let OutputItem::Message(msg) = item {
                Some(msg)
            } else {
                None
            }
        });

        if let Some(msg) = message
            && let Some(content) = msg.content.first()
            && let Some(text) = &content.text
        {
            let mut result = text.clone();

            // Optionally append search results.
            if include_results
                && let Some(file_search) = self.output.iter().find_map(|item| match item {
                    OutputItem::FileSearchCall(fs) => Some(fs),
                    _ => None,
                })
                && let Some(results) = &file_search.results
            {
                result.push_str("\n\n---\n### Search Results:\n");
                for (i, r) in results.iter().take(5).enumerate() {
                    if let (Some(filename), Some(score)) = (
                        r.get("filename").and_then(|v| v.as_str()),
                        r.get("score").and_then(|v| v.as_f64()),
                    ) {
                        result.push_str(&format!(
                            "\n{}. **{}** (score: {:.2})",
                            i + 1,
                            filename,
                            score
                        ));
                        if let Some(text_snippet) = r.get("text").and_then(|v| v.as_str()) {
                            let preview = text_snippet
                                .lines()
                                .take(2)
                                .collect::<Vec<_>>()
                                .join("\n   ");
                            if !preview.is_empty() {
                                result.push_str(&format!("\n   {}", preview.trim()));
                            }
                        }
                    }
                }
            }

            return result;
        }

        // Fallback to empty string if no text found
        String::from("No response text found")
    }
}

#[derive(Clone)]
pub struct ApiConfig {
    pub api_key: String,
    pub default_model: String,
}

impl ApiConfig {
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            default_model: default_model.into(),
        }
    }
}

pub fn is_allowed_upload_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "c" | "cpp"
            | "css"
            | "csv"
            | "doc"
            | "docx"
            | "gif"
            | "go"
            | "html"
            | "java"
            | "jpeg"
            | "jpg"
            | "js"
            | "json"
            | "md"
            | "pdf"
            | "php"
            | "pkl"
            | "png"
            | "pptx"
            | "py"
            | "rb"
            | "tar"
            | "tex"
            | "ts"
            | "txt"
            | "webp"
            | "xlsx"
            | "xml"
            | "zip"
    )
}

/// Returns true if a file extension is considered "binary/non-text" for CodeQuery indexing.
///
/// CodeQuery is optimized for code/config/text search. Even if OpenAI's Files API accepts a
/// broader set of formats, uploading binary blobs (especially images/media/archives) is usually
/// noise for semantic code search and can waste tokens/bytes.
pub fn is_codequery_binary_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        // Images.
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg" | "heic"
            | "avif"
            // Audio/video.
            | "mp3" | "wav" | "flac" | "m4a" | "ogg" | "mp4" | "mov" | "mkv" | "webm"
            // Archives/bundles.
            | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar"
            // Common executables/artifacts.
            | "exe" | "dll" | "so" | "dylib" | "a" | "lib" | "o" | "obj" | "class" | "jar"
            | "wasm"
            // Common binary data formats that are unlikely to be helpful for code search.
            | "pkl" | "db" | "sqlite" | "sqlite3"
            // Office/PDF docs are typically binary; keep CodeQuery focused on text sources by default.
            | "pdf" | "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx"
    )
}

pub fn is_codequery_indexable_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        // Only actual source-code files. Keep this conservative to avoid indexing config/docs.
        "rs" | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "swift"
            | "py"
            | "rb"
            | "php"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
    )
}

pub fn is_codequery_indexable_filename(_file_name: &str) -> bool {
    // Intentionally empty: CodeQuery should only index explicit code/config-by-extension files.
    // (No special-casing for extensionless files.)
    false
}

pub fn is_codequery_indexable_path(path: &std::path::Path) -> bool {
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(name) => name,
        None => return false,
    };

    // Keep CodeQuery focused on code/config; skip dotfiles by default.
    if file_name.starts_with('.') {
        return false;
    }

    if is_codequery_indexable_filename(file_name) {
        return true;
    }

    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => ext,
        None => return false,
    };

    // Keep Markdown/docs out of the code index.
    if ext.eq_ignore_ascii_case("md") {
        return false;
    }

    // Hard block binary/media formats.
    if is_codequery_binary_ext(ext) {
        return false;
    }

    is_codequery_indexable_ext(ext)
}

/// Computes the appropriate filename for upload to OpenAI.
///
/// Returns the original filename if it has an allowed extension,
/// otherwise appends .txt to make it compatible with OpenAI's API.
///
/// # Performance
///
/// Uses `Cow<str>` to avoid allocations when the filename doesn't need modification.
pub fn compute_upload_filename(original_filename: &str) -> Cow<'_, str> {
    let p = std::path::Path::new(original_filename);
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");

    if !ext.is_empty() && is_allowed_upload_ext(ext) {
        // No allocation needed - return borrowed string
        Cow::Borrowed(original_filename)
    } else {
        // Need to allocate for the modified filename
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(original_filename);

        if stem == original_filename {
            Cow::Owned(format!("{}.txt", original_filename))
        } else {
            Cow::Owned(format!("{}.txt", stem))
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct FileObj {
    pub id: String,
}

#[derive(Deserialize, Debug)]
pub struct VectorStore {
    pub id: String,
}

#[derive(Serialize, Debug)]
struct VectorStoreCreate {
    name: String,
}

#[derive(Serialize, Debug)]
struct VectorStoreFileCreate {
    file_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    attributes: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunking_strategy: Option<serde_json::Value>,
}

/// Vector store with file_counts for efficient indexing status checks
#[derive(Deserialize, Debug)]
pub struct VectorStoreDetails {
    pub id: String,
    #[serde(default)]
    pub file_counts: FileCounts,
}

#[derive(Deserialize, Debug, Default)]
pub struct FileCounts {
    #[serde(default)]
    pub in_progress: u64,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub cancelled: u64,
    #[serde(default)]
    pub total: u64,
}

#[derive(Deserialize, Debug)]
pub struct VectorStoreFilesList {
    pub data: Vec<VectorStoreFileItem>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub last_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct VectorStoreFileItem {
    pub id: String,
    pub status: String,
    // Some API variants return a nested file object
    #[serde(default)]
    pub file: Option<FileInfo>,
    // Others return a file_id; accept both to be robust
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
pub struct FileInfo {
    pub id: String,
    pub filename: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Tunable settings for a CodeQuery invocation.
pub struct CodeQueryOptions<'a> {
    pub concurrent_limit: usize,
    pub timeout_ms: u64,
    pub model: Option<&'a str>,
    pub max_num_results: Option<u32>,
    pub include_results: bool,
}

/// Computes SHA256 hash of file contents
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

async fn looks_binary_by_content(path: &str) -> Result<bool> {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    // Read a small prefix and look for NUL bytes, which is a strong signal of binary data.
    // This intentionally errs on the side of skipping to avoid uploading blobs into CodeQuery.
    let mut file = File::open(path)
        .await
        .with_context(|| format!("Failed to open file: {}", path))?;

    let mut buf = vec![0u8; 8192];
    let n = file.read(&mut buf).await?;
    Ok(buf[..n].contains(&0))
}

/// Computes SHA256 hash of bytes
pub fn compute_bytes_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Uploads a file to OpenAI's file storage system.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `path_or_url` - Either a local file path or a URL (http:// or https://)
///
/// # Returns
///
/// The file ID assigned by OpenAI
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read or downloaded
/// - The API request fails
/// - The response cannot be parsed
pub async fn upload_file(client: &Client, cfg: &ApiConfig, path_or_url: &str) -> Result<String> {
    let url = format!("{}/files", BASE_URL);
    let form = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        let bytes = client
            .get(path_or_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        // Use rsplit().next() to avoid iterating the entire iterator; satisfies clippy
        let name = path_or_url.rsplit('/').next().unwrap_or("file");
        let eff = compute_upload_filename(name);
        let part = multipart::Part::bytes(bytes.to_vec()).file_name(eff.into_owned());
        multipart::Form::new()
            .part("file", part)
            .text("purpose", "assistants")
    } else {
        let bytes = tokio::fs::read(path_or_url)
            .await
            .with_context(|| format!("opening {}", path_or_url))?;
        let name = std::path::Path::new(path_or_url)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let eff = compute_upload_filename(&name);
        let part = multipart::Part::bytes(bytes).file_name(eff.into_owned());
        multipart::Form::new()
            .part("file", part)
            .text("purpose", "assistants")
    };
    let res = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;
    let obj: FileObj = res.json().await?;
    Ok(obj.id)
}

/// Uploads multiple files to a vector store in batch.
///
/// This function handles batch uploading of files with progress tracking and error recovery.
/// It processes files concurrently for better performance while respecting rate limits.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `file_paths` - List of file paths or URLs to upload
/// * `vector_store_id` - The vector store ID to upload files to
/// * `concurrent_limit` - Maximum number of concurrent uploads (recommended: 5-10)
///
/// # Returns
///
/// A tuple containing:
/// - Vector of successfully uploaded file IDs with their paths
/// - Vector of failed uploads with error messages
///
/// # Example
///
/// ```ignore
/// let (successes, failures) = upload_files_batch(
///     &client,
///     &config,
///     vec!["file1.txt".to_string(), "file2.pdf".to_string()],
///     "vs_abc123",
///     5
/// ).await?;
/// ```
pub async fn upload_files_batch(
    client: &Client,
    cfg: &ApiConfig,
    file_paths: Vec<String>,
    vector_store_id: &str,
    concurrent_limit: usize,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    use futures::stream::{self, StreamExt};

    let mut successes = Vec::new();
    let mut failures = Vec::new();

    // Process files in chunks to avoid overwhelming the API
    let chunks: Vec<_> = file_paths
        .chunks(concurrent_limit)
        .map(|c| c.to_vec())
        .collect();

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        tracing::info!("Processing chunk {}/{}", chunk_idx + 1, chunks.len());

        let results: Vec<_> = stream::iter(chunk.iter())
            .map(|path| async move {
                let path = path.clone();

                // First upload the file
                let file_id = match upload_file(client, cfg, &path).await {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::error!("Failed to upload {}: {}", path, e);
                        return Err((path, format!("Upload failed: {}", e)));
                    }
                };

                // Then attach it to the vector store
                match add_file_to_vector_store(client, cfg, vector_store_id, &file_id).await {
                    Ok(_) => {
                        // Wait for file to be processed
                        if let Err(e) =
                            wait_for_vector_file_ready(client, cfg, vector_store_id, 1000, 30000)
                                .await
                        {
                            tracing::warn!(
                                "File {} uploaded but processing incomplete: {}",
                                path,
                                e
                            );
                        }
                        tracing::info!("Successfully uploaded and attached: {}", path);
                        Ok((path, file_id))
                    }
                    Err(e) => {
                        tracing::error!("Failed to attach {} to store: {}", path, e);
                        Err((path, format!("Attach failed: {}", e)))
                    }
                }
            })
            .buffer_unordered(concurrent_limit)
            .collect()
            .await;

        for result in results {
            match result {
                Ok((path, file_id)) => successes.push((path, file_id)),
                Err((path, error)) => failures.push((path, error)),
            }
        }

        // Small delay between chunks to avoid rate limiting
        if chunk_idx < chunks.len() - 1 {
            sleep(Duration::from_millis(1000)).await;
        }
    }

    Ok((successes, failures))
}

/// Creates a new vector store with the specified name.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `name` - The name for the new vector store
///
/// # Returns
///
/// The vector store ID of the newly created store
///
/// # Errors
///
/// Returns an error if the API request fails or the response cannot be parsed.
pub async fn create_vector_store(client: &Client, cfg: &ApiConfig, name: &str) -> Result<String> {
    let url = format!("{}/vector_stores", BASE_URL);
    let res = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&VectorStoreCreate {
            name: name.to_string(),
        })
        .send()
        .await
        .with_context(|| "send create_vector_store")?;
    let res = if let Err(_err) = res.error_for_status_ref() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("create_vector_store: HTTP {} {}", status.as_u16(), body);
    } else {
        res
    };
    let vs: VectorStore = res.json().await?;
    Ok(vs.id)
}

#[derive(Deserialize, Debug)]
pub struct VectorStoreList {
    pub data: Vec<VectorStoreEntry>,
}

#[derive(Deserialize, Debug)]
pub struct VectorStoreEntry {
    pub id: String,
    pub name: Option<String>,
}

pub async fn list_vector_stores(client: &Client, cfg: &ApiConfig) -> Result<Vec<VectorStoreEntry>> {
    let url = format!("{}/vector_stores", BASE_URL);
    let res = client
        .get(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await
        .with_context(|| "send list_vector_stores")?;
    let res = if let Err(_err) = res.error_for_status_ref() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("list_vector_stores: HTTP {} {}", status.as_u16(), body);
    } else {
        res
    };
    let list: VectorStoreList = res.json().await?;
    Ok(list.data)
}

/// Fetches vector store details including file_counts for efficient status polling.
pub async fn get_vector_store_details(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<VectorStoreDetails> {
    let url = format!("{}/vector_stores/{}", BASE_URL, vs_id);
    let res = client
        .get(&url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await?
        .error_for_status()?;
    let details: VectorStoreDetails = res.json().await?;
    Ok(details)
}

/// Waits for all files in a vector store to finish processing using file_counts polling.
///
/// This is more efficient than polling individual files - it makes a single API call
/// to check the aggregate counts instead of listing all files.
///
/// Returns early with an error if any files are in a terminal failure state.
pub async fn wait_for_vector_store_ready(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    poll_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let details = get_vector_store_details(client, cfg, vs_id).await?;
        let counts = &details.file_counts;

        // Fail fast on terminal failure states
        if counts.failed > 0 || counts.cancelled > 0 {
            anyhow::bail!(
                "vector store has {} failed and {} cancelled files",
                counts.failed,
                counts.cancelled
            );
        }

        // Check if all files are completed (no in_progress files)
        if counts.in_progress == 0 && counts.total > 0 {
            tracing::debug!("Vector store ready: {} files completed", counts.completed);
            return Ok(());
        }

        // Also handle case where store is empty (total == 0)
        if counts.total == 0 {
            tracing::debug!("Vector store is empty, returning early");
            return Ok(());
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            anyhow::bail!(
                "timeout waiting for indexing: {}/{} files still in progress",
                counts.in_progress,
                counts.total
            );
        }

        tracing::debug!(
            "Waiting for indexing: {}/{} in progress",
            counts.in_progress,
            counts.total
        );
        sleep(Duration::from_millis(poll_ms)).await;
    }
}

pub async fn add_file_to_vector_store(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
) -> Result<()> {
    let url = format!("{}/vector_stores/{}/files", BASE_URL, vs_id);
    client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&VectorStoreFileCreate {
            file_id: file_id.to_string(),
            attributes: None,
            chunking_strategy: None,
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn add_file_to_vector_store_with(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
    attributes: Option<serde_json::Map<String, serde_json::Value>>,
    chunking_strategy: Option<serde_json::Value>,
) -> Result<()> {
    let url = format!("{}/vector_stores/{}/files", BASE_URL, vs_id);
    client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&VectorStoreFileCreate {
            file_id: file_id.to_string(),
            attributes,
            chunking_strategy,
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn wait_for_vector_file_ready(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    poll_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    let url = format!("{}/vector_stores/{}/files", BASE_URL, vs_id);
    let start = std::time::Instant::now();
    loop {
        let res = client
            .get(&url)
            .bearer_auth(&cfg.api_key)
            .header("OpenAI-Beta", "assistants=v2")
            .send()
            .await?
            .error_for_status()?;
        let list: VectorStoreFilesList = res.json().await?;

        if !list.data.is_empty() {
            // Fail fast on terminal non-success states
            if let Some(failed) = list
                .data
                .iter()
                .find(|f| f.status == "failed" || f.status == "cancelled")
            {
                anyhow::bail!(
                    "vector store file {} is in terminal status '{}'",
                    failed.id,
                    failed.status
                );
            }
            if list.data.iter().all(|f| f.status == "completed") {
                break;
            }
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            anyhow::bail!("timeout waiting for indexing");
        }
        sleep(Duration::from_millis(poll_ms)).await;
    }
    Ok(())
}

/// Lists all files in a vector store with automatic pagination.
///
/// Fetches all pages of results and returns them as a single list.
pub async fn list_vector_store_files(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<VectorStoreFilesList> {
    let base_url = format!("{}/vector_stores/{}/files", BASE_URL, vs_id);
    let mut all_files = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let mut url = base_url.clone();
        if let Some(ref cursor) = after {
            url = format!("{}?after={}", url, cursor);
        }

        let res = client
            .get(&url)
            .bearer_auth(&cfg.api_key)
            .header("OpenAI-Beta", "assistants=v2")
            .send()
            .await?
            .error_for_status()?;

        let page: VectorStoreFilesList = res.json().await?;
        let has_more = page.has_more;

        // Get last item ID for next page cursor
        after = page.data.last().map(|f| f.id.clone());
        all_files.extend(page.data);

        if !has_more || after.is_none() {
            break;
        }
    }

    Ok(VectorStoreFilesList {
        data: all_files,
        has_more: false,
        last_id: None,
    })
}

pub async fn delete_vector_store_file(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
) -> Result<()> {
    let url = format!("{}/vector_stores/{}/files/{}", BASE_URL, vs_id, file_id);
    client
        .delete(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn get_file(client: &Client, cfg: &ApiConfig, file_id: &str) -> Result<FileInfo> {
    let url = format!("{}/files/{}", BASE_URL, file_id);
    let res = client
        .get(url)
        .bearer_auth(&cfg.api_key)
        .send()
        .await?
        .error_for_status()?;
    let info: FileInfo = res.json().await?;
    Ok(info)
}

pub async fn list_vector_store_files_with_details(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<Vec<FileInfo>> {
    let files_list = list_vector_store_files(client, cfg, vs_id).await?;
    let mut detailed_files = Vec::new();

    for item in files_list.data {
        // Get the actual file ID (not the relationship ID)
        let file_id = item
            .file_id
            .as_ref()
            .or_else(|| item.file.as_ref().map(|f| &f.id));

        if let Some(fid) = file_id {
            // Try to get full file details
            match get_file(client, cfg, fid).await {
                Ok(file_info) => detailed_files.push(file_info),
                Err(_) => {
                    // Fallback to basic info if we can't get details
                    detailed_files.push(FileInfo {
                        id: fid.clone(),
                        filename: item.file.as_ref().and_then(|f| f.filename.clone()),
                        purpose: None,
                        bytes: None,
                        created_at: None,
                        attributes: None,
                    });
                }
            }
        }
    }

    Ok(detailed_files)
}

#[derive(Serialize, Debug)]
struct ResponsesCreate<'a> {
    model: &'a str,
    input: &'a str,
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<Vec<&'a str>>,
}

pub async fn responses_with_file_search(
    client: &Client,
    cfg: &ApiConfig,
    model: &str,
    query: &str,
    vector_store_id: &str,
    max_num_results: Option<u32>,
    include_results: bool,
) -> Result<serde_json::Value> {
    let url = format!("{}/responses", BASE_URL);
    let mut tool =
        serde_json::json!({ "type": "file_search", "vector_store_ids": [vector_store_id] });
    if let Some(n) = max_num_results {
        tool["max_num_results"] = serde_json::json!(n);
    }
    let include = if include_results {
        Some(vec!["file_search_call.results"])
    } else {
        None
    };
    let body = ResponsesCreate {
        model,
        input: query,
        tools: vec![tool],
        include,
    };
    let res = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let value: serde_json::Value = res.json().await?;
    Ok(value)
}

pub async fn file_search_run(
    client: &Client,
    cfg: &ApiConfig,
    file_path_or_url: &str,
    query: &str,
    model: Option<&str>,
    max_num_results: Option<u32>,
    include_results: bool,
) -> Result<serde_json::Value> {
    let file_id = upload_file(client, cfg, file_path_or_url).await?;
    let vs_id = create_vector_store(client, cfg, "knowledge_base").await?;
    add_file_to_vector_store(client, cfg, &vs_id, &file_id).await?;
    wait_for_vector_file_ready(client, cfg, &vs_id, 1000, 60_000).await?;
    let resp = responses_with_file_search(
        client,
        cfg,
        model.unwrap_or(&cfg.default_model),
        query,
        &vs_id,
        max_num_results,
        include_results,
    )
    .await?;
    Ok(serde_json::json!({"file_id": file_id, "vector_store_id": vs_id, "response": resp}))
}

/// Reindexes files in a vector store based on file hashes
///
/// This function:
/// 1. Lists all files currently in the vector store
/// 2. Computes hashes for local files
/// 3. Compares hashes to detect changes
/// 4. Deletes old versions of changed files
/// 5. Uploads changed/new files
/// 6. Deletes removed files
///
/// # Arguments
///
/// * `client` - HTTP client
/// * `cfg` - API configuration
/// * `vector_store_id` - Target vector store
/// * `file_paths` - Local files to sync
/// * `concurrent_limit` - Max concurrent operations
/// * `skip_per_file_wait` - Skip per-file indexing waits when true
///
/// # Returns
///
/// Summary of operations performed
pub async fn reindex_files(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    concurrent_limit: usize,
    skip_per_file_wait: bool,
) -> Result<serde_json::Value> {
    use futures::stream::{self, StreamExt};

    // Step 1: Get current files in vector store with their hashes
    let store_files_list = list_vector_store_files(client, cfg, vector_store_id).await?;

    // Build maps for path-based, hash-based, and filename-based lookups
    // path_map: path -> (file_id, hash) - for files with path attribute
    // hash_map: hash -> (path_or_filename, file_id) - for detecting moved files
    // filename_map: filename -> (file_id, hash) - fallback for legacy files without path attribute
    let mut path_map: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut hash_map: HashMap<String, (String, String)> = HashMap::new();
    let mut filename_map: HashMap<String, (String, Option<String>)> = HashMap::new();

    for file in store_files_list.data {
        // Extract path from attributes (full path)
        let path_attr = file
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("path"))
            .and_then(|p| p.as_str())
            .map(String::from);

        // Extract filename
        let filename = file
            .filename
            .clone()
            .or_else(|| file.file.as_ref().and_then(|f| f.filename.clone()));

        // Extract hash from attributes
        let hash = file
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("hash"))
            .and_then(|h| h.as_str())
            .map(String::from);

        // Primary key is path attribute if present
        if let Some(ref p) = path_attr {
            path_map.insert(p.clone(), (file.id.clone(), hash.clone()));
        }

        // Also track by filename for legacy file matching
        if let Some(ref fname) = filename {
            filename_map.insert(fname.clone(), (file.id.clone(), hash.clone()));
        }

        // Track by hash for detecting moved files
        let key = path_attr.or(filename);
        if let (Some(h), Some(k)) = (hash, key) {
            hash_map.insert(h, (k, file.id));
        }
    }

    // Step 2: Process local files
    let mut to_upload = Vec::new();
    let mut to_skip = Vec::new();
    let mut to_delete: HashMap<String, String> = HashMap::new(); // file_id -> reason
    let mut errors = Vec::new();

    // Check each local file
    for path in file_paths {
        // Compute hash of local file
        let local_hash = match compute_file_hash(path).await {
            Ok(h) => h,
            Err(e) => {
                errors.push((path.clone(), format!("Failed to hash: {}", e)));
                continue;
            }
        };

        // Extract filename for fallback matching
        let filename = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);

        // Check by path first (highest priority - exact match)
        if let Some((file_id, store_hash)) = path_map.get(path).cloned() {
            if store_hash.as_ref() == Some(&local_hash) {
                // Path and hash both match - skip upload
                to_skip.push(path.clone());
                path_map.remove(path);
                hash_map.remove(&local_hash);
                if let Some(ref fname) = filename {
                    filename_map.remove(fname);
                }
            } else {
                // Same path, different hash - content changed
                to_delete.insert(file_id.clone(), format!("content changed: {}", path));
                to_upload.push((path.clone(), local_hash.clone()));
                path_map.remove(path);
                if let Some(old_hash) = store_hash {
                    hash_map.remove(&old_hash);
                }
                if let Some(ref fname) = filename {
                    filename_map.remove(fname);
                }
            }
        } else if let Some((old_key, file_id)) = hash_map.get(&local_hash).cloned() {
            // Same hash at different location - file was moved
            to_delete.insert(
                file_id.clone(),
                format!("moved from {} to {}", old_key, path),
            );
            to_upload.push((path.clone(), local_hash.clone()));
            path_map.remove(&old_key);
            hash_map.remove(&local_hash);
            if let Some(ref fname) = filename {
                filename_map.remove(fname);
            }
        } else if let Some(ref fname) = filename {
            // Check by filename as fallback for legacy files
            if let Some((file_id, store_hash)) = filename_map.get(fname).cloned() {
                if store_hash.as_ref() == Some(&local_hash) {
                    // Filename and hash match - skip (legacy file still current)
                    to_skip.push(path.clone());
                } else {
                    // Filename matches but hash differs - content changed
                    to_delete.insert(
                        file_id.clone(),
                        format!("content changed (legacy): {}", fname),
                    );
                    to_upload.push((path.clone(), local_hash.clone()));
                }
                filename_map.remove(fname);
            } else {
                // Completely new file
                to_upload.push((path.clone(), local_hash));
            }
        } else {
            // Completely new file
            to_upload.push((path.clone(), local_hash));
        }
    }

    // Step 3: Delete old versions of changed/moved files
    for (file_id, reason) in &to_delete {
        tracing::debug!("Deleting file {}: {}", file_id, reason);
        if let Err(e) = delete_vector_store_file(client, cfg, vector_store_id, file_id).await {
            tracing::warn!("Failed to delete {}: {}", file_id, e);
        }
    }

    // Step 4: Upload changed/new files with path, hash in attributes
    let mut uploaded = Vec::new();
    let mut upload_errors = Vec::new();

    let chunks: Vec<_> = to_upload
        .chunks(concurrent_limit)
        .map(|c| c.to_vec())
        .collect();
    for chunk in chunks {
        let chunk_len = chunk.len();
        let results: Vec<_> = stream::iter(chunk.into_iter())
            .map(|(path, hash)| async move {
                // Upload file
                let file_id = match upload_file(client, cfg, &path).await {
                    Ok(id) => id,
                    Err(e) => return Err((path.clone(), format!("Upload failed: {}", e))),
                };

                // Create attributes with path, hash, and timestamp for future reindexing
                let mut attributes = serde_json::Map::new();
                attributes.insert("path".to_string(), serde_json::Value::String(path.clone()));
                attributes.insert("hash".to_string(), serde_json::Value::String(hash.clone()));
                attributes.insert(
                    "indexed_at".to_string(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );

                // Attach to vector store with hash
                match add_file_to_vector_store_with(
                    client,
                    cfg,
                    vector_store_id,
                    &file_id,
                    Some(attributes),
                    None,
                )
                .await
                {
                    Ok(_) => {
                        if !skip_per_file_wait
                            && let Err(e) = wait_for_vector_file_ready(
                                client,
                                cfg,
                                vector_store_id,
                                1000,
                                30000,
                            )
                            .await
                        {
                            tracing::warn!(
                                "File {} uploaded but processing incomplete: {}",
                                path,
                                e
                            );
                        }
                        Ok((path, file_id, hash))
                    }
                    Err(e) => Err((path.clone(), format!("Attach failed: {}", e))),
                }
            })
            .buffer_unordered(concurrent_limit)
            .collect()
            .await;

        for result in results {
            match result {
                Ok((path, file_id, hash)) => uploaded.push(serde_json::json!({
                    "path": path,
                    "file_id": file_id,
                    "hash": hash,
                    "action": "uploaded"
                })),
                Err((path, error)) => upload_errors.push((path, error)),
            }
        }

        if chunk_len > 0 {
            sleep(Duration::from_millis(500)).await;
        }
    }

    // Step 5: Delete orphan files that no longer exist locally
    let mut deleted = Vec::new();
    let mut delete_errors = Vec::new();

    // Collect orphan file_ids from both maps, deduplicating by file_id
    let mut orphan_files: HashMap<String, String> = HashMap::new(); // file_id -> path/filename

    // Remaining entries in path_map are files not in file_paths (orphans)
    for (path, (file_id, _)) in path_map {
        orphan_files.insert(file_id, path);
    }

    // Also check filename_map for legacy orphans not in path_map
    for (filename, (file_id, _)) in filename_map {
        orphan_files.entry(file_id).or_insert(filename);
    }

    // Delete all orphan files
    for (file_id, key) in orphan_files {
        match delete_vector_store_file(client, cfg, vector_store_id, &file_id).await {
            Ok(_) => deleted.push(serde_json::json!({
                "path": key,
                "file_id": file_id,
                "action": "deleted"
            })),
            Err(e) => delete_errors.push((key, e.to_string())),
        }
    }

    // Combine all errors
    let all_errors: Vec<_> = errors
        .into_iter()
        .chain(upload_errors)
        .chain(delete_errors)
        .map(|(path, error)| serde_json::json!({"path": path, "error": error}))
        .collect();

    // Total deletions = changed/moved files + orphan files
    let total_deleted = to_delete.len() + deleted.len();

    Ok(serde_json::json!({
        "summary": {
            "total_files": file_paths.len(),
            "uploaded": uploaded.len(),
            "skipped": to_skip.len(),
            "deleted": total_deleted,
            "errors": all_errors.len()
        },
        "uploaded": uploaded,
        "skipped": to_skip,
        "deleted": deleted,
        "errors": all_errors
    }))
}

pub async fn reindex_with_retry(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    concurrent_limit: usize,
    skip_per_file_wait: bool,
) -> Result<serde_json::Value> {
    const MAX_ATTEMPTS: usize = 3;
    const BACKOFF_MS: [u64; MAX_ATTEMPTS] = [200, 500, 1000];

    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..MAX_ATTEMPTS {
        let is_last_attempt = attempt + 1 == MAX_ATTEMPTS;

        match reindex_files(
            client,
            cfg,
            vector_store_id,
            file_paths,
            concurrent_limit,
            skip_per_file_wait,
        )
        .await
        {
            Ok(summary) => {
                let maybe_errors = summary.get("errors").and_then(|v| v.as_array());

                let Some(errors) = maybe_errors else {
                    return Ok(summary);
                };

                if errors.is_empty() {
                    return Ok(summary);
                }

                let mut should_retry = false;
                let mut first_error_message: Option<String> = None;

                for entry in errors {
                    if let Some(msg) = entry.get("error").and_then(|v| v.as_str()) {
                        if first_error_message.is_none() {
                            first_error_message = Some(msg.to_string());
                        }
                        if is_transient_error_message(msg) {
                            should_retry = true;
                        }
                    }
                }

                let first_error =
                    first_error_message.unwrap_or_else(|| "unknown error".to_string());
                let summary_details =
                    serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
                let err = anyhow!(
                    "Reindex completed with {} error(s); sample: {}. Details: {}",
                    errors.len(),
                    first_error,
                    summary_details
                );

                if should_retry && !is_last_attempt {
                    tracing::warn!(
                        attempt = attempt + 1,
                        "Reindex attempt {} returned errors; retrying",
                        attempt + 1
                    );
                    last_error = Some(err);
                } else {
                    return Err(err);
                }
            }
            Err(e) => {
                if !is_transient_error(&e) || is_last_attempt {
                    return Err(e);
                }

                tracing::warn!(
                    attempt = attempt + 1,
                    "Reindex attempt {} failed: {}; retrying",
                    attempt + 1,
                    e
                );
                last_error = Some(e);
            }
        }

        if attempt + 1 < MAX_ATTEMPTS {
            let base_delay = BACKOFF_MS
                .get(attempt)
                .copied()
                .unwrap_or(*BACKOFF_MS.last().unwrap_or(&1000));
            let jitter_ms = 50 * (attempt as u64 + 1);
            sleep(Duration::from_millis(base_delay + jitter_ms)).await;
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("Reindex failed with unknown error")))
}

fn is_transient_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(req_err) = cause.downcast_ref::<reqwest::Error>() {
            if req_err.is_timeout() || req_err.is_connect() {
                return true;
            }
            if let Some(status) = req_err.status() {
                return status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            }
        }
        false
    })
}

fn is_transient_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("temporarily unavailable")
        || lower.contains("try again")
        || message.contains("429")
        || message.contains("500")
        || message.contains("502")
        || message.contains("503")
        || message.contains("504")
}

/// High-level orchestration for reindexing (optional) and querying a vector store.
///
/// When `file_paths` is non-empty the function:
/// 1. Validates paths are local and exist.
/// 2. Reindexes changed files with retry/backoff.
/// 3. Waits once for batch indexing to complete.
/// 4. Issues the semantic search request.
///
/// Returns the extracted response text plus an optional reindex summary.
pub async fn code_query(
    client: &Client,
    cfg: &ApiConfig,
    vector_store_id: &str,
    file_paths: &[String],
    query: &str,
    options: CodeQueryOptions<'_>,
) -> Result<(String, Option<serde_json::Value>)> {
    let CodeQueryOptions {
        concurrent_limit,
        timeout_ms,
        model,
        max_num_results,
        include_results,
    } = options;

    if query.trim().is_empty() {
        return Err(anyhow!("query must not be empty"));
    }

    // Validate that inputs are local file paths before attempting uploads to avoid partial runs.
    for path in file_paths {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Err(anyhow!(
                "remote paths are not supported in CodeQuery: {}",
                path
            ));
        }
    }

    let mut reindex_summary: Option<serde_json::Value> = None;
    if !file_paths.is_empty() {
        // Filter out known-binary extensions and content that looks binary. We do this at the
        // orchestration layer so those paths are treated as "absent locally", which causes any
        // previously-indexed binary blobs to be deleted as orphans.
        let mut filtered_paths: Vec<String> = Vec::new();
        let mut filtered_out: Vec<serde_json::Value> = Vec::new();

        for path in file_paths {
            let meta = tokio::fs::metadata(path)
                .await
                .with_context(|| format!("Failed to access file path: {}", path))?;
            if !meta.is_file() {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "not a regular file"
                }));
                continue;
            }

            if !is_codequery_indexable_path(std::path::Path::new(path)) {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "not an indexable code/config file"
                }));
                continue;
            }

            if looks_binary_by_content(path).await? {
                filtered_out.push(serde_json::json!({
                    "path": path,
                    "reason": "binary content (NUL byte detected)"
                }));
                continue;
            }

            filtered_paths.push(path.clone());
        }

        if filtered_paths.is_empty() {
            return Err(anyhow!(
                "No indexable files provided for CodeQuery after filtering non-code files"
            ));
        }

        let summary = reindex_with_retry(
            client,
            cfg,
            vector_store_id,
            &filtered_paths,
            concurrent_limit,
            true,
        )
        .await
        .map_err(|e| anyhow!("code_query reindex failed: {}", e))?;

        // Attach filter diagnostics for transparency. This is additive to the existing summary.
        let mut summary = summary;
        if let Some(root) = summary.as_object_mut() {
            if !filtered_out.is_empty() {
                root.insert(
                    "filtered_out".to_string(),
                    serde_json::Value::Array(filtered_out),
                );
            }
            if let Some(obj) = root.get_mut("summary").and_then(|v| v.as_object_mut()) {
                obj.insert(
                    "requested_files".to_string(),
                    serde_json::json!(file_paths.len()),
                );
                obj.insert(
                    "indexed_files".to_string(),
                    serde_json::json!(filtered_paths.len()),
                );
            }
        }

        // Use file_counts polling instead of per-file status checks
        wait_for_vector_store_ready(client, cfg, vector_store_id, 1000, timeout_ms).await?;
        reindex_summary = Some(summary);
    }

    let model_to_use = model.unwrap_or(&cfg.default_model);
    let raw_response = responses_with_file_search(
        client,
        cfg,
        model_to_use,
        query,
        vector_store_id,
        max_num_results,
        include_results,
    )
    .await?;

    let response_text = match serde_json::from_value::<ResponseObject>(raw_response.clone()) {
        Ok(parsed) => parsed.extract_text(include_results),
        Err(err) => {
            tracing::warn!("Failed to deserialize response object: {}", err);
            serde_json::to_string(&raw_response)
                .unwrap_or_else(|_| "Failed to decode response".into())
        }
    };

    Ok((response_text, reindex_summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_config_creation() {
        let config = ApiConfig::new("test_key", "gpt-4");
        assert_eq!(config.api_key, "test_key");
        assert_eq!(config.default_model, "gpt-4");
    }

    #[test]
    fn test_allowed_upload_extensions() {
        assert!(is_allowed_upload_ext("txt"));
        assert!(is_allowed_upload_ext("TXT")); // Case insensitive
        assert!(is_allowed_upload_ext("pdf"));
        assert!(is_allowed_upload_ext("py"));
        assert!(is_allowed_upload_ext("js"));
        assert!(!is_allowed_upload_ext("exe"));
        assert!(!is_allowed_upload_ext("bin"));
        assert!(!is_allowed_upload_ext("unknown"));
    }

    #[test]
    fn test_codequery_binary_ext() {
        assert!(is_codequery_binary_ext("png"));
        assert!(is_codequery_binary_ext("JPG"));
        assert!(is_codequery_binary_ext("zip"));
        assert!(is_codequery_binary_ext("exe"));
        assert!(!is_codequery_binary_ext("rs"));
        assert!(!is_codequery_binary_ext("toml"));
    }

    #[test]
    fn test_codequery_indexable_path() {
        assert!(is_codequery_indexable_path(std::path::Path::new(
            "src/main.rs"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "Cargo.toml"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "Makefile"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "Dockerfile"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "Justfile"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(".env")));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "README.md"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "config.yaml"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "config.yml"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "data.json"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "logo.png"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            "archive.zip"
        )));
        assert!(!is_codequery_indexable_path(std::path::Path::new(
            ".gitignore"
        )));
    }

    #[test]
    fn test_compute_upload_filename() {
        // Allowed extensions should remain unchanged (no allocation)
        assert_eq!(compute_upload_filename("test.txt").as_ref(), "test.txt");
        assert_eq!(
            compute_upload_filename("document.pdf").as_ref(),
            "document.pdf"
        );
        assert_eq!(compute_upload_filename("script.py").as_ref(), "script.py");

        // Disallowed extensions get stripped and .txt appended (allocation)
        assert_eq!(compute_upload_filename("test.exe").as_ref(), "test.txt");
        assert_eq!(compute_upload_filename("binary.bin").as_ref(), "binary.txt");
        assert_eq!(
            compute_upload_filename("unknown.xyz").as_ref(),
            "unknown.txt"
        );

        // Files without extensions should get .txt appended
        assert_eq!(compute_upload_filename("README").as_ref(), "README.txt");
        assert_eq!(compute_upload_filename("Makefile").as_ref(), "Makefile.txt");

        // Special case: already has .txt shouldn't double-add
        assert_eq!(compute_upload_filename("file.txt").as_ref(), "file.txt");
    }

    #[test]
    fn test_response_object_extract_text_simple() {
        let response = ResponseObject {
            id: "test_id".to_string(),
            object: "response".to_string(),
            created_at: 0,
            status: "completed".to_string(),
            model: "gpt-4".to_string(),
            output: vec![OutputItem::Message(MessageOutput {
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                status: "completed".to_string(),
                content: vec![ContentItem {
                    content_type: "text".to_string(),
                    text: Some("This is the answer".to_string()),
                    annotations: None,
                }],
            })],
            error: None,
            usage: None,
        };

        assert_eq!(response.extract_text(false), "This is the answer");
    }

    #[test]
    fn test_response_object_extract_text_no_message() {
        let response = ResponseObject {
            id: "test_id".to_string(),
            object: "response".to_string(),
            created_at: 0,
            status: "completed".to_string(),
            model: "gpt-4".to_string(),
            output: vec![],
            error: None,
            usage: None,
        };

        assert_eq!(response.extract_text(false), "No response text found");
    }

    #[test]
    fn test_response_object_extract_text_with_file_search() {
        let response = ResponseObject {
            id: "test_id".to_string(),
            object: "response".to_string(),
            created_at: 0,
            status: "completed".to_string(),
            model: "gpt-4".to_string(),
            output: vec![
                OutputItem::FileSearchCall(FileSearchOutput {
                    id: "fs_1".to_string(),
                    status: "completed".to_string(),
                    queries: Some(vec!["test query".to_string()]),
                    results: Some(vec![serde_json::json!({
                        "filename": "test.txt",
                        "score": 0.95,
                        "text": "Sample content from file"
                    })]),
                }),
                OutputItem::Message(MessageOutput {
                    id: "msg_1".to_string(),
                    role: "assistant".to_string(),
                    status: "completed".to_string(),
                    content: vec![ContentItem {
                        content_type: "text".to_string(),
                        text: Some("Answer based on search".to_string()),
                        annotations: None,
                    }],
                }),
            ],
            error: None,
            usage: None,
        };

        let without_results = response.extract_text(false);
        assert_eq!(without_results, "Answer based on search");

        let with_results = response.extract_text(true);
        assert!(with_results.contains("Answer based on search"));
        assert!(with_results.contains("Search Results"));
        assert!(with_results.contains("test.txt"));
        assert!(with_results.contains("0.95"));
    }

    #[test]
    fn test_file_info_structure() {
        let file_info = FileInfo {
            id: "file_123".to_string(),
            filename: Some("test.txt".to_string()),
            purpose: Some("assistants".to_string()),
            bytes: Some(1024),
            created_at: Some(1234567890),
            attributes: None,
        };

        assert_eq!(file_info.id, "file_123");
        assert_eq!(file_info.filename.unwrap(), "test.txt");
        assert_eq!(file_info.bytes.unwrap(), 1024);
    }

    #[test]
    fn test_vector_store_file_item() {
        let item = VectorStoreFileItem {
            id: "vsf_123".to_string(),
            status: "completed".to_string(),
            file: Some(FileInfo {
                id: "file_456".to_string(),
                filename: Some("doc.pdf".to_string()),
                purpose: None,
                bytes: None,
                created_at: None,
                attributes: None,
            }),
            file_id: Some("file_456".to_string()),
            filename: Some("doc.pdf".to_string()),
            attributes: None,
        };

        assert_eq!(item.id, "vsf_123");
        assert_eq!(item.status, "completed");
        assert!(item.file.is_some());
        assert_eq!(item.file.unwrap().filename.unwrap(), "doc.pdf");
    }
}
