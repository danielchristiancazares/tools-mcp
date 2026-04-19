mod adapters;
mod codequery_cache;
mod discovery;
mod ports;
mod store_resolution;
mod tool_handler;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<tools::CodeQueryTool>();
}
