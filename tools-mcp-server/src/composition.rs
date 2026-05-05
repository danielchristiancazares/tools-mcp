//! Composition root: wire feature crates into a single MCP tool registry.

use tools_mcp_core::ToolRegistry;

/// Constructs the tool registry with all available MCP tools.
pub fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register::<crate::ping::PingTool>();
    registry.register::<crate::gemini_gate::GeminiGateTool>();
    tools_mcp_webfetch::register_tools(&mut registry);
    tools_mcp_local::register_tools(&mut registry);
    tools_mcp_codequery::register_tools(&mut registry);
    tools_mcp_git::register_tools(&mut registry);

    registry
}
