mod delete;
mod edit;
mod fileops;
mod glob;
mod handlers;
mod outline;
mod pwsh;
mod read;
mod search;
mod write;

use tools_mcp_core::ToolRegistry;

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
    registry.register::<pwsh::PwshTool>();
    registry.register::<search::SearchTool>();
}

pub fn start_search_cache_warmer() {
    handlers::start_search_cache_warmer();
}
