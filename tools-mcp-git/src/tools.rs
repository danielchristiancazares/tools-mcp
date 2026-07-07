use crate::git::{
    handle_git_add, handle_git_apply, handle_git_blame, handle_git_branch, handle_git_checkout,
    handle_git_commit, handle_git_diff, handle_git_hunks, handle_git_log, handle_git_restore,
    handle_git_show, handle_git_snapshot, handle_git_stage_hunks, handle_git_stash,
    handle_git_status,
};
use tools_mcp_core::{
    ToolRegistry,
    config::{MAX_GIT_PATHSPECS, MAX_GIT_SELECTED_HUNKS, MAX_GIT_STDIN_BYTES},
    define_mcp_tool,
};

define_mcp_tool! {
    GitSnapshotTool,
    name: "git_snapshot",
    description: "Return a concise read-only Git worktree snapshot: porcelain status, counts, and staged/unstaged diff stats.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory for the git commands"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds for each git command"},
            "untracked": {"type": "boolean", "default": true, "description": "Include untracked files in status output (when false, uses `-uno`)"},
            "include_diff_stats": {"type": "boolean", "default": false, "description": "Include unstaged and staged `git diff --stat` summaries (opt-in)"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional path list to snapshot (passed after `--`)"}
        },
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_snapshot
}

define_mcp_tool! {
    GitStatusTool,
    name: "GitStatus",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_status
}

define_mcp_tool! {
    GitDiffTool,
    name: "GitDiff",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_diff
}

define_mcp_tool! {
    GitApplyTool,
    name: "GitApply",
    description: "Apply a supported tracked-file textual unified diff through git apply with explicit target and stdin-fed patch bytes.",
    schema: {
        "type": "object",
        "properties": {
            "patch": {"type": "string", "minLength": 1, "maxLength": MAX_GIT_STDIN_BYTES, "description": "Unified diff patch to feed to git apply on stdin"},
            "target": {"type": "string", "enum": ["cached", "index_worktree", "worktree"], "default": "cached", "description": "Apply target: cached=index only, index_worktree=--index, worktree=working tree only"},
            "check_only": {"type": "boolean", "default": false, "description": "Run git apply --check without mutating"},
            "reverse": {"type": "boolean", "default": false, "description": "Reverse-apply the patch (-R)"},
            "three_way": {"type": "boolean", "default": false, "description": "Use git apply --3way (valid only for cached and index_worktree)"},
            "recount": {"type": "boolean", "default": true, "description": "Pass --recount"},
            "unidiff_zero": {"type": "boolean", "default": false, "description": "Pass --unidiff-zero"},
            "whitespace": {"type": "string", "enum": ["nowarn", "warn", "fix", "error", "error-all"], "default": "nowarn", "description": "git apply whitespace mode"},
            "working_dir": {"type": "string", "description": "Repository root working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"}
        },
        "required": ["patch"],
        "additionalProperties": false
    },
    handler: handle_git_apply
}

