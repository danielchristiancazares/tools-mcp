//! Workspace / filesystem tool use-case facades (delegates to existing modules; behavior unchanged).
//!
//! Git and process-backed tools remain in [`crate::git`] and [`crate::process_utils`]; this module
//! documents the application-boundary for local file editing.

#[allow(unused_imports)]
pub use crate::smart_file_edit::handle_edit;
