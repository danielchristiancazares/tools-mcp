use crate::tool_registry::McpTool;
use crate::RpcResponse;
use crate::git_tools::{handle_git_status, handle_git_diff, handle_git_restore, handle_git_add, handle_git_commit};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub struct GitStatusTool;

impl McpTool for GitStatusTool {
    const NAME: &'static str = "GitStatus";
    const ALIASES: &'static [&'static str] = &["git_status", "git-status"];
    const DESCRIPTION: &'static str = "Show working tree status: staged, modified, and untracked files.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "working_dir": {"type": "string", "description": "Optional working directory for the git command"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds before the command is aborted"},
                "porcelain": {"type": "boolean", "default": true, "description": "Use porcelain output (`--porcelain=1`) when true"},
                "branch": {"type": "boolean", "default": true, "description": "Include branch info (`-b`) in porcelain mode"},
                "untracked": {"type": "boolean", "default": true, "description": "Include untracked files in porcelain mode (when false, uses `-uno`)"}
            },
            "required": []
        })
    }

    fn execute(id: Option<Value>, args: Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_git_status(id, args).await })
    }
}

pub struct GitDiffTool;

impl McpTool for GitDiffTool {
    const NAME: &'static str = "GitDiff";
    const ALIASES: &'static [&'static str] = &["git_diff", "git-diff"];
    const DESCRIPTION: &'static str = "Show file changes in the working tree or staging area.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "working_dir": {"type": "string", "description": "Optional working directory for the git command"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds before the command is aborted"},
                "cached": {"type": "boolean", "default": false, "description": "Diff staged changes (`--cached`)"},
                "stat": {"type": "boolean", "default": false, "description": "Show diffstat only (`--stat`)"},
                "name_only": {"type": "boolean", "default": false, "description": "Show only changed file names (`--name-only`)"},
                "unified": {"type": "integer", "minimum": 0, "description": "Number of context lines (`-U<N>`)"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional path list to diff (passed after `--`)"},
                "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum bytes captured from stdout before truncation"}
            },
            "required": []
        })
    }

    fn execute(id: Option<Value>, args: Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_git_diff(id, args).await })
    }
}

pub struct GitRestoreTool;

impl McpTool for GitRestoreTool {
    const NAME: &'static str = "GitRestore";
    const ALIASES: &'static [&'static str] = &["git_restore", "git-restore"];
    const DESCRIPTION: &'static str = "Discard uncommitted changes to specific files. WARNING: destructive.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Paths to restore (passed after `--`)"},
                "working_dir": {"type": "string", "description": "Optional working directory for the git command"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds before the command is aborted"},
                "staged": {"type": "boolean", "default": false, "description": "Restore the index/staging area (`--staged`)"},
                "worktree": {"type": "boolean", "default": true, "description": "Restore the working tree (`--worktree`) (default true)"}
            },
            "required": ["paths"]
        })
    }

    fn execute(id: Option<Value>, args: Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_git_restore(id, args).await })
    }
}

pub struct GitAddTool;

impl McpTool for GitAddTool {
    const NAME: &'static str = "GitAdd";
    const ALIASES: &'static [&'static str] = &["git_add", "git-add"];
    const DESCRIPTION: &'static str = "Stage files for commit.";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files to stage"},
                "all": {"type": "boolean", "default": false, "description": "Stage all changes (`-A`)"},
                "update": {"type": "boolean", "default": false, "description": "Stage modified/deleted only (`-u`)"},
                "working_dir": {"type": "string", "description": "Optional working directory"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"}
            },
            "required": []
        })
    }

    fn execute(id: Option<Value>, args: Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_git_add(id, args).await })
    }
}

pub struct GitCommitTool;

impl McpTool for GitCommitTool {
    const NAME: &'static str = "GitCommit";
    const ALIASES: &'static [&'static str] = &["git_commit", "git-commit"];
    const DESCRIPTION: &'static str = "Create a conventional commit (type(scope): message).";

    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "type": {"type": "string", "description": "Commit type: feat, fix, docs, style, refactor, test, chore, etc."},
                "scope": {"type": "string", "description": "Optional scope/area of change"},
                "message": {"type": "string", "description": "Commit description"},
                "working_dir": {"type": "string", "description": "Optional working directory"},
                "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"}
            },
            "required": ["type", "message"]
        })
    }

    fn execute(id: Option<Value>, args: Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> {
        Box::pin(async move { handle_git_commit(id, args).await })
    }
}
