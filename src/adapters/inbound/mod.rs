//! Inbound adapters: MCP JSON-RPC framing, routing, and tool dispatch.

mod mcp_server;

pub use mcp_server::{RpcRequest, dispatch_jsonrpc_request};
