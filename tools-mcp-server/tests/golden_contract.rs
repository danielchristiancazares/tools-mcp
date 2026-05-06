//! Golden-style contract tests: freeze observable MCP protocol and tool response shapes.
//! These complement `integration_test.rs` with explicit structural assertions.

mod support;

use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::{send_mcp_message, workspace_root};

fn documented_tool_names() -> BTreeSet<String> {
    let readme =
        std::fs::read_to_string(workspace_root().join("README.md")).expect("README.md readable");

    readme
        .lines()
        .filter_map(|line| {
            line.split("**Tool name**: `")
                .nth(1)
                .and_then(|rest| rest.split('`').next())
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn readme_text() -> String {
    std::fs::read_to_string(workspace_root().join("README.md")).expect("README.md readable")
}

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
        tools.len() >= 17,
        "initialize must embed full tool list (min 17), got {}",
        tools.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(!names.contains(&"CodeQuery"));
    assert!(names.contains(&"GeminiGate"));
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
fn golden_batch_returns_responses_for_requests_only() {
    let request = json!([
        {
            "jsonrpc": "2.0",
            "id": 9012,
            "method": "ping",
            "params": {}
        },
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        },
        {
            "jsonrpc": "2.0",
            "id": "client-response",
            "result": {}
        },
        {
            "jsonrpc": "2.0",
            "id": 9013,
            "method": "mcp/tools/list",
            "params": {}
        }
    ]);

    let response = send_mcp_message(&request).expect("batch request");
    let responses = response.as_array().expect("batch response array");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["jsonrpc"], "2.0");
    assert_eq!(responses[0]["id"], 9012);
    assert_eq!(responses[0]["result"]["content"][0]["text"], "pong");
    assert_eq!(responses[1]["jsonrpc"], "2.0");
    assert_eq!(responses[1]["id"], 9013);
    assert!(responses[1]["result"]["tools"].is_array());
}

#[test]
fn golden_batch_reports_invalid_items_without_dropping_valid_requests() {
    let request = json!([
        1,
        {
            "jsonrpc": "2.0",
            "id": 9014,
            "method": "ping",
            "params": {}
        }
    ]);

    let response = send_mcp_message(&request).expect("batch with invalid item");
    let responses = response.as_array().expect("batch response array");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["jsonrpc"], "2.0");
    assert!(responses[0]["id"].is_null());
    assert_eq!(responses[0]["error"]["code"], -32600);
    assert_eq!(responses[1]["id"], 9014);
    assert_eq!(responses[1]["result"]["content"][0]["text"], "pong");
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
fn golden_tools_call_missing_name_is_invalid_params() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9011,
        "method": "mcp/tools/call",
        "params": {
            "arguments": {}
        }
    });
    let response = send_mcp_message(&request).expect("missing tool name");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9011);
    assert!(response["result"].is_null());
    let err = response["error"].as_object().expect("protocol error");
    assert_eq!(err["code"], -32602);
    assert!(err["message"].as_str().unwrap().contains("tool name"));
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
        tools.len() >= 17,
        "tools/list must return full tool list, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"Ping"));
    assert!(names.contains(&"GeminiGate"));
    assert!(!names.contains(&"CodeQuery"));
}

#[test]
fn golden_readme_tool_inventory_matches_tools_list() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9010,
        "method": "mcp/tools/list",
        "params": {}
    });
    let response = send_mcp_message(&request).expect("tools list");
    let actual: BTreeSet<String> = response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .map(ToOwned::to_owned)
        .collect();

    let documented = documented_tool_names();
    assert_eq!(
        actual, documented,
        "README MCP Tools section must document exactly the tools exposed by tools/list"
    );
}

#[test]
fn golden_readme_documents_observable_protocol_error_codes() {
    let readme = readme_text();
    for code in ["-32700", "-32600", "-32601", "-32602", "-32603"] {
        assert!(
            readme.contains(code),
            "README Error Codes section must document observable JSON-RPC error code {code}"
        );
    }
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
