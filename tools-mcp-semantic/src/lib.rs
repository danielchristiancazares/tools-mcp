mod chunking;
mod discovery;
mod embedding;
mod manifest;
mod model;
mod store;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    tools::register_tools(registry);
}
