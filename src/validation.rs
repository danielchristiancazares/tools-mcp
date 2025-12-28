use crate::RpcResponse;
use serde_json::Value;
use std::path::Path;

/// Validate that a string is not empty after trimming whitespace
///
/// # Arguments
/// * `value` - The string to validate
/// * `field_name` - The name of the field (for error messages)
/// * `id` - The RPC request ID (for error responses)
///
/// # Returns
/// Ok(()) if the string is non-empty after trimming, or an error response
pub fn validate_non_empty(
    value: &str,
    field_name: &str,
    id: Option<Value>,
) -> Result<(), RpcResponse<'static>> {
    if value.trim().is_empty() {
        return Err(RpcResponse::err(id, format!("{} is required", field_name)));
    }
    Ok(())
}

/// Validate that a path exists
///
/// # Arguments
/// * `path` - The path to validate
/// * `id` - The RPC request ID (for error responses)
///
/// # Returns
/// Ok(()) if the path exists, or an error response
pub fn validate_path_exists(
    path: &Path,
    id: Option<Value>,
) -> Result<(), RpcResponse<'static>> {
    if !path.exists() {
        return Err(RpcResponse::err(
            id,
            format!("path does not exist: {}", path.display()),
        ));
    }
    Ok(())
}

/// Validate that a path points to a file (not a directory)
///
/// # Arguments
/// * `path` - The path to validate
/// * `id` - The RPC request ID (for error responses)
///
/// # Returns
/// Ok(()) if the path is a file, or an error response
pub fn validate_is_file(
    path: &Path,
    id: Option<Value>,
) -> Result<(), RpcResponse<'static>> {
    if !path.is_file() {
        return Err(RpcResponse::err(
            id,
            format!(
                "path is not a file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}
