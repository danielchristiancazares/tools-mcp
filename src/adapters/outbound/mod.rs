//! Outbound adapters implementing [`crate::ports`].

pub mod file_search_engine;
pub mod webfetch_default;

pub use file_search_engine::FileSearchCoreEngine;
pub use webfetch_default::RunFetchWebFetcher;
