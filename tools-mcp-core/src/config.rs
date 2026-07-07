//! Shared configuration constants across tools.

/// Default timeout for git commands.
pub const DEFAULT_GIT_TIMEOUT_MS: u64 = 30_000;

/// Maximum git timeout.
pub const MAX_GIT_TIMEOUT_MS: u64 = 300_000;

/// Default stdout capture for git commands.
pub const DEFAULT_GIT_STDOUT_BYTES: usize = 200_000;

/// Default stderr capture for git commands.
pub const DEFAULT_GIT_STDERR_BYTES: usize = 100_000;

/// Maximum output capture limit.
pub const MAX_OUTPUT_BYTES: usize = 5_000_000;

/// Maximum stdin payload for git commands that explicitly consume stdin.
pub const MAX_GIT_STDIN_BYTES: usize = MAX_OUTPUT_BYTES;

/// Maximum number of patch-header paths accepted by git patch tools.
pub const MAX_GIT_PATCH_PATHS: usize = 1_000;

/// Maximum number of literal path filters accepted by git hunk tools.
pub const MAX_GIT_PATHSPECS: usize = 1_000;

/// Maximum total UTF-8 bytes across literal path filters for git hunk tools.
pub const MAX_GIT_PATHSPEC_BYTES: usize = 16_384;

/// Conservative maximum argument-vector byte budget for git hunk/apply tools.
pub const MAX_GIT_ARG_BYTES: usize = 24_000;

/// Maximum number of selected hunk IDs accepted by `GitStageHunks`.
pub const MAX_GIT_SELECTED_HUNKS: usize = 10_000;

/// Maximum number of files parsed from one git diff for hunk enumeration.
pub const MAX_GIT_DIFF_FILES: usize = 1_000;

/// Maximum number of hunks parsed from one git diff for hunk enumeration.
pub const MAX_GIT_DIFF_HUNKS: usize = 10_000;

/// Maximum total raw hunk body bytes parsed from one git diff.
pub const MAX_GIT_HUNK_BODY_BYTES: usize = 4_000_000;

/// Maximum estimated structured JSON response size for git hunk enumeration.
pub const MAX_GIT_STRUCTURED_RESPONSE_BYTES: usize = 4_000_000;

/// Default limit for glob file matches.
pub const DEFAULT_GLOB_LIMIT: usize = 1000;

/// Maximum limit for glob file matches.
pub const MAX_GLOB_LIMIT: usize = 10_000;

/// Default timeout for `PowerShell` commands.
pub const DEFAULT_PWSH_TIMEOUT_MS: u64 = 60_000;

/// Maximum `PowerShell` command timeout.
pub const MAX_PWSH_TIMEOUT_MS: u64 = 300_000;

/// Maximum stdout capture for `PowerShell` commands.
pub const MAX_PWSH_STDOUT_BYTES: usize = 1_000_000;

/// Maximum stderr capture for `PowerShell` commands.
pub const MAX_PWSH_STDERR_BYTES: usize = 500_000;

/// Default cap for the `details` field in tool-level error payloads so a stray upstream
/// response can't flood the model's context.
pub const MAX_ERROR_DETAIL_CHARS: usize = 1200;
