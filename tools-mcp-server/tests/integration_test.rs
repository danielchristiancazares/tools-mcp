mod support;

use serde_json::{Value, json};
use std::collections::BTreeSet;
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

fn git_bin() -> &'static str {
    if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
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

fn git_version_at_least(required_major: u32, required_minor: u32) -> bool {
    let Ok(output) = Command::new(git_bin()).arg("--version").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(version) = text
        .split_whitespace()
        .find(|part| part.as_bytes().first().is_some_and(u8::is_ascii_digit))
    else {
        return false;
    };
    let mut parts = version.split('.');
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
    match (major, minor) {
        (Some(major), Some(minor)) => {
            major > required_major || (major == required_major && minor >= required_minor)
        }
        _ => false,
    }
}

fn run_git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new(git_bin())
        .args(args)
        .current_dir(repo)
        .status()
        .expect("git command should start");
    assert!(status.success(), "git {args:?} failed");
}

fn init_git_fixture(repo: &std::path::Path) {
    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.email", "test@example.com"]);
    run_git(repo, &["config", "user.name", "Test User"]);
    run_git(repo, &["config", "core.autocrlf", "false"]);
}

fn git_stdout(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(git_bin())
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command should start");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).expect("git stdout should be utf-8")
}

fn try_run_git(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(git_bin())
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command should start")
}

fn expected_tool_names_without_pwsh() -> BTreeSet<&'static str> {
    [
        "Ping",
        "AdoWorkItems",
        "WebFetch",
        "Search",
        "search_context",
        "Read",
        "Edit",
        "Write",
        "Delete",
        "Move",
        "Copy",
        "ListDir",
        "CountLines",
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
    .collect()
}

fn expected_tool_names_with_semantic_without_pwsh() -> BTreeSet<&'static str> {
    let mut names = expected_tool_names_without_pwsh();
    names.insert("SemanticIndex");
    names.insert("SemanticSearch");
    names
}

fn workspace_tempdir(prefix: &str) -> tempfile::TempDir {
    let root: PathBuf = workspace_root().join("target").join("test-work");
    std::fs::create_dir_all(&root).expect("failed to create workspace test-work directory");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("failed to create workspace tempdir")
}

#[cfg(unix)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_symlink(_target: &std::path::Path, _link: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlinks are not supported on this platform",
    ))
}

#[cfg(unix)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_file_symlink(_target: &std::path::Path, _link: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "file symlinks are not supported on this platform",
    ))
}

fn git_index_with_extension(signature: &[u8; 4], payload_len: u32) -> Vec<u8> {
    let mut index = Vec::new();
    index.extend_from_slice(b"DIRC");
    index.extend_from_slice(&2u32.to_be_bytes());
    index.extend_from_slice(&0u32.to_be_bytes());
    index.extend_from_slice(signature);
    index.extend_from_slice(&payload_len.to_be_bytes());
    index.resize(index.len() + payload_len as usize, 0);
    index.resize(index.len() + 20, 0);
    index
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

    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_PWSH_TOOL");
    command.env_remove("MCP_SEMANTIC_BACKEND");
    let response =
        support::send_mcp_message_with_command(&request, command).expect("Failed to list tools");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 3);

    let tools = response["result"]["tools"].as_array().unwrap();

    let tool_names: BTreeSet<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        tools.len(),
        tool_names.len(),
        "tools/list must not contain duplicate names"
    );
    assert_eq!(tool_names, expected_tool_names_without_pwsh());
    assert!(!tool_names.contains("CodeQuery"));
}

#[test]
fn test_count_lines_groups_extension_counts_by_directory() {
    let root = workspace_tempdir("count-lines-");
    let alpha = root.path().join("alpha");
    let beta = root.path().join("beta");
    let empty = root.path().join("empty");
    std::fs::create_dir_all(alpha.join("src")).expect("create alpha src");
    std::fs::create_dir_all(alpha.join("target")).expect("create alpha target");
    std::fs::create_dir_all(&beta).expect("create beta");
    std::fs::create_dir_all(&empty).expect("create empty");
    std::fs::create_dir_all(root.path().join("target")).expect("create root target");
    std::fs::write(alpha.join("src").join("lib.rs"), "one\ntwo\n").expect("write alpha");
    std::fs::write(alpha.join("target").join("generated.rs"), "ignored\n")
        .expect("write generated");
    std::fs::write(beta.join("main.rs"), "one\r\ntwo\r\nthree").expect("write beta");
    std::fs::write(root.path().join("target").join("root.rs"), "ignored\n")
        .expect("write root target");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 302,
        "method": "mcp/tools/call",
        "params": {
            "name": "CountLines",
            "arguments": {
                "path": root.path().display().to_string(),
                "extension": ".rs"
            }
        }
    });

    let response = send_mcp_message(&request).expect("CountLines call");
    let result = &response["result"];
    assert_eq!(response["id"], 302);
    assert_eq!(result["isError"], false, "expected success: {result}");
    assert_eq!(result["extension"], "rs");
    assert_eq!(result["directory_count"], 3);
    assert_eq!(result["total_files"], 2);
    assert_eq!(result["total_lines"], 5);
    assert_eq!(result["directories"][0]["directory"], "beta");
    assert_eq!(result["directories"][0]["files"], 1);
    assert_eq!(result["directories"][0]["lines"], 3);
    assert_eq!(result["directories"][1]["directory"], "alpha");
    assert_eq!(result["directories"][1]["files"], 1);
    assert_eq!(result["directories"][1]["lines"], 2);
    assert_eq!(result["directories"][2]["directory"], "empty");
    assert_eq!(result["directories"][2]["files"], 0);
    assert_eq!(result["directories"][2]["lines"], 0);
}

#[test]
fn test_semantic_tools_disabled_when_backend_env_missing() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 300,
        "method": "mcp/tools/list",
        "params": {}
    });

    let mut command = spawn_server();
    command.env_remove("MCP_ENABLE_PWSH_TOOL");
    command.env_remove("MCP_SEMANTIC_BACKEND");
    let response = support::send_mcp_message_with_command(&request, command)
        .expect("Failed to list tools without semantic backend");

    let tools = response["result"]["tools"].as_array().unwrap();
    let tool_names: BTreeSet<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    assert_eq!(tool_names, expected_tool_names_without_pwsh());
    assert!(!tool_names.contains("SemanticIndex"));
    assert!(!tool_names.contains("SemanticSearch"));
}

#[test]
fn test_semantic_tools_register_when_backend_env_present() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 301,
        "method": "mcp/tools/list",
        "params": {}
    });

    for backend in ["", "lancedb", "qdrant", "none"] {
        let mut command = spawn_server();
        command.env_remove("MCP_ENABLE_PWSH_TOOL");
        command.env("MCP_SEMANTIC_BACKEND", backend);
        let response = support::send_mcp_message_with_command(&request, command)
            .expect("Failed to list tools with semantic backend env present");

        let tools = response["result"]["tools"].as_array().unwrap();
        let tool_names: BTreeSet<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

        assert_eq!(
            tool_names,
            expected_tool_names_with_semantic_without_pwsh(),
            "semantic tools should register when MCP_SEMANTIC_BACKEND={backend:?} is present"
        );
    }
}

#[test]
fn test_semantic_index_reports_indexed_and_updated_counts() {
    let temp_dir = tempfile::Builder::new()
        .prefix("semantic-index-response-")
        .tempdir()
        .expect("failed to create semantic index tempdir");
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("failed to create semantic source fixture");
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n")
        .expect("failed to write semantic source fixture");

    let manifest_dir = temp_dir
        .path()
        .join(".tools-mcp")
        .join("semantic-index")
        .join("jina_embeddings_v2_base_code");
    std::fs::create_dir_all(&manifest_dir).expect("failed to create semantic manifest fixture");
    let manifest = json!({
        "version": 1,
        "workspace": temp_dir.path().display().to_string(),
        "model_id": "jina-embeddings-v2-base-code",
        "table_name": null,
        "vector_dim": null,
        "files": {
            "src/lib.rs": {
                // SHA-256 of "pub fn existing() {}\n"; matching the manifest keeps this
                // test on the unchanged fast path without loading the embedding model.
                "file_hash": "7ac40acbc4397427e64e103d0c196383179962a91119b69aa255c67aae5402b4",
                "chunk_ids": ["src/lib.rs:1"],
                "indexed_at": "2026-01-01T00:00:00Z"
            }
        }
    });
    std::fs::write(
        manifest_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)
            .expect("failed to serialize semantic manifest fixture"),
    )
    .expect("failed to write semantic manifest fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "mcp/tools/call",
        "params": {
            "name": "SemanticIndex",
            "arguments": {
                "path": "src",
                "no_ignore": true
            }
        }
    });

    let mut command = spawn_server();
    command.current_dir(temp_dir.path());
    command.env("MCP_SEMANTIC_BACKEND", "lancedb");
    let response = support::send_mcp_message_with_command(&request, command)
        .expect("Failed to call SemanticIndex");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 30);
    let result = &response["result"];
    assert_eq!(result["isError"], false);
    assert_eq!(result["indexed_files"], 1);
    assert_eq!(result["indexed_chunks"], 1);
    assert_eq!(result["updated_files"], 0);
    assert_eq!(result["updated_chunks"], 0);
    assert_eq!(result["skipped_files"], 0);

    let text = result["content"][0]["text"]
        .as_str()
        .expect("missing SemanticIndex content text");
    assert!(
        text.contains("Indexed 1 file(s), updated 0 file(s)"),
        "expected indexed/updated summary, got {text}"
    );
    assert!(
        !text.contains("skipped"),
        "summary text should not describe already-indexed files as skipped: {text}"
    );
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
fn test_search_empty_results_render_zero_results_message() {
    let dir = workspace_tempdir("search-empty-results");
    std::fs::write(dir.path().join("notes.txt"), "intro\nunrelated line\n")
        .expect("write empty-results fixture");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 410,
        "method": "mcp/tools/call",
        "params": {
            "name": "Search",
            "arguments": {
                "pattern": "definitely-missing-needle",
                "path": dir.path().to_string_lossy().to_string(),
                "fixed_strings": true,
                "no_ignore": true,
                "max_results": 20,
                "timeout_ms": 20000
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call empty-result Search tool");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 410);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["count"], 0);
    assert_eq!(response["result"]["match_count"], 0);
    assert_eq!(response["result"]["event_count"], 0);

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("missing Search content text");
    assert_eq!(text, "0 results found");
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
fn test_git_apply_cached_apply_and_unproved_nonzero_state() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-state");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = "line 1\nline 2\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");
    let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
    std::fs::write(dir.path().join("story.txt"), base).expect("restore base content");

    let working_dir = dir.path().to_string_lossy().to_string();
    let apply_request = json!({
        "jsonrpc": "2.0",
        "id": 505,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": working_dir,
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let apply_response = send_mcp_message(&apply_request).expect("GitApply response");
    assert_eq!(
        apply_response["result"]["isError"], false,
        "{apply_response:?}"
    );
    assert_eq!(apply_response["result"]["state"], "applied");
    assert_eq!(apply_response["result"]["applied"], true);
    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(cached.contains("line 2 edited"), "{cached}");

    let bad_patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1,3 +1,3 @@\n",
        " line 1\n",
        "-line does not exist\n",
        "+line 2 twice edited\n",
        " line 3\n"
    );
    let failed_apply_request = json!({
        "jsonrpc": "2.0",
        "id": 506,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": working_dir,
                "patch": bad_patch,
                "target": "cached"
            }
        }
    });
    let failed_apply_response =
        send_mcp_message(&failed_apply_request).expect("GitApply failed response");
    assert_eq!(
        failed_apply_response["result"]["isError"], true,
        "{failed_apply_response:?}"
    );
    assert_eq!(failed_apply_response["result"]["state"], "state_unknown");
    assert_eq!(
        failed_apply_response["result"]["state_unknown_reason"],
        "unproved_git_nonzero"
    );
    assert_eq!(failed_apply_response["result"]["applied"], false);
}

#[test]
fn test_git_apply_non_check_context_mismatch_is_state_unknown_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply context-mismatch test: git not found on PATH");
        return;
    }

    for target in ["cached", "worktree", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-context-mismatch-{target}-"));
        init_git_fixture(dir.path());

        let story_base = "story line 1\nstory line 2\nstory line 3\n";
        let notes_base = "note line 1\nnote line 2\nnote line 3\n";
        std::fs::write(dir.path().join("story.txt"), story_base).expect("write story base");
        std::fs::write(dir.path().join("notes.txt"), notes_base).expect("write notes base");
        run_git(dir.path(), &["add", "story.txt", "notes.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        let bad_patch = concat!(
            "diff --git a/story.txt b/story.txt\n",
            "--- a/story.txt\n",
            "+++ b/story.txt\n",
            "@@ -1,3 +1,3 @@\n",
            " story line 1\n",
            "-story line 2\n",
            "+story line 2 edited\n",
            " story line 3\n",
            "diff --git a/notes.txt b/notes.txt\n",
            "--- a/notes.txt\n",
            "+++ b/notes.txt\n",
            "@@ -1,3 +1,3 @@\n",
            " note line 1\n",
            "-note line does not exist\n",
            "+note line 2 edited\n",
            " note line 3\n"
        );

        let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_before = git_stdout(dir.path(), &["diff"]);
        let status_before = git_stdout(dir.path(), &["status", "--short"]);
        let story_before =
            std::fs::read_to_string(dir.path().join("story.txt")).expect("read story before");
        let notes_before =
            std::fs::read_to_string(dir.path().join("notes.txt")).expect("read notes before");

        let request = json!({
            "jsonrpc": "2.0",
            "id": 580,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": bad_patch,
                    "target": target
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply context-mismatch response");

        assert_eq!(response["result"]["isError"], true, "{response:?}");
        assert_eq!(response["result"]["error_type"], "unproved_git_nonzero");
        assert_eq!(response["result"]["state"], "state_unknown");
        assert_eq!(
            response["result"]["state_unknown_reason"],
            "unproved_git_nonzero"
        );
        assert_eq!(response["result"]["applied"], false);
        assert_eq!(response["result"]["checked"], false);
        assert_eq!(response["result"]["target"], target);

        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "cached diff changed for target={target}"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            worktree_before,
            "worktree diff changed for target={target}"
        );
        assert_eq!(
            git_stdout(dir.path(), &["status", "--short"]),
            status_before,
            "status changed for target={target}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("story.txt")).expect("read story after"),
            story_before,
            "story.txt changed for target={target}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).expect("read notes after"),
            notes_before,
            "notes.txt changed for target={target}"
        );
    }
}

#[test]
fn test_git_apply_rejects_malformed_truncated_patches_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply malformed/truncated patch test: git not found on PATH");
        return;
    }

    let cases = [
        (
            "truncated multi-file patch",
            concat!(
                "diff --git a/story.txt b/story.txt\n",
                "--- a/story.txt\n",
                "+++ b/story.txt\n",
                "@@ -1,2 +1,2 @@\n",
                "-story line 1\n",
                "diff --git a/notes.txt b/notes.txt\n",
                "--- a/notes.txt\n",
                "+++ b/notes.txt\n",
                "@@ -1 +1 @@\n",
                "-note line 1\n",
                "+note line 1 edited\n"
            ),
        ),
        (
            "malformed hunk body line",
            concat!(
                "diff --git a/story.txt b/story.txt\n",
                "--- a/story.txt\n",
                "+++ b/story.txt\n",
                "@@ -1 +1 @@\n",
                "story line without diff prefix\n",
                "+story line 1 edited\n"
            ),
        ),
    ];

    for (target_index, target) in ["cached", "worktree", "index_worktree"]
        .into_iter()
        .enumerate()
    {
        let dir = workspace_tempdir(&format!("git-apply-malformed-{target}-"));
        init_git_fixture(dir.path());

        let story_base = "story line 1\nstory line 2\nstory line 3\n";
        let notes_base = "note line 1\nnote line 2\nnote line 3\n";
        std::fs::write(dir.path().join("story.txt"), story_base).expect("write story base");
        std::fs::write(dir.path().join("notes.txt"), notes_base).expect("write notes base");
        run_git(dir.path(), &["add", "story.txt", "notes.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_before = git_stdout(dir.path(), &["diff"]);
        let status_before = git_stdout(dir.path(), &["status", "--short"]);
        let story_before =
            std::fs::read_to_string(dir.path().join("story.txt")).expect("read story before");
        let notes_before =
            std::fs::read_to_string(dir.path().join("notes.txt")).expect("read notes before");

        for (case_index, (label, patch)) in cases.iter().enumerate() {
            let request = json!({
                "jsonrpc": "2.0",
                "id": 582 + target_index * 10 + case_index,
                "method": "mcp/tools/call",
                "params": {
                    "name": "GitApply",
                    "arguments": {
                        "working_dir": dir.path().to_string_lossy().to_string(),
                        "patch": patch,
                        "target": target
                    }
                }
            });
            let response = send_mcp_message(&request).expect("GitApply malformed response");

            assert_eq!(
                response["result"]["isError"], true,
                "{label} target={target}: {response:?}"
            );
            assert_eq!(
                response["result"]["error_type"], "unsupported_patch_record",
                "{label} target={target}: {response:?}"
            );
            assert_eq!(
                response["result"]["parser_error"]["error_type"], "diff_parse_error",
                "{label} target={target}: {response:?}"
            );
            assert!(
                response["result"]["state"].is_null(),
                "{label} target={target}: validation failure should not return apply state: {response:?}"
            );
            assert!(
                response["result"]["applied"].is_null(),
                "{label} target={target}: validation failure should not return applied=false as if git ran: {response:?}"
            );
            assert!(
                response["result"]["checked"].is_null(),
                "{label} target={target}: validation failure should not return checked=false as if git ran: {response:?}"
            );

            assert_eq!(
                git_stdout(dir.path(), &["diff", "--cached"]),
                cached_before,
                "{label} target={target}: cached diff changed"
            );
            assert_eq!(
                git_stdout(dir.path(), &["diff"]),
                worktree_before,
                "{label} target={target}: worktree diff changed"
            );
            assert_eq!(
                git_stdout(dir.path(), &["status", "--short"]),
                status_before,
                "{label} target={target}: status changed"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("story.txt")).expect("read story after"),
                story_before,
                "{label} target={target}: story.txt changed"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("notes.txt")).expect("read notes after"),
                notes_before,
                "{label} target={target}: notes.txt changed"
            );
        }
    }
}

