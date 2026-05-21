//! Tool handler implementations.
//!
//! This module contains the core implementation logic for various MCP tools.
//! Each handler is imported by its corresponding tool definition in the parent module.
//!
//! # Structure
//!
//! - [`read_file`]: File reading with line range support
//! - [`ripgrep`]: Content search using ugrep

mod read_file;
mod ripgrep;
mod search_contract;
mod search_file_selection;
mod search_memory;
#[cfg(test)]
mod search_parity;

pub use read_file::handle_read_file;
pub use ripgrep::handle_search;

pub(crate) fn start_search_cache_warmer() {
    search_memory::start_search_cache_warmer();
}
