//! Composition root: wiring concrete tools into [`crate::tool_registry::ToolRegistry`].

use crate::tool_registry::ToolRegistry;

/// Constructs the tool registry with all available MCP tools.
pub fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register::<crate::tools::PingTool>();
    registry.register::<crate::tools::WebFetchTool>();
    registry.register::<crate::tools::SearchTool>();
    registry.register::<crate::tools::CodeQueryTool>();
    registry.register::<crate::tools::ReadTool>();
    registry.register::<crate::tools::EditTool>();
    registry.register::<crate::tools::WriteTool>();
    registry.register::<crate::tools::DeleteTool>();
    registry.register::<crate::tools::GlobTool>();
    registry.register::<crate::tools::MoveTool>();
    registry.register::<crate::tools::CopyTool>();
    registry.register::<crate::tools::ListDirTool>();
    registry.register::<crate::tools::BuildTool>();
    registry.register::<crate::tools::TestTool>();
    registry.register::<crate::tools::OutlineTool>();
    registry.register::<crate::tools::PwshTool>();
    registry.register::<crate::tools::GitStatusTool>();
    registry.register::<crate::tools::GitDiffTool>();
    registry.register::<crate::tools::GitRestoreTool>();
    registry.register::<crate::tools::GitAddTool>();
    registry.register::<crate::tools::GitCommitTool>();
    registry.register::<crate::tools::GitLogTool>();
    registry.register::<crate::tools::GitBranchTool>();
    registry.register::<crate::tools::GitCheckoutTool>();
    registry.register::<crate::tools::GitStashTool>();
    registry.register::<crate::tools::GitShowTool>();
    registry.register::<crate::tools::GitBlameTool>();

    registry
}
