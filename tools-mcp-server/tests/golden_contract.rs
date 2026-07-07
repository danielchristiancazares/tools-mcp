//! Golden-style contract tests: freeze observable MCP protocol and tool response shapes.
//! These complement `integration_test.rs` with explicit structural assertions.

mod support;

use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::{send_mcp_message, send_mcp_message_with_command, spawn_server, workspace_root};

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

fn expected_tool_names_without_pwsh() -> BTreeSet<String> {
    [
        "Ping",
        "AdoWorkItems",
        "WebFetch",
        "Search",
        "search_context",
        "SemanticIndex",
        "SemanticSearch",
        "Read",
        "Edit",
        "Write",
        "Delete",
        "Move",
        "Copy",
        "ListDir",
        "Glob",
        "Outline",
        "git_snapshot",
        "GitStatus",
        "GitDiff",
        "GitApply",
        "GitHunks",
        "GitStageHunks",
        "GitRestore",
        "GitAdd",
        "GitCommit",
        "GitLog",
        "GitBranch",
        "GitCheckout",
        "GitStash",
        "GitShow",
        "GitBlame",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn served_tool_names(tools: &[Value]) -> BTreeSet<String> {
    tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .map(ToOwned::to_owned)
        .collect()
}

fn served_tools_without_pwsh() -> Vec<Value> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9100,
        "method": "mcp/tools/list",
        "params": {}
    });
    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_PWSH_TOOL");
    let response = send_mcp_message_with_command(&request, command).expect("tools list");
    response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone()
}

fn tool_schema<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("missing tool {name}"))
        .get("inputSchema")
        .expect("inputSchema")
}

fn schema_property<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema
        .get("properties")
        .and_then(|properties| properties.get(name))
        .unwrap_or_else(|| panic!("missing schema property {name}"))
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
    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_PWSH_TOOL");
    let response = send_mcp_message_with_command(&request, command).expect("initialize");
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
    let names = served_tool_names(tools);
    assert_eq!(
        tools.len(),
        names.len(),
        "initialize embedded tool list must not contain duplicate names"
    );
    assert_eq!(names, expected_tool_names_without_pwsh());
    assert!(!names.contains("CodeQuery"));
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
    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_PWSH_TOOL");
    let response = send_mcp_message_with_command(&request, command).expect("tools list");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9006);
    let tools = response["result"]["tools"].as_array().expect("tools array");
    let names = served_tool_names(tools);
    assert_eq!(
        tools.len(),
        names.len(),
        "tools/list must not contain duplicate names"
    );
    assert_eq!(names, expected_tool_names_without_pwsh());
    assert!(!names.contains("CodeQuery"));
}

