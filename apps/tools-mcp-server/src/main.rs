//! MCP server binary: stdin/stdout JSON-RPC loop plus feature-crate composition.

use anyhow::Result;
use tokio::io::{self, BufReader};
use tools_mcp_core::{
    RpcResponse, read_mcp_message, should_skip_headers, write_mcp_response_with_mode,
};
use tracing::{error, info};

use crate::composition::build_tool_registry;
use crate::mcp_server::{RpcRequest, dispatch_jsonrpc_request};

mod composition;
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

        let req: RpcRequest = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(r) => {
                info!("Received request: method={}, id={:?}", &r.method, &r.id);
                r
            }
            Err(e) => {
                error!("invalid json: {}", e);
                error!("parse error details: {e}");
                let parse_error = RpcResponse::protocol_error(None, -32700, "Parse error");
                let skip_headers = if message.has_headers {
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
                continue;
            }
        };

        let tools_slice: Vec<_> = tools.clone();

        let Some((resp, should_exit)) =
            dispatch_jsonrpc_request(req, &registry, &tools_slice).await
        else {
            continue;
        };

        info!("Sending response for request id: {:?}", resp.id);
        let skip_headers = if message.has_headers {
            false
        } else {
            should_skip_headers()
        };
        if let Err(e) = write_mcp_response_with_mode(&mut writer, &resp, skip_headers).await {
            error!("failed to write MCP response: {}", e);
            break;
        }
        info!("Response sent successfully");

        if should_exit {
            info!("shutdown requested");
            break;
        }
    }

    Ok(())
}
