mod path_policy;
mod smart_file_edit;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    tools::register_tools(registry);
}

pub fn start_search_cache_warmer() {
    tools::start_search_cache_warmer();
}

#[doc(hidden)]
pub fn benchmark_render_numbered_range(
    bytes: &[u8],
    start_line: usize,
    resolved_end: usize,
) -> usize {
    tools::benchmark_render_numbered_range(bytes, start_line, resolved_end)
}