#[test]
fn golden_readme_tool_inventory_matches_tools_list() {
    // The README enumerates every documented tool, including ones that are
    // gated behind opt-in env vars (e.g. Pwsh requires MCP_ENABLE_PWSH_TOOL).
    // Enable those gates here so the README inventory and the live
    // `tools/list` set are comparable.
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9010,
        "method": "mcp/tools/list",
        "params": {}
    });
    let mut command = spawn_server();
    command.env("MCP_ENABLE_PWSH_TOOL", "true");
    let response = send_mcp_message_with_command(&request, command).expect("tools list");
    let tools = response["result"]["tools"].as_array().expect("tools array");
    let actual = served_tool_names(tools);
    assert_eq!(
        tools.len(),
        actual.len(),
        "README inventory comparison must use a duplicate-free served tool list"
    );

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
    let mut command = spawn_server();
    command.env("MCP_ENABLE_PWSH_TOOL", "true");
    let response = send_mcp_message_with_command(&request, command).expect("tools list");
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
fn golden_git_hunk_tool_schemas_expose_selection_constraints() {
    let tools = served_tools_without_pwsh();

    let apply_schema = tool_schema(&tools, "GitApply");
    assert_eq!(apply_schema["required"], json!(["patch"]));
    let apply_patch = schema_property(apply_schema, "patch");
    assert_eq!(apply_patch["minLength"], 1);
    assert_eq!(apply_patch["maxLength"], 5_000_000);
    assert_eq!(
        schema_property(apply_schema, "target")["enum"],
        json!(["cached", "index_worktree", "worktree"])
    );
    assert_eq!(
        schema_property(apply_schema, "whitespace")["enum"],
        json!(["nowarn", "warn", "fix", "error", "error-all"])
    );

    let hunks_schema = tool_schema(&tools, "GitHunks");
    let hunks_paths = schema_property(hunks_schema, "paths");
    assert_eq!(hunks_paths["maxItems"], 1_000);
    assert_eq!(hunks_paths["items"]["minLength"], 1);
    assert_eq!(schema_property(hunks_schema, "context")["minimum"], 0);
    assert_eq!(schema_property(hunks_schema, "staged")["default"], false);
    assert_eq!(
        schema_property(hunks_schema, "include_advanced_templates")["default"],
        false
    );

    let stage_schema = tool_schema(&tools, "GitStageHunks");
    assert_eq!(stage_schema["required"], json!(["diff_id", "hunk_ids"]));
    assert_eq!(
        schema_property(stage_schema, "diff_id")["pattern"],
        "^sha256:[0-9a-f]{64}$"
    );
    let hunk_ids = schema_property(stage_schema, "hunk_ids");
    assert_eq!(hunk_ids["minItems"], 1);
    assert_eq!(hunk_ids["maxItems"], 10_000);
    assert_eq!(hunk_ids["uniqueItems"], true);
    assert_eq!(hunk_ids["items"]["maxLength"], 96);
    assert_eq!(
        hunk_ids["items"]["pattern"],
        "^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.[0-9a-f]{64}$"
    );
    assert_eq!(
        schema_property(stage_schema, "action")["enum"],
        json!(["prepare_commit", "stage_only", "unstage"])
    );
    assert_eq!(
        schema_property(stage_schema, "action")["default"],
        "prepare_commit"
    );
    let stage_paths = schema_property(stage_schema, "paths");
    assert_eq!(stage_paths["maxItems"], 1_000);
    assert_eq!(stage_paths["items"]["minLength"], 1);
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

#[test]
fn golden_edit_requires_prior_read_snapshot() {
    let dir = tempfile::Builder::new()
        .prefix("golden-edit-no-read-")
        .tempdir_in(workspace_root())
        .expect("tempdir in workspace");
    let path = dir.path().join("target.txt");
    std::fs::write(&path, "alpha\nbeta\n").expect("write");
    let path_arg = path.display().to_string();

    let request = json!({
        "jsonrpc": "2.0",
        "id": 9101,
        "method": "mcp/tools/call",
        "params": {
            "name": "Edit",
            "arguments": {
                "path": path_arg,
                "old_snippet": "beta",
                "new_snippet": "BETA"
            }
        }
    });
    let response = send_mcp_message(&request).expect("edit without read");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    let payload: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().expect("payload"))
            .expect("json payload");
    assert_eq!(payload["status"], "no_snapshot");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "alpha\nbeta\n"
    );
}

#[test]
fn golden_read_then_edit_batch_applies_edit() {
    // Read and Edit share one server process within a batch, so the Read snapshot lets the
    // Edit proceed with nothing copied between the two calls.
    let dir = tempfile::Builder::new()
        .prefix("golden-read-edit-")
        .tempdir_in(workspace_root())
        .expect("tempdir in workspace");
    let path = dir.path().join("target.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").expect("write");
    let path_arg = path.display().to_string();

    let batch = json!([
        {
            "jsonrpc": "2.0",
            "id": 9111,
            "method": "mcp/tools/call",
            "params": {
                "name": "Read",
                "arguments": { "path": path_arg.clone() }
            }
        },
        {
            "jsonrpc": "2.0",
            "id": 9112,
            "method": "mcp/tools/call",
            "params": {
                "name": "Edit",
                "arguments": {
                    "path": path_arg,
                    "old_snippet": "beta",
                    "new_snippet": "BETA"
                }
            }
        }
    ]);

    let response = send_mcp_message(&batch).expect("read+edit batch");
    let responses = response.as_array().expect("batch response array");
    assert_eq!(responses.len(), 2);

    assert_eq!(responses[0]["id"], 9111);
    assert_eq!(responses[0]["result"]["isError"], false);

    assert_eq!(responses[1]["id"], 9112);
    assert_eq!(responses[1]["result"]["isError"], false);
    let payload: Value = serde_json::from_str(
        responses[1]["result"]["content"][0]["text"]
            .as_str()
            .expect("payload"),
    )
    .expect("json payload");
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "alpha\nBETA\ngamma\n"
    );
}
