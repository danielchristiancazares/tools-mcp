mod adapters;
mod ports;
mod services;
mod tools;
mod webfetch_tool;

mod webfetch;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<tools::WebFetchTool>();
}

#[doc(hidden)]
pub fn benchmark_browser_available() -> bool {
    webfetch::browser::BrowserPool::is_available()
}
