//! # OpenAI Vector Store API Client Library
//!
//! This library provides a comprehensive Rust client for interacting with OpenAI's Vector Stores
//! API, enabling file uploads, vector store management, semantic search, and intelligent file
//! reindexing capabilities.
//!
//! ## Overview
//!
//! The library wraps OpenAI's Assistants v2 API to provide:
//!
//! - **File Upload**: Upload local files or URLs to OpenAI's file storage with automatic format
//!   validation and extension handling
//! - **Vector Store Management**: Create, list, and query vector stores for semantic search
//! - **Semantic Search**: Execute natural language queries against indexed files using OpenAI's
//!   Responses API with file search tools
//! - **Intelligent Reindexing**: Hash-based file synchronization that detects changes, moves,
//!   and deletions to minimize API calls
//! - **Response Processing**: Type-safe deserialization of OpenAI API responses with structured
//!   output extraction
//!
//! ## Architecture
//!
//! The library follows a layered design:
//!
//! ```text
//! +------------------+     +--------------------+     +------------------+
//! |   code_query()   | --> |  reindex_files()   | --> |  upload_file()   |
//! |  (orchestration) |     |  (sync & diff)     |     |  (file I/O)      |
//! +------------------+     +--------------------+     +------------------+
//!         |                        |                          |
//!         v                        v                          v
//! +------------------+     +--------------------+     +------------------+
//! | responses_with_  | --> | add_file_to_       | --> |  OpenAI API      |
//! | file_search()    |     | vector_store()     |     |  (HTTP)          |
//! +------------------+     +--------------------+     +------------------+
//! ```
//!
//! ## Main Components
//!
//! - [`ApiConfig`]: Configuration container for OpenAI API authentication and default model
//! - [`ResponseObject`]: Type-safe representation of OpenAI Responses API output
//! - [`VectorStoreDetails`]: Vector store metadata including file processing status
//! - [`FileInfo`]: File metadata including attributes for hash-based tracking
//!
//! ## File Format Support
//!
//! The library automatically validates and converts file formats for OpenAI compatibility:
//!
//! **Supported extensions** (passed through unchanged):
//! `c`, `cpp`, `css`, `csv`, `doc`, `docx`, `gif`, `go`, `html`, `java`, `jpeg`, `jpg`,
//! `js`, `json`, `md`, `pdf`, `php`, `pkl`, `png`, `pptx`, `py`, `rb`, `tar`, `tex`,
//! `ts`, `txt`, `webp`, `xlsx`, `xml`, `zip`
//!
//! **Unsupported extensions**: Automatically converted to `.txt` for compatibility.
//!
//! ## CodeQuery Indexing
//!
//! For semantic code search via [`code_query`], the library applies stricter filtering:
//!
//! - **Indexable**: Source code files (`rs`, `c`, `cpp`, `go`, `java`, `py`, `js`, `ts`, etc.)
//! - **Excluded**: Binary files, images, archives, Office documents, dotfiles, markdown
//! - **Binary detection**: Files containing NUL bytes are automatically skipped
//!
//! ## Polling and Timeouts
//!
//! Vector store file indexing is asynchronous. The library provides polling utilities:
//!
//! - [`wait_for_vector_store_ready`]: Polls aggregate file counts (efficient for batch operations)
//! - [`wait_for_vector_file_ready`]: Polls individual file status (legacy, per-file)
//!
//! Both support configurable poll intervals and timeouts, with fail-fast behavior on terminal
//! error states (failed/cancelled files).
//!
//! ## Error Handling
//!
//! All functions return `anyhow::Result<T>` with contextual error messages. The library
//! distinguishes between:
//!
//! - **Transient errors**: Timeouts, rate limits (429), server errors (5xx) - eligible for retry
//! - **Permanent errors**: Invalid requests, authentication failures, file not found
//!
//! The [`reindex_with_retry`] function implements automatic retry with exponential backoff for
//! transient failures.
//!
//! ## Example Usage
//!
//! ### Basic File Upload
//!
//! ```no_run
//! use file_search_core::{ApiConfig, upload_file};
//! use reqwest::Client;
//!
//! async fn example() -> anyhow::Result<()> {
//!     let client = Client::new();
//!     let config = ApiConfig::new("your-api-key", "gpt-4o");
//!     
//!     // Upload a local file
//!     let file_id = upload_file(&client, &config, "document.pdf").await?;
//!     println!("Uploaded file: {}", file_id);
//!     
//!     // Upload from URL
//!     let url_file_id = upload_file(
//!         &client,
//!         &config,
//!         "https://example.com/data.json"
//!     ).await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ### Creating a Vector Store and Querying
//!
//! ```no_run
//! use file_search_core::{
//!     ApiConfig, create_vector_store, add_file_to_vector_store,
//!     wait_for_vector_store_ready, responses_with_file_search, upload_file,
//! };
//! use reqwest::Client;
//!
//! async fn search_example() -> anyhow::Result<()> {
//!     let client = Client::new();
//!     let config = ApiConfig::new("your-api-key", "gpt-4o");
//!     
//!     // Create vector store
//!     let vs_id = create_vector_store(&client, &config, "my-knowledge-base").await?;
//!     
//!     // Upload and attach file
//!     let file_id = upload_file(&client, &config, "docs/api.md").await?;
//!     add_file_to_vector_store(&client, &config, &vs_id, &file_id).await?;
//!     
//!     // Wait for indexing
//!     wait_for_vector_store_ready(&client, &config, &vs_id, 1000, 60000).await?;
//!     
//!     // Query
//!     let response = responses_with_file_search(
//!         &client,
//!         &config,
//!         "gpt-4o",
//!         "How do I authenticate API requests?",
//!         &vs_id,
//!         Some(5),  // max results
//!         true,     // include search results
//!     ).await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ### High-Level Code Query
//!
//! ```no_run
//! use file_search_core::{ApiConfig, code_query, CodeQueryOptions};
//! use reqwest::Client;
//!
//! async fn code_search() -> anyhow::Result<()> {
//!     let client = Client::new();
//!     let config = ApiConfig::new("your-api-key", "gpt-4o");
//!     
//!     let files = vec![
//!         "src/main.rs".to_string(),
//!         "src/lib.rs".to_string(),
//!     ];
//!     
//!     let options = CodeQueryOptions {
//!         concurrent_limit: 5,
//!         timeout_ms: 60000,
//!         model: None,  // use default
//!         max_num_results: Some(10),
//!         include_results: true,
//!     };
//!     
//!     let (answer, reindex_summary) = code_query(
//!         &client,
//!         &config,
//!         "vs_abc123",
//!         &files,
//!         "How does error handling work in this codebase?",
//!         options,
//!     ).await?;
//!     
//!     println!("Answer: {}", answer);
//!     if let Some(summary) = reindex_summary {
//!         println!("Reindex: {}", summary);
//!     }
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Environment Requirements
//!
//! - **OPENAI_API_KEY**: Required environment variable for authentication
//! - **Network**: HTTPS access to `api.openai.com`
//!
//! ## Thread Safety
//!
//! All public functions are `async` and operate on shared `&Client` and `&ApiConfig` references,
//! making them safe to call concurrently from multiple tasks. The library does not maintain
//! internal mutable state.

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, StatusCode, multipart};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use tokio::time::{Duration, sleep};

