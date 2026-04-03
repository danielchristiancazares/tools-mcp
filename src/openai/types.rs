//! `OpenAI` API type definitions.
//!
//! This module contains all the public types used for `OpenAI` API interactions,
//! including response structures, configuration, and file metadata.

use serde::{Deserialize, Serialize};

/// Represents a complete response from `OpenAI`'s Responses API.
///
/// This struct captures the full response payload from a Responses API call,
/// including the generated output, status information, and optional error/usage data.
#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseObject {
    /// Unique identifier for this response (e.g., `"resp_abc123"`).
    pub id: String,
    /// Object type, always `"response"` for the Responses API.
    pub object: String,
    /// Unix timestamp (seconds) when the response was created.
    pub created_at: i64,
    /// Current status: "completed", "failed", "`in_progress`", etc.
    pub status: String,
    /// Model used to generate the response (e.g., `"gpt-4o"`).
    pub model: String,
    /// List of output items produced by the model.
    pub output: Vec<OutputItem>,
    /// Error details if the response failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    /// Token usage statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

/// An individual output item from the Responses API.
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
    #[serde(other)]
    Other,
}

/// The assistant's message output containing generated text.
#[derive(Debug, Deserialize, Serialize)]
pub struct MessageOutput {
    /// Unique identifier for this message.
    pub id: String,
    /// The role that produced this message, typically "assistant".
    pub role: String,
    /// Processing status.
    pub status: String,
    /// List of content blocks in this message.
    #[serde(default)]
    pub content: Vec<ContentItem>,
}

/// Output from a `file_search` tool invocation.
#[derive(Debug, Deserialize, Serialize)]
pub struct FileSearchOutput {
    /// Unique identifier for this tool call.
    pub id: String,
    /// Processing status.
    pub status: String,
    /// The search queries generated and executed by the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    /// Search results from the vector store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<serde_json::Value>>,
}

/// A content block within a message output.
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentItem {
    /// The type of content: `"output_text"`, `"refusal"`, etc.
    #[serde(rename = "type")]
    pub content_type: String,
    /// The text content (present for "`output_text`" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Annotations attached to this content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,
}

impl ResponseObject {
    /// Extracts the main text content from the response, optionally including search results.
    #[must_use]
    pub fn extract_text(&self, include_results: bool) -> String {
        use std::fmt::Write as _;

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
                        r.get("score").and_then(serde_json::Value::as_f64),
                    ) {
                        let _ = write!(
                            result,
                            "\n{}. **{}** (score: {:.2})",
                            i + 1,
                            filename,
                            score
                        );
                        if let Some(text_snippet) = r.get("text").and_then(|v| v.as_str()) {
                            let mut preview = String::new();
                            for (line_idx, line) in text_snippet.lines().take(2).enumerate() {
                                if line_idx > 0 {
                                    preview.push_str("\n   ");
                                }
                                preview.push_str(line);
                            }
                            let preview = preview.trim();
                            if !preview.is_empty() {
                                result.push_str("\n   ");
                                result.push_str(preview);
                            }
                        }
                    }
                }
            }

            return result;
        }

        String::from("No response text found")
    }
}

/// Extracts the main text content from a raw Responses API JSON payload.
///
/// This is a fast-path for callers that already have a `serde_json::Value` response and want to
/// avoid deserializing into [`ResponseObject`] just to extract text.
#[must_use]
pub fn extract_text_from_response_value(
    response: &serde_json::Value,
    include_results: bool,
) -> String {
    use std::fmt::Write as _;

    let message_text = response
        .get("output")
        .and_then(|v| v.as_array())
        .and_then(|output| {
            output.iter().find_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                    item.get("content")
                        .and_then(|c| c.as_array())
                        .and_then(|c| c.first())
                        .and_then(|c0| c0.get("text").and_then(|t| t.as_str()))
                } else {
                    None
                }
            })
        })
        .unwrap_or("No response text found");

    let mut result = message_text.to_string();

    if include_results {
        let results = response
            .get("output")
            .and_then(|v| v.as_array())
            .and_then(|output| {
                output.iter().find_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("file_search_call") {
                        item.get("results").and_then(|r| r.as_array())
                    } else {
                        None
                    }
                })
            });

        if let Some(results) = results {
            result.push_str("\n\n---\n### Search Results:\n");
            for (i, r) in results.iter().take(5).enumerate() {
                let (Some(filename), Some(score)) = (
                    r.get("filename").and_then(|v| v.as_str()),
                    r.get("score").and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };

                let _ = write!(
                    result,
                    "\n{}. **{}** (score: {:.2})",
                    i + 1,
                    filename,
                    score
                );

                if let Some(text_snippet) = r.get("text").and_then(|v| v.as_str()) {
                    let mut preview = String::new();
                    for (line_idx, line) in text_snippet.lines().take(2).enumerate() {
                        if line_idx > 0 {
                            preview.push_str("\n   ");
                        }
                        preview.push_str(line);
                    }
                    let preview = preview.trim();
                    if !preview.is_empty() {
                        result.push_str("\n   ");
                        result.push_str(preview);
                    }
                }
            }
        }
    }

    result
}