#[test]
fn test_git_apply_worktree_writing_rejects_symlink_and_hardlink_leaves_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply worktree-leaf safety test: git not found on PATH");
        return;
    }

    fn setup_story_patch(
        prefix: &str,
        target: &str,
        base: &str,
        edited: &str,
    ) -> (tempfile::TempDir, String) {
        let dir = workspace_tempdir(&format!("{prefix}-{target}-"));
        init_git_fixture(dir.path());

        let story_path = dir.path().join("story.txt");
        std::fs::write(&story_path, base).expect("write story base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(&story_path, edited).expect("write story edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        std::fs::write(&story_path, base).expect("restore story base");
        run_git(dir.path(), &["update-index", "--refresh"]);

        (dir, patch)
    }

    fn git_apply_response(
        repo: &std::path::Path,
        patch: &str,
        target: &str,
        request_id: usize,
    ) -> Value {
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": repo.to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target
                }
            }
        });
        send_mcp_message(&request).expect("GitApply worktree-leaf safety response")
    }

    fn assert_validation_rejection(response: &Value, target: &str, label: &str) {
        assert_eq!(
            response["result"]["isError"], true,
            "{label} target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["error_type"], "unsupported_patch_record",
            "{label} target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["path"], "story.txt",
            "{label} target={target}: {response:?}"
        );
        assert!(
            response["result"]["state"].is_null(),
            "{label} target={target}: validation failure should not return apply state: {response:?}"
        );
        assert!(
            response["result"]["applied"].is_null(),
            "{label} target={target}: validation failure should not return applied=false as if git ran: {response:?}"
        );
        assert!(
            response["result"]["checked"].is_null(),
            "{label} target={target}: validation failure should not return checked=false as if git ran: {response:?}"
        );
    }

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";
    let mut exercised_case = false;

    for (target_index, target) in ["worktree", "index_worktree"].into_iter().enumerate() {
        let (dir, patch) = setup_story_patch("git-apply-symlink-leaf", target, base, edited);
        let story_path = dir.path().join("story.txt");
        let symlink_target_path = dir.path().join("symlink-target.txt");
        std::fs::write(&symlink_target_path, base).expect("write symlink target base");
        std::fs::remove_file(&story_path).expect("replace story with symlink");

        match create_file_symlink(&symlink_target_path, &story_path) {
            Ok(()) => {
                exercised_case = true;
                assert!(
                    std::fs::symlink_metadata(&story_path)
                        .expect("read symlink metadata before")
                        .file_type()
                        .is_symlink(),
                    "test fixture must replace story.txt with a file symlink"
                );

                let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
                let worktree_before = git_stdout(dir.path(), &["diff"]);
                let status_before = git_stdout(dir.path(), &["status", "--short"]);
                let story_before =
                    std::fs::read_to_string(&story_path).expect("read symlinked story before");
                let symlink_target_before = std::fs::read_to_string(&symlink_target_path)
                    .expect("read symlink target before");

                let response = git_apply_response(dir.path(), &patch, target, 590 + target_index);
                assert_validation_rejection(&response, target, "symlink final leaf");

                assert_eq!(
                    git_stdout(dir.path(), &["diff", "--cached"]),
                    cached_before,
                    "symlink final leaf target={target}: cached diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["diff"]),
                    worktree_before,
                    "symlink final leaf target={target}: worktree diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["status", "--short"]),
                    status_before,
                    "symlink final leaf target={target}: status changed"
                );
                assert!(
                    std::fs::symlink_metadata(&story_path)
                        .expect("read symlink metadata after")
                        .file_type()
                        .is_symlink(),
                    "symlink final leaf target={target}: story.txt stopped being a symlink"
                );
                assert_eq!(
                    std::fs::read_to_string(&story_path).expect("read symlinked story after"),
                    story_before,
                    "symlink final leaf target={target}: story.txt content changed"
                );
                assert_eq!(
                    std::fs::read_to_string(&symlink_target_path)
                        .expect("read symlink target after"),
                    symlink_target_before,
                    "symlink final leaf target={target}: symlink target content changed"
                );
            }
            Err(err) => {
                eprintln!(
                    "Skipping GitApply file-symlink final-leaf subcase for target={target}: {err}"
                );
            }
        }

        let (dir, patch) = setup_story_patch("git-apply-hardlink-leaf", target, base, edited);
        let story_path = dir.path().join("story.txt");
        let hardlink_path = dir.path().join("story-hardlink.txt");

        match std::fs::hard_link(&story_path, &hardlink_path) {
            Ok(()) => {
                exercised_case = true;
                let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
                let worktree_before = git_stdout(dir.path(), &["diff"]);
                let status_before = git_stdout(dir.path(), &["status", "--short"]);
                let story_before =
                    std::fs::read_to_string(&story_path).expect("read hardlinked story before");
                let hardlink_before =
                    std::fs::read_to_string(&hardlink_path).expect("read hardlink before");

                let response = git_apply_response(dir.path(), &patch, target, 594 + target_index);
                assert_validation_rejection(&response, target, "hardlink final leaf");
                assert!(
                    response["result"]["link_count"]
                        .as_u64()
                        .is_some_and(|count| count > 1),
                    "hardlink final leaf target={target}: response should include link_count > 1: {response:?}"
                );

                assert_eq!(
                    git_stdout(dir.path(), &["diff", "--cached"]),
                    cached_before,
                    "hardlink final leaf target={target}: cached diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["diff"]),
                    worktree_before,
                    "hardlink final leaf target={target}: worktree diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["status", "--short"]),
                    status_before,
                    "hardlink final leaf target={target}: status changed"
                );
                assert_eq!(
                    std::fs::read_to_string(&story_path).expect("read hardlinked story after"),
                    story_before,
                    "hardlink final leaf target={target}: story.txt content changed"
                );
                assert_eq!(
                    std::fs::read_to_string(&hardlink_path).expect("read hardlink after"),
                    hardlink_before,
                    "hardlink final leaf target={target}: hardlink content changed"
                );
            }
            Err(err) => {
                eprintln!(
                    "Skipping GitApply hardlink final-leaf subcase for target={target}: {err}"
                );
            }
        }
    }

    assert!(
        exercised_case,
        "GitApply worktree-leaf safety test did not exercise any symlink or hardlink fixture"
    );
}

#[test]
fn test_git_apply_cached_target_allows_hardlinked_worktree_leaf_without_worktree_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply cached hardlink-leaf test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-cached-hardlink-leaf-");
    init_git_fixture(dir.path());

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";
    let story_path = dir.path().join("story.txt");
    let hardlink_path = dir.path().join("story-hardlink.txt");
    std::fs::write(&story_path, base).expect("write story base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(&story_path, edited).expect("write story edit");
    let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
    std::fs::write(&story_path, base).expect("restore story base");
    run_git(dir.path(), &["update-index", "--refresh"]);
    if let Err(err) = std::fs::hard_link(&story_path, &hardlink_path) {
        eprintln!("Skipping GitApply cached hardlink-leaf test: hard links are unavailable: {err}");
        return;
    }
    let story_before = std::fs::read_to_string(&story_path).expect("read story before");
    let hardlink_before = std::fs::read_to_string(&hardlink_path).expect("read hardlink before");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 598,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply cached hardlink-leaf response");

    assert_eq!(response["result"]["isError"], false, "{response:?}");
    assert_eq!(response["result"]["state"], "applied", "{response:?}");
    assert_eq!(response["result"]["applied"], true, "{response:?}");
    assert_eq!(response["result"]["target"], "cached", "{response:?}");
    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.contains("line 2 edited"),
        "cached target should stage the patch despite hardlinked worktree leaf: {cached}"
    );
    assert_eq!(
        std::fs::read_to_string(&story_path).expect("read story after"),
        story_before,
        "cached target must not mutate hardlinked story.txt worktree content"
    );
    assert_eq!(
        std::fs::read_to_string(&hardlink_path).expect("read hardlink after"),
        hardlink_before,
        "cached target must not mutate hardlinked companion content"
    );
}