/// Base URL for all OpenAI API requests.
///
/// All API endpoints are constructed by appending paths to this base URL.
pub const BASE_URL: &str = "https://api.openai.com/v1";

/// Represents a complete response from OpenAI's Responses API.
///
/// This struct captures the full response payload from a Responses API call,
/// including the generated output, status information, and optional error/usage data.
///
/// # Structure
///
/// The response contains one or more [`OutputItem`] entries in the `output` vector,
/// typically including:
/// - A [`OutputItem::FileSearchCall`] when file search tools are used
/// - A [`OutputItem::Message`] containing the assistant's response
///
/// # Example
///
/// ```ignore
/// let response: ResponseObject = serde_json::from_value(api_response)?;
/// let text = response.extract_text(true);  // include search results
/// println!("Response: {}", text);
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseObject {
    /// Unique identifier for this response (e.g., "resp_abc123").
    pub id: String,
    /// Object type, always "response" for Responses API.
    pub object: String,
    /// Unix timestamp (seconds) when the response was created.
    pub created_at: i64,
    /// Current status: "completed", "failed", "in_progress", etc.
    pub status: String,
    /// Model used to generate the response (e.g., "gpt-4o").
    pub model: String,
    /// List of output items produced by the model.
    pub output: Vec<OutputItem>,
    /// Error details if the response failed (typically a JSON object with "code" and "message").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    /// Token usage statistics (prompt_tokens, completion_tokens, total_tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

/// An individual output item from the Responses API.
///
/// The Responses API can produce multiple output items per request, discriminated by the `type`
/// field. This enum deserializes each known type into a strongly-typed variant.
///
/// # Variants
///
/// - `Message`: The assistant's generated text response
/// - `FileSearchCall`: Results from the file_search tool invocation
/// - `Other`: Catch-all for unknown or unsupported output types
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum OutputItem {
    /// A message output containing the assistant's response text.
    #[serde(rename = "message")]
    Message(MessageOutput),
    /// A file search tool call with query and results.
    #[serde(rename = "file_search_call")]
    FileSearchCall(FileSearchOutput),
    /// Catch-all variant for unrecognized output types.
    ///
    /// This ensures forward compatibility when OpenAI adds new output types.
    #[serde(other)]
    Other,
}

/// The assistant's message output containing generated text.
///
/// This represents the main response from the model, including any text content
/// and optional annotations (citations, file references, etc.).
#[derive(Debug, Deserialize, Serialize)]
pub struct MessageOutput {
    /// Unique identifier for this message (e.g., "msg_abc123").
    pub id: String,
    /// The role that produced this message, typically "assistant".
    pub role: String,
    /// Processing status: "completed", "in_progress", etc.
    pub status: String,
    /// List of content blocks in this message.
    ///
    /// Most responses contain a single text content block, but may include
    /// multiple blocks for complex outputs.
    #[serde(default)]
    pub content: Vec<ContentItem>,
}

/// Output from a file_search tool invocation.
///
/// When the Responses API uses the file_search tool, this struct captures
/// the search queries executed and the matching results from the vector store.
#[derive(Debug, Deserialize, Serialize)]
pub struct FileSearchOutput {
    /// Unique identifier for this tool call (e.g., "fs_abc123").
    pub id: String,
    /// Processing status: "completed", "in_progress", etc.
    pub status: String,
    /// The search queries generated and executed by the model.
    ///
    /// The model may reformulate the user's question into multiple
    /// search queries for better recall.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    /// Search results from the vector store.
    ///
    /// Each result is a JSON object containing:
    /// - `filename`: The source file name
    /// - `score`: Relevance score (0.0 to 1.0)
    /// - `text`: The matching text snippet
    ///
    /// Only populated when `include: ["file_search_call.results"]` is specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<serde_json::Value>>,
}

/// A content block within a message output.
///
/// Messages can contain multiple content blocks of different types.
/// Currently, the primary type is "output_text" for text content.
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentItem {
    /// The type of content: "output_text", "refusal", etc.
    #[serde(rename = "type")]
    pub content_type: String,
    /// The text content (present for "output_text" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Annotations attached to this content (citations, file references).
    ///
    /// Each annotation is a JSON object with type-specific fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,
}

impl ResponseObject {
    /// Extracts the main text content from the response, optionally including search results.
    ///
    /// This method navigates the response structure to find the assistant's message content
    /// and formats it for display. When `include_results` is true, it also appends a
    /// formatted summary of file search results.
    ///
    /// # Arguments
    ///
    /// * `include_results` - If true, appends formatted file search results to the output
    ///
    /// # Returns
    ///
    /// The extracted text content. Returns `"No response text found"` if the response
    /// contains no message output or the message has no text content.
    ///
    /// # Output Format
    ///
    /// When `include_results` is true and search results are present:
    ///
    /// ```text
    /// [Main response text]
    ///
    /// ---
    /// ### Search Results:
    ///
    /// 1. **filename.rs** (score: 0.95)
    ///    First two lines of matching text...
    /// 2. **another.rs** (score: 0.87)
    ///    Matching snippet preview...
    /// ```
    ///
    /// # Example
    ///
    /// ```ignore
    /// let response: ResponseObject = serde_json::from_value(api_response)?;
    ///
    /// // Get just the answer
    /// let answer = response.extract_text(false);
    ///
    /// // Get answer with search results appended
    /// let full_output = response.extract_text(true);
    /// ```
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

/// Configuration for OpenAI API authentication and defaults.
///
/// This struct holds the credentials and default settings needed to make API requests.
/// It is designed to be cloned and shared across multiple API calls.
///
/// # Security Note
///
/// The API key is stored in plain text. Ensure this struct is not logged or serialized
/// in ways that could expose the key. Load the key from environment variables or a
/// secure secrets manager.
///
/// # Example
///
/// ```
/// use file_search_core::ApiConfig;
///
/// // Create from environment variable
/// let config = ApiConfig::new(
///     std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY required"),
///     "gpt-4o",
/// );
/// ```
#[derive(Clone)]
pub struct ApiConfig {
    /// OpenAI API key for authentication (Bearer token).
    ///
    /// Obtain from <https://platform.openai.com/api-keys>.
    pub api_key: String,
    /// Default model to use when not explicitly specified.
    ///
    /// Common values: "gpt-4o", "gpt-4o-mini", "gpt-4-turbo".
    pub default_model: String,
}

impl ApiConfig {
    /// Creates a new API configuration.
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key (can be any type that implements `Into<String>`)
    /// * `default_model` - Default model identifier for API calls
    ///
    /// # Example
    ///
    /// ```
    /// use file_search_core::ApiConfig;
    ///
    /// let config = ApiConfig::new("sk-...", "gpt-4o");
    /// assert_eq!(config.default_model, "gpt-4o");
    /// ```
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            default_model: default_model.into(),
        }
    }
}

