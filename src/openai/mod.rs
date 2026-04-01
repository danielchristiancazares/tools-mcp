//! `OpenAI` API client modules.
//!
//! This module provides utilities for interacting with `OpenAI`'s APIs:
//!
//! - [`file_ext`]: File extension validation for uploads and `CodeQuery` indexing
//! - [`hash`]: SHA256 hashing utilities for content change detection
//! - [`types`]: API type definitions (responses, configuration, metadata)

pub mod file_ext;
pub mod hash;
pub mod types;

// Re-export file extension utilities
pub use file_ext::{
    compute_upload_filename, is_allowed_upload_ext, is_codequery_binary_ext,
    is_codequery_indexable_ext, is_codequery_indexable_filename, is_codequery_indexable_path,
};

// Re-export hash utilities
pub use hash::{compute_bytes_hash, compute_file_hash};
// Note: looks_binary_by_content is pub(crate) in hash.rs for use by lib.rs

// Re-export commonly used types
pub use types::{
    ApiConfig, CodeQueryOptions, ContentItem, FileCounts, FileInfo, FileObj, FileSearchOutput,
    MessageOutput, OutputItem, ResponseObject, VectorStore, VectorStoreDetails, VectorStoreEntry,
    VectorStoreFileItem, VectorStoreFilesList, VectorStoreList,
};