define_mcp_tool! {
    GitHunksTool,
    name: "GitHunks",
    description: "Enumerate staged or unstaged unified-diff hunks with snapshot-scoped IDs for supported tracked text modifications.",
    schema: {
        "type": "object",
        "properties": {
            "staged": {"type": "boolean", "default": false, "description": "Enumerate staged hunks with git diff --cached when true"},
            "paths": {"type": "array", "maxItems": MAX_GIT_PATHSPECS, "items": {"type": "string", "minLength": 1}, "description": "Literal repo-relative POSIX path filters"},
            "context": {"type": "integer", "minimum": 0, "default": 3, "description": "Unified diff context lines"},
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum diff bytes captured before rejection"},
            "working_dir": {"type": "string", "description": "Repository root working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "include_advanced_templates": {"type": "boolean", "default": false, "description": "Include the stage_only template in addition to the recommended default template"}
        },
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_hunks
}

define_mcp_tool! {
    GitStageHunksTool,
    name: "GitStageHunks",
    description: "Stage or unstage selected GitHunks hunk IDs; default action prepares a verified commit-ready staged group.",
    schema: {
        "type": "object",
        "properties": {
            "diff_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$", "description": "diff_id returned by GitHunks"},
            "hunk_ids": {"type": "array", "minItems": 1, "maxItems": MAX_GIT_SELECTED_HUNKS, "uniqueItems": true, "items": {"type": "string", "maxLength": 96, "pattern": "^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.[0-9a-f]{64}$"}, "description": "Hunk IDs returned by GitHunks"},
            "action": {"type": "string", "enum": ["prepare_commit", "stage_only", "unstage"], "default": "prepare_commit", "description": "prepare_commit verifies a clean full index and returns a GitCommit template"},
            "context": {"type": "integer", "minimum": 0, "default": 3, "description": "Context value used by GitHunks"},
            "paths": {"type": "array", "maxItems": MAX_GIT_PATHSPECS, "items": {"type": "string", "minLength": 1}, "description": "Literal path scope used by GitHunks"},
            "max_bytes": {"type": "integer", "minimum": 1, "maximum": 5000000, "default": 200000, "description": "Maximum diff bytes captured during recompute"},
            "working_dir": {"type": "string", "description": "Repository root working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "commit_type": {"type": "string", "description": "Optional GitCommit type for the returned template"},
            "commit_scope": {"type": "string", "description": "Optional GitCommit scope for the returned template"},
            "commit_message": {"type": "string", "description": "Optional GitCommit message for the returned template"}
        },
        "required": ["diff_id", "hunk_ids"],
        "additionalProperties": false
    },
    handler: handle_git_stage_hunks
}

define_mcp_tool! {
    GitRestoreTool,
    name: "GitRestore",
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
        "required": ["paths"],
        "additionalProperties": false
    },
    handler: handle_git_restore
}

define_mcp_tool! {
    GitAddTool,
    name: "GitAdd",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_add
}

define_mcp_tool! {
    GitCommitTool,
    name: "GitCommit",
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
        "required": ["type", "message"],
        "additionalProperties": false
    },
    handler: handle_git_commit
}

define_mcp_tool! {
    GitLogTool,
    name: "GitLog",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_log
}

define_mcp_tool! {
    GitBranchTool,
    name: "GitBranch",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_branch
}

define_mcp_tool! {
    GitCheckoutTool,
    name: "GitCheckout",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_checkout
}

define_mcp_tool! {
    GitStashTool,
    name: "GitStash",
    description: "Stash changes in a dirty working directory.",
    schema: {
        "type": "object",
        "properties": {
            "working_dir": {"type": "string", "description": "Optional working directory"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 30000, "description": "Timeout in milliseconds"},
            "action": {"type": "string", "enum": ["push", "save", "pop", "apply", "drop", "list", "show", "clear"], "default": "push", "description": "Stash action to perform (save is an alias for push)"},
            "message": {"type": "string", "description": "Message for the stash (with push)"},
            "index": {"type": "integer", "minimum": 0, "description": "Stash index for pop/apply/drop/show"},
            "include_untracked": {"type": "boolean", "default": false, "description": "Include untracked files (with push)"}
        },
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_stash
}

define_mcp_tool! {
    GitShowTool,
    name: "GitShow",
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
        "required": [],
        "additionalProperties": false
    },
    handler: handle_git_show
}

define_mcp_tool! {
    GitBlameTool,
    name: "GitBlame",
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
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_git_blame
}

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<GitSnapshotTool>();
    registry.register::<GitStatusTool>();
    registry.register::<GitDiffTool>();
    registry.register::<GitApplyTool>();
    registry.register::<GitHunksTool>();
    registry.register::<GitStageHunksTool>();
    registry.register::<GitRestoreTool>();
    registry.register::<GitAddTool>();
    registry.register::<GitCommitTool>();
    registry.register::<GitLogTool>();
    registry.register::<GitBranchTool>();
    registry.register::<GitCheckoutTool>();
    registry.register::<GitStashTool>();
    registry.register::<GitShowTool>();
    registry.register::<GitBlameTool>();
}
