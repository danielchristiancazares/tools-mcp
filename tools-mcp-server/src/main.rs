//! MCP server binary: stdin/stdout JSON-RPC loop plus feature-crate composition.

use anyhow::Result;
use tokio::io::{self, BufReader};
use tools_mcp_core::{
    RpcResponse, read_mcp_message, should_skip_headers, write_mcp_payload_with_mode,
    write_mcp_response_with_mode,
};
use tracing::{error, info};

use crate::composition::build_tool_registry;
use crate::mcp_server::{
    ParseRpcRequestError, RpcMessage, dispatch_jsonrpc_batch, dispatch_jsonrpc_request,
    parse_rpc_message,
};

mod composition;
mod gemini_gate;
mod mcp_server;
mod ping;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout;
    let reader = BufReader::new(stdin);
    let mut reader = reader;

    let registry = build_tool_registry();
    let tools = registry.list();

    loop {
        let message = match read_mcp_message(&mut reader).await {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(read_err) => {
                error!("failed to read MCP message: {}", read_err.error);
                let parse_error = RpcResponse::protocol_error(None, -32700, "Parse error");
                let skip_headers = if read_err.response_has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) =
                    write_mcp_response_with_mode(&mut writer, &parse_error, skip_headers).await
                {
                    error!("failed to write parse error response: {}", write_err);
                    break;
                }
                if read_err.should_continue {
                    continue;
                }
                break;
            }
        };
        let line = message.body;
        if line.trim().is_empty() {
            continue;
        }

        let message_kind = match parse_rpc_message(&line) {
            Ok(RpcMessage::Request(r)) => {
                info!("Received request: method={}, id={:?}", &r.method, &r.id);
                RpcMessage::Request(r)
            }
            Ok(RpcMessage::Batch(items)) => {
                info!("Received batch request with {} item(s)", items.len());
                RpcMessage::Batch(items)
            }
            Ok(RpcMessage::Response) => {
                info!("Received JSON-RPC response");
                continue;
            }
            Err(ParseRpcRequestError::Parse(e)) => {
                error!("invalid json: {}", e);
                let response = RpcResponse::protocol_error(None, -32700, "Parse error");
                let skip_headers = if message.has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) =
                    write_mcp_response_with_mode(&mut writer, &response, skip_headers).await
                {
                    error!("failed to write parse error response: {}", write_err);
                    break;
                }
                continue;
            }
            Err(ParseRpcRequestError::InvalidRequest { id, message: msg }) => {
                error!("invalid request: {}", msg);
                let response = RpcResponse::protocol_error(id, -32600, msg);
                let skip_headers = if message.has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) =
                    write_mcp_response_with_mode(&mut writer, &response, skip_headers).await
                {
                    error!("failed to write invalid request response: {}", write_err);
                    break;
                }
                continue;
            }
        };

        let tools_slice: Vec<_> = tools.clone();
        let skip_headers = if message.has_headers {
            false
        } else {
            should_skip_headers()
        };

        let should_exit = match message_kind {
            RpcMessage::Request(req) => {
                let Some((resp, should_exit)) =
                    dispatch_jsonrpc_request(req, &registry, &tools_slice).await
                else {
                    continue;
                };

                info!("Sending response for request id: {:?}", resp.id);
                if let Err(e) = write_mcp_response_with_mode(&mut writer, &resp, skip_headers).await
                {
                    error!("failed to write MCP response: {}", e);
                    break;
                }
                should_exit
            }
            RpcMessage::Batch(items) => {
                let Some((responses, should_exit)) =
                    dispatch_jsonrpc_batch(items, &registry, &tools_slice).await
                else {
                    continue;
                };

                info!("Sending batch response with {} item(s)", responses.len());
                if let Err(e) =
                    write_mcp_payload_with_mode(&mut writer, &responses, skip_headers).await
                {
                    error!("failed to write MCP batch response: {}", e);
                    break;
                }
                should_exit
            }
            RpcMessage::Response => continue,
        };
        info!("Response sent successfully");

        if should_exit {
            info!("shutdown requested");
            break;
        }
    }

    Ok(())
}
