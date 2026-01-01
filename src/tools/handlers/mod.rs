//! Tool handler implementations.
//!
//! This module contains the core implementation logic for various MCP tools.
//! Each handler is imported by its corresponding tool definition in the parent module.
//!
//! # Structure
//!
//! - [`read_file`]: File reading with line range support
//! - [`ripgrep`]: Content search using ugrep
//! - [`script_runner`]: Build/test script execution

mod read_file;
mod ripgrep;
mod script_runner;

pub use read_file::handle_read_file;
pub use ripgrep::handle_ripgrep;
pub use script_runner::{ScriptConfig, run_script_tool};
