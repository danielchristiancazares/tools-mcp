mod support;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use support::{
    read_server_response, send_mcp_message, send_mcp_message_with_headers, spawn_server,
    workspace_root,
};

const READ_HANDLER_PATH: &str = "tools-mcp-local/src/tools/handlers/read_file.rs";

fn ugrep_bin() -> &'static str {
    if cfg!(target_os = "windows") {
        "ugrep.exe"
    } else {
        "ugrep"
    }
}

fn command_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn workspace_tempdir(prefix: &str) -> tempfile::TempDir {
    let root: PathBuf = workspace_root().join("target").join("test-work");
    std::fs::create_dir_all(&root).expect("failed to create workspace test-work directory");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("failed to create workspace tempdir")
}

#[test]
fn test_ping() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message(&request).expect("Failed to send ping");

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

    let response = send_mcp_message(&request).expect("Failed to initialize");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["serverInfo"]["name"], "tools-mcp-server");
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

    let response = send_mcp_message(&request).expect("Failed to list tools");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 3);

    let tools = response["result"]["tools"].as_array().unwrap();
    // Tool inventory can grow over time; assert a minimum and validate key tools exist.
    assert!(
        tools.len() >= 16,
        "expected at least 16 tools, got {}",
        tools.len()
    );

    // Check that essential tools exist
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    assert!(tool_names.contains(&"Ping"));
    assert!(tool_names.contains(&"WebFetch"));
    assert!(tool_names.contains(&"Search"));
    assert!(tool_names.contains(&"SemanticIndex"));
    assert!(tool_names.contains(&"SemanticSearch"));
    assert!(tool_names.contains(&"search_context"));
    assert!(!tool_names.contains(&"CodeQuery"));
    assert!(tool_names.contains(&"Read"));
    assert!(tool_names.contains(&"Edit"));
    assert!(tool_names.contains(&"git_snapshot"));
    assert!(tool_names.contains(&"GitStatus"));
    assert!(tool_names.contains(&"GitDiff"));
    assert!(tool_names.contains(&"GitRestore"));
    assert!(tool_names.contains(&"GitAdd"));
    assert!(tool_names.contains(&"GitCommit"));
    assert!(tool_names.contains(&"Write"));
    assert!(tool_names.contains(&"Delete"));
    assert!(tool_names.contains(&"Glob"));
    assert!(tool_names.contains(&"Outline"));
}

#[test]
fn test_ping_tool_call() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "mcp/tools/call",
        "params": {
            "name": "Ping",
            "arguments": {}
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call ping tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 4);
    assert_eq!(
        response["result"]["content"][0]["text"].as_str(),
        Some("pong")
    );
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn test_unknown_fields_are_rejected_for_tool_requests() {
    let ping_request = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "mcp/tools/call",
        "params": {
            "name": "Ping",
            "arguments": {
                "bogus": true
            }
        }
    });
    let ping_response = send_mcp_message(&ping_request).expect("Ping should reject unknown field");
    assert_eq!(ping_response["jsonrpc"], "2.0");
    assert_eq!(ping_response["id"], 8);
    assert_eq!(ping_response["result"]["isError"], true);
    assert!(
        ping_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown fields are not allowed")
    );

    let git_status_request = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStatus",
            "arguments": {
                "bogus": true
            }
        }
    });
    let git_status_response =
        send_mcp_message(&git_status_request).expect("GitStatus should reject unknown field");
    assert_eq!(git_status_response["jsonrpc"], "2.0");
    assert_eq!(git_status_response["id"], 9);
    assert_eq!(git_status_response["result"]["isError"], true);
    assert!(
        git_status_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown fields are not allowed")
    );
}

