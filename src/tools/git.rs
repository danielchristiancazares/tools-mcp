use crate::define_mcp_tool;
use crate::git_tools::{handle_git_status, handle_git_diff, handle_git_restore, handle_git_add, handle_git_commit};

define_mcp_tool! {
    GitStatusTool,
    name: "GitStatus",
    aliases: ["git_status", "git-status"],
    description: "Show working tree status: staged, modified, and untracked files.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory for the git command"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds before the command is aborted"},
            "porcelain": {"type": "boolean", "default": true, "description": "Use porcelain output (`--porcelain=1`) when true"},
            "branch": {"type": "boolean", "default": true, "description": "Include branch info (`-b`) in porcelain mode"},
            "untracked": {"type": "boolean", "default": true, "description": "Include untracked files in porcelain mode (when false, uses `-uno`)"}
        },
        "required": []
    },
    handler: handle_git_status
}

define_mcp_tool! {
    GitDiffTool,
    name: "GitDiff",
    aliases: ["git_diff", "git-diff"],
    description: "Show file changes in the working tree or staging area.",
    schema: {
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
    },
    handler: handle_git_diff
}

define_mcp_tool! {
    GitRestoreTool,
    name: "GitRestore",
    aliases: ["git_restore", "git-restore"],
    description: "Discard uncommitted changes to specific files. WARNING: destructive.",
    schema: {
        "type": "object",
        "properties": {
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Paths to restore (passed after `--`)"},
            "working_dir": {"type": "string", "description": "Optional working directory for the git command"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds before the command is aborted"},
            "staged": {"type": "boolean", "default": false, "description": "Restore the index/staging area (`--staged`)"},
            "worktree": {"type": "boolean", "default": true, "description": "Restore the working tree (`--worktree`) (default true)"}
        },
        "required": ["paths"]
    },
    handler: handle_git_restore
}

define_mcp_tool! {
    GitAddTool,
    name: "GitAdd",
    aliases: ["git_add", "git-add"],
    description: "Stage files for commit.",
    schema: {
        "type": "object",
        "properties": {
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Files to stage"},
            "all": {"type": "boolean", "default": false, "description": "Stage all changes (`-A`)"},
            "update": {"type": "boolean", "default": false, "description": "Stage modified/deleted only (`-u`)"},
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"}
        },
        "required": []
    },
    handler: handle_git_add
}

define_mcp_tool! {
    GitCommitTool,
    name: "GitCommit",
    aliases: ["git_commit", "git-commit"],
    description: "Create a conventional commit (type(scope): message).",
    schema: {
        "type": "object",
        "properties": {
            "type": {"type": "string", "description": "Commit type: feat, fix, docs, style, refactor, test, chore, etc."},
            "scope": {"type": "string", "description": "Optional scope/area of change"},
            "message": {"type": "string", "description": "Commit description"},
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"}
        },
        "required": ["type", "message"]
    },
    handler: handle_git_commit
}
