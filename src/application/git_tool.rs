//! Application-layer entry points for Git MCP tools (delegates to [`crate::git`]).

#[allow(unused_imports)]
pub use crate::git::{
    handle_git_add, handle_git_blame, handle_git_branch, handle_git_checkout, handle_git_commit,
    handle_git_diff, handle_git_log, handle_git_restore, handle_git_show, handle_git_stash,
    handle_git_status,
};
