//! Golden-style contract tests: freeze observable MCP protocol and tool response shapes.
//! These complement `integration_test.rs` with explicit structural assertions.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Once;

static INIT: Once = Once::new();

fn setup() {
    INIT.call_once(|| {
        let output = Command::new("cargo")
            .args(["build", "--release"])
            .output()
            .expect("Failed to build project");
        if !output.status.success() {
            panic!("Build failed: {}", String::from_utf8_lossy(&output.stderr));
        }
    });
}

fn send_mcp_message(message: Value) -> Result<Value, Box<dyn std::error::Error>> {
    setup();
    let mut child = Command::new("cargo")
        .args(["run", "--release", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let msg_str = message.to_string();
    stdin.write_all(msg_str.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    drop(stdin);
    let mut reader = BufReader::new(stdout);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }
        if line.starts_with("Content-Length:") || line.trim().is_empty() {
            continue;
        }
        response = line;
        break;
    }
    let _ = child.kill();
    let _ = child.wait();
    if response.is_empty() {
        Err("No response received".into())
    } else {
        Ok(serde_json::from_str(&response)?)
    }
}

#[test]
fn golden_initialize_has_tools_capabilities_and_protocol_version() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9001,
        "method": "mcp/initialize",
        "params": {}
    });
    let response = send_mcp_message(request).expect("initialize");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 9001);
    let result = response["result"].as_object().expect("result object");
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert!(result["serverInfo"]["name"].is_string());
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
                "name": "ping",
                "arguments": {}
            }
        }
    });
    let response = send_mcp_message(request).expect("nested call");
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
            "toolName": "ping",
            "args": {}
        }
    });
    let response = send_mcp_message(request).expect("alias call");
    assert_eq!(response["result"]["content"][0]["text"], "pong");
}