#[test]
fn test_git_apply_worktree_writing_rejects_directory_leaf_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply directory-leaf safety test: git not found on PATH");
        return;
    }

    fn setup_story_patch(target: &str, base: &str, edited: &str) -> (tempfile::TempDir, String) {
        let dir = workspace_tempdir(&format!("git-apply-directory-leaf-{target}-"));
        init_git_fixture(dir.path());

        let story_path = dir.path().join("story.txt");
        std::fs::write(&story_path, base).expect("write story base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(&story_path, edited).expect("write story edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        std::fs::write(&story_path, base).expect("restore story base");
        run_git(dir.path(), &["update-index", "--refresh"]);

        (dir, patch)
    }

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";

    for (target_index, target) in ["worktree", "index_worktree"].into_iter().enumerate() {
        let (dir, patch) = setup_story_patch(target, base, edited);
        let story_path = dir.path().join("story.txt");
        let marker_path = story_path.join("marker.txt");
        std::fs::remove_file(&story_path).expect("replace story with directory");
        std::fs::create_dir(&story_path).expect("create directory leaf");
        std::fs::write(&marker_path, "directory marker\n").expect("write directory marker");

        let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_before = git_stdout(dir.path(), &["diff"]);
        let status_before = git_stdout(dir.path(), &["status", "--short"]);
        let marker_before = std::fs::read_to_string(&marker_path).expect("read marker before");

        let request = json!({
            "jsonrpc": "2.0",
            "id": 602 + target_index,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply directory-leaf response");

        assert_eq!(
            response["result"]["isError"], true,
            "directory leaf target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["error_type"], "unsupported_patch_record",
            "directory leaf target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["path"], "story.txt",
            "directory leaf target={target}: {response:?}"
        );
        assert!(
            response["result"]["state"].is_null(),
            "directory leaf target={target}: validation failure should not return apply state: {response:?}"
        );
        assert!(
            response["result"]["applied"].is_null(),
            "directory leaf target={target}: validation failure should not return applied=false as if git ran: {response:?}"
        );
        assert!(
            response["result"]["checked"].is_null(),
            "directory leaf target={target}: validation failure should not return checked=false as if git ran: {response:?}"
        );

        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "directory leaf target={target}: cached diff changed"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            worktree_before,
            "directory leaf target={target}: worktree diff changed"
        );
        assert_eq!(
            git_stdout(dir.path(), &["status", "--short"]),
            status_before,
            "directory leaf target={target}: status changed"
        );
        assert!(
            std::fs::symlink_metadata(&story_path)
                .expect("read directory leaf metadata after")
                .is_dir(),
            "directory leaf target={target}: story.txt stopped being a directory"
        );
        assert_eq!(
            std::fs::read_to_string(&marker_path).expect("read marker after"),
            marker_before,
            "directory leaf target={target}: marker content changed"
        );
    }
}

#[test]
fn test_git_apply_worktree_writing_rejects_symlink_ancestor_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply symlink-ancestor safety test: git not found on PATH");
        return;
    }

    fn setup_nested_story_patch(
        target: &str,
        base: &str,
        edited: &str,
    ) -> (tempfile::TempDir, String) {
        let dir = workspace_tempdir(&format!("git-apply-symlink-ancestor-{target}-"));
        init_git_fixture(dir.path());

        let nested_dir = dir.path().join("dir");
        std::fs::create_dir(&nested_dir).expect("create nested dir");
        let story_path = nested_dir.join("story.txt");
        std::fs::write(&story_path, base).expect("write nested story base");
        run_git(dir.path(), &["add", "dir/story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(&story_path, edited).expect("write nested story edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "dir/story.txt"]);
        std::fs::write(&story_path, base).expect("restore nested story base");
        run_git(dir.path(), &["update-index", "--refresh"]);

        (dir, patch)
    }

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";
    let mut exercised_case = false;

    for (target_index, target) in ["worktree", "index_worktree"].into_iter().enumerate() {
        let (dir, patch) = setup_nested_story_patch(target, base, edited);
        let nested_dir = dir.path().join("dir");
        let story_path = nested_dir.join("story.txt");
        let symlink_target_dir = dir.path().join("symlink-target-dir");
        let symlink_target_story = symlink_target_dir.join("story.txt");
        std::fs::create_dir(&symlink_target_dir).expect("create symlink target dir");
        std::fs::write(&symlink_target_story, base).expect("write symlink target story base");
        std::fs::remove_file(&story_path).expect("remove original nested story");
        std::fs::remove_dir(&nested_dir).expect("replace nested dir with symlink");

        match create_dir_symlink(&symlink_target_dir, &nested_dir) {
            Ok(()) => {
                exercised_case = true;
                assert!(
                    std::fs::symlink_metadata(&nested_dir)
                        .expect("read symlink ancestor metadata before")
                        .file_type()
                        .is_symlink(),
                    "test fixture must replace dir/ with a directory symlink"
                );

                let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
                let worktree_before = git_stdout(dir.path(), &["diff"]);
                let status_before = git_stdout(dir.path(), &["status", "--short"]);
                let story_before =
                    std::fs::read_to_string(&story_path).expect("read symlinked story before");
                let symlink_target_before = std::fs::read_to_string(&symlink_target_story)
                    .expect("read symlink target story before");

                let request = json!({
                    "jsonrpc": "2.0",
                    "id": 600 + target_index,
                    "method": "mcp/tools/call",
                    "params": {
                        "name": "GitApply",
                        "arguments": {
                            "working_dir": dir.path().to_string_lossy().to_string(),
                            "patch": patch,
                            "target": target
                        }
                    }
                });
                let response =
                    send_mcp_message(&request).expect("GitApply symlink ancestor response");

                assert_eq!(
                    response["result"]["isError"], true,
                    "symlink ancestor target={target}: {response:?}"
                );
                assert_eq!(
                    response["result"]["error_type"], "unsupported_patch_record",
                    "symlink ancestor target={target}: {response:?}"
                );
                assert_eq!(
                    response["result"]["path"], "dir/story.txt",
                    "symlink ancestor target={target}: {response:?}"
                );
                assert!(
                    response["result"]["state"].is_null(),
                    "symlink ancestor target={target}: validation failure should not return apply state: {response:?}"
                );
                assert!(
                    response["result"]["applied"].is_null(),
                    "symlink ancestor target={target}: validation failure should not return applied=false as if git ran: {response:?}"
                );
                assert!(
                    response["result"]["checked"].is_null(),
                    "symlink ancestor target={target}: validation failure should not return checked=false as if git ran: {response:?}"
                );

                assert_eq!(
                    git_stdout(dir.path(), &["diff", "--cached"]),
                    cached_before,
                    "symlink ancestor target={target}: cached diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["diff"]),
                    worktree_before,
                    "symlink ancestor target={target}: worktree diff changed"
                );
                assert_eq!(
                    git_stdout(dir.path(), &["status", "--short"]),
                    status_before,
                    "symlink ancestor target={target}: status changed"
                );
                assert!(
                    std::fs::symlink_metadata(&nested_dir)
                        .expect("read symlink ancestor metadata after")
                        .file_type()
                        .is_symlink(),
                    "symlink ancestor target={target}: dir/ stopped being a symlink"
                );
                assert_eq!(
                    std::fs::read_to_string(&story_path).expect("read symlinked story after"),
                    story_before,
                    "symlink ancestor target={target}: dir/story.txt content changed"
                );
                assert_eq!(
                    std::fs::read_to_string(&symlink_target_story)
                        .expect("read symlink target story after"),
                    symlink_target_before,
                    "symlink ancestor target={target}: symlink target content changed"
                );
            }
            Err(err) => {
                eprintln!("Skipping GitApply symlink-ancestor subcase for target={target}: {err}");
            }
        }
    }

    if !exercised_case {
        eprintln!(
            "Skipping GitApply symlink-ancestor safety test: directory symlinks are unavailable"
        );
    }
}

#[test]
fn test_git_apply_worktree_writing_rejects_non_directory_ancestor_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply non-directory-ancestor safety test: git not found on PATH");
        return;
    }

    fn setup_nested_story_patch(
        target: &str,
        base: &str,
        edited: &str,
    ) -> (tempfile::TempDir, String) {
        let dir = workspace_tempdir(&format!("git-apply-non-dir-ancestor-{target}-"));
        init_git_fixture(dir.path());

        let nested_dir = dir.path().join("dir");
        std::fs::create_dir(&nested_dir).expect("create nested dir");
        let story_path = nested_dir.join("story.txt");
        std::fs::write(&story_path, base).expect("write nested story base");
        run_git(dir.path(), &["add", "dir/story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(&story_path, edited).expect("write nested story edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "dir/story.txt"]);
        std::fs::write(&story_path, base).expect("restore nested story base");
        run_git(dir.path(), &["update-index", "--refresh"]);

        (dir, patch)
    }

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";

    for (target_index, target) in ["worktree", "index_worktree"].into_iter().enumerate() {
        let (dir, patch) = setup_nested_story_patch(target, base, edited);
        let nested_dir = dir.path().join("dir");
        let story_path = nested_dir.join("story.txt");
        std::fs::remove_file(&story_path).expect("remove original nested story");
        std::fs::remove_dir(&nested_dir).expect("replace nested dir with file");
        std::fs::write(&nested_dir, "not a directory\n").expect("write non-directory ancestor");

        let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_before = git_stdout(dir.path(), &["diff"]);
        let status_before = git_stdout(dir.path(), &["status", "--short"]);
        let ancestor_before =
            std::fs::read_to_string(&nested_dir).expect("read file ancestor before");

        let request = json!({
            "jsonrpc": "2.0",
            "id": 604 + target_index,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target
                }
            }
        });
        let response =
            send_mcp_message(&request).expect("GitApply non-directory ancestor response");

        assert_eq!(
            response["result"]["isError"], true,
            "non-directory ancestor target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["error_type"], "unsupported_patch_record",
            "non-directory ancestor target={target}: {response:?}"
        );
        assert_eq!(
            response["result"]["path"], "dir/story.txt",
            "non-directory ancestor target={target}: {response:?}"
        );
        assert!(
            response["result"]["state"].is_null(),
            "non-directory ancestor target={target}: validation failure should not return apply state: {response:?}"
        );
        assert!(
            response["result"]["applied"].is_null(),
            "non-directory ancestor target={target}: validation failure should not return applied=false as if git ran: {response:?}"
        );
        assert!(
            response["result"]["checked"].is_null(),
            "non-directory ancestor target={target}: validation failure should not return checked=false as if git ran: {response:?}"
        );

        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "non-directory ancestor target={target}: cached diff changed"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            worktree_before,
            "non-directory ancestor target={target}: worktree diff changed"
        );
        assert_eq!(
            git_stdout(dir.path(), &["status", "--short"]),
            status_before,
            "non-directory ancestor target={target}: status changed"
        );
        assert!(
            std::fs::symlink_metadata(&nested_dir)
                .expect("read non-directory ancestor metadata after")
                .is_file(),
            "non-directory ancestor target={target}: dir stopped being a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(&nested_dir).expect("read file ancestor after"),
            ancestor_before,
            "non-directory ancestor target={target}: ancestor file content changed"
        );
    }
}

#[test]
fn test_git_apply_non_check_target_semantics() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply target-semantics test: git not found on PATH");
        return;
    }

    for target in ["worktree", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-target-{target}-"));
        init_git_fixture(dir.path());

        let base = "line 1\nline 2\nline 3\n";
        let edited = "line 1\nline 2 edited\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.path().join("story.txt"), edited).expect("write edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        std::fs::write(dir.path().join("story.txt"), base).expect("restore base content");
        run_git(dir.path(), &["update-index", "--refresh"]);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 573,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply target response");
        assert_eq!(
            response["result"]["isError"], false,
            "{target}: {response:?}"
        );
        assert_eq!(response["result"]["state"], "applied");
        assert_eq!(response["result"]["applied"], true);
        assert_eq!(response["result"]["checked"], false);
        assert_eq!(response["result"]["target"], target);

        let cached = git_stdout(dir.path(), &["diff", "--cached"]);
        let unstaged = git_stdout(dir.path(), &["diff"]);
        let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
        assert_eq!(
            current, edited,
            "target {target} should update worktree content"
        );
        if target == "worktree" {
            assert!(
                cached.trim().is_empty(),
                "worktree target must not mutate index: {cached}"
            );
            assert!(
                unstaged.contains("line 2 edited"),
                "worktree target should leave an unstaged diff: {unstaged}"
            );
        } else {
            assert!(
                cached.contains("line 2 edited"),
                "index_worktree target should stage the edit: {cached}"
            );
            assert!(
                unstaged.trim().is_empty(),
                "index_worktree target should leave no unstaged diff: {unstaged}"
            );
        }
    }
}