/// Configuration for `OpenAI` API authentication and defaults.
#[derive(Clone)]
pub struct ApiConfig {
    /// `OpenAI` API key for authentication.
    pub api_key: String,
    /// Default model to use when not explicitly specified.
    pub default_model: String,
}

impl ApiConfig {
    /// Creates a new API configuration.
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            default_model: default_model.into(),
        }
    }
}

/// Response from `OpenAI`'s file upload endpoint.
#[derive(Deserialize, Debug)]
pub struct FileObj {
    /// Unique file identifier (e.g., "file-abc123").
    pub id: String,
}

/// Response from vector store creation.
#[derive(Deserialize, Debug)]
pub struct VectorStore {
    /// Unique vector store identifier (e.g., "`vs_abc123`").
    pub id: String,
}

/// Request payload for creating a new vector store.
#[derive(Serialize, Debug)]
pub(crate) struct VectorStoreCreate {
    /// Human-readable name for the vector store.
    pub name: String,
}

/// Request payload for adding a file to a vector store.
#[derive(Serialize, Debug)]
pub(crate) struct VectorStoreFileCreate {
    /// The file ID to attach.
    pub file_id: String,
    /// Optional metadata attributes stored with the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional chunking configuration for the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunking_strategy: Option<serde_json::Value>,
}

/// Detailed vector store information including file processing status.
#[derive(Deserialize, Debug)]
pub struct VectorStoreDetails {
    /// Unique vector store identifier.
    pub id: String,
    /// Aggregate counts of files in each processing state.
    #[serde(default)]
    pub file_counts: FileCounts,
}

/// Aggregate file processing counts for a vector store.
#[derive(Deserialize, Debug, Default)]
pub struct FileCounts {
    /// Number of files currently being indexed.
    #[serde(default)]
    pub in_progress: u64,
    /// Number of files successfully indexed and searchable.
    #[serde(default)]
    pub completed: u64,
    /// Number of files that failed to index.
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
#[derive(Deserialize, Debug)]
pub struct VectorStoreFilesList {
    /// Files in this page of results.
    pub data: Vec<VectorStoreFileItem>,
    /// True if more pages are available.
    #[serde(default)]
    pub has_more: bool,
    /// Cursor for fetching the next page.
    #[serde(default)]
    pub last_id: Option<String>,
}

/// A file attached to a vector store.
#[derive(Deserialize, Debug)]
pub struct VectorStoreFileItem {
    /// Unique identifier for this vector store file relationship.
    pub id: String,
    /// Indexing status: "`in_progress`", "completed", "failed", "cancelled".
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
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Detailed information about an uploaded file.
#[derive(Deserialize, Debug)]
pub struct FileInfo {
    /// Unique file identifier.
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
    /// Custom metadata attributes.
    #[serde(default)]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Paginated list of vector stores.
#[derive(Deserialize, Debug)]
pub struct VectorStoreList {
    /// Vector stores in this page of results.
    pub data: Vec<VectorStoreEntry>,
    /// True if more pages are available.
    #[serde(default)]
    pub has_more: bool,
}

/// Summary information about a vector store.
#[derive(Deserialize, Debug)]
pub struct VectorStoreEntry {
    /// Unique vector store identifier.
    pub id: String,
    /// Human-readable name assigned at creation.
    pub name: Option<String>,
    /// Unix timestamp when created.
    #[serde(default)]
    pub created_at: Option<i64>,
}

/// Configuration options for a [`code_query`](crate::code_query) invocation.
pub struct CodeQueryOptions<'a> {
    /// Maximum number of concurrent file upload/indexing operations.
    pub concurrent_limit: usize,
    /// Maximum time in milliseconds to wait for file indexing.
    pub timeout_ms: u64,
    /// Model to use for the query. If `None`, uses default.
    pub model: Option<&'a str>,
    /// Maximum number of search results to return.
    pub max_num_results: Option<u32>,
    /// Whether to include search result snippets in the response.
    pub include_results: bool,
}

/// Request payload for the Responses API.
#[derive(Serialize)]
pub(crate) struct ResponsesCreate<'a> {
    pub model: &'a str,
    pub input: &'a str,
    pub tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<&'a str>>,
}
