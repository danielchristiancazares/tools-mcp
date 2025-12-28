//! OpenAI API client modules.
//!
//! This module provides utilities for interacting with OpenAI's APIs:
//! - [`file_ext`]: File extension validation for uploads and CodeQuery indexing

pub mod file_ext;

// Re-export commonly used items
pub use file_ext::{
    compute_upload_filename, is_allowed_upload_ext, is_codequery_binary_ext,
    is_codequery_indexable_ext, is_codequery_indexable_filename, is_codequery_indexable_path,
};
