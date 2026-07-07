mod apply;
mod diff;
mod hunks;
mod inspect;
mod mutating;
mod snapshot;
mod status;

pub(crate) use apply::{handle_git_apply, handle_git_stage_hunks};
#[cfg(feature = "bench-api")]
pub(crate) use diff::benchmark_parse_diff_manifest;
pub(crate) use diff::handle_git_diff;
pub(crate) use hunks::handle_git_hunks;
pub(crate) use inspect::{handle_git_blame, handle_git_log, handle_git_show};
pub(crate) use mutating::{
    handle_git_add, handle_git_branch, handle_git_checkout, handle_git_commit, handle_git_restore,
    handle_git_stash,
};
pub(crate) use snapshot::handle_git_snapshot;
pub(crate) use status::handle_git_status;
