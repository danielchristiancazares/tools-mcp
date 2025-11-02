use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Once;

static INIT: Once = Once::new();

fn setup() {
    INIT.call_once(|| {
        // Build the binary once before all tests
        let output = Command::new("cargo")
            .args(&["build", "--release"])
            .output()
            .expect("Failed to build project");

        if !output.status.success() {
            panic!("Build failed: {}", String::from_utf8_lossy(&output.stderr));
        }
    });
}

/// Helper function to send a message to the MCP server and get response
fn send_mcp_message(message: Value) -> Result<Value, Box<dyn std::error::Error>> {
    setup();

    let mut child = Command::new("cargo")
        .args(&["run", "--release", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Send the message
    let msg_str = message.to_string();
    stdin.write_all(msg_str.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;

    // Close stdin to signal EOF to the server
    drop(stdin);

    // Read response with timeout
    let mut reader = BufReader::new(stdout);
    let mut response = String::new();

    // Read until we get a complete JSON response
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;

        if bytes_read == 0 {
            break; // EOF
        }

        // Skip Content-Length headers if present
        if line.starts_with("Content-Length:") || line.trim().is_empty() {
            continue;
        }

        response = line;
        break;
    }

    // Kill the child process (in case it hasn't exited)
    let _ = child.kill();
    let _ = child.wait(); // Clean up zombie

    // Parse and return the JSON response
    if response.is_empty() {
        Err("No response received".into())
    } else {
        Ok(serde_json::from_str(&response)?)
    }
}

/// Helper to send message with Content-Length header
fn send_mcp_message_with_headers(message: Value) -> Result<Value, Box<dyn std::error::Error>> {
    setup();

    let mut child = Command::new("cargo")
        .args(&["run", "--release", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Send with Content-Length header
    let msg_str = message.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", msg_str.len());
    stdin.write_all(header.as_bytes())?;
    stdin.write_all(msg_str.as_bytes())?;
    stdin.flush()?;

    // Close stdin to signal EOF
    drop(stdin);

    // Read response with headers
    let mut reader = BufReader::new(stdout);
    let mut content_length = 0;

    // Read headers
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;

        if bytes == 0 {
            break; // EOF
        }

        if line.starts_with("Content-Length:") {
            content_length = line
                .trim()
                .strip_prefix("Content-Length:")
                .unwrap()
                .trim()
                .parse()?;
        } else if line.trim().is_empty() && content_length > 0 {
            break;
        }
    }

    // Read body based on Content-Length
    let mut buffer = vec![0u8; content_length];
    reader.read_exact(&mut buffer)?;
    let response_str = String::from_utf8(buffer)?;

    // Kill the child process and clean up
    let _ = child.kill();
    let _ = child.wait();

    Ok(serde_json::from_str(&response_str)?)
}

#[test]
fn test_ping() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message(request).expect("Failed to send ping");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(response["result"]["content"][0]["text"].as_str() == Some("pong"));
}

#[test]
fn test_mcp_initialize() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "mcp/initialize",
        "params": {
            "capabilities": {}
        }
    });

    let response = send_mcp_message(request).expect("Failed to initialize");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);
    assert!(response["result"]["serverInfo"]["name"].is_string());
    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
}

#[test]
fn test_tools_list() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "mcp/tools/list",
        "params": {}
    });

    let response = send_mcp_message(request).expect("Failed to list tools");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 3);

    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3); // Simplified to 3 user-facing tools

    // Check that essential tools exist
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    assert!(tool_names.contains(&"ping"));
    assert!(tool_names.contains(&"WebFetch"));
    assert!(tool_names.contains(&"CodeQuery"));
}

#[test]
fn test_ping_tool_call() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "mcp/tools/call",
        "params": {
            "name": "ping",
            "arguments": {}
        }
    });

    let response = send_mcp_message(request).expect("Failed to call ping tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 4);
    assert_eq!(
        response["result"]["content"][0]["text"].as_str(),
        Some("pong")
    );
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn test_error_handling_unknown_method() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "unknown/method",
        "params": {}
    });

    let response = send_mcp_message(request).expect("Failed to send unknown method");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 5);
    assert!(response["error"]["code"].is_i64());
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Method not found"));
}

#[test]
fn test_error_handling_unknown_tool() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "mcp/tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    });

    let response = send_mcp_message(request).expect("Failed to call unknown tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 6);
    // Tool errors are returned in result, not error
    assert!(response["result"]["isError"].is_null() || response["error"].is_object());
}

#[test]
fn test_content_length_headers() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message_with_headers(request).expect("Failed with headers");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 7);
    assert!(response["result"]["content"][0]["text"].as_str() == Some("pong"));
}

#[test]
fn test_protocol_aliases() {
    // Test various protocol aliases
    let aliases = vec![
        "initialize",
        "server/initialize",
        "tools/list",
        "server/tools/list",
    ];

    for (i, method) in aliases.iter().enumerate() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 10 + i,
            "method": method,
            "params": {}
        });

        let response = send_mcp_message(request).expect(&format!("Failed with alias: {}", method));
        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response["result"].is_object() || response["error"].is_object());
    }
}

