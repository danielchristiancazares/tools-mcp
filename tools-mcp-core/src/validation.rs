//! Input validation and value clamping utilities.
//!
//! Provides reusable validators and clamping helpers for MCP tool parameters,
//! reducing duplication across tool implementations.

use crate::tool_outcome::ToolCallOutcome;
use serde_json::Value;

/// Validate that a string contains at least one non-whitespace character.
#[inline]
pub fn validate_non_empty(
    value: &str,
    field_name: &str,
    _id: Option<Value>,
) -> Result<(), ToolCallOutcome> {
    if value.chars().all(char::is_whitespace) {
        return Err(ToolCallOutcome::err(format_args!(
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

#[cfg(test)]
mod tests {
    use super::{clamp_bytes, clamp_limit, clamp_timeout, validate_non_empty};
    use serde_json::json;

    #[test]
    fn validate_non_empty_rejects_empty_and_unicode_whitespace_with_error_contract() {
        for (value, field_name, expected_message) in [
            ("", "path", "path is required (non-empty string)"),
            (
                " \t\r\n",
                "pattern",
                "pattern is required (non-empty string)",
            ),
            (
                "\u{2003}\u{2009}",
                "query",
                "query is required (non-empty string)",
            ),
        ] {
            let outcome = validate_non_empty(value, field_name, Some(json!({"ignored": true})))
                .expect_err("whitespace-only values must be rejected");

            assert_eq!(
                outcome.0,
                json!({
                    "content": [{"type": "text", "text": expected_message}],
                    "isError": true
                })
            );
        }
    }

    #[test]
    fn validate_non_empty_accepts_values_containing_non_whitespace() {
        for value in ["x", " x ", "\u{2003}x", "x\u{2009}", "0"] {
            validate_non_empty(value, "path", None).expect("non-empty value should be accepted");
        }
    }

    #[test]
    fn clamp_helpers_apply_defaults_and_bounds() {
        assert_eq!(clamp_timeout(None, 10, 1, 100), 10);
        assert_eq!(clamp_timeout(Some(0), 10, 1, 100), 1);
        assert_eq!(clamp_timeout(Some(101), 10, 1, 100), 100);

        assert_eq!(clamp_bytes(None, 10, 100), 10);
        assert_eq!(clamp_bytes(Some(0), 10, 100), 1);
        assert_eq!(clamp_bytes(Some(101), 10, 100), 100);

        assert_eq!(clamp_limit(None, 10, 5, 100), 10);
        assert_eq!(clamp_limit(Some(4), 10, 5, 100), 5);
        assert_eq!(clamp_limit(Some(101), 10, 5, 100), 100);
    }
}