#[test]
fn test_git_apply_index_lock_classification_by_target() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply index-lock test: git not found on PATH");
        return;
    }

    for target in ["cached", "index_worktree", "worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-index-lock-{target}-"));
        init_git_fixture(dir.path());

        let base = "line 1\nline 2\nline 3\n";
        let edited = "line 1\nline 2 locked\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.path().join("story.txt"), edited).expect("write edited");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        std::fs::write(dir.path().join("story.txt"), base).expect("restore base");

        let lock_rel = git_stdout(dir.path(), &["rev-parse", "--git-path", "index.lock"]);
        let lock_path = dir.path().join(lock_rel.trim());
        std::fs::write(&lock_path, "fixture lock").expect("create index lock");

        let request = json!({
            "jsonrpc": "2.0",
            "id": 528,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply index-lock response");
        std::fs::remove_file(&lock_path).expect("remove fixture index lock");

        let cached = git_stdout(dir.path(), &["diff", "--cached"]);
        let unstaged = git_stdout(dir.path(), &["diff"]);
        let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");

        if target == "worktree" {
            assert_eq!(
                response["result"]["isError"], false,
                "{target}: {response:?}"
            );
            assert_eq!(response["result"]["state"], "applied");
            assert_eq!(response["result"]["applied"], true);
            assert!(
                cached.trim().is_empty(),
                "worktree target must not stage while fixture lock exists: {cached}"
            );
            assert_eq!(
                current, edited,
                "worktree target should apply despite an unrelated index lock"
            );
            assert!(
                unstaged.contains("line 2 locked"),
                "worktree target should leave an unstaged diff: {unstaged}"
            );
        } else {
            assert_eq!(
                response["result"]["isError"], true,
                "{target}: {response:?}"
            );
            assert_eq!(response["result"]["error_type"], "index_locked");
            assert_eq!(response["result"]["state"], "failed");
            assert_eq!(response["result"]["applied"], false);
            assert!(
                cached.trim().is_empty(),
                "index-writing target {target} must not stage after index lock failure: {cached}"
            );
            assert!(
                unstaged.trim().is_empty(),
                "index-writing target {target} must not mutate worktree diff after index lock failure: {unstaged}"
            );
            assert_eq!(
                current, base,
                "index-writing target {target} should leave worktree content unchanged"
            );
        }
    }
}

#[test]
fn test_git_apply_reverse_cached_removes_staged_patch() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply reverse cached test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-reverse-cached");
    init_git_fixture(dir.path());

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(dir.path().join("story.txt"), edited).expect("write edit");
    let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
    std::fs::write(dir.path().join("story.txt"), base).expect("restore base content");
    run_git(dir.path(), &["update-index", "--refresh"]);

    let working_dir = dir.path().to_string_lossy().to_string();
    let apply_request = json!({
        "jsonrpc": "2.0",
        "id": 574,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": working_dir,
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let apply_response = send_mcp_message(&apply_request).expect("GitApply cached response");
    assert_eq!(
        apply_response["result"]["isError"], false,
        "{apply_response:?}"
    );
    assert!(git_stdout(dir.path(), &["diff", "--cached"]).contains("line 2 edited"));

    let reverse_request = json!({
        "jsonrpc": "2.0",
        "id": 575,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": working_dir,
                "patch": patch,
                "target": "cached",
                "reverse": true
            }
        }
    });
    let reverse_response = send_mcp_message(&reverse_request).expect("GitApply reverse response");
    assert_eq!(
        reverse_response["result"]["isError"], false,
        "{reverse_response:?}"
    );
    assert_eq!(reverse_response["result"]["state"], "applied");
    assert_eq!(reverse_response["result"]["applied"], true);
    assert_eq!(reverse_response["result"]["reverse"], true);

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged = git_stdout(dir.path(), &["diff"]);
    let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
    assert!(
        cached.trim().is_empty(),
        "reverse cached apply should clear staged patch: {cached}"
    );
    assert!(
        unstaged.trim().is_empty(),
        "reverse cached apply should not mutate worktree diff: {unstaged}"
    );
    assert_eq!(current, base);
}

#[test]
fn test_git_new_hunk_tools_reject_invalid_request_shapes_before_git() {
    let valid_hash = "a".repeat(64);
    let cases = [
        (
            576,
            "GitApply",
            json!({
                "patch": "diff --git a/story.txt b/story.txt\n",
                "unexpected": true
            }),
        ),
        (
            577,
            "GitHunks",
            json!({
                "unexpected": true
            }),
        ),
        (
            578,
            "GitStageHunks",
            json!({
                "diff_id": format!("sha256:{valid_hash}"),
                "hunk_ids": [format!("0.0.{valid_hash}")],
                "reverse": true
            }),
        ),
        (
            579,
            "GitHunks",
            json!({
                "timeout_ms": 99
            }),
        ),
    ];

    for (request_id, tool_name, arguments) in cases {
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "mcp/tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        });
        let response = send_mcp_message(&request).expect("invalid request response");
        assert_eq!(
            response["result"]["isError"], true,
            "{tool_name}: {response:?}"
        );
        assert_eq!(
            response["result"]["error_type"], "invalid_request",
            "{tool_name}: {response:?}"
        );
    }
}

#[test]
fn test_git_apply_check_only_does_not_mutate_any_target() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply check-only test: git not found on PATH");
        return;
    }

    for target in ["cached", "worktree", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-check-only-{target}-"));
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);

        let base = "line 1\nline 2\nline 3\n";
        let edited = "line 1\nline 2 edited\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.path().join("story.txt"), edited).expect("write edit");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        std::fs::write(dir.path().join("story.txt"), base).expect("restore base content");
        run_git(dir.path(), &["update-index", "--refresh"]);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 523,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target,
                    "check_only": true
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply check-only response");
        assert_eq!(
            response["result"]["isError"], false,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["state"], "checked",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["checked"], true,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["applied"], false,
            "{target}: {response:?}"
        );

        let cached = git_stdout(dir.path(), &["diff", "--cached"]);
        let unstaged = git_stdout(dir.path(), &["diff"]);
        let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
        assert!(
            cached.trim().is_empty(),
            "check-only target {target} must not mutate index: {cached}"
        );
        assert!(
            unstaged.trim().is_empty(),
            "check-only target {target} must not mutate worktree diff: {unstaged}"
        );
        assert_eq!(
            current, base,
            "check-only target {target} must leave file content unchanged"
        );
    }
}

#[test]
fn test_git_apply_check_only_nonzero_is_failed_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply check-only failure test: git not found on PATH");
        return;
    }

    let bad_patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1,3 +1,3 @@\n",
        " line 1\n",
        "-line does not exist\n",
        "+line 2 edited\n",
        " line 3\n"
    );

    for target in ["cached", "worktree", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-check-fail-{target}-"));
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);

        let base = "line 1\nline 2\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
        run_git(dir.path(), &["update-index", "--refresh"]);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 525,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": bad_patch,
                    "target": target,
                    "check_only": true
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply failed check-only response");
        assert_eq!(
            response["result"]["isError"], true,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["state"], "failed",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["checked"], false,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["applied"], false,
            "{target}: {response:?}"
        );
        assert!(
            response["result"]["state_unknown_reason"].is_null(),
            "{target}: {response:?}"
        );

        let cached = git_stdout(dir.path(), &["diff", "--cached"]);
        let unstaged = git_stdout(dir.path(), &["diff"]);
        let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
        assert!(
            cached.trim().is_empty(),
            "failed check-only target {target} must not mutate index: {cached}"
        );
        assert!(
            unstaged.trim().is_empty(),
            "failed check-only target {target} must not mutate worktree diff: {unstaged}"
        );
        assert_eq!(
            current, base,
            "failed check-only target {target} must leave file content unchanged"
        );
    }
}

#[test]
fn test_git_apply_three_way_conflict_reports_unmerged_index() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply three-way conflict test: git not found on PATH");
        return;
    }
    if !git_version_at_least(2, 32) {
        eprintln!("Skipping GitApply three-way conflict test: git >= 2.32 is required");
        return;
    }

    for target in ["cached", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-three-way-conflict-{target}-"));
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);

        let base = "line 1\nline 2\nline 3\n";
        let patch_side = "line 1\nline 2 patch\nline 3\n";
        let local_side = "line 1\nline 2 local\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.path().join("story.txt"), patch_side).expect("write patch side");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        run_git(dir.path(), &["reset", "--hard", "HEAD"]);
        std::fs::write(dir.path().join("story.txt"), local_side).expect("write local side");
        run_git(dir.path(), &["add", "story.txt"]);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 526,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target,
                    "three_way": true
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply three-way conflict response");
        assert_eq!(
            response["result"]["isError"], true,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["state"], "state_unknown",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["state_unknown_reason"], "three_way_conflict",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["error_type"], "three_way_conflict",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["conflicted"], true,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["applied"], false,
            "{target}: {response:?}"
        );

        let unmerged = git_stdout(dir.path(), &["ls-files", "-u"]);
        assert!(
            unmerged.contains("story.txt"),
            "three-way target {target} must leave unmerged index entries: {unmerged}"
        );
        if target == "index_worktree" {
            let current =
                std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
            assert!(
                current.contains("<<<<<<< ours") && current.contains(">>>>>>> theirs"),
                "index_worktree three-way conflict should write conflict markers: {current}"
            );
        }
    }
}

#[test]
fn test_git_apply_and_stage_hunks_reject_preexisting_unmerged_index_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping git unmerged-index rejection test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-tools-preexisting-unmerged-index");
    init_git_fixture(dir.path());

    let base = "line 1\nline 2\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    run_git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 feature\nline 3\n",
    )
    .expect("write feature side");
    run_git(dir.path(), &["commit", "-q", "-am", "feature"]);

    run_git(dir.path(), &["checkout", "-q", "master"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 master\nline 3\n",
    )
    .expect("write master side");
    run_git(dir.path(), &["commit", "-q", "-am", "master"]);

    let merge = try_run_git(dir.path(), &["merge", "feature"]);
    assert!(
        !merge.status.success(),
        "merge should create an unmerged index for this fixture"
    );
    let unmerged_before = git_stdout(dir.path(), &["ls-files", "-u"]);
    assert!(
        !unmerged_before.trim().is_empty(),
        "fixture must have unmerged entries"
    );
    let status_before = git_stdout(dir.path(), &["status", "--short"]);
    let file_before = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
    let working_dir = dir.path().to_string_lossy().to_string();

    let patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1,3 +1,3 @@\n",
        " line 1\n",
        "-line 2\n",
        "+line 2 apply\n",
        " line 3\n"
    );
    let apply_request = json!({
        "jsonrpc": "2.0",
        "id": 568,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": working_dir,
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let apply_response = send_mcp_message(&apply_request).expect("GitApply unmerged response");
    assert_eq!(
        apply_response["result"]["isError"], true,
        "{apply_response:?}"
    );
    assert_eq!(apply_response["result"]["error_type"], "unmerged_index");

    let fake_hash = "a".repeat(64);
    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 569,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": format!("sha256:{fake_hash}"),
                "hunk_ids": [format!("0.0.{fake_hash}")],
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks unmerged response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["error_type"], "unmerged_index");

    assert_eq!(
        git_stdout(dir.path(), &["ls-files", "-u"]),
        unmerged_before,
        "unmerged_index rejection must preserve unmerged entries"
    );
    assert_eq!(
        git_stdout(dir.path(), &["status", "--short"]),
        status_before,
        "unmerged_index rejection must preserve worktree/index status"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("story.txt")).expect("read story"),
        file_before,
        "unmerged_index rejection must not rewrite the conflicted file"
    );
}

#[test]
fn test_git_apply_three_way_check_only_reports_checked_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply three-way check-only test: git not found on PATH");
        return;
    }
    if !git_version_at_least(2, 32) {
        eprintln!("Skipping GitApply three-way check-only test: git >= 2.32 is required");
        return;
    }

    for target in ["cached", "index_worktree"] {
        let dir = workspace_tempdir(&format!("git-apply-three-way-check-{target}-"));
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);

        let base = "line 1\nline 2\nline 3\n";
        let patch_side = "line 1\nline 2 patch\nline 3\n";
        let local_side = "line 1\nline 2 local\nline 3\n";
        std::fs::write(dir.path().join("story.txt"), base).expect("write base");
        run_git(dir.path(), &["add", "story.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.path().join("story.txt"), patch_side).expect("write patch side");
        let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
        run_git(dir.path(), &["reset", "--hard", "HEAD"]);
        std::fs::write(dir.path().join("story.txt"), local_side).expect("write local side");
        run_git(dir.path(), &["add", "story.txt"]);
        let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_before =
            std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");

        let request = json!({
            "jsonrpc": "2.0",
            "id": 527,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": target,
                    "three_way": true,
                    "check_only": true
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply three-way check-only response");
        assert_eq!(
            response["result"]["isError"], false,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["state"], "checked",
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["checked"], true,
            "{target}: {response:?}"
        );
        assert_eq!(
            response["result"]["applied"], false,
            "{target}: {response:?}"
        );
        assert!(
            response["result"]["stderr"]
                .as_str()
                .is_some_and(|stderr| stderr.contains("conflict")),
            "{target}: {response:?}"
        );

        let unmerged = git_stdout(dir.path(), &["ls-files", "-u"]);
        let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
        let worktree_after =
            std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
        assert!(
            unmerged.trim().is_empty(),
            "three-way check-only target {target} must not create unmerged entries: {unmerged}"
        );
        assert_eq!(
            cached_after, cached_before,
            "three-way check-only target {target} must not mutate the index"
        );
        assert_eq!(
            worktree_after, worktree_before,
            "three-way check-only target {target} must not mutate the worktree"
        );
    }
}

#[test]
fn test_git_apply_rejects_three_way_worktree_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply worktree three-way test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-three-way-worktree");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = "line 1\nline 2\nline 3\n";
    let edited = "line 1\nline 2 edited\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(dir.path().join("story.txt"), edited).expect("write edit");
    let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);
    std::fs::write(dir.path().join("story.txt"), base).expect("restore base content");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 524,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "worktree",
                "three_way": true
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply incompatible-options response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(response["result"]["error_type"], "incompatible_options");

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged = git_stdout(dir.path(), &["diff"]);
    let current = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");
    assert!(
        cached.trim().is_empty(),
        "incompatible-options rejection must not mutate index: {cached}"
    );
    assert!(
        unstaged.trim().is_empty(),
        "incompatible-options rejection must not mutate worktree diff: {unstaged}"
    );
    assert_eq!(
        current, base,
        "incompatible-options rejection must leave file content unchanged"
    );
}

