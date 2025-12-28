/// Macro to parse RPC request parameters with error handling.
///
/// This macro consolidates the repeated pattern of parsing RPC request parameters
/// and returning early on error. It reduces ~4 lines of boilerplate to a single
/// macro call.
///
/// # Syntax
///
/// ```ignore
/// parse_rpc_request!(RequestType, id, args);
/// ```
///
/// # Expands to
///
/// ```ignore
/// let req = match RpcResponse::parse::<RequestType>(id.clone(), args) {
///     Ok(req) => req,
///     Err(resp) => return resp,
/// };
/// ```
///
/// # Arguments
///
/// * `$req_type` - The request struct type to deserialize into
/// * `$id` - The RPC request ID (typically `Option<Value>`)
/// * `$args` - The request arguments as serde_json::Value
///
/// # Note
///
/// The macro assumes `RpcResponse` is in scope. The `$id` parameter is cloned
/// automatically by the macro.
///
/// # Example
///
/// ```ignore
/// async fn handle_something(id: Option<Value>, args: Value) -> RpcResponse<'static> {
///     #[derive(Deserialize)]
///     struct Request { pattern: String }
///
///     parse_rpc_request!(Request, id, args);
///     // req is now available
///
///     RpcResponse::ok(id, json!({"result": "ok"}))
/// }
/// ```
#[macro_export]
macro_rules! parse_rpc_request {
    ($req_type:ty, $id:expr, $args:expr) => {
        let req = match $crate::RpcResponse::parse::<$req_type>($id.clone(), $args) {
            Ok(req) => req,
            Err(resp) => return resp,
        };
    };
}
