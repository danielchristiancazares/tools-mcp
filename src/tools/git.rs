use crate::define_mcp_tool;
use crate::git::{
    handle_git_add, handle_git_blame, handle_git_branch, handle_git_checkout, handle_git_commit,
    handle_git_diff, handle_git_log, handle_git_restore, handle_git_show, handle_git_stash,
    handle_git_status,
};

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
    description: "Show file changes in the working tree or staging area. When from_ref, to_ref, and output_dir are provided, writes per-file patches to the directory.",
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
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum bytes captured from stdout before truncation"},
            "from_ref": {"type": "string", "description": "Starting ref (tag/branch/commit) for ref-to-ref comparison"},
            "to_ref": {"type": "string", "description": "Ending ref (tag/branch/commit) for ref-to-ref comparison"},
            "output_dir": {"type": "string", "description": "Directory to write per-file patches (creates if missing). Required with from_ref/to_ref."}
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

define_mcp_tool! {
    GitLogTool,
    name: "GitLog",
    aliases: ["git_log", "git-log"],
    description: "Show commit history with configurable format and filters.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "max_count": {"type": "integer", "minimum": 1, "description": "Limit number of commits to show"},
            "oneline": {"type": "boolean", "default": false, "description": "Show each commit on a single line"},
            "format": {"type": "string", "description": "Pretty-print format (e.g., '%H %s' for hash and subject)"},
            "author": {"type": "string", "description": "Filter commits by author"},
            "since": {"type": "string", "description": "Show commits after date (e.g., '2024-01-01', '2 weeks ago')"},
            "until": {"type": "string", "description": "Show commits before date"},
            "grep": {"type": "string", "description": "Filter commits by message pattern"},
            "path": {"type": "string", "description": "Show commits affecting this path"},
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum output bytes"}
        },
        "required": []
    },
    handler: handle_git_log
}

define_mcp_tool! {
    GitBranchTool,
    name: "GitBranch",
    aliases: ["git_branch", "git-branch"],
    description: "List, create, rename, or delete branches.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "list_all": {"type": "boolean", "default": false, "description": "List both local and remote branches (`-a`)"},
            "list_remote": {"type": "boolean", "default": false, "description": "List only remote branches (`-r`)"},
            "create": {"type": "string", "description": "Create a new branch with this name"},
            "delete": {"type": "string", "description": "Delete this branch (`-d`, must be merged)"},
            "force_delete": {"type": "string", "description": "Force delete this branch (`-D`)"},
            "rename": {"type": "string", "description": "Rename this branch (requires new_name)"},
            "new_name": {"type": "string", "description": "New name when renaming a branch"}
        },
        "required": []
    },
    handler: handle_git_branch
}

define_mcp_tool! {
    GitCheckoutTool,
    name: "GitCheckout",
    aliases: ["git_checkout", "git-checkout"],
    description: "Switch branches or restore working tree files.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "branch": {"type": "string", "description": "Branch to switch to"},
            "create_branch": {"type": "string", "description": "Create and switch to a new branch (`-b`)"},
            "commit": {"type": "string", "description": "Checkout a specific commit (detached HEAD)"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Restore these paths from HEAD or specified commit"}
        },
        "required": []
    },
    handler: handle_git_checkout
}

define_mcp_tool! {
    GitStashTool,
    name: "GitStash",
    aliases: ["git_stash", "git-stash"],
    description: "Stash changes in a dirty working directory.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "action": {"type": "string", "enum": ["push", "pop", "apply", "drop", "list", "show", "clear"], "default": "push", "description": "Stash action to perform"},
            "message": {"type": "string", "description": "Message for the stash (with push)"},
            "index": {"type": "integer", "minimum": 0, "description": "Stash index for pop/apply/drop/show"},
            "include_untracked": {"type": "boolean", "default": false, "description": "Include untracked files (with push)"}
        },
        "required": []
    },
    handler: handle_git_stash
}

define_mcp_tool! {
    GitShowTool,
    name: "GitShow",
    aliases: ["git_show", "git-show"],
    description: "Show commit details and diff.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "commit": {"type": "string", "description": "Commit to show (default: HEAD)"},
            "stat": {"type": "boolean", "default": false, "description": "Show diffstat only"},
            "name_only": {"type": "boolean", "default": false, "description": "Show only names of changed files"},
            "format": {"type": "string", "description": "Pretty-print format for commit info"},
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum output bytes"}
        },
        "required": []
    },
    handler: handle_git_show
}

define_mcp_tool! {
    GitBlameTool,
    name: "GitBlame",
    aliases: ["git_blame", "git-blame"],
    description: "Show what revision and author last modified each line of a file.",
    schema: {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File path to blame"},
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "start_line": {"type": "integer", "minimum": 1, "description": "Start line number for range"},
            "end_line": {"type": "integer", "minimum": 1, "description": "End line number for range"},
            "commit": {"type": "string", "description": "Blame at specific commit instead of HEAD"},
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum output bytes"}
        },
        "required": ["path"]
    },
    handler: handle_git_blame
}
