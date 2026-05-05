//! # `OpenAI` Vector Store API Client Library
//!
//! This crate provides a focused Rust client for `OpenAI` vector stores, file uploads,
//! response-based file search, and hash-based `CodeQuery` reindexing.
//!
//! ## Example
//!
//! ```no_run
//! use openai_file_search_core::{ApiConfig, CodeQueryOptions, code_query};
//! use reqwest::Client;
//!
//! async fn example() -> anyhow::Result<()> {
//!     let client = Client::new();
//!     let config = ApiConfig::new("your-api-key", "gpt-4o");
//!     let options = CodeQueryOptions {
//!         concurrent_limit: 5,
//!         timeout_ms: 60_000,
//!         model: None,
//!         max_num_results: Some(10),
//!         include_results: true,
//!     };
//!
//!     let files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
//!     let (answer, _summary) = code_query(
//!         &client,
//!         &config,
//!         "vs_abc123",
//!         &files,
//!         "How does error handling work in this crate?",
//!         options,
//!     )
//!     .await?;
//!     println!("{answer}");
//!     Ok(())
//! }
//! ```

mod files;
mod openai;
mod reindex;
mod responses;
mod vector_stores;

pub use files::{upload_file, upload_files_batch};
pub use openai::file_ext::{
    compute_upload_filename, is_allowed_upload_ext, is_codequery_binary_ext,
    is_codequery_indexable_ext, is_codequery_indexable_filename, is_codequery_indexable_path,
};
pub use openai::hash::{compute_bytes_hash, compute_file_hash};
pub use openai::types::{
    ApiConfig, CodeQueryOptions, ContentItem, FileCounts, FileInfo, FileObj, FileSearchOutput,
    MessageOutput, OutputItem, ResponseObject, VectorStore, VectorStoreDetails, VectorStoreEntry,
    VectorStoreFileItem, VectorStoreFilesList, VectorStoreList,
};
pub use reindex::{code_query, reindex_files, reindex_with_retry};
pub use responses::{file_search_run, responses_with_file_search};
pub use vector_stores::{
    add_file_to_vector_store, add_file_to_vector_store_with, create_vector_store,
    delete_vector_store_file, get_file, get_vector_store_details, get_vector_store_file,
    list_vector_store_files, list_vector_store_files_with_details, list_vector_stores,
    wait_for_vector_file_ready, wait_for_vector_store_file_ready, wait_for_vector_store_ready,
};

/// Base URL for all `OpenAI` API requests.
pub const BASE_URL: &str = "https://api.openai.com/v1";

async fn openai_response_for_status(
    response: reqwest::Response,
    operation: &str,
) -> anyhow::Result<reqwest::Response> {
    if response.error_for_status_ref().is_err() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("{operation}: HTTP {} {}", status.as_u16(), body);
    }
    Ok(response)
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
