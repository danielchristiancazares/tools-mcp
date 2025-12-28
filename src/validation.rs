use crate::RpcResponse;
use serde_json::Value;

/// Validate that a string is not empty after trimming whitespace.
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
