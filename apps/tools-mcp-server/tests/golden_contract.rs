//! Golden-style contract tests: freeze observable MCP protocol and tool response shapes.
//! These complement `integration_test.rs` with explicit structural assertions.

mod support;

use serde_json::{Value, json};
use support::send_mcp_message;

#[test]
fn golden_initialize_has_tools_capabilities_and_protocol_version() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9001,
        "method": "mcp/initialize",
        "params": {}
    });
    let response = send_mcp_message(&request).expect("initialize");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9001);
    let result = response["result"].as_object().expect("result object");
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(result["serverInfo"]["name"], "tools-mcp-server");
    assert!(result["serverInfo"]["version"].is_string());
    let caps = result["capabilities"].as_object().expect("capabilities");
    assert_eq!(caps["tools"]["list"], true);
    assert_eq!(caps["tools"]["call"], true);
    let tools = result["tools"].as_array().expect("tools in init");
    assert!(
        tools.len() >= 18,
        "initialize must embed full tool list (min 18), got {}",
        tools.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"CodeQuery"));
    assert!(names.contains(&"WebFetch"));
}

#[test]
fn golden_tools_call_accepts_nested_call_shape() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9002,
        "method": "mcp/tools/call",
        "params": {
            "call": {
                "name": "Ping",
                "arguments": {}
            }
        }
    });
    let response = send_mcp_message(&request).expect("nested call");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9002);
    assert_eq!(response["result"]["content"][0]["text"], "pong");
}

#[test]
fn golden_tools_call_accepts_toolname_and_args_aliases() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9003,
        "method": "mcp/tools/call",
        "params": {
            "toolName": "Ping",
            "args": {}
        }
    });
    let response = send_mcp_message(&request).expect("alias call");
    assert_eq!(response["result"]["content"][0]["text"], "pong");
}

#[test]
fn golden_unknown_method_returns_protocol_error() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9004,
        "method": "mcp/does_not_exist",
        "params": {}
    });
    let response = send_mcp_message(&request).expect("unknown method");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9004);
    assert!(response["result"].is_null());
    let err = response["error"].as_object().expect("protocol error");
    assert_eq!(err["code"], -32601);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("Method not found")
    );
}

#[test]
fn golden_unknown_tool_returns_protocol_error() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9005,
        "method": "mcp/tools/call",
        "params": {
            "name": "DefinitelyNotAToolName",
            "arguments": {}
        }
    });
    let response = send_mcp_message(&request).expect("unknown tool");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9005);
    assert!(response["result"].is_null());
    let err = response["error"].as_object().expect("protocol error");
    assert_eq!(err["code"], -32601);
    assert!(err["message"].as_str().unwrap().contains("Unknown tool"));
}

#[test]
fn golden_tools_list_returns_tools_array() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9006,
        "method": "mcp/tools/list",
        "params": {}
    });
    let response = send_mcp_message(&request).expect("tools list");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9006);
    let tools = response["result"]["tools"].as_array().expect("tools array");
    assert!(
        tools.len() >= 18,
        "tools/list must return full tool list, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"Ping"));
}

#[test]
fn golden_all_object_tool_schemas_disallow_unknown_fields() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9009,
        "method": "mcp/tools/list",
        "params": {}
    });
    let response = send_mcp_message(&request).expect("tools list");
    let tools = response["result"]["tools"].as_array().expect("tools array");

    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("<unknown>");
        let schema = tool["inputSchema"].as_object().expect("inputSchema object");
        if schema.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "tool {name} must set additionalProperties=false"
            );
        }
    }
}

#[test]
fn golden_webfetch_blocks_localhost_ssrf() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9007,
        "method": "mcp/tools/call",
        "params": {
            "name": "WebFetch",
            "arguments": {
                "url": "http://localhost:1234/"
            }
        }
    });
    let response = send_mcp_message(&request).expect("webfetch ssrf");
    assert_eq!(response["jsonrpc"], "2.0");
    let result = response["result"].as_object().expect("tool result");
    assert_eq!(result["isError"], true);
    let content = result["content"].as_array().expect("content");
    let text = content[0]["text"].as_str().expect("text");
    assert!(
        text.contains("WebFetch blocked") || text.contains("safety"),
        "unexpected WebFetch error text: {text}"
    );
    assert_eq!(result["error_type"], "ssrf_blocked");
}

#[test]
fn golden_edit_invalid_args_returns_tool_error() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9008,
        "method": "mcp/tools/call",
        "params": {
            "name": "Edit",
            "arguments": {
                "path": "nope.txt",
                "old_snippet": "x"
            }
        }
    });
    let response = send_mcp_message(&request).expect("edit invalid");
    assert_eq!(response["jsonrpc"], "2.0");
    let result = response["result"].as_object().expect("tool result");
    assert_eq!(result["isError"], true);
}
