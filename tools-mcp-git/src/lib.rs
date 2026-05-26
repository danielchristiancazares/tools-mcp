mod git;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    if std::env::var("MCP_ENABLE_GIT").ok().as_deref() != Some("true") {
        return;
    }
    tools::register_tools(registry);
}

/// Bench-only surface for parser hot-path measurements.
#[cfg(feature = "bench-api")]
#[doc(hidden)]
pub mod bench {
    pub fn parse_diff_manifest_weight(name_status: &str, numstat: &str) -> usize {
        crate::git::benchmark_parse_diff_manifest(name_status, numstat)
    }
}
