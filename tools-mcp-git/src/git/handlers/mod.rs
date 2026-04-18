mod diff;
mod inspect;
mod mutating;
mod status;

pub use diff::handle_git_diff;
pub use inspect::{handle_git_blame, handle_git_log, handle_git_show};
pub use mutating::{
    handle_git_add, handle_git_branch, handle_git_checkout, handle_git_commit, handle_git_restore,
    handle_git_stash,
};
pub use status::handle_git_status;
