mod delete;
mod edit;
mod fileops;
mod glob;
mod handlers;
mod outline;
mod pwsh;
mod read;
pub(crate) mod scope_cache;
mod search;
mod search_context;
mod write;

use std::env;

use tools_mcp_core::ToolRegistry;
use tracing::warn;

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<read::ReadTool>();
    registry.register::<edit::EditTool>();
    registry.register::<write::WriteTool>();
    registry.register::<delete::DeleteTool>();
    registry.register::<glob::GlobTool>();
    registry.register::<fileops::MoveTool>();
    registry.register::<fileops::CopyTool>();
    registry.register::<fileops::ListDirTool>();
    registry.register::<outline::OutlineTool>();

    if env::var("MCP_ENABLE_PWSH_TOOL").ok().as_deref() == Some("true") {
        registry.register::<pwsh::PwshTool>();
    } else {
        warn!(
            "Pwsh tool is disabled by default; set MCP_ENABLE_PWSH_TOOL=true to enable host shell execution"
        );
    }

    registry.register::<search::SearchTool>();
    registry.register::<search_context::SearchContextTool>();
}

pub fn start_search_cache_warmer() {
    handlers::start_search_cache_warmer();
}

#[doc(hidden)]
pub(crate) fn benchmark_render_numbered_range(
    bytes: &[u8],
    start_line: usize,
    resolved_end: usize,
) -> usize {
    handlers::benchmark_render_numbered_range(bytes, start_line, resolved_end)
}
