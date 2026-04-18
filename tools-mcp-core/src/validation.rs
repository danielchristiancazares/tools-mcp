//! Input validation and value clamping utilities.
//!
//! Provides reusable validators and clamping helpers for MCP tool parameters,
//! reducing duplication across tool implementations.

use crate::tool_outcome::ToolCallOutcome;
use serde_json::Value;

/// Validate that a string is not empty after trimming whitespace.
pub fn validate_non_empty(
    value: &str,
    field_name: &str,
    _id: Option<Value>,
) -> Result<(), ToolCallOutcome> {
    if value.trim().is_empty() {
        return Err(ToolCallOutcome::err(format!(
            "{field_name} is required (non-empty string)"
        )));
    }
    Ok(())
}

/// Clamp a timeout value to a valid range, applying a default if `None`.
///
/// Common pattern for tool timeout parameters.
#[inline]
pub fn clamp_timeout(value: Option<u64>, default: u64, min: u64, max: u64) -> u64 {
    value.unwrap_or(default).clamp(min, max)
}

/// Clamp a byte limit to a valid range, applying a default if `None`.
///
/// Ensures byte limits stay within configured bounds.
#[inline]
pub fn clamp_bytes(value: Option<usize>, default: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(1, max)
}

/// Clamp an optional count/limit value, applying a default if `None`.
///
/// Generic clamping for result limits, context lines, etc.
#[inline]
pub fn clamp_limit(value: Option<usize>, default: usize, min: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(min, max)
}
