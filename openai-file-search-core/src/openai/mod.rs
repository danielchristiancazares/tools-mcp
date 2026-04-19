//! `OpenAI` API client submodules (crate-internal).
//!
//! - [`file_ext`]: File extension validation for uploads and `CodeQuery` indexing
//! - [`hash`]: SHA256 hashing utilities for content change detection
//! - [`types`]: API type definitions (responses, configuration, metadata)

pub(crate) mod file_ext;
pub(crate) mod hash;
pub(crate) mod types;
