use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

pub fn spawn_server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tools-mcp-server"));
    command
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("MCP_ENABLE_GIT", "true");
    command
}

pub fn read_server_response<R: BufRead>(
    reader: &mut R,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut content_length = None;
    let mut line_response = None;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
            continue;
        }

        if line.trim().is_empty() {
            if let Some(len) = content_length {
                let mut buffer = vec![0u8; len];
                reader.read_exact(&mut buffer)?;
                return Ok(String::from_utf8(buffer)?);
            }
            continue;
        }

        line_response = Some(line);
        break;
    }

    line_response.ok_or_else(|| "No response received".into())
}

pub fn send_mcp_message(message: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    send_mcp_message_with_command(message, spawn_server())
}

/// Variant of [`send_mcp_message`] that lets the caller customize the spawned
/// server process (e.g. set environment variables that gate optional tools).
#[allow(dead_code)]
pub fn send_mcp_message_with_command(
    message: &Value,
    mut command: Command,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let msg_str = message.to_string();
    stdin.write_all(msg_str.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    drop(stdin);

    let mut reader = BufReader::new(stdout);
    let response = read_server_response(&mut reader)?;

    let _ = child.kill();
    let _ = child.wait();

    Ok(serde_json::from_str(&response)?)
}

#[allow(dead_code)]
pub fn send_mcp_message_with_headers(message: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut child = spawn_server().spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let msg_str = message.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", msg_str.len());
    stdin.write_all(header.as_bytes())?;
    stdin.write_all(msg_str.as_bytes())?;
    stdin.flush()?;
    drop(stdin);

    let mut reader = BufReader::new(stdout);
    let response = read_server_response(&mut reader)?;

    let _ = child.kill();
    let _ = child.wait();

    Ok(serde_json::from_str(&response)?)
}
