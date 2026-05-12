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
}