/// Checks if a file extension is allowed for direct upload to OpenAI.
///
/// OpenAI's Files API only accepts specific file formats. This function validates
/// whether a given extension is in the allowed list.
///
/// # Arguments
///
/// * `ext` - The file extension to check (without the leading dot)
///
/// # Returns
///
/// `true` if the extension is allowed, `false` otherwise.
///
/// # Case Sensitivity
///
/// The check is case-insensitive: "TXT", "txt", and "Txt" all return `true`.
///
/// # Allowed Extensions
///
/// Source code: `c`, `cpp`, `css`, `go`, `html`, `java`, `js`, `json`, `php`, `py`, `rb`, `ts`
/// Documents: `csv`, `doc`, `docx`, `md`, `pdf`, `pptx`, `tex`, `txt`, `xlsx`, `xml`
/// Images: `gif`, `jpeg`, `jpg`, `png`, `webp`
/// Archives: `tar`, `zip`
/// Other: `pkl`
///
/// # Example
///
/// ```
/// use file_search_core::is_allowed_upload_ext;
///
/// assert!(is_allowed_upload_ext("pdf"));
/// assert!(is_allowed_upload_ext("PDF"));  // case-insensitive
/// assert!(!is_allowed_upload_ext("exe"));
/// ```
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

/// Checks if a file extension indicates binary content unsuitable for CodeQuery indexing.
///
/// CodeQuery is optimized for semantic code search. Binary files (images, archives, executables)
/// produce noise in search results and waste API tokens. This function identifies extensions
/// that should be excluded from code indexing, even if OpenAI's Files API technically accepts them.
///
/// # Arguments
///
/// * `ext` - The file extension to check (without the leading dot)
///
/// # Returns
///
/// `true` if the extension indicates binary content that should be skipped.
///
/// # Categories Blocked
///
/// - **Images**: png, jpg, jpeg, gif, webp, bmp, ico, tif, tiff, svg, heic, avif
/// - **Audio/Video**: mp3, wav, flac, m4a, ogg, mp4, mov, mkv, webm
/// - **Archives**: zip, tar, gz, tgz, bz2, xz, 7z, rar
/// - **Executables**: exe, dll, so, dylib, a, lib, o, obj, class, jar, wasm
/// - **Binary data**: pkl, db, sqlite, sqlite3
/// - **Office/PDF**: pdf, doc, docx, ppt, pptx, xls, xlsx
///
/// # Example
///
/// ```
/// use file_search_core::is_codequery_binary_ext;
///
/// assert!(is_codequery_binary_ext("png"));    // image
/// assert!(is_codequery_binary_ext("zip"));    // archive
/// assert!(is_codequery_binary_ext("pdf"));    // document
/// assert!(!is_codequery_binary_ext("rs"));    // source code
/// assert!(!is_codequery_binary_ext("toml"));  // config file
/// ```
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

/// Checks if a file extension indicates source code suitable for CodeQuery indexing.
///
/// This function identifies actual source code files that benefit from semantic search.
/// It is intentionally conservative, excluding configuration files, documentation,
/// and other text files that are better searched with exact-match tools.
///
/// # Arguments
///
/// * `ext` - The file extension to check (without the leading dot)
///
/// # Returns
///
/// `true` if the extension indicates source code that should be indexed.
///
/// # Supported Languages
///
/// - **Rust**: rs
/// - **C/C++**: c, h, cpp, hpp
/// - **Go**: go
/// - **Java/Kotlin**: java, kt, kts
/// - **Swift**: swift
/// - **Python**: py
/// - **Ruby**: rb
/// - **PHP**: php
/// - **JavaScript/TypeScript**: js, jsx, ts, tsx
///
/// # Example
///
/// ```
/// use file_search_core::is_codequery_indexable_ext;
///
/// assert!(is_codequery_indexable_ext("rs"));
/// assert!(is_codequery_indexable_ext("py"));
/// assert!(!is_codequery_indexable_ext("md"));    // documentation
/// assert!(!is_codequery_indexable_ext("toml"));  // config
/// assert!(!is_codequery_indexable_ext("json"));  // data
/// ```
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

/// Checks if a filename (without extension) should be indexed by CodeQuery.
///
/// This function is intentionally a no-op that always returns `false`. CodeQuery
/// only indexes files with explicit code extensions, avoiding special-casing for
/// extensionless files like `Makefile`, `Dockerfile`, etc.
///
/// # Arguments
///
/// * `_file_name` - The filename to check (unused)
///
/// # Returns
///
/// Always returns `false`.
///
/// # Design Rationale
///
/// Extensionless files (Makefile, Dockerfile, Justfile, etc.) are typically
/// configuration or build files that are better searched with exact-match tools.
/// Including them in semantic code search would add noise.
pub fn is_codequery_indexable_filename(_file_name: &str) -> bool {
    // Intentionally empty: CodeQuery should only index explicit code/config-by-extension files.
    // (No special-casing for extensionless files.)
    false
}

