/// Shared configuration constants across tools.

/// Default timeout for script execution (build, test).
pub const DEFAULT_SCRIPT_TIMEOUT_MS: u64 = 120_000;

/// Maximum stdout capture for script execution.
pub const MAX_SCRIPT_STDOUT_BYTES: usize = 1_000_000;

/// Maximum stderr capture for script execution.
pub const MAX_SCRIPT_STDERR_BYTES: usize = 1_000_000;

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

/// Default limit for glob file matches.
pub const DEFAULT_GLOB_LIMIT: usize = 1000;

/// Maximum limit for glob file matches.
pub const MAX_GLOB_LIMIT: usize = 10_000;

/// Default timeout for PowerShell commands.
pub const DEFAULT_PWSH_TIMEOUT_MS: u64 = 60_000;

/// Maximum PowerShell command timeout.
pub const MAX_PWSH_TIMEOUT_MS: u64 = 300_000;

/// Maximum stdout capture for PowerShell commands.
pub const MAX_PWSH_STDOUT_BYTES: usize = 1_000_000;

/// Maximum stderr capture for PowerShell commands.
pub const MAX_PWSH_STDERR_BYTES: usize = 500_000;
