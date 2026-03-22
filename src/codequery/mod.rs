//! Compatibility facade: CodeQuery MCP tool implementation lives in [`crate::application::codequery_tool`].
//! Vector store name cache is [`crate::codequery_cache`].

pub use crate::application::codequery_tool::handle_code_query;
#[allow(unused_imports)]
pub use crate::codequery_cache::{cache_store_id, load_store_id_from_cache};