#[test]
fn test_git_commit_pre_commit_hook_observes_eof_on_stdin_while_protocol_stays_open() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitCommit hook stdin test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-commit-hook-stdin-eof");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write story");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join("story.txt"), "line 1 edited\n").expect("edit story");
    run_git(dir.path(), &["add", "story.txt"]);

    let hook = dir.path().join(".git").join("hooks").join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\n\
         if IFS= read -r line; then\n\
         \tprintf 'unexpected stdin: %s\\n' \"$line\" > hook-stdin.txt\n\
         \texit 1\n\
         fi\n\
         printf 'stdin eof\\n' > hook-stdin.txt\n",
    )
    .expect("write pre-commit hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook)
            .expect("hook metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).expect("chmod hook");
    }

    let request = json!({
        "jsonrpc": "2.0",
        "id": 519,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitCommit",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "type": "test",
                "message": "hook stdin eof",
                "timeout_ms": 2000
            }
        }
    });

    let mut child = spawn_server().spawn().expect("server should spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let msg = request.to_string();
    stdin.write_all(msg.as_bytes()).expect("write request");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush request");

    let mut reader = BufReader::new(stdout);
    let response_text = read_server_response(&mut reader).expect("GitCommit response");
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    let response: Value = serde_json::from_str(&response_text).expect("response JSON");
    assert_eq!(response["result"]["isError"], false, "{response:?}");
    assert_eq!(response["result"]["timed_out"], false, "{response:?}");
    assert!(
        response["result"]["commit_hash"].is_string(),
        "commit hash should be returned: {response:?}"
    );
    let hook_stdin =
        std::fs::read_to_string(dir.path().join("hook-stdin.txt")).expect("hook stdin marker");
    assert_eq!(hook_stdin, "stdin eof\n");

    let log = git_stdout(dir.path(), &["log", "--oneline", "-1"]);
    assert!(log.contains("test: hook stdin eof"), "{log}");
}

#[test]
fn test_cancelled_git_commit_suppresses_response_but_does_not_rollback_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitCommit cancellation test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-commit-cancel-no-rollback");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write story");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join("story.txt"), "line 1 cancelled\n").expect("edit story");
    run_git(dir.path(), &["add", "story.txt"]);

    let hook = dir.path().join(".git").join("hooks").join("pre-commit");
    let hook_started = dir.path().join("hook-cancel-started.txt");
    std::fs::write(
        &hook,
        "#!/bin/sh\n\
         printf 'started\\n' > hook-cancel-started.txt\n\
         sleep 2\n\
         printf 'finished\\n' > hook-cancel-finished.txt\n",
    )
    .expect("write pre-commit hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook)
            .expect("hook metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).expect("chmod hook");
    }

    let commit_request = json!({
        "jsonrpc": "2.0",
        "id": 520,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitCommit",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "type": "test",
                "message": "cancel no rollback",
                "timeout_ms": 10000
            }
        }
    });
    let cancel_notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {
            "requestId": 520,
            "reason": "integration test cancellation"
        }
    });
    let first_ping = json!({
        "jsonrpc": "2.0",
        "id": 521,
        "method": "mcp/tools/call",
        "params": {
            "name": "Ping",
            "arguments": {}
        }
    });
    let second_ping = json!({
        "jsonrpc": "2.0",
        "id": 522,
        "method": "mcp/tools/call",
        "params": {
            "name": "Ping",
            "arguments": {}
        }
    });

    let mut child = spawn_server().spawn().expect("server should spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let msg = commit_request.to_string();
    stdin.write_all(msg.as_bytes()).expect("write commit");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush commit");

    let hook_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !hook_started.exists() && std::time::Instant::now() < hook_deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        hook_started.exists(),
        "pre-commit hook should start before cancellation is sent"
    );

    for message in [&cancel_notification, &first_ping] {
        let msg = message.to_string();
        stdin.write_all(msg.as_bytes()).expect("write message");
        stdin.write_all(b"\n").expect("write newline");
    }
    stdin.flush().expect("flush cancellation and ping");

    let first_response_text = read_server_response(&mut reader).expect("first ping response");
    let first_response: Value =
        serde_json::from_str(&first_response_text).expect("first response JSON");
    assert_eq!(
        first_response["id"], 521,
        "cancellation notifications have no response and the in-flight commit response should not win the first response slot: {first_response:?}"
    );

    let commit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut latest_log = String::new();
    while std::time::Instant::now() < commit_deadline {
        let output = try_run_git(dir.path(), &["log", "--oneline", "-1"]);
        if output.status.success() {
            latest_log = String::from_utf8_lossy(&output.stdout).into_owned();
            if latest_log.contains("test: cancel no rollback") {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        latest_log.contains("test: cancel no rollback"),
        "cancelled GitCommit should still complete its mutation; latest log was {latest_log:?}"
    );
    std::thread::sleep(std::time::Duration::from_millis(250));

    let msg = second_ping.to_string();
    stdin.write_all(msg.as_bytes()).expect("write second ping");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush second ping");
    let second_response_text = read_server_response(&mut reader).expect("second ping response");
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    let second_response: Value =
        serde_json::from_str(&second_response_text).expect("second response JSON");
    assert_eq!(
        second_response["id"], 522,
        "the cancelled GitCommit terminal response must remain suppressed after the mutation completes: {second_response:?}"
    );
}

#[test]
fn test_git_hunks_rejects_unsupported_object_format() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks object-format test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-sha256");
    let init = try_run_git(dir.path(), &["init", "-q", "--object-format=sha256"]);
    if !init.status.success() {
        eprintln!(
            "Skipping GitHunks object-format test: git does not support sha256 init: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        return;
    }

    let object_format = git_stdout(dir.path(), &["rev-parse", "--show-object-format"]);
    if object_format.trim() != "sha256" {
        eprintln!(
            "Skipping GitHunks object-format test: expected sha256 repo, got {object_format:?}"
        );
        return;
    }

    let working_dir = dir.path().to_string_lossy().to_string();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 509,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(
        response["result"]["error_type"],
        "unsupported_object_format"
    );
    assert_eq!(response["result"]["object_format"], "sha256");
}

#[test]
fn test_git_hunks_rejects_sparse_checkout_config() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks sparse-checkout config test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-sparse-config");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "core.sparseCheckout", "true"]);

    let working_dir = dir.path().to_string_lossy().to_string();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 510,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(
        response["result"]["error_type"],
        "unsupported_repository_metadata"
    );
    assert_eq!(response["result"]["config_key"], "core.sparseCheckout");
}

#[test]
fn test_git_apply_and_hunks_reject_repository_config_include_path() {
    if !command_available(git_bin()) {
        eprintln!("Skipping git config include rejection test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-config-include-rejection");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write story");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    let config = dir.path().join(".git").join("config");
    let mut config_file = std::fs::OpenOptions::new()
        .append(true)
        .open(&config)
        .expect("open repo config");
    writeln!(config_file, "[include]\n\tpath = ../shared.gitconfig").expect("append include");

    let patch = "diff --git a/story.txt b/story.txt\n\
                 index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n\
                 --- a/story.txt\n\
                 +++ b/story.txt\n\
                 @@ -1 +1 @@\n\
                 -line 1\n\
                 +line 1 edited\n";
    for (offset, (name, arguments)) in [
        (
            "GitHunks",
            json!({
                "working_dir": dir.path().to_string_lossy().to_string()
            }),
        ),
        (
            "GitApply",
            json!({
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 515 + offset,
            "method": "mcp/tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        });
        let response = send_mcp_message(&request).expect("git config include response");
        assert_eq!(response["result"]["isError"], true, "{response:?}");
        assert_eq!(
            response["result"]["error_type"],
            "unsupported_repository_metadata"
        );
        assert_eq!(response["result"]["config_key"], "include.path");
    }
}

#[test]
fn test_git_hunks_rejects_split_and_sparse_index_config() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks repository feature config test: git not found on PATH");
        return;
    }

    for (idx, key) in ["core.splitIndex", "index.sparse"].into_iter().enumerate() {
        let dir = workspace_tempdir(&format!("git-hunks-feature-config-{idx}"));
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", key, "true"]);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 520 + idx,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitHunks",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string()
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitHunks response");
        assert_eq!(response["result"]["isError"], true, "{response:?}");
        assert_eq!(
            response["result"]["error_type"],
            "unsupported_repository_metadata"
        );
        assert_eq!(response["result"]["config_key"], key);
    }
}

#[test]
fn test_git_hunks_rejects_split_index_link_extension() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks split-index extension test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-link-extension");
    run_git(dir.path(), &["init", "-q"]);
    std::fs::write(
        dir.path().join(".git").join("index"),
        git_index_with_extension(b"link", 20),
    )
    .expect("write index with split-index link extension");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 515,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string()
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(
        response["result"]["error_type"],
        "unsupported_repository_metadata"
    );
    assert_eq!(response["result"]["index_extension"], "link");
}

#[test]
fn test_git_hunks_rejects_linked_worktree_metadata() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks linked-worktree metadata test: git not found on PATH");
        return;
    }

    let main = workspace_tempdir("git-hunks-linked-main");
    let linked = workspace_tempdir("git-hunks-linked-worktree");
    init_git_fixture(main.path());
    std::fs::write(main.path().join("story.txt"), "line 1\n").expect("write base");
    run_git(main.path(), &["add", "story.txt"]);
    run_git(main.path(), &["commit", "-q", "-m", "initial"]);

    let linked_path = linked.path().to_string_lossy().to_string();
    run_git(
        main.path(),
        &["worktree", "add", "-q", &linked_path, "HEAD"],
    );

    let request = json!({
        "jsonrpc": "2.0",
        "id": 516,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": linked_path
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks linked-worktree response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(
        response["result"]["error_type"],
        "unsupported_repository_metadata"
    );
}

#[test]
fn test_git_hunks_rejects_authority_escaping_object_metadata() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks metadata authority test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-metadata-authority");
    run_git(dir.path(), &["init", "-q"]);
    let Some(target) = workspace_root().parent().map(std::path::Path::to_path_buf) else {
        eprintln!("Skipping GitHunks metadata authority test: workspace root has no parent");
        return;
    };
    let link = dir.path().join(".git").join("objects").join("aa");
    if let Err(err) = create_dir_symlink(&target, &link) {
        eprintln!(
            "Skipping GitHunks metadata authority test: directory symlinks unavailable: {err}"
        );
        return;
    }

    let request = json!({
        "jsonrpc": "2.0",
        "id": 517,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string()
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks metadata authority response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(
        response["result"]["error_type"],
        "git_metadata_outside_authority"
    );
    assert_eq!(
        response["result"]["path"],
        link.to_string_lossy().to_string()
    );
}

#[test]
fn test_git_hunks_reports_binary_change_as_unsupported_hunkless_record() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks binary unsupported-record test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-binary-unsupported");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("image.bin"), b"\x00\x01base").expect("write binary base");
    run_git(dir.path(), &["add", "image.bin"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join("image.bin"), b"\x00\x02changed").expect("write binary edit");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 580,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string()
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks binary response");
    assert_eq!(response["result"]["isError"], false, "{response:?}");
    let file = &response["result"]["files"][0];
    assert_eq!(file["path"], "image.bin");
    assert_eq!(file["status"], "modified");
    assert_eq!(file["binary"], true);
    assert_eq!(file["supported_for_stage_hunks"], false);
    assert_eq!(file["unsupported_reason"], "binary");
    assert_eq!(file["hunks"], json!([]));
}

#[test]
fn test_git_hunks_reports_mode_only_change_as_unsupported_hunkless_record() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks mode-only unsupported-record test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-mode-only-unsupported");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("script.sh"), "echo base\n").expect("write base");
    run_git(dir.path(), &["add", "script.sh"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    run_git(dir.path(), &["update-index", "--chmod=+x", "script.sh"]);

    let request = json!({
        "jsonrpc": "2.0",
        "id": 581,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "staged": true
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitHunks mode-only response");
    assert_eq!(response["result"]["isError"], false, "{response:?}");
    let file = &response["result"]["files"][0];
    assert_eq!(file["path"], "script.sh");
    assert_eq!(file["status"], "mode_changed");
    assert_eq!(file["binary"], false);
    assert_eq!(file["supported_for_stage_hunks"], false);
    assert_eq!(file["unsupported_reason"], "hunkless");
    assert_eq!(file["hunks"], json!([]));
}

#[test]
fn test_git_apply_rejects_skip_worktree_index_flag() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply skip-worktree test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-skip-worktree");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    run_git(
        dir.path(),
        &["update-index", "--skip-worktree", "story.txt"],
    );

    let patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1 +1 @@\n",
        "-line 1\n",
        "+line 1 edited\n"
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": 511,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(response["result"]["error_type"], "unsupported_patch_record");
    assert_eq!(response["result"]["path"], "story.txt");

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "skip-worktree rejection must not mutate the index: {cached}"
    );
}

#[test]
fn test_git_apply_rejects_assume_unchanged_index_flag() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply assume-unchanged test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-assume-unchanged");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    run_git(
        dir.path(),
        &["update-index", "--assume-unchanged", "story.txt"],
    );

    let patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1 +1 @@\n",
        "-line 1\n",
        "+line 1 edited\n"
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": 514,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(response["result"]["error_type"], "unsupported_patch_record");
    assert_eq!(response["result"]["path"], "story.txt");

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "assume-unchanged rejection must not mutate the index: {cached}"
    );
}