/// Determines if a file path should be indexed by CodeQuery.
///
/// This is the primary entry point for file filtering in CodeQuery. It applies
/// multiple rules to determine whether a file is suitable for semantic code search:
///
/// 1. **Dotfiles excluded**: Files starting with `.` are skipped
/// 2. **Filename check**: Calls [`is_codequery_indexable_filename`] (currently always false)
/// 3. **Markdown excluded**: `.md` files are skipped (documentation, not code)
/// 4. **Binary blocked**: Extensions matching [`is_codequery_binary_ext`] are skipped
/// 5. **Code required**: Only extensions matching [`is_codequery_indexable_ext`] pass
///
/// # Arguments
///
/// * `path` - The file path to evaluate
///
/// # Returns
///
/// `true` if the file should be indexed for semantic code search.
///
/// # Example
///
/// ```
/// use file_search_core::is_codequery_indexable_path;
/// use std::path::Path;
///
/// assert!(is_codequery_indexable_path(Path::new("src/main.rs")));
/// assert!(is_codequery_indexable_path(Path::new("lib/utils.py")));
/// assert!(!is_codequery_indexable_path(Path::new("README.md")));
/// assert!(!is_codequery_indexable_path(Path::new(".gitignore")));
/// assert!(!is_codequery_indexable_path(Path::new("Cargo.toml")));
/// assert!(!is_codequery_indexable_path(Path::new("logo.png")));
/// ```
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

/// Computes the appropriate filename for upload to OpenAI's Files API.
///
/// OpenAI only accepts files with specific extensions. This function ensures compatibility
/// by either passing through allowed extensions unchanged or converting unsupported
/// extensions to `.txt`.
///
/// # Arguments
///
/// * `original_filename` - The original filename (with or without path components)
///
/// # Returns
///
/// A [`Cow<str>`] containing:
/// - The original filename (borrowed) if the extension is allowed
/// - A modified filename with `.txt` extension (owned) if the extension is not allowed
///
/// # Performance
///
/// Uses `Cow<str>` to avoid heap allocation when the filename is already valid.
/// Only allocates when modification is needed.
///
/// # Behavior
///
/// - Files with allowed extensions: returned unchanged
/// - Files with disallowed extensions: extension stripped, `.txt` appended
/// - Files without extensions: `.txt` appended
///
/// # Example
///
/// ```
/// use file_search_core::compute_upload_filename;
///
/// // Allowed extensions pass through (no allocation)
/// assert_eq!(compute_upload_filename("document.pdf").as_ref(), "document.pdf");
/// assert_eq!(compute_upload_filename("script.py").as_ref(), "script.py");
///
/// // Disallowed extensions get converted (allocates)
/// assert_eq!(compute_upload_filename("binary.exe").as_ref(), "binary.txt");
/// assert_eq!(compute_upload_filename("data.xyz").as_ref(), "data.txt");
///
/// // No extension gets .txt appended (allocates)
/// assert_eq!(compute_upload_filename("README").as_ref(), "README.txt");
/// assert_eq!(compute_upload_filename("Makefile").as_ref(), "Makefile.txt");
/// ```
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

/// Response from OpenAI's file upload endpoint.
///
/// Returned when a file is successfully uploaded to OpenAI's storage.
/// The `id` can be used to attach the file to vector stores or assistants.
#[derive(Deserialize, Debug)]
pub struct FileObj {
    /// Unique file identifier (e.g., "file-abc123").
    ///
    /// Use this ID with [`add_file_to_vector_store`] to index the file.
    pub id: String,
}

/// Response from vector store creation.
///
/// Returned when a new vector store is successfully created.
#[derive(Deserialize, Debug)]
pub struct VectorStore {
    /// Unique vector store identifier (e.g., "vs_abc123").
    pub id: String,
}

/// Request payload for creating a new vector store.
#[derive(Serialize, Debug)]
struct VectorStoreCreate {
    /// Human-readable name for the vector store.
    name: String,
}

/// Request payload for adding a file to a vector store.
#[derive(Serialize, Debug)]
struct VectorStoreFileCreate {
    /// The file ID to attach (from [`upload_file`]).
    file_id: String,
    /// Optional metadata attributes stored with the file.
    ///
    /// Common attributes:
    /// - `path`: Original file path for reindexing
    /// - `hash`: SHA256 hash for change detection
    /// - `indexed_at`: ISO 8601 timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    attributes: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional chunking configuration for the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    chunking_strategy: Option<serde_json::Value>,
}

/// Detailed vector store information including file processing status.
///
/// This struct provides aggregate file counts for efficient polling of indexing
/// progress without needing to list individual files.
///
/// # Usage
///
/// Use [`get_vector_store_details`] to fetch this data, then check `file_counts`
/// to determine if all files have finished processing.
///
/// # Example
///
/// ```ignore
/// let details = get_vector_store_details(&client, &config, "vs_abc123").await?;
/// if details.file_counts.in_progress == 0 {
///     println!("All {} files indexed!", details.file_counts.completed);
/// }
/// ```
#[derive(Deserialize, Debug)]
pub struct VectorStoreDetails {
    /// Unique vector store identifier.
    pub id: String,
    /// Aggregate counts of files in each processing state.
    #[serde(default)]
    pub file_counts: FileCounts,
}

/// Aggregate file processing counts for a vector store.
///
/// These counts reflect the current state of all files attached to a vector store.
/// Use for efficient batch indexing status checks instead of polling individual files.
///
/// # State Transitions
///
/// Files progress through states: `in_progress` -> `completed` | `failed` | `cancelled`
///
/// # Invariant
///
/// `total == in_progress + completed + failed + cancelled`
#[derive(Deserialize, Debug, Default)]
pub struct FileCounts {
    /// Number of files currently being indexed.
    #[serde(default)]
    pub in_progress: u64,
    /// Number of files successfully indexed and searchable.
    #[serde(default)]
    pub completed: u64,
    /// Number of files that failed to index (permanent error).
    #[serde(default)]
    pub failed: u64,
    /// Number of files whose indexing was cancelled.
    #[serde(default)]
    pub cancelled: u64,
    /// Total number of files attached to the vector store.
    #[serde(default)]
    pub total: u64,
}

