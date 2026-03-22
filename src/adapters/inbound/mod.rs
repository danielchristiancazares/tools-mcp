//! Inbound adapters: MCP JSON-RPC framing, routing, and tool dispatch.

mod mcp_server;

pub use mcp_server::{RpcRequest, build_tool_registry, dispatch_jsonrpc_request};