#[test]
fn test_git_apply_rejects_intent_to_add_index_flag() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply intent-to-add test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-intent-to-add");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(
        dir.path(),
        &["commit", "--allow-empty", "-q", "-m", "initial"],
    );
    std::fs::write(dir.path().join("intent.txt"), "line 1\n").expect("write intent file");
    run_git(dir.path(), &["add", "-N", "intent.txt"]);

    let patch = concat!(
        "diff --git a/intent.txt b/intent.txt\n",
        "--- a/intent.txt\n",
        "+++ b/intent.txt\n",
        "@@ -1 +1 @@\n",
        "-line 1\n",
        "+line 1 edited\n"
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": 513,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(response["result"]["error_type"], "unsupported_patch_record");
    assert_eq!(response["result"]["path"], "intent.txt");

    let cached = git_stdout(dir.path(), &["diff", "--cached", "--", "intent.txt"]);
    assert!(
        cached.trim().is_empty(),
        "intent-to-add rejection must not stage the patch: {cached}"
    );
}

#[test]
fn test_git_apply_rejects_all_zero_index_header() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply all-zero index-header test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-zero-index");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    std::fs::write(dir.path().join("story.txt"), "line 1\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let patch = concat!(
        "diff --git a/story.txt b/story.txt\n",
        "index 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222 100644\n",
        "--- a/story.txt\n",
        "+++ b/story.txt\n",
        "@@ -1 +1 @@\n",
        "-line 1\n",
        "+line 1 edited\n"
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": 512,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": dir.path().to_string_lossy().to_string(),
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let response = send_mcp_message(&request).expect("GitApply response");
    assert_eq!(response["result"]["isError"], true, "{response:?}");
    assert_eq!(response["result"]["error_type"], "unsupported_patch_record");
    assert_eq!(
        response["result"]["unsupported_reason"],
        "unsupported_index_header"
    );

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "all-zero index-header rejection must not mutate the index: {cached}"
    );
}

#[test]
fn test_git_apply_rejects_path_escape_as_invalid_patch_path() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitApply invalid patch path test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-apply-invalid-path");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(
        dir.path(),
        &["commit", "--allow-empty", "-q", "-m", "initial"],
    );

    for (offset, patch_path) in ["../escape.txt", "GIT~1/config"].into_iter().enumerate() {
        let patch = format!(
            "diff --git a/{patch_path} b/{patch_path}\n\
             --- a/{patch_path}\n\
             +++ b/{patch_path}\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n"
        );
        let request = json!({
            "jsonrpc": "2.0",
            "id": 516 + offset,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitApply",
                "arguments": {
                    "working_dir": dir.path().to_string_lossy().to_string(),
                    "patch": patch,
                    "target": "cached"
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitApply response");
        assert_eq!(response["result"]["isError"], true, "{response:?}");
        assert_eq!(response["result"]["error_type"], "invalid_patch_path");

        let cached = git_stdout(dir.path(), &["diff", "--cached"]);
        assert!(
            cached.trim().is_empty(),
            "invalid patch path rejection must not mutate the index: {cached}"
        );
    }
}

#[test]
fn test_git_hunks_stage_hunks_prepare_commit_flow() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks/GitStageHunks test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-stage");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut edited: Vec<String> = (1..=20).map(|n| format!("line {n}\n")).collect();
    edited[1] = "line 2 edited\n".to_string();
    edited[14] = "line 15 edited\n".to_string();
    std::fs::write(dir.path().join("story.txt"), edited.concat()).expect("write edits");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 501,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["recommended_next_action"],
        "prepare_commit"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_ids: Vec<String> = hunks_response["result"]["files"][0]["hunks"]
        .as_array()
        .expect("hunks")
        .iter()
        .map(|hunk| hunk["id"].as_str().expect("hunk id").to_string())
        .collect();
    assert_eq!(hunk_ids.len(), 2, "{hunks_response:?}");

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 502,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_ids[0].clone()],
                "context": 3,
                "commit_type": "test",
                "commit_message": "stage first hunk"
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], false,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["commit_ready"], true);
    assert_eq!(stage_response["result"]["verification_state"], "verified");
    assert!(
        stage_response["result"]["post_apply_staged_diff_id"].is_string(),
        "{stage_response:?}"
    );
    assert!(
        stage_response["result"]["post_apply_unstaged_diff_id"].is_string(),
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["full_index_clean_before"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["full_index_verified_after"], true,
        "{stage_response:?}"
    );
    let pre_commit_verification = &stage_response["result"]["pre_commit_verification"];
    assert_eq!(
        pre_commit_verification["full_index_clean_before"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        pre_commit_verification["full_index_verified_after"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        pre_commit_verification["post_apply_full_staged_diff_id"],
        stage_response["result"]["post_apply_full_staged_diff_id"],
        "{stage_response:?}"
    );
    assert_eq!(
        pre_commit_verification["post_apply_full_unstaged_diff_id"],
        stage_response["result"]["post_apply_full_unstaged_diff_id"],
        "{stage_response:?}"
    );
    let commit_template = &stage_response["result"]["commit_call_template"];
    assert_eq!(commit_template["name"], "GitCommit");
    assert_eq!(
        commit_template["arguments"]["working_dir"].as_str(),
        Some(working_dir.as_str())
    );
    assert_eq!(commit_template["arguments"]["type"], "test");
    assert_eq!(commit_template["arguments"]["message"], "stage first hunk");
    assert!(commit_template["arguments"]["scope"].is_null());
    assert_eq!(commit_template["placeholders"]["type"], false);
    assert_eq!(commit_template["placeholders"]["message"], false);
    assert_eq!(commit_template["placeholders"]["scope"], true);
    assert!(
        stage_response["result"]["next_actions"]
            .as_array()
            .expect("next_actions")
            .iter()
            .any(|action| action
                .as_str()
                .is_some_and(|text| text.contains("hunk IDs are expired"))),
        "{stage_response:?}"
    );

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged = git_stdout(dir.path(), &["diff"]);
    assert!(cached.contains("line 2 edited"), "{cached}");
    assert!(!cached.contains("line 15 edited"), "{cached}");
    assert!(unstaged.contains("line 15 edited"), "{unstaged}");
}

#[test]
fn test_git_stage_hunks_prepare_commit_detects_post_index_hook_unstaged_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks post-index hook test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-post-index-hook");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\n").expect("write story base");
    std::fs::write(dir.path().join("other.txt"), "other base\n").expect("write other base");
    run_git(dir.path(), &["add", "story.txt", "other.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2 selected\n")
        .expect("write story edit");
    std::fs::write(dir.path().join("other.txt"), "other before hook\n").expect("write other edit");

    let hook = dir
        .path()
        .join(".git")
        .join("hooks")
        .join("post-index-change");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf 'other changed by hook\\n' > other.txt\n",
    )
    .expect("write post-index-change hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook)
            .expect("hook metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).expect("chmod hook");
    }

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 565,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3,
                "paths": ["story.txt"]
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 566,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3,
                "paths": ["story.txt"]
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");

    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"], "commit_group_verification_mismatch",
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["commit_ready"], false);
    assert_eq!(
        stage_response["result"]["verification_state"],
        "verification_mismatch"
    );
    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(cached.contains("line 2 selected"), "{cached}");
    let other = std::fs::read_to_string(dir.path().join("other.txt")).expect("read other");
    assert_eq!(other, "other changed by hook\n");
}

#[test]
fn test_git_stage_hunks_prepare_commit_detects_post_index_hook_unselected_body_relocation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks post-index hook relocation test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-post-index-hook-relocation");
    init_git_fixture(dir.path());
    std::fs::write(dir.path().join("story.txt"), "story old\n").expect("write story base");
    std::fs::write(dir.path().join("other.txt"), "target\nmiddle\ntarget\n")
        .expect("write other base");
    run_git(dir.path(), &["add", "story.txt", "other.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(dir.path().join("story.txt"), "story selected\n").expect("write story edit");
    std::fs::write(dir.path().join("other.txt"), "changed\nmiddle\ntarget\n")
        .expect("write other pre-hook edit");

    let hook = dir
        .path()
        .join(".git")
        .join("hooks")
        .join("post-index-change");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf 'target\\nmiddle\\nchanged\\n' > other.txt\n",
    )
    .expect("write post-index-change hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook)
            .expect("hook metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).expect("chmod hook");
    }

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 567,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 0,
                "paths": ["story.txt"]
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 568,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 0,
                "paths": ["story.txt"]
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");

    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"], "commit_group_verification_mismatch",
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["commit_ready"], false);
    assert_eq!(
        stage_response["result"]["verification_state"],
        "verification_mismatch"
    );
    let cached = git_stdout(dir.path(), &["diff", "--cached", "-U0"]);
    assert!(cached.contains("story selected"), "{cached}");
    let other = std::fs::read_to_string(dir.path().join("other.txt")).expect("read other");
    assert_eq!(other, "target\nmiddle\nchanged\n");
}

#[test]
fn test_git_hunks_stage_only_template_is_opt_in() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks advanced-template test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-advanced-template");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let default_request = json!({
        "jsonrpc": "2.0",
        "id": 538,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let default_response = send_mcp_message(&default_request).expect("GitHunks default response");
    assert_eq!(
        default_response["result"]["isError"], false,
        "{default_response:?}"
    );
    assert_eq!(
        default_response["result"]["recommended_next_action"],
        "prepare_commit"
    );
    assert!(
        default_response["result"]["advanced_stage_only_template"].is_null(),
        "{default_response:?}"
    );
    assert_eq!(
        default_response["result"]["recommended_next_action_template"]["name"],
        "GitStageHunks"
    );
    assert_eq!(
        default_response["result"]["recommended_next_action_template"]["arguments"]["action"],
        "prepare_commit"
    );

    let advanced_request = json!({
        "jsonrpc": "2.0",
        "id": 539,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3,
                "include_advanced_templates": true
            }
        }
    });
    let advanced_response =
        send_mcp_message(&advanced_request).expect("GitHunks advanced response");
    assert_eq!(
        advanced_response["result"]["isError"], false,
        "{advanced_response:?}"
    );
    let advanced = &advanced_response["result"]["advanced_stage_only_template"];
    assert_eq!(advanced["name"], "GitStageHunks");
    assert_eq!(advanced["arguments"]["action"], "stage_only");
    assert_eq!(
        advanced["arguments"]["diff_id"],
        advanced_response["result"]["diff_id"]
    );
    assert_eq!(advanced["arguments"]["hunk_ids"], json!([]));

    run_git(dir.path(), &["add", "story.txt"]);
    let staged_request = json!({
        "jsonrpc": "2.0",
        "id": 540,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "staged": true,
                "context": 3,
                "include_advanced_templates": true
            }
        }
    });
    let staged_response = send_mcp_message(&staged_request).expect("GitHunks staged response");
    assert_eq!(
        staged_response["result"]["isError"], false,
        "{staged_response:?}"
    );
    assert_eq!(
        staged_response["result"]["recommended_next_action"],
        "unstage"
    );
    assert!(
        staged_response["result"]["advanced_stage_only_template"].is_null(),
        "{staged_response:?}"
    );
}

#[test]
fn test_git_hunks_stage_hunks_handles_quoted_utf8_path() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks quoted UTF-8 path test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-utf8-path");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let file_name = "café.txt";
    std::fs::write(dir.path().join(file_name), "line 1\nline 2\n").expect("write base");
    run_git(dir.path(), &["add", file_name]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join(file_name), "line 1\nline 2 edited\n").expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 521,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "paths": [file_name],
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    assert_eq!(hunks_response["result"]["files"][0]["path"], file_name);
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 522,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "paths": [file_name],
                "context": 3,
                "commit_type": "test",
                "commit_message": "stage utf8 path"
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], false,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["commit_ready"], true);
    assert_eq!(stage_response["result"]["verification_state"], "verified");

    let cached = git_stdout(dir.path(), &["diff", "--cached", "--", file_name]);
    assert!(cached.contains("line 2 edited"), "{cached}");
}