/// Paginated list of files in a vector store.
///
/// Returned by [`list_vector_store_files`]. Supports cursor-based pagination
/// for vector stores with many files.
#[derive(Deserialize, Debug)]
pub struct VectorStoreFilesList {
    /// Files in this page of results.
    pub data: Vec<VectorStoreFileItem>,
    /// True if more pages are available.
    #[serde(default)]
    pub has_more: bool,
    /// Cursor for fetching the next page (ID of last item).
    #[serde(default)]
    pub last_id: Option<String>,
}

/// A file attached to a vector store.
///
/// Represents the relationship between a file and a vector store, including
/// the file's indexing status and optional metadata attributes.
///
/// # API Compatibility
///
/// The OpenAI API has returned file information in different formats over time.
/// This struct accepts both nested `file` objects and flat `file_id` fields
/// for robustness across API versions.
#[derive(Deserialize, Debug)]
pub struct VectorStoreFileItem {
    /// Unique identifier for this vector store file relationship.
    ///
    /// Note: This is NOT the underlying file ID. Use `file_id` or `file.id` instead.
    pub id: String,
    /// Indexing status: "in_progress", "completed", "failed", "cancelled".
    pub status: String,
    /// Nested file object (present in some API responses).
    #[serde(default)]
    pub file: Option<FileInfo>,
    /// Direct file ID reference (present in some API responses).
    #[serde(default)]
    pub file_id: Option<String>,
    /// Filename (may be present at top level in some responses).
    #[serde(default)]
    pub filename: Option<String>,
    /// Custom metadata attributes attached when the file was added.
    ///
    /// Used by [`reindex_files`] to store `path`, `hash`, and `indexed_at`.
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Detailed information about an uploaded file.
///
/// Contains metadata about a file in OpenAI's storage, including its
/// original filename, size, and custom attributes.
#[derive(Deserialize, Debug)]
pub struct FileInfo {
    /// Unique file identifier (e.g., "file-abc123").
    pub id: String,
    /// Original filename as uploaded.
    pub filename: Option<String>,
    /// File purpose: "assistants", "fine-tune", etc.
    #[serde(default)]
    pub purpose: Option<String>,
    /// File size in bytes.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Unix timestamp when the file was uploaded.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Custom metadata attributes (path, hash, indexed_at for reindexing).
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Configuration options for a [`code_query`] invocation.
///
/// Controls concurrency, timeouts, model selection, and result formatting
/// for semantic code search operations.
///
/// # Example
///
/// ```
/// use file_search_core::CodeQueryOptions;
///
/// let options = CodeQueryOptions {
///     concurrent_limit: 5,      // Max 5 concurrent file uploads
///     timeout_ms: 60000,        // 60 second timeout for indexing
///     model: Some("gpt-4o"),    // Use GPT-4o for the query
///     max_num_results: Some(10), // Return up to 10 search results
///     include_results: true,     // Include result snippets in output
/// };
/// ```
pub struct CodeQueryOptions<'a> {
    /// Maximum number of concurrent file upload/indexing operations.
    ///
    /// Higher values improve throughput but may trigger rate limits.
    /// Recommended range: 3-10.
    pub concurrent_limit: usize,
    /// Maximum time in milliseconds to wait for file indexing to complete.
    ///
    /// Applies to the aggregate wait after all files are uploaded.
    /// Default: 60000 (60 seconds).
    pub timeout_ms: u64,
    /// Model to use for the query. If `None`, uses [`ApiConfig::default_model`].
    pub model: Option<&'a str>,
    /// Maximum number of search results to return from the vector store.
    ///
    /// If `None`, uses OpenAI's default (typically 10).
    pub max_num_results: Option<u32>,
    /// Whether to include search result snippets in the response.
    ///
    /// When `true`, the response includes matching text snippets and scores.
    /// When `false`, only the synthesized answer is returned.
    pub include_results: bool,
}

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
/// indicator of binary data. This is used by [`code_query`] to skip binary files
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
async fn looks_binary_by_content(path: &str) -> Result<bool> {
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

/// Uploads a file to OpenAI's file storage system.
///
/// Supports both local files and remote URLs. The file is uploaded with purpose
/// "assistants" for use with vector stores and the Responses API.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `path_or_url` - Either a local file path or a URL (http:// or https://)
///
/// # Returns
///
/// The file ID assigned by OpenAI (e.g., "file-abc123"). Use this ID with
/// [`add_file_to_vector_store`] to index the file for search.
///
/// # File Extension Handling
///
/// The function uses [`compute_upload_filename`] to ensure compatibility:
/// - Allowed extensions are passed through unchanged
/// - Unsupported extensions are converted to `.txt`
///
/// # Errors
///
/// Returns an error if:
/// - **Local file**: Cannot be opened or read (file not found, permission denied)
/// - **Remote URL**: HTTP request fails, non-2xx response, or download error
/// - **API error**: OpenAI rejects the upload (invalid format, quota exceeded)
/// - **Parse error**: Response cannot be deserialized
///
/// # Example
///
/// ```ignore
/// // Upload a local file
/// let file_id = upload_file(&client, &config, "docs/api.md").await?;
///
/// // Upload from a URL
/// let url_id = upload_file(
///     &client,
///     &config,
///     "https://example.com/data.json"
/// ).await?;
/// ```
///
/// # Performance
///
/// - Local files are read entirely into memory before upload
/// - Remote files are downloaded entirely before upload
/// - For very large files, consider streaming uploads (not currently supported)
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

/// Uploads multiple files to a vector store in batch with concurrent processing.
///
/// This function handles batch uploading of files with:
/// - Concurrent uploads for improved throughput
/// - Automatic attachment to the specified vector store
/// - Per-file indexing wait with configurable timeout
/// - Progress logging via `tracing`
/// - Graceful error handling (continues on individual failures)
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `file_paths` - List of local file paths or URLs to upload
/// * `vector_store_id` - Target vector store ID (e.g., "vs_abc123")
/// * `concurrent_limit` - Maximum concurrent uploads (recommended: 5-10)
///
/// # Returns
///
/// A tuple `(successes, failures)` where:
/// - `successes`: Vec of `(path, file_id)` for successfully uploaded files
/// - `failures`: Vec of `(path, error_message)` for failed uploads
///
/// # Processing Flow
///
/// For each file:
/// 1. Upload to OpenAI's file storage via [`upload_file`]
/// 2. Attach to vector store via [`add_file_to_vector_store`]
/// 3. Wait for indexing via [`wait_for_vector_file_ready`] (30s timeout)
///
/// Files are processed in chunks of `concurrent_limit` with a 1-second delay
/// between chunks to avoid rate limiting.
///
/// # Error Handling
///
/// Individual file failures do not abort the batch. Errors are collected and
/// returned in the `failures` vector. Common failure reasons:
/// - File not found or permission denied
/// - API rate limit exceeded
/// - Indexing timeout (file still processing after 30s)
///
/// # Example
///
/// ```ignore
/// let files = vec![
///     "src/main.rs".to_string(),
///     "src/lib.rs".to_string(),
///     "README.md".to_string(),
/// ];
///
/// let (successes, failures) = upload_files_batch(
///     &client,
///     &config,
///     files,
///     "vs_abc123",
///     5,  // 5 concurrent uploads
/// ).await?;
///
/// println!("Uploaded: {}, Failed: {}", successes.len(), failures.len());
/// for (path, error) in &failures {
///     eprintln!("  {} - {}", path, error);
/// }
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
/// Vector stores are containers for indexed files that enable semantic search
/// via the file_search tool in the Responses API.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `name` - Human-readable name for the vector store (e.g., "my-codebase")
///
/// # Returns
///
/// The vector store ID (e.g., "vs_abc123"). Use this ID with:
/// - [`add_file_to_vector_store`] to attach files
/// - [`responses_with_file_search`] to query the store
/// - [`list_vector_store_files`] to list attached files
///
/// # Errors
///
/// Returns an error if:
/// - Network request fails
/// - API returns non-2xx status (quota exceeded, invalid request)
/// - Response cannot be parsed
///
/// # Example
///
/// ```ignore
/// let vs_id = create_vector_store(&client, &config, "project-docs").await?;
/// println!("Created vector store: {}", vs_id);
/// ```
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

/// Response from the list vector stores endpoint.
#[derive(Deserialize, Debug)]
pub struct VectorStoreList {
    /// Vector stores in this response.
    pub data: Vec<VectorStoreEntry>,
}

/// Summary information about a vector store.
///
/// Returned by [`list_vector_stores`]. For detailed information including
/// file counts, use [`get_vector_store_details`].
#[derive(Deserialize, Debug)]
pub struct VectorStoreEntry {
    /// Unique vector store identifier (e.g., "vs_abc123").
    pub id: String,
    /// Human-readable name assigned at creation.
    pub name: Option<String>,
}

/// Lists all vector stores in the account.
///
/// Returns summary information for each vector store. Use this to find
/// existing stores by name or to enumerate available stores.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
///
/// # Returns
///
/// A vector of [`VectorStoreEntry`] with ID and name for each store.
///
/// # Example
///
/// ```ignore
/// let stores = list_vector_stores(&client, &config).await?;
/// for store in stores {
///     println!("{}: {:?}", store.id, store.name);
/// }
///
/// // Find a store by name
/// let my_store = stores.iter().find(|s| s.name.as_deref() == Some("my-codebase"));
/// ```
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

/// Fetches detailed vector store information including file processing status.
///
/// Returns aggregate file counts which are more efficient for status polling
/// than listing individual files.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID to query
///
/// # Returns
///
/// [`VectorStoreDetails`] containing the store ID and [`FileCounts`] with
/// in_progress, completed, failed, cancelled, and total counts.
///
/// # Example
///
/// ```ignore
/// let details = get_vector_store_details(&client, &config, "vs_abc123").await?;
/// let counts = &details.file_counts;
///
/// println!("Status: {}/{} completed, {} in progress",
///     counts.completed, counts.total, counts.in_progress);
///
/// if counts.failed > 0 {
///     eprintln!("Warning: {} files failed to index", counts.failed);
/// }
/// ```
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

/// Waits for all files in a vector store to finish processing.
///
/// Polls the vector store's aggregate file counts until all files have completed
/// indexing or a timeout/failure occurs. This is more efficient than polling
/// individual file statuses.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID to monitor
/// * `poll_ms` - Milliseconds between status checks (e.g., 1000 for 1 second)
/// * `timeout_ms` - Maximum wait time in milliseconds before timing out
///
/// # Returns
///
/// `Ok(())` when all files have completed indexing successfully.
///
/// # Errors
///
/// Returns an error if:
/// - Any files enter a terminal failure state (failed or cancelled)
/// - The timeout expires with files still in progress
/// - An API request fails
///
/// # Behavior
///
/// - **Fail-fast**: Returns immediately if any files have failed/cancelled
/// - **Empty store**: Returns immediately if the store has no files
/// - **Logging**: Emits debug logs with progress updates
///
/// # Example
///
/// ```ignore
/// // Wait up to 60 seconds, polling every second
/// wait_for_vector_store_ready(&client, &config, "vs_abc123", 1000, 60000).await?;
/// println!("All files indexed and ready for search");
/// ```
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

/// Attaches a file to a vector store for indexing.
///
/// Once attached, the file will be processed and indexed asynchronously. Use
/// [`wait_for_vector_store_ready`] or [`wait_for_vector_file_ready`] to wait
/// for indexing to complete.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Target vector store ID
/// * `file_id` - File ID from [`upload_file`]
///
/// # Errors
///
/// Returns an error if:
/// - The file ID is invalid or already deleted
/// - The vector store ID is invalid
/// - API request fails
///
/// # Example
///
/// ```ignore
/// let file_id = upload_file(&client, &config, "doc.md").await?;
/// add_file_to_vector_store(&client, &config, "vs_abc123", &file_id).await?;
/// wait_for_vector_store_ready(&client, &config, "vs_abc123", 1000, 30000).await?;
/// ```
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

/// Attaches a file to a vector store with custom attributes and chunking.
///
/// Extended version of [`add_file_to_vector_store`] that allows attaching
/// metadata attributes (for reindexing) and custom chunking configuration.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Target vector store ID
/// * `file_id` - File ID from [`upload_file`]
/// * `attributes` - Optional metadata to attach to the file:
///   - `path`: Original file path (for change detection)
///   - `hash`: SHA256 hash (for change detection)
///   - `indexed_at`: ISO 8601 timestamp
/// * `chunking_strategy` - Optional chunking configuration (OpenAI default if None)
///
/// # Example
///
/// ```ignore
/// let mut attrs = serde_json::Map::new();
/// attrs.insert("path".to_string(), json!("src/main.rs"));
/// attrs.insert("hash".to_string(), json!("abc123..."));
/// attrs.insert("indexed_at".to_string(), json!("2024-01-15T12:00:00Z"));
///
/// add_file_to_vector_store_with(
///     &client,
///     &config,
///     "vs_abc123",
///     &file_id,
///     Some(attrs),
///     None,  // default chunking
/// ).await?;
/// ```
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

/// Waits for all files in a vector store to complete indexing (legacy method).
///
/// Polls the file list endpoint until all files have "completed" status.
/// For new code, prefer [`wait_for_vector_store_ready`] which uses aggregate
/// file counts and is more efficient.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID to monitor
/// * `poll_ms` - Milliseconds between status checks
/// * `timeout_ms` - Maximum wait time before timing out
///
/// # Errors
///
/// Returns an error if:
/// - Any file enters a terminal failure state (failed/cancelled)
/// - Timeout expires with files still in progress
/// - API request fails
///
/// # Comparison with `wait_for_vector_store_ready`
///
/// | Aspect | This function | `wait_for_vector_store_ready` |
/// |--------|---------------|-------------------------------|
/// | API calls | Lists all files each poll | Single GET per poll |
/// | Efficiency | O(n) per poll | O(1) per poll |
/// | Use case | Legacy compatibility | Recommended for new code |
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
/// Fetches all pages of results and returns them as a single consolidated list.
/// Used by [`reindex_files`] to detect existing files for change detection.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID to list files from
///
/// # Returns
///
/// A [`VectorStoreFilesList`] containing all files in the store. The `has_more`
/// field will be `false` and `last_id` will be `None` since all pages are fetched.
///
/// # Pagination
///
/// Automatically follows cursor-based pagination to retrieve all files.
/// For stores with many files, this may make multiple API calls.
///
/// # Example
///
/// ```ignore
/// let files = list_vector_store_files(&client, &config, "vs_abc123").await?;
/// println!("Store contains {} files", files.data.len());
///
/// for file in &files.data {
///     println!("  {} - {}", file.id, file.status);
/// }
/// ```
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

/// Removes a file from a vector store.
///
/// Detaches the file from the vector store and removes it from the search index.
/// The underlying file in OpenAI's storage is NOT deleted.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID
/// * `file_id` - File ID to remove (the relationship ID from the vector store)
///
/// # Use Cases
///
/// - Removing outdated files during reindexing
/// - Cleaning up files that failed to index
/// - Removing files that are no longer relevant
///
/// # Note
///
/// The `file_id` here is the vector store file relationship ID (from
/// [`VectorStoreFileItem::id`]), not the underlying file ID.
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

/// Retrieves metadata for a file in OpenAI's storage.
///
/// Returns detailed information about an uploaded file, including its
/// filename, size, and creation timestamp.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `file_id` - File ID to query (e.g., "file-abc123")
///
/// # Returns
///
/// [`FileInfo`] with the file's metadata.
///
/// # Errors
///
/// Returns an error if the file ID is invalid or the file has been deleted.
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

/// Lists all files in a vector store with full file metadata.
///
/// Combines [`list_vector_store_files`] with [`get_file`] to retrieve
/// complete [`FileInfo`] for each file in the store.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vs_id` - Vector store ID to list files from
///
/// # Returns
///
/// A vector of [`FileInfo`] with full metadata for each file.
///
/// # Performance
///
/// Makes N+1 API calls (1 to list files, N to get details for each file).
/// For stores with many files, this can be slow. Consider using
/// [`list_vector_store_files`] if you only need IDs and status.
///
/// # Error Handling
///
/// If fetching details for a specific file fails, a fallback [`FileInfo`]
/// with only the ID and filename is used instead of failing the entire operation.
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

/// Request payload for the Responses API.
#[derive(Serialize, Debug)]
struct ResponsesCreate<'a> {
    /// Model to use for generating the response.
    model: &'a str,
    /// User query or prompt.
    input: &'a str,
    /// Tools available to the model (e.g., file_search).
    tools: Vec<serde_json::Value>,
    /// Optional fields to include in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<Vec<&'a str>>,
}

/// Executes a semantic search query against a vector store using the Responses API.
///
/// Sends a natural language query to OpenAI's Responses API with the file_search tool
/// configured to search the specified vector store. The model generates a response
/// based on the relevant content found.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `model` - Model to use (e.g., "gpt-4o", "gpt-4o-mini")
/// * `query` - Natural language search query
/// * `vector_store_id` - Vector store to search
/// * `max_num_results` - Maximum search results to retrieve (None for API default)
/// * `include_results` - If true, includes search result snippets in the response
///
/// # Returns
///
/// Raw JSON response from the Responses API. Use [`ResponseObject`] to parse
/// and extract the text content via [`ResponseObject::extract_text`].
///
/// # Example
///
/// ```ignore
/// let response = responses_with_file_search(
///     &client,
///     &config,
///     "gpt-4o",
///     "How does authentication work?",
///     "vs_abc123",
///     Some(5),
///     true,
/// ).await?;
///
/// let parsed: ResponseObject = serde_json::from_value(response)?;
/// println!("{}", parsed.extract_text(true));
/// ```
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

/// One-shot file search: uploads a file, creates a vector store, and queries it.
///
/// Convenience function that performs the complete file search workflow in one call:
/// 1. Upload the file to OpenAI
/// 2. Create a new vector store named "knowledge_base"
/// 3. Attach the file to the vector store
/// 4. Wait for indexing to complete
/// 5. Execute the search query
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `file_path_or_url` - Local file path or URL to upload
/// * `query` - Natural language search query
/// * `model` - Model to use (None for default from config)
/// * `max_num_results` - Maximum search results (None for API default)
/// * `include_results` - Include search result snippets in response
///
/// # Returns
///
/// JSON object containing:
/// - `file_id`: The uploaded file's ID
/// - `vector_store_id`: The created vector store's ID
/// - `response`: The Responses API result
///
/// # Use Case
///
/// Best for one-off queries against a single file. For repeated queries or
/// multiple files, use the individual functions to reuse the vector store.
///
/// # Example
///
/// ```ignore
/// let result = file_search_run(
///     &client,
///     &config,
///     "documentation.pdf",
///     "What are the API rate limits?",
///     None,
///     Some(5),
///     true,
/// ).await?;
///
/// println!("Vector store: {}", result["vector_store_id"]);
/// ```
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

/// Synchronizes local files with a vector store using hash-based change detection.
///
/// This is the core reindexing function that intelligently syncs local files to a
/// vector store, minimizing API calls by detecting unchanged files via SHA256 hashes.
///
/// # Algorithm
///
/// 1. **Fetch existing files**: Lists all files in the vector store with their attributes
/// 2. **Build lookup maps**: Creates maps by path, hash, and filename for matching
/// 3. **Detect changes**: For each local file:
///    - Same path + same hash: Skip (unchanged)
///    - Same path + different hash: Delete old, upload new (content changed)
///    - Different path + same hash: Delete old, upload new (file moved)
///    - No match: Upload new (new file)
/// 4. **Clean orphans**: Delete store files not in local file list
/// 5. **Wait for indexing**: Optionally wait for each file to complete
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vector_store_id` - Target vector store ID
/// * `file_paths` - Local file paths to sync (must be local, not URLs)
/// * `concurrent_limit` - Maximum concurrent upload operations
/// * `skip_per_file_wait` - If true, skip waiting for each file to index (faster but
///   requires calling [`wait_for_vector_store_ready`] afterward)
///
/// # Returns
///
/// JSON object with operation summary:
/// ```json
/// {
///   "summary": {
///     "total_files": 10,
///     "uploaded": 3,
///     "skipped": 5,
///     "deleted": 2,
///     "errors": 0
///   },
///   "uploaded": [{"path": "...", "file_id": "...", "hash": "...", "action": "uploaded"}],
///   "skipped": ["path1", "path2"],
///   "deleted": [{"path": "...", "file_id": "...", "action": "deleted"}],
///   "errors": []
/// }
/// ```
///
/// # File Attributes
///
/// Uploaded files are tagged with attributes for future reindexing:
/// - `path`: Original file path
/// - `hash`: SHA256 hash of file contents
/// - `indexed_at`: ISO 8601 timestamp
///
/// # Error Handling
///
/// Individual file errors do not abort the operation. Errors are collected
/// and returned in the `errors` array of the summary.
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

/// Reindexes files with automatic retry on transient failures.
///
/// Wrapper around [`reindex_files`] that implements retry logic with exponential
/// backoff for transient errors (timeouts, rate limits, server errors).
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vector_store_id` - Target vector store ID
/// * `file_paths` - Local file paths to sync
/// * `concurrent_limit` - Maximum concurrent upload operations
/// * `skip_per_file_wait` - Skip per-file indexing waits
///
/// # Retry Behavior
///
/// - **Max attempts**: 3
/// - **Backoff**: 200ms, 500ms, 1000ms (plus jitter)
/// - **Transient errors**: Timeouts, connection errors, HTTP 429/5xx
/// - **Permanent errors**: Not retried (invalid requests, auth failures)
///
/// # Returns
///
/// Same as [`reindex_files`] on success.
///
/// # Errors
///
/// Returns an error if:
/// - All retry attempts fail with transient errors
/// - A permanent (non-retryable) error occurs
/// - The final attempt has errors in the summary
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

/// Determines if an error is transient and eligible for retry.
///
/// Checks the error chain for reqwest errors indicating temporary failures:
/// - Timeouts
/// - Connection errors
/// - HTTP 429 (Too Many Requests)
/// - HTTP 5xx (Server errors)
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

/// Checks if an error message indicates a transient failure.
///
/// Used to detect retryable errors from error summaries returned by [`reindex_files`].
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

/// High-level semantic code search with automatic file synchronization.
///
/// This is the primary entry point for semantic code search. It combines file
/// reindexing and querying into a single operation, handling all the complexity
/// of change detection, uploading, and waiting for indexing.
///
/// # Workflow
///
/// When `file_paths` is non-empty:
/// 1. **Validate paths**: Ensures all paths are local files (not URLs)
/// 2. **Filter files**: Excludes binary files, non-code files, and dotfiles
/// 3. **Reindex with retry**: Syncs changed files with automatic retry on failures
/// 4. **Wait for indexing**: Polls until all files are indexed
/// 5. **Execute query**: Runs semantic search against the vector store
///
/// When `file_paths` is empty, skips directly to the query step.
///
/// # Arguments
///
/// * `client` - HTTP client for making API requests
/// * `cfg` - API configuration containing the authentication key
/// * `vector_store_id` - Target vector store ID
/// * `file_paths` - Local file paths to sync before querying (can be empty)
/// * `query` - Natural language search query
/// * `options` - Configuration options (see [`CodeQueryOptions`])
///
/// # Returns
///
/// A tuple containing:
/// - The extracted response text (answer from the model)
/// - Optional reindex summary (if files were synced)
///
/// # File Filtering
///
/// Only source code files are indexed (see [`is_codequery_indexable_path`]):
/// - **Included**: `.rs`, `.py`, `.js`, `.ts`, `.go`, `.java`, etc.
/// - **Excluded**: Binary files, images, archives, markdown, config files
/// - **Skipped**: Files with NUL bytes (detected as binary)
///
/// Filtered files are reported in the reindex summary under `filtered_out`.
///
/// # Error Handling
///
/// Returns an error if:
/// - The query is empty
/// - Any file path is a URL (not supported)
/// - No indexable files remain after filtering
/// - Reindexing fails after all retry attempts
/// - The query request fails
///
/// # Example
///
/// ```ignore
/// use file_search_core::{ApiConfig, code_query, CodeQueryOptions};
///
/// let options = CodeQueryOptions {
///     concurrent_limit: 5,
///     timeout_ms: 60000,
///     model: Some("gpt-4o"),
///     max_num_results: Some(10),
///     include_results: true,
/// };
///
/// let (answer, summary) = code_query(
///     &client,
///     &config,
///     "vs_my_codebase",
///     &["src/main.rs".to_string(), "src/lib.rs".to_string()],
///     "How does the authentication middleware work?",
///     options,
/// ).await?;
///
/// println!("Answer: {}", answer);
/// if let Some(s) = summary {
///     println!("Synced {} files", s["summary"]["uploaded"]);
/// }
/// ```
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
