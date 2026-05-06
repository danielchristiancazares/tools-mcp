mod smart_file_edit;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    tools::register_tools(registry);
}

pub fn start_search_cache_warmer() {
    tools::start_search_cache_warmer();
}