#[test]
fn test_git_stage_hunks_unstage_flow() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks unstage test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-unstage");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=8).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut edited: Vec<String> = (1..=8).map(|n| format!("line {n}\n")).collect();
    edited[1] = "line 2 edited\n".to_string();
    std::fs::write(dir.path().join("story.txt"), edited.concat()).expect("write edit");
    run_git(dir.path(), &["add", "story.txt"]);

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 503,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "staged": true,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks staged response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["recommended_next_action"],
        "unstage"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_ids: Vec<String> = hunks_response["result"]["files"][0]["hunks"]
        .as_array()
        .expect("hunks")
        .iter()
        .map(|hunk| hunk["id"].as_str().expect("hunk id").to_string())
        .collect();
    assert_eq!(hunk_ids.len(), 1, "{hunks_response:?}");

    let unstage_request = json!({
        "jsonrpc": "2.0",
        "id": 504,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_ids[0].clone()],
                "action": "unstage",
                "context": 3
            }
        }
    });
    let unstage_response = send_mcp_message(&unstage_request).expect("GitStageHunks unstage");
    assert_eq!(
        unstage_response["result"]["isError"], false,
        "{unstage_response:?}"
    );
    assert_eq!(unstage_response["result"]["verification_state"], "verified");
    assert_eq!(unstage_response["result"]["commit_ready"], false);
    assert!(
        unstage_response["result"]["post_apply_staged_diff_id"].is_string(),
        "{unstage_response:?}"
    );
    assert!(
        unstage_response["result"]["post_apply_unstaged_diff_id"].is_string(),
        "{unstage_response:?}"
    );

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged = git_stdout(dir.path(), &["diff"]);
    assert!(!cached.contains("line 2 edited"), "{cached}");
    assert!(unstaged.contains("line 2 edited"), "{unstaged}");
}

#[test]
fn test_git_stage_hunks_reports_index_locked() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks index lock test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-index-lock");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=8).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut edited: Vec<String> = (1..=8).map(|n| format!("line {n}\n")).collect();
    edited[1] = "line 2 edited\n".to_string();
    std::fs::write(dir.path().join("story.txt"), edited.concat()).expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 507,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let lock_rel = git_stdout(dir.path(), &["rev-parse", "--git-path", "index.lock"]);
    let lock_path = dir.path().join(lock_rel.trim());
    std::fs::write(&lock_path, "fixture lock").expect("create index lock");
    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 508,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    std::fs::remove_file(&lock_path).expect("remove fixture index lock");

    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["error_type"], "index_locked");
    assert_eq!(stage_response["result"]["state"], "failed");
    assert_eq!(stage_response["result"]["commit_ready"], false);

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "index should remain unstaged after index lock failure: {cached}"
    );
}

#[test]
fn test_git_stage_hunks_prepare_commit_rejects_dirty_index_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks dirty-index test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-dirty-index");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=10).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write story");
    std::fs::write(dir.path().join("other.txt"), "other base\n").expect("write other");
    run_git(dir.path(), &["add", "story.txt", "other.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut edited: Vec<String> = (1..=10).map(|n| format!("line {n}\n")).collect();
    edited[1] = "line 2 edited\n".to_string();
    std::fs::write(dir.path().join("story.txt"), edited.concat()).expect("write story edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 528,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    std::fs::write(dir.path().join("other.txt"), "other staged\n").expect("write other edit");
    run_git(dir.path(), &["add", "other.txt"]);
    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 529,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["error_type"], "index_not_clean");
    assert_eq!(stage_response["result"]["commit_ready"], false);

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(
        cached_after, cached_before,
        "index_not_clean must not mutate staged diff"
    );
    assert_eq!(
        unstaged_after, unstaged_before,
        "index_not_clean must not mutate unstaged diff"
    );
}

#[test]
fn test_git_stage_hunks_stage_only_allows_separate_staged_change() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks stage-only test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-stage-only");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=10).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write story");
    std::fs::write(dir.path().join("other.txt"), "other base\n").expect("write other");
    run_git(dir.path(), &["add", "story.txt", "other.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut edited: Vec<String> = (1..=10).map(|n| format!("line {n}\n")).collect();
    edited[1] = "line 2 edited\n".to_string();
    std::fs::write(dir.path().join("story.txt"), edited.concat()).expect("write story edit");
    std::fs::write(dir.path().join("other.txt"), "other staged\n").expect("write other edit");
    run_git(dir.path(), &["add", "other.txt"]);

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 530,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 531,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "action": "stage_only",
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], false,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["verification_state"], "verified");
    assert_eq!(stage_response["result"]["commit_ready"], false);
    assert!(stage_response["result"]["commit_call_template"].is_null());

    let cached = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged = git_stdout(dir.path(), &["diff"]);
    assert!(cached.contains("line 2 edited"), "{cached}");
    assert!(cached.contains("other staged"), "{cached}");
    assert!(
        !unstaged.contains("line 2 edited"),
        "selected story hunk should be staged: {unstaged}"
    );
}

#[test]
fn test_git_stage_hunks_reports_direction_check_unavailable_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks direction-check-unavailable test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-direction-check-unavailable");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n")
        .expect("write story base");
    std::fs::write(dir.path().join("other.txt"), "other base\n").expect("write other base");
    run_git(dir.path(), &["add", "story.txt", "other.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write story edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 532,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let large_staged = (0..200)
        .map(|idx| format!("other staged line {idx:03} with enough text to exceed cap\n"))
        .collect::<String>();
    std::fs::write(dir.path().join("other.txt"), large_staged).expect("write large staged diff");
    run_git(dir.path(), &["add", "other.txt"]);

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 533,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "action": "stage_only",
                "context": 3,
                "max_bytes": 500
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"], "direction_check_unavailable",
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["cause_error_type"], "diff_output_too_large",
        "{stage_response:?}"
    );

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(
        cached_after, cached_before,
        "direction_check_unavailable must not mutate staged diff"
    );
    assert_eq!(
        unstaged_after, unstaged_before,
        "direction_check_unavailable must not mutate unstaged diff"
    );

    let stale_diff_id = format!("sha256:{}", "0".repeat(64));
    let stale_stage_request = json!({
        "jsonrpc": "2.0",
        "id": 536,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": stale_diff_id,
                "hunk_ids": [hunk_id],
                "action": "stage_only",
                "context": 3,
                "max_bytes": 500
            }
        }
    });
    let stale_stage_response =
        send_mcp_message(&stale_stage_request).expect("GitStageHunks stale response");
    assert_eq!(
        stale_stage_response["result"]["isError"], true,
        "{stale_stage_response:?}"
    );
    assert_eq!(
        stale_stage_response["result"]["error_type"], "stale_diff",
        "{stale_stage_response:?}"
    );
    assert_eq!(
        stale_stage_response["result"]["direction_check_unavailable"], true,
        "{stale_stage_response:?}"
    );
    assert_eq!(
        stale_stage_response["result"]["cause_error_type"], "diff_output_too_large",
        "{stale_stage_response:?}"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "stale_diff with unavailable direction check must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "stale_diff with unavailable direction check must not mutate unstaged diff"
    );
}

#[test]
fn test_git_stage_hunks_rejects_direction_mismatch_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks direction-mismatch test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-direction-mismatch");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = "line 1\nline 2\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 534,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 535,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "action": "unstage",
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["error_type"], "direction_mismatch");

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(cached_after, cached_before);
    assert_eq!(unstaged_after, unstaged_before);
}

#[test]
fn test_git_stage_hunks_rejects_invalid_hunk_ids_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks invalid-ID test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-invalid-ids");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 541,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let invalid_hash = "a".repeat(64);
    let cases = [
        (
            542,
            vec![format!("01.0.{invalid_hash}")],
            "malformed_hunk_ids",
        ),
        (
            543,
            vec![format!("184467440737095516160.0.{invalid_hash}")],
            "malformed_hunk_ids",
        ),
        (
            544,
            vec![hunk_id.clone(), hunk_id.clone()],
            "malformed_hunk_ids",
        ),
        (
            545,
            vec![format!("0.999.{invalid_hash}")],
            "unknown_hunk_ids",
        ),
    ];

    for (request_id, hunk_ids, expected_error_type) in cases {
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitStageHunks",
                "arguments": {
                    "working_dir": working_dir,
                    "diff_id": diff_id,
                    "hunk_ids": hunk_ids,
                    "context": 3
                }
            }
        });
        let response = send_mcp_message(&request).expect("GitStageHunks response");
        assert_eq!(response["result"]["isError"], true, "{response:?}");
        assert_eq!(
            response["result"]["error_type"], expected_error_type,
            "{response:?}"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "{expected_error_type} must not mutate staged diff"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            unstaged_before,
            "{expected_error_type} must not mutate unstaged diff"
        );
    }
}

#[test]
fn test_git_hunks_and_stage_hunks_reject_invalid_paths_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks invalid-path test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-invalid-paths");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let valid_hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 548,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let valid_hunks_response = send_mcp_message(&valid_hunks_request).expect("GitHunks response");
    assert_eq!(
        valid_hunks_response["result"]["isError"], false,
        "{valid_hunks_response:?}"
    );
    let diff_id = valid_hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = valid_hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    for (offset, invalid_path) in ["", "   ", "bad\u{0}path", "GIT~1/config"]
        .into_iter()
        .enumerate()
    {
        let hunks_request = json!({
            "jsonrpc": "2.0",
            "id": 549 + offset,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitHunks",
                "arguments": {
                    "working_dir": working_dir,
                    "paths": [invalid_path],
                    "context": 3
                }
            }
        });
        let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
        assert_eq!(
            hunks_response["result"]["isError"], true,
            "{hunks_response:?}"
        );
        assert_eq!(
            hunks_response["result"]["error_type"], "invalid_pathspec",
            "{hunks_response:?}"
        );

        let stage_request = json!({
            "jsonrpc": "2.0",
            "id": 552 + offset,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitStageHunks",
                "arguments": {
                    "working_dir": working_dir,
                    "diff_id": diff_id,
                    "hunk_ids": [hunk_id.clone()],
                    "paths": [invalid_path],
                    "context": 3
                }
            }
        });
        let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
        assert_eq!(
            stage_response["result"]["isError"], true,
            "{stage_response:?}"
        );
        assert_eq!(
            stage_response["result"]["error_type"], "invalid_pathspec",
            "{stage_response:?}"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "invalid_pathspec must not mutate staged diff"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            unstaged_before,
            "invalid_pathspec must not mutate unstaged diff"
        );
    }
}

#[test]
fn test_git_hunks_and_stage_hunks_reject_path_caps_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks path-cap test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-path-caps");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let valid_hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 555,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let valid_hunks_response = send_mcp_message(&valid_hunks_request).expect("GitHunks response");
    assert_eq!(
        valid_hunks_response["result"]["isError"], false,
        "{valid_hunks_response:?}"
    );
    let diff_id = valid_hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = valid_hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let too_many_paths: Vec<String> = (0..=1000).map(|idx| format!("path-{idx}.txt")).collect();

    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 556,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "paths": too_many_paths.clone(),
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], true,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["error_type"], "path_complexity_limit",
        "{hunks_response:?}"
    );

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 557,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "paths": too_many_paths,
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"], "path_complexity_limit",
        "{stage_response:?}"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "path_complexity_limit must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "path_complexity_limit must not mutate unstaged diff"
    );
}

#[test]
fn test_git_hunks_and_stage_hunks_reject_truncated_diff_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitHunks truncated-diff test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-truncated-diff");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let valid_hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 565,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let valid_hunks_response = send_mcp_message(&valid_hunks_request).expect("GitHunks response");
    assert_eq!(
        valid_hunks_response["result"]["isError"], false,
        "{valid_hunks_response:?}"
    );
    let diff_id = valid_hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = valid_hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);

    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 566,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3,
                "max_bytes": 1
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], true,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["error_type"], "diff_output_too_large",
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["truncated_stdout"], true,
        "{hunks_response:?}"
    );

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 567,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3,
                "max_bytes": 1
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"], "diff_output_too_large",
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["truncated_stdout"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "diff_output_too_large must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "diff_output_too_large must not mutate unstaged diff"
    );
}

#[test]
fn test_git_stage_hunks_rejects_unsupported_hunk_ids_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks unsupported-ID test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-unsupported-ids");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::remove_file(dir.path().join("story.txt")).expect("delete tracked file");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 546,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["files"][0]["supported_for_stage_hunks"],
        false
    );
    assert_eq!(
        hunks_response["result"]["files"][0]["unsupported_reason"],
        "unsupported_change_kind"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();
    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 547,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"],
        "unsupported_hunk_ids"
    );
    assert!(stage_response["result"]["unsupported_hunk_ids"].is_array());

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(cached_after, cached_before);
    assert_eq!(unstaged_after, unstaged_before);
}

#[test]
fn test_git_stage_hunks_rejects_stale_diff_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks stale-diff test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-stale-diff");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = "line 1\nline 2\nline 3\n";
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write first edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 534,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 changed after enumeration\nline 3\n",
    )
    .expect("write stale edit");
    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);

    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 535,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(stage_response["result"]["error_type"], "stale_diff");

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(cached_after, cached_before);
    assert_eq!(unstaged_after, unstaged_before);
}

