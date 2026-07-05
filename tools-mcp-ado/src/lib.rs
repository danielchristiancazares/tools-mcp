mod tools;
mod work_items;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<tools::AdoWorkItemsTool>();
}