#[test]
fn test_code_query_requires_api_key() {
    setup();

    // Spawn process WITHOUT OPENAI_API_KEY in environment
    let mut child = Command::new("cargo")
        .args(&["run", "--release", "--quiet"])
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let request = json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "mcp/tools/call",
        "params": {
            "name": "CodeQuery",
            "arguments": {
                "vector_store_name": "test-store",
                "query": "How does this work?"
            }
        }
    });

    // Send message
    let msg_str = request.to_string();
    stdin.write_all(msg_str.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Read response
    let mut reader = BufReader::new(stdout);
    let mut response = String::new();

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).unwrap();
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

    let json_response: Value = serde_json::from_str(&response).expect("Failed to parse response");

    assert_eq!(json_response["jsonrpc"], "2.0");
    assert_eq!(json_response["id"], 30);
    assert_eq!(
        json_response["result"]["content"][0]["text"].as_str(),
        Some("OPENAI_API_KEY not set")
    );
    assert_eq!(json_response["result"]["isError"].as_bool(), Some(true));
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use std::env;

    fn skip_if_no_api_key() -> bool {
        if env::var("OPENAI_API_KEY").is_err() {
            eprintln!("Skipping API test: OPENAI_API_KEY not set");
            return true;
        }
        false
    }

    #[test]
    #[ignore] // Ignore by default since it requires API key
    fn test_create_store_tool() {
        if skip_if_no_api_key() {
            return;
        }

        let store_name = format!("test-store-{}", uuid::Uuid::new_v4());
        let request = json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "mcp/tools/call",
            "params": {
                "name": "create-store",
                "arguments": {
                    "name": store_name
                }
            }
        });

        let response = send_mcp_message(request).expect("Failed to create store");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&store_name));
    }

    #[test]
    #[ignore] // Ignore by default since it requires API key
    fn test_list_stores_tool() {
        if skip_if_no_api_key() {
            return;
        }

        let request = json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "mcp/tools/call",
            "params": {
                "name": "list-stores",
                "arguments": {}
            }
        });

        let response = send_mcp_message(request).expect("Failed to list stores");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["result"]["isError"], false);

        // Parse the JSON string in the response
        let content_str = response["result"]["content"][0]["text"].as_str().unwrap();
        let stores: Value = serde_json::from_str(content_str).unwrap();
        assert!(stores["vector_stores"].is_array());
    }

    #[test]
    #[ignore] // Requires OpenAI API
    fn test_code_query_without_reindex() {
        if skip_if_no_api_key() {
            return;
        }

        let store_name = format!("test-code-query-{}", uuid::Uuid::new_v4());
        let create_request = json!({
            "jsonrpc": "2.0",
            "id": 60,
            "method": "mcp/tools/call",
            "params": {
                "name": "create-store",
                "arguments": {
                    "name": store_name
                }
            }
        });

        let create_response =
            send_mcp_message(create_request).expect("Failed to create store for CodeQuery test");
        assert_eq!(create_response["jsonrpc"], "2.0");
        let store_info_text = create_response["result"]["content"][0]["text"]
            .as_str()
            .expect("missing store info text");
        let store_info: Value =
            serde_json::from_str(store_info_text).expect("failed to parse store info json");
        let vector_store_id = store_info["vector_store_id"]
            .as_str()
            .expect("missing vector_store_id")
            .to_string();

        let query_request = json!({
            "jsonrpc": "2.0",
            "id": 61,
            "method": "mcp/tools/call",
            "params": {
                "name": "query",
                "arguments": {
                    "vector_store_ids": [vector_store_id],
                    "query": "Summarize repository purpose.",
                    "include_results": false
                }
            }
        });

        let query_response =
            send_mcp_message(query_request).expect("Failed to call query with API key");
        assert_eq!(query_response["jsonrpc"], "2.0");
        assert_eq!(query_response["id"], 61);
        assert_eq!(query_response["result"]["isError"], false);
        assert!(
            query_response["result"]["content"][0]["text"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "CodeQuery response text should not be empty"
        );
    }
}

#[cfg(test)]
mod stress_tests {
    use super::*;

    #[test]
    fn test_multiple_sequential_requests() {
        for i in 1..=10 {
            let request = json!({
                "jsonrpc": "2.0",
                "id": 100 + i,
                "method": "ping",
                "params": {}
            });

            let response = send_mcp_message(request).expect(&format!("Failed request {}", i));
            assert_eq!(response["id"], 100 + i);
            assert_eq!(
                response["result"]["content"][0]["text"].as_str(),
                Some("pong")
            );
        }
    }

    #[test]
    fn test_large_payload() {
        let large_string = "x".repeat(10000);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 200,
            "method": "mcp/tools/call",
            "params": {
                "name": "ping",
                "arguments": {
                    "unused": large_string
                }
            }
        });

        let response = send_mcp_message(request).expect("Failed with large payload");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 200);
    }
}