#[test]
fn test_git_stage_hunks_rejects_scope_mismatch_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks scope-mismatch test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-scope-mismatch");
    init_git_fixture(dir.path());

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 558,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let cases = [
        (
            559,
            json!({
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 0
            }),
            "context mismatch",
        ),
        (
            560,
            json!({
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "paths": ["story.txt"],
                "context": 3
            }),
            "path-scope mismatch",
        ),
    ];

    for (request_id, arguments, label) in cases {
        let stage_request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "mcp/tools/call",
            "params": {
                "name": "GitStageHunks",
                "arguments": arguments
            }
        });
        let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
        assert_eq!(
            stage_response["result"]["isError"], true,
            "{label}: {stage_response:?}"
        );
        assert_eq!(
            stage_response["result"]["error_type"], "stale_diff",
            "{label}: {stage_response:?}"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff", "--cached"]),
            cached_before,
            "{label} must not mutate staged diff"
        );
        assert_eq!(
            git_stdout(dir.path(), &["diff"]),
            unstaged_before,
            "{label} must not mutate unstaged diff"
        );
    }
}

#[test]
fn test_git_stage_hunks_rejects_ambiguous_same_body_subset_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks ambiguous duplicate-body test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-ambiguous-same-body");
    init_git_fixture(dir.path());

    let base = concat!(
        "old\n",
        "middle 1\n",
        "middle 2\n",
        "middle 3\n",
        "middle 4\n",
        "old\n"
    );
    let edited = concat!(
        "new\n",
        "middle 1\n",
        "middle 2\n",
        "middle 3\n",
        "middle 4\n",
        "new\n"
    );
    std::fs::write(dir.path().join("story.txt"), base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join("story.txt"), edited).expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 570,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 0
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let file_hunks = hunks_response["result"]["files"][0]["hunks"]
        .as_array()
        .expect("hunks array");
    assert_eq!(file_hunks.len(), 2, "{hunks_response:?}");
    assert_eq!(file_hunks[0]["body"], file_hunks[1]["body"]);
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_ids = file_hunks
        .iter()
        .map(|hunk| hunk["id"].as_str().expect("hunk id").to_string())
        .collect::<Vec<_>>();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let subset_request = json!({
        "jsonrpc": "2.0",
        "id": 571,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_ids[0].clone()],
                "context": 0
            }
        }
    });
    let subset_response =
        send_mcp_message(&subset_request).expect("GitStageHunks ambiguous response");
    assert_eq!(
        subset_response["result"]["isError"], true,
        "{subset_response:?}"
    );
    assert_eq!(
        subset_response["result"]["error_type"],
        "ambiguous_hunk_ids"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "ambiguous_hunk_ids must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "ambiguous_hunk_ids must not mutate unstaged diff"
    );

    let all_request = json!({
        "jsonrpc": "2.0",
        "id": 572,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": hunk_ids,
                "context": 0
            }
        }
    });
    let all_response =
        send_mcp_message(&all_request).expect("GitStageHunks duplicate-all response");
    assert_eq!(all_response["result"]["isError"], false, "{all_response:?}");
    assert_eq!(all_response["result"]["verification_state"], "verified");
    assert_eq!(all_response["result"]["commit_ready"], true);

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(
        cached_after.matches("+new\n").count(),
        2,
        "both duplicate hunks should be staged: {cached_after}"
    );
    assert!(
        unstaged_after.trim().is_empty(),
        "staging both duplicate hunks should leave no unstaged diff: {unstaged_after}"
    );
}

#[test]
fn test_git_new_hunk_tools_reject_subdirectory_working_dir_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!(
            "Skipping new git hunk tools subdirectory-working-dir test: git not found on PATH"
        );
        return;
    }

    let dir = workspace_tempdir("git-hunks-subdirectory-working-dir");
    init_git_fixture(dir.path());
    std::fs::create_dir(dir.path().join("nested")).expect("create nested dir");

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\nline 3\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.path().join("story.txt"),
        "line 1\nline 2 edited\nline 3\n",
    )
    .expect("write edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let subdir = dir.path().join("nested").to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 561,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();
    let patch = git_stdout(dir.path(), &["diff", "--", "story.txt"]);

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let file_before = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");

    let subdir_hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 562,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": subdir,
                "context": 3
            }
        }
    });
    let subdir_hunks_response =
        send_mcp_message(&subdir_hunks_request).expect("GitHunks subdir response");
    assert_eq!(
        subdir_hunks_response["result"]["isError"], true,
        "{subdir_hunks_response:?}"
    );
    assert_eq!(
        subdir_hunks_response["result"]["error_type"],
        "working_dir_not_worktree_root"
    );

    let subdir_stage_request = json!({
        "jsonrpc": "2.0",
        "id": 563,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": subdir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "context": 3
            }
        }
    });
    let subdir_stage_response =
        send_mcp_message(&subdir_stage_request).expect("GitStageHunks subdir response");
    assert_eq!(
        subdir_stage_response["result"]["isError"], true,
        "{subdir_stage_response:?}"
    );
    assert_eq!(
        subdir_stage_response["result"]["error_type"],
        "working_dir_not_worktree_root"
    );

    let subdir_apply_request = json!({
        "jsonrpc": "2.0",
        "id": 564,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "working_dir": subdir,
                "patch": patch,
                "target": "cached"
            }
        }
    });
    let subdir_apply_response =
        send_mcp_message(&subdir_apply_request).expect("GitApply subdir response");
    assert_eq!(
        subdir_apply_response["result"]["isError"], true,
        "{subdir_apply_response:?}"
    );
    assert_eq!(
        subdir_apply_response["result"]["error_type"],
        "working_dir_not_worktree_root"
    );

    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "subdirectory working_dir rejection must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "subdirectory working_dir rejection must not mutate unstaged diff"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("story.txt")).expect("read story"),
        file_before,
        "subdirectory working_dir rejection must not mutate worktree content"
    );
}

#[test]
fn test_git_new_hunk_tools_reject_omitted_working_dir_from_server_subdirectory() {
    if !command_available(git_bin()) {
        eprintln!(
            "Skipping new git hunk tools omitted-working-dir subdirectory test: git not found on PATH"
        );
        return;
    }

    let dir = workspace_tempdir("git-hunks-omitted-working-dir-subdirectory");
    init_git_fixture(dir.path());
    let subdir = dir.path().join("nested");
    std::fs::create_dir(&subdir).expect("create nested dir");

    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2\n").expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.path().join("story.txt"), "line 1\nline 2 edited\n").expect("write edit");

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let file_before = std::fs::read_to_string(dir.path().join("story.txt")).expect("read story");

    let call_from_subdir = |request: Value| {
        let mut command = spawn_server();
        command.current_dir(&subdir);
        support::send_mcp_message_with_command(&request, command)
            .expect("server response from subdirectory authority")
    };

    let hunks_response = call_from_subdir(json!({
        "jsonrpc": "2.0",
        "id": 565,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "context": 3
            }
        }
    }));
    assert_eq!(
        hunks_response["result"]["isError"], true,
        "{hunks_response:?}"
    );
    assert_eq!(
        hunks_response["result"]["error_type"],
        "repo_not_found_within_authority"
    );

    let apply_response = call_from_subdir(json!({
        "jsonrpc": "2.0",
        "id": 566,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitApply",
            "arguments": {
                "patch": "not a diff\n",
                "target": "cached"
            }
        }
    }));
    assert_eq!(
        apply_response["result"]["isError"], true,
        "{apply_response:?}"
    );
    assert_eq!(
        apply_response["result"]["error_type"],
        "repo_not_found_within_authority"
    );

    let stage_response = call_from_subdir(json!({
        "jsonrpc": "2.0",
        "id": 567,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "diff_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "hunk_ids": [
                    "0.0.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ],
                "context": 3
            }
        }
    }));
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"],
        "repo_not_found_within_authority"
    );

    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached"]),
        cached_before,
        "omitted working_dir rejection from a subdirectory authority must not mutate staged diff"
    );
    assert_eq!(
        git_stdout(dir.path(), &["diff"]),
        unstaged_before,
        "omitted working_dir rejection from a subdirectory authority must not mutate unstaged diff"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("story.txt")).expect("read story"),
        file_before,
        "omitted working_dir rejection from a subdirectory authority must not mutate worktree content"
    );
}

#[test]
fn test_git_stage_hunks_rejects_same_path_mixed_direction_without_mutation() {
    if !command_available(git_bin()) {
        eprintln!("Skipping GitStageHunks mixed-direction test: git not found on PATH");
        return;
    }

    let dir = workspace_tempdir("git-hunks-mixed-direction");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);

    let base = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(dir.path().join("story.txt"), &base).expect("write base");
    run_git(dir.path(), &["add", "story.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial"]);

    let mut staged_edit: Vec<String> = (1..=20).map(|n| format!("line {n}\n")).collect();
    staged_edit[1] = "line 2 staged\n".to_string();
    std::fs::write(dir.path().join("story.txt"), staged_edit.concat()).expect("write staged edit");
    run_git(dir.path(), &["add", "story.txt"]);

    let mut mixed_edit = staged_edit;
    mixed_edit[14] = "line 15 unstaged\n".to_string();
    std::fs::write(dir.path().join("story.txt"), mixed_edit.concat()).expect("write unstaged edit");

    let working_dir = dir.path().to_string_lossy().to_string();
    let hunks_request = json!({
        "jsonrpc": "2.0",
        "id": 536,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitHunks",
            "arguments": {
                "working_dir": working_dir,
                "context": 3
            }
        }
    });
    let hunks_response = send_mcp_message(&hunks_request).expect("GitHunks response");
    assert_eq!(
        hunks_response["result"]["isError"], false,
        "{hunks_response:?}"
    );
    let diff_id = hunks_response["result"]["diff_id"]
        .as_str()
        .expect("diff_id")
        .to_string();
    let hunk_id = hunks_response["result"]["files"][0]["hunks"][0]["id"]
        .as_str()
        .expect("hunk id")
        .to_string();

    let cached_before = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_before = git_stdout(dir.path(), &["diff"]);
    let stage_request = json!({
        "jsonrpc": "2.0",
        "id": 537,
        "method": "mcp/tools/call",
        "params": {
            "name": "GitStageHunks",
            "arguments": {
                "working_dir": working_dir,
                "diff_id": diff_id,
                "hunk_ids": [hunk_id],
                "action": "stage_only",
                "context": 3
            }
        }
    });
    let stage_response = send_mcp_message(&stage_request).expect("GitStageHunks response");
    assert_eq!(
        stage_response["result"]["isError"], true,
        "{stage_response:?}"
    );
    assert_eq!(
        stage_response["result"]["error_type"],
        "mixed_direction_file"
    );
    assert_eq!(stage_response["result"]["path"], "story.txt");

    let cached_after = git_stdout(dir.path(), &["diff", "--cached"]);
    let unstaged_after = git_stdout(dir.path(), &["diff"]);
    assert_eq!(cached_after, cached_before);
    assert_eq!(unstaged_after, unstaged_before);
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
fn test_ado_work_items_validates_arguments_before_network() {
    // Organization and project are valid, but no lookup selector is provided, so the
    // tool rejects the call during argument validation before any Azure CLI or network
    // activity. This exercises tool registration and the request path deterministically.
    let request = json!({
        "jsonrpc": "2.0",
        "id": 32,
        "method": "mcp/tools/call",
        "params": {
            "name": "AdoWorkItems",
            "arguments": {
                "organization": "contoso",
                "project": "Tools"
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call AdoWorkItems");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 32);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["error_type"], "selector_required");
}

#[test]
fn test_ado_work_items_rejects_invalid_resource_argument() {
    // A non-GUID, non-https `resource` is rejected before the Azure CLI is invoked, so a
    // caller-influenced value cannot reach the token command.
    let request = json!({
        "jsonrpc": "2.0",
        "id": 33,
        "method": "mcp/tools/call",
        "params": {
            "name": "AdoWorkItems",
            "arguments": {
                "organization": "contoso",
                "project": "Tools",
                "id": 123,
                "resource": "--cloud-name=AzureUSGovernment"
            }
        }
    });

    let response = send_mcp_message(&request).expect("Failed to call AdoWorkItems");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 33);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["error_type"], "invalid_resource");
}

#[test]
fn test_git_tools_disabled_by_default() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 999,
        "method": "mcp/tools/list",
        "params": {}
    });

    let cases: [Option<&str>; 8] = [
        None,
        Some("false"),
        Some("1"),
        Some("TRUE"),
        Some(""),
        Some(" true"),
        Some("true "),
        Some(" true "),
    ];

    for case in cases {
        let mut command = spawn_server();
        match case {
            Some(value) => {
                command.env("MCP_ENABLE_GIT", value);
            }
            None => {
                command.env_remove("MCP_ENABLE_GIT");
            }
        }

        let response = support::send_mcp_message_with_command(&request, command)
            .expect("Failed to send tools/list with env var");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 999);

        let tools = response["result"]["tools"].as_array().unwrap();

        let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(
            !tool_names.contains(&"git_snapshot"),
            "Expected git_snapshot to be disabled for MCP_ENABLE_GIT={case:?}"
        );
        for name in tool_names {
            assert!(
                !name.starts_with("Git"),
                "Expected Git tools to be disabled for MCP_ENABLE_GIT={case:?}, but found tool: {name}"
            );
        }
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
