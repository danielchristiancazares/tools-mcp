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

/// Clamp a timeout value to a valid range.
///
/// This helper reduces repeated `value.clamp(min, max)` patterns throughout
/// the codebase by providing a consistent way to validate timeout parameters
/// with configurable defaults and limits.
///
/// # Arguments
///
/// * `value` - Optional timeout in milliseconds to validate
/// * `default` - Default timeout to use if value is None
/// * `max` - Maximum allowed timeout value
///
/// # Returns
///
/// The timeout clamped to [100, max], with 100ms as minimum
///
/// # Example
///
/// ```ignore
/// let timeout = clamp_timeout(Some(5_000), DEFAULT_GIT_TIMEOUT_MS, MAX_GIT_TIMEOUT_MS);
/// ```
pub fn clamp_timeout(value: Option<u64>, default: u64, max: u64) -> u64 {
    value.unwrap_or(default).clamp(100, max)
}

/// Clamp a byte limit value to a valid range.
///
/// This helper reduces repeated `.clamp()` patterns for byte limit parameters
/// by providing a consistent way to validate output capture limits.
///
/// # Arguments
///
/// * `value` - Optional byte limit to validate
/// * `default` - Default limit to use if value is None
/// * `max` - Maximum allowed limit
///
/// # Returns
///
/// The byte limit clamped to [1, max]
///
/// # Example
///
/// ```ignore
/// let limit = clamp_bytes(Some(500_000), MAX_SCRIPT_STDOUT_BYTES, MAX_OUTPUT_BYTES);
/// ```
pub fn clamp_bytes(value: Option<usize>, default: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(1, max)
}
