//! # MCP File Search Server
//!
//! JSON-RPC 2.0 over stdin/stdout. Composition root: tool wiring and I/O loop live here;
//! routing is delegated to [`crate::adapters::inbound`].

use anyhow::Result;
use tokio::io::{self, BufReader};
use tracing::{error, info};

use crate::adapters::inbound::{RpcRequest, build_tool_registry, dispatch_jsonrpc_request};
use mcp_protocol::{read_mcp_message, should_skip_headers, write_mcp_response_with_mode};

mod adapters;
mod application;
mod codequery;
mod codequery_cache;
mod config;
mod git;
mod mcp_protocol;
mod ports;
mod process_utils;
mod response;
mod smart_file_edit;
mod tool_registry;
mod tools;
mod validation;
mod webfetch;

pub use response::{RpcError, RpcResponse};

/// Re-export of the `file_search_core` library as `crate::core` for tool modules.
#[doc(hidden)]
pub mod core {
    pub use file_search_core::*;
}

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

    while let Some(message) = match read_mcp_message(&mut reader).await {
        Ok(v) => v,
        Err(e) => {
            error!("failed to read MCP message: {}", e);
            None
        }
    } {
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
                let resp = RpcResponse::protocol_error(None, -32700, "Parse error");
                let skip_headers = if message.has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) =
                    write_mcp_response_with_mode(&mut writer, &resp, skip_headers).await
                {
                    error!("failed to write parse-error response: {}", write_err);
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
