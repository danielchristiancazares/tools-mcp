mod git;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    if std::env::var("MCP_ENABLE_GIT").ok().as_deref() != Some("true") {
        return;
    }
    tools::register_tools(registry);
}