#[test]
fn test_read_file_no_line_numbers_by_default() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 40,
        "method": "mcp/tools/call",
        "params": {
            "name": "Read",
            "arguments": {
                "path": READ_HANDLER_PATH,
                "start_line": 1,
                "end_line": 1
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call Read tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 40);
    assert_eq!(response["result"]["isError"], false);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Read content text");
    assert!(
        !text.starts_with("1\t"),
        "expected no line number prefix by default (raw content)"
    );
    assert!(
        text.contains("File reading handler implementation."),
        "expected Read source content"
    );
    assert_eq!(response["result"]["start_line"], 1);
    assert_eq!(response["result"]["end_line"], 1);
    assert!(response["result"]["total_lines"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn test_read_file_shows_line_numbers_when_enabled() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "mcp/tools/call",
        "params": {
            "name": "Read",
            "arguments": {
                "path": READ_HANDLER_PATH,
                "start_line": 1,
                "end_line": 1,
                "show_line_numbers": true
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call Read tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 41);
    assert_eq!(response["result"]["isError"], false);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Read content text");
    assert!(
        text.starts_with("1\t"),
        "expected line number prefix when show_line_numbers is true"
    );
    assert!(
        text.contains("File reading handler implementation."),
        "expected Read source content"
    );
    assert_eq!(response["result"]["start_line"], 1);
    assert_eq!(response["result"]["end_line"], 1);
    assert!(response["result"]["total_lines"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn test_search_fixed_string_default_smart_uses_memory_backend() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "handle_read_file",
                "path": READ_HANDLER_PATH,
                "fixed_strings": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 41);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["pattern"], "handle_read_file");
    assert_eq!(response["result"]["path"], READ_HANDLER_PATH);
    assert_eq!(response["result"]["backend"], "memory");
    assert!(matches!(
        response["result"]["index_cache"].as_str(),
        Some("hit" | "miss")
    ));
    assert!(response["result"]["count"].as_u64().unwrap_or(0) >= 1);
    assert!(response["result"]["matches"].is_array());
}

#[test]
fn test_search_literal_default_options_uses_memory_backend() {
    let dir = workspace_tempdir("search-literal-default-memory");
    std::fs::write(
        dir.path().join("notes.txt"),
        "intro\ncandidate COMMONNEEDLE suffix\nunrelated line\n",
    )
    .expect("write literal fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 409,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "commonneedle",
                "path": dir.path().to_string_lossy().to_string(),
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call default literal Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 409);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "memory");
    assert_eq!(response["result"]["count"], 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(
        text.contains("COMMONNEEDLE"),
        "expected smart-case literal match, got: {text}"
    );
}

#[test]
fn test_search_context_returns_numbered_file_window() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 414,
        "method": "mcp/tools/call",
        "params": {
            "name": "search_context",
            "arguments": {
                "pattern": "File reading handler implementation.",
                "path": READ_HANDLER_PATH,
                "fixed_strings": true,
                "context_lines": 0,
                "max_matches": 1,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call search_context tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 414);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["pattern"],
        "File reading handler implementation."
    );
    assert_eq!(response["result"]["path"], READ_HANDLER_PATH);
    assert_eq!(response["result"]["context_lines"], 0);
    assert_eq!(response["result"]["windows"].as_array().unwrap().len(), 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing search_context content text");
    assert!(
        text.contains(READ_HANDLER_PATH),
        "expected window header, got: {text}"
    );
    assert!(
        text.contains(">1\t//! File reading handler implementation."),
        "expected marked matching line, got: {text}"
    );
}

#[test]
fn test_search_seeded_regex_uses_memory_backend() {
    let dir = workspace_tempdir("search-regex-memory");
    std::fs::write(
        dir.path().join("match.txt"),
        "intro\ncandidate needle middle haystack suffix\n",
    )
    .expect("write regex match fixture");
    std::fs::write(
        dir.path().join("false-positive.txt"),
        "needle on one line\nhaystack on another\n",
    )
    .expect("write regex false positive fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 413,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "needle.*haystack",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "sensitive",
                "fixed_strings": false,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call regex Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 413);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "memory");
    assert_eq!(response["result"]["count"], 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(
        text.contains("match.txt"),
        "expected regex match, got: {text}"
    );
    assert!(
        !text.contains("false-positive.txt"),
        "regex Phase Two should remove cross-line false positives, got: {text}"
    );
}

#[test]
fn test_search_common_seeded_regex_escape_uses_memory_backend() {
    let dir = workspace_tempdir("search-regex-escape-memory");
    std::fs::write(
        dir.path().join("match.txt"),
        "intro\ncandidate needle123haystack suffix\n",
    )
    .expect("write regex escape match fixture");
    std::fs::write(dir.path().join("miss.txt"), "needleabc haystack\n")
        .expect("write regex escape miss fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 416,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "needle\\d+haystack",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "sensitive",
                "fixed_strings": false,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call escaped regex Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 416);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "memory");
    assert_eq!(response["result"]["count"], 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(
        text.contains("match.txt"),
        "expected escaped regex match, got: {text}"
    );
    assert!(
        !text.contains("miss.txt"),
        "escaped regex verifier should remove false positives, got: {text}"
    );
}

#[test]
fn test_search_unseeded_regex_falls_back_to_ugrep() {
    let ugrep_bin = ugrep_bin();
    if !command_available(ugrep_bin) {
        eprintln!("Skipping unseeded regex fallback test: {ugrep_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("search-regex-fallback");
    std::fs::write(dir.path().join("numbers.txt"), "12345\nabcde\n")
        .expect("write regex fallback fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 414,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "^[0-9]+$",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "sensitive",
                "fixed_strings": false,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call regex fallback Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 414);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "ugrep");
    assert_eq!(
        response["result"]["fallback_reason"],
        "query_without_required_trigram"
    );
    assert_eq!(response["result"]["count"], 1);
}

#[test]
fn test_search_fuzzy_fixed_string_uses_memory_backend() {
    let dir = workspace_tempdir("search-fuzzy-memory");
    std::fs::write(
        dir.path().join("notes.txt"),
        "intro\ncandidate foobarbzzqux suffix\nunrelated line\n",
    )
    .expect("write fuzzy fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 410,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "foobarbazqux",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "sensitive",
                "fixed_strings": true,
                "fuzzy": 1,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call fuzzy Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 410);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "memory");
    assert_eq!(response["result"]["pattern"], "foobarbazqux");
    assert_eq!(
        response["result"]["path"].as_str(),
        Some(dir.path().to_string_lossy().as_ref())
    );
    assert_eq!(response["result"]["count"], 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(
        text.contains("foobarbzzqux"),
        "expected fuzzy match, got: {text}"
    );
}

#[test]
fn test_search_unsupported_fuzzy_mode_falls_back_to_ugrep() {
    let ugrep_bin = ugrep_bin();
    if !command_available(ugrep_bin) {
        eprintln!("Skipping fuzzy Search fallback test: {ugrep_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("search-fuzzy-fallback");
    std::fs::write(
        dir.path().join("notes.txt"),
        "candidate foobarbzzqux suffix\n",
    )
    .expect("write fuzzy fallback fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 411,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "foobarbazqux",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "insensitive",
                "fixed_strings": true,
                "fuzzy": 1,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call fuzzy fallback Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 411);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "ugrep");
    let fallback_reason = response["result"]["fallback_reason"]
        .as_str()
        .expect("missing fuzzy fallback reason");
    assert!(
        fallback_reason.contains("fuzzy"),
        "expected fuzzy-specific fallback_reason, got: {fallback_reason}"
    );
    assert!(response["result"]["count"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn test_search_glob_filtered_fixed_string_uses_memory_backend() {
    let dir = workspace_tempdir("search-glob-memory");
    std::fs::write(
        dir.path().join("keep.rs"),
        "fn main() { /* sharedneedle */ }\n",
    )
    .expect("write matching rust fixture");
    std::fs::write(dir.path().join("skip.txt"), "sharedneedle in text\n")
        .expect("write non-glob fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 412,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "sharedneedle",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "sensitive",
                "fixed_strings": true,
                "glob": ["*.rs"],
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call glob-filtered Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 412);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "memory");
    assert_eq!(response["result"]["count"], 1);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(text.contains("keep.rs"), "expected glob match, got: {text}");
    assert!(
        !text.contains("skip.txt"),
        "glob-filtered memory search should exclude non-matching files, got: {text}"
    );
}

#[test]
fn test_search_ugrep_fallback_preserves_slash_glob_or_semantics() {
    let ugrep_bin = ugrep_bin();
    if !command_available(ugrep_bin) {
        eprintln!("Skipping ugrep glob fallback test: {ugrep_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("search-glob-ugrep-fallback");
    let nested = dir.path().join("tools-mcp-server").join("tests");
    std::fs::create_dir_all(&nested).expect("create nested fixture directory");
    std::fs::write(dir.path().join("README.md"), "Search root hit\n").expect("write root fixture");
    std::fs::write(nested.join("integration_test.rs"), "backend nested hit\n")
        .expect("write nested fixture");
    std::fs::write(dir.path().join("skip.txt"), "Search skipped by glob\n")
        .expect("write skipped fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 415,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "Search|backend",
                "path": dir.path().to_string_lossy().to_string(),
                "case": "smart",
                "word_regexp": true,
                "glob": ["README.md", "tools-mcp-server/tests/integration_test.rs"],
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response =
        send_mcp_message(&request).expect("Failed to call glob-filtered ugrep Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 415);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["backend"], "ugrep");
    assert_eq!(
        response["result"]["fallback_reason"],
        "unsupported_word_regexp"
    );
    assert_eq!(response["result"]["count"], 2);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert!(
        text.contains("README.md"),
        "expected root glob match, got: {text}"
    );
    // Render uses the OS-native path separator (no slash-normalization).
    // Normalize for the substring check so this passes on Windows too.
    assert!(
        text.replace('\\', "/")
            .contains("tools-mcp-server/tests/integration_test.rs"),
        "expected slash glob match, got: {text}"
    );
    assert!(
        !text.contains("skip.txt"),
        "glob-filtered ugrep search should exclude non-matching files, got: {text}"
    );
}

#[test]
fn test_git_status_tool_call_if_git_installed() {
    let git_bin = if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
    };

    let git_available = Command::new(git_bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !git_available {
        eprintln!("Skipping GitStatus test: {git_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-status");

    // Initialize a small repo and create an untracked file so porcelain output is non-empty.
    let init_status = Command::new(git_bin)
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("failed to run git init");
    assert!(init_status.success(), "git init failed");

    std::fs::write(dir.path().join("foo.txt"), "hello\n").expect("write file failed");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStatus",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string()
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call GitStatus tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 42);
    assert_eq!(response["result"]["isError"], false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing GitStatus content text");
    assert!(
        text.contains("foo.txt"),
        "expected porcelain output to mention foo.txt, got: {text}"
    );
}

#[test]
fn test_git_snapshot_tool_call_if_git_installed() {
    let git_bin = if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
    };

    let git_available = Command::new(git_bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !git_available {
        eprintln!("Skipping git_snapshot test: {git_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-snapshot");
    let init_status = Command::new(git_bin)
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("failed to run git init");
    assert!(init_status.success(), "git init failed");

    std::fs::write(dir.path().join("foo.txt"), "hello\n").expect("write file failed");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 43,
        "method": "mcp/tools/call",
        "params": {
            "name": "git_snapshot",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string()
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call git_snapshot tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 43);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["clean"], false);
    assert_eq!(response["result"]["counts"]["untracked"], 1);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing git_snapshot content text");
    assert!(
        text.contains("foo.txt"),
        "expected snapshot output to mention foo.txt, got: {text}"
    );
}

#[test]
fn test_git_diff_ref_export_preserves_rename_metadata() {
    let git_bin = if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
    };

    let git_available = Command::new(git_bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !git_available {
        eprintln!("Skipping GitDiff test: {git_bin} not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-diff-repo");
    let patches_dir = workspace_tempdir("git-diff-patches");

    let init_status = Command::new(git_bin)
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(init_status.success(), "git init failed");

    let email_status = Command::new(git_bin)
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .status()
        .expect("git config email");
    assert!(email_status.success(), "git config email failed");

    let name_status = Command::new(git_bin)
        .args(["config", "user.name", "Test User"])
        .current_dir(dir.path())
        .status()
        .expect("git config name");
    assert!(name_status.success(), "git config name failed");

    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(dir.path().join("src/old.txt"), "hello\n").expect("write old");

    let add_status = Command::new(git_bin)
        .args(["add", "."])
        .current_dir(dir.path())
        .status()
        .expect("git add");
    assert!(add_status.success(), "git add failed");

    let first_commit = Command::new(git_bin)
        .args(["commit", "-q", "-m", "init"])
        .current_dir(dir.path())
        .status()
        .expect("first commit");
    assert!(first_commit.success(), "first commit failed");

    let mv_status = Command::new(git_bin)
        .args(["mv", "src/old.txt", "src/new.txt"])
        .current_dir(dir.path())
        .status()
        .expect("git mv");
    assert!(mv_status.success(), "git mv failed");

    let second_commit = Command::new(git_bin)
        .args(["commit", "-q", "-m", "rename"])
        .current_dir(dir.path())
        .status()
        .expect("second commit");
    assert!(second_commit.success(), "second commit failed");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 420,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitDiff",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "from_ref": "HEAD~1",
                "to_ref": "HEAD",
                "output_dir": patches_dir.path().to_string_lossy().to_string()
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call GitDiff");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 420);
    assert_eq!(response["result"]["isError"], false);

    let files = response["result"]["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["status"], "renamed");
    assert_eq!(files[0]["old_path"], "src/old.txt");
    assert_eq!(files[0]["path"], "src/new.txt");

    let patch_file = files[0]["patch_file"].as_str().expect("patch file");
    let patch_text =
        std::fs::read_to_string(patches_dir.path().join(patch_file)).expect("read patch");
    assert!(patch_text.contains("rename from src/old.txt"));
    assert!(patch_text.contains("rename to src/new.txt"));
}

#[test]
fn test_error_handling_unknown_method() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "unknown/method",
        "params": {}
    });

    let response = send_mcp_message(&request).expect("Failed to send unknown method");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 5);
    assert!(response["error"]["code"].is_i64());
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Method not found")
    );
}

#[test]
fn test_error_handling_invalid_json_returns_parse_error() {
    let mut child = spawn_server().spawn().expect("failed to spawn mcp server");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":99,"method":"ping""#)
        .expect("write invalid json");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush");
    drop(stdin);

    let mut reader = BufReader::new(stdout);
    let mut response = String::new();

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).expect("read line");
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

    assert!(!response.is_empty(), "expected parse error response");
    let json_response: Value = serde_json::from_str(&response).expect("parse response");
    assert_eq!(json_response["jsonrpc"], "2.0");
    assert!(json_response["id"].is_null());
    assert_eq!(json_response["error"]["code"], -32700);
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

    let response = send_mcp_message(&request).expect("Failed to call unknown tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 6);
    // Tool errors are returned in result, not error
    assert!(response["result"]["isError"].is_null() || response["error"].is_object());
}

#[test]
fn test_explicit_null_id_receives_response_with_null_id() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": null,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message(&request).expect("explicit null id should receive response");

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(
        response
            .as_object()
            .expect("response object")
            .contains_key("id")
    );
    assert!(response["id"].is_null());
    assert_eq!(response["result"]["content"][0]["text"], "pong");
}

#[test]
fn test_invalid_request_shape_returns_invalid_request_not_parse_error() {
    let request = json!({
        "jsonrpc": "1.0",
        "id": 610,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message(&request).expect("invalid request response");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 610);
    assert_eq!(response["error"]["code"], -32600);
}

#[test]
fn test_invalid_request_id_type_returns_null_id() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": {"not": "valid"},
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message(&request).expect("invalid id response");

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["id"].is_null());
    assert_eq!(response["error"]["code"], -32600);
}

#[test]
fn test_invalid_json_returns_parse_error_response() {
    let mut child = spawn_server().spawn().expect("Failed to spawn");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":1,"method":"ping""#)
        .unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

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

    assert!(!response.is_empty(), "expected parse-error response");
    let json_response: Value = serde_json::from_str(&response).expect("Failed to parse response");
    assert_eq!(json_response["jsonrpc"], "2.0");
    assert_eq!(json_response["id"], Value::Null);
    assert_eq!(json_response["error"]["code"], -32700);
    assert_eq!(json_response["error"]["message"], "Parse error");
}

#[test]
fn test_content_length_headers() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "ping",
        "params": {}
    });

    let response = send_mcp_message_with_headers(&request).expect("Failed with headers");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 7);
    assert!(response["result"]["content"][0]["text"].as_str() == Some("pong"));
}

#[test]
fn test_header_framing_invalid_utf8_returns_parse_error_and_server_recovers() {
    let mut child = spawn_server().spawn().expect("Failed to spawn");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    stdin
        .write_all(b"Content-Length: 2\r\n\r\n\xff\xff")
        .expect("write invalid utf8 frame");
    stdin.flush().expect("flush invalid utf8");

    let parse_error = read_server_response(&mut reader).expect("parse error response");
    let parse_error_json: Value = serde_json::from_str(&parse_error).expect("parse error json");
    assert_eq!(parse_error_json["jsonrpc"], "2.0");
    assert_eq!(parse_error_json["id"], Value::Null);
    assert_eq!(parse_error_json["error"]["code"], -32700);

    let request = json!({
        "jsonrpc": "2.0",
        "id": 701,
        "method": "ping",
        "params": {}
    });
    let request_str = request.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", request_str.len());
    stdin.write_all(header.as_bytes()).expect("write header");
    stdin
        .write_all(request_str.as_bytes())
        .expect("write request body");
    stdin.flush().expect("flush valid request");
    drop(stdin);

    let response = read_server_response(&mut reader).expect("ping response");
    let response_json: Value = serde_json::from_str(&response).expect("ping json");
    assert_eq!(response_json["jsonrpc"], "2.0");
    assert_eq!(response_json["id"], 701);
    assert_eq!(response_json["result"]["content"][0]["text"], "pong");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_protocol_aliases() {
    // Test various protocol aliases
    let aliases = [
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

        let response =
            send_mcp_message(&request).unwrap_or_else(|_| panic!("Failed with alias: {method}"));
        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response["result"].is_object() || response["error"].is_object());
    }
}

#[test]
fn test_webfetch_blocks_localhost_ssrf() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 31,
        "method": "mcp/tools/call",
        "params": {
            "name": "WebFetch",
            "arguments": {
                "url": "http://localhost:1234/"
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call WebFetch");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 31);
    assert_eq!(response["result"]["isError"], true);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing WebFetch error text");
    assert!(
        text.to_ascii_lowercase().contains("ssrf")
            || text.to_ascii_lowercase().contains("localhost"),
        "expected WebFetch error to mention SSRF/localhost, got: {text}"
    );

    assert_eq!(response["result"]["error_type"], "ssrf_blocked");
}

#[test]
fn test_git_tools_disabled_by_default() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 999,
        "method": "mcp/tools/list",
        "params": {}
    });

    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_GIT");

    let response = support::send_mcp_message_with_command(&request, command)
        .expect("Failed to send tools/list with env var");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 999);

    let tools = response["result"]["tools"].as_array().unwrap();

    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        !tool_names.contains(&"git_snapshot"),
        "Expected git_snapshot to be disabled by default"
    );
    for name in tool_names {
        assert!(
            !name.starts_with("Git"),
            "Expected Git tools to be disabled by default, but found tool: {}",
            name
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

            let response =
                send_mcp_message(&request).unwrap_or_else(|_| panic!("Failed request {i}"));
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
                "name": "Ping",
                "arguments": {
                    "unused": large_string
                }
            }
        });

        let response = send_mcp_message(&request).expect("Failed with large payload");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 200);
    }
}
