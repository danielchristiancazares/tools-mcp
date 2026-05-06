//! MCP protocol message framing and I/O.
//!
//! This module handles the low-level protocol details of MCP communication:
//! - Content-Length framed message reading
//! - Raw JSON line detection (auto-detect mode)
//! - Response serialization with optional headers
//!
//! # Framing Modes
//!
//! MCP messages can be framed in two ways:
//!
//! ## Content-Length Headers (default)
//! ```text
//! Content-Length: 42\r\n
//! \r\n
//! {"jsonrpc":"2.0","id":1,"method":"ping"}
//! ```
//!
//! ## Raw JSON Lines (`MCP_SKIP_HEADERS=true`)
//! ```text
//! {"jsonrpc":"2.0","id":1,"method":"ping"}
//! ```
//!
//! The reader auto-detects the format based on whether input starts with `{`.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::RpcResponse;

/// Cached value of `MCP_SKIP_HEADERS` environment variable.
static SKIP_HEADERS: OnceLock<bool> = OnceLock::new();

/// Maximum allowed size for inbound MCP messages (10 MiB).
///
/// This limit applies to both Content-Length framed messages and raw JSON lines.
/// It protects against memory exhaustion from malicious Content-Length headers.
pub const MAX_MCP_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

/// Determines whether to skip Content-Length headers in responses.
///
/// When `MCP_SKIP_HEADERS=true`, the server outputs raw JSON without
/// HTTP-style Content-Length framing. Required for Codex compatibility.
///
/// The environment variable is read once and cached for process lifetime.
pub fn should_skip_headers() -> bool {
    *SKIP_HEADERS.get_or_init(|| {
        std::env::var("MCP_SKIP_HEADERS")
            .unwrap_or_default()
            .eq_ignore_ascii_case("true")
    })
}

/// Represents a decoded MCP message and its framing.
#[derive(Debug, Clone)]
pub struct McpMessage {
    /// The JSON message body.
    pub body: String,
    /// True when the message used Content-Length headers.
    pub has_headers: bool,
}

/// Additional metadata for failures while decoding an MCP message.
#[derive(Debug)]
pub struct McpReadError {
    pub error: io::Error,
    pub response_has_headers: bool,
    pub should_continue: bool,
}

/// Reads a single MCP message from an async buffered reader.
///
/// Supports two input formats:
/// - **Content-Length framed**: HTTP-style headers followed by JSON body
/// - **Raw JSON**: Lines starting with `{` or `[` are parsed directly
///
/// # Returns
///
/// - `Ok(Some(message))` - Successfully read a message
/// - `Ok(None)` - Clean EOF (no more messages)
/// - `Err(...)` - I/O or protocol error
///
/// # Protocol Details
///
/// - Handles both CRLF and LF line endings
/// - Consumes trailing newlines after the message body
/// - Ignores empty lines before headers (for robustness)
/// - Case-insensitive header name matching
pub async fn read_mcp_message<R>(
    reader: &mut R,
) -> std::result::Result<Option<McpMessage>, McpReadError>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    let mut content_length: Option<usize> = None;
    let mut saw_headers = false;
    let mut saw_non_empty_non_json_line = false;

    // Parse headers or detect raw JSON
    loop {
        let line_bytes =
            read_line_bytes_bounded(reader, saw_headers || saw_non_empty_non_json_line).await?;
        let bytes_read = line_bytes.len();

        // Clean EOF - no more messages
        if bytes_read == 0 {
            if content_length.is_some() {
                return Err(McpReadError {
                    error: io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "unexpected EOF while reading headers",
                    ),
                    response_has_headers: saw_headers || saw_non_empty_non_json_line,
                    should_continue: false,
                });
            }
            if saw_non_empty_non_json_line {
                return Err(McpReadError {
                    error: io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"),
                    response_has_headers: true,
                    should_continue: false,
                });
            }
            return Ok(None);
        }

        let line = String::from_utf8_lossy(&line_bytes).into_owned();
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);

        // Empty line signals end of headers. If we've seen header-like lines but no
        // Content-Length yet, this is a malformed framed message.
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            if saw_non_empty_non_json_line {
                return Err(McpReadError {
                    error: io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"),
                    response_has_headers: true,
                    should_continue: false,
                });
            }
            continue; // Skip leading blank lines
        }

        // Auto-detect raw JSON mode (line starts with { or [)
        let trimmed_start = trimmed.trim_start();
        if content_length.is_none()
            && !saw_non_empty_non_json_line
            && (trimmed_start.starts_with('{') || trimmed_start.starts_with('['))
        {
            return Ok(Some(McpMessage {
                body: trimmed.to_owned(),
                has_headers: false,
            }));
        }

        // Parse Content-Length header (case-insensitive)
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let len = value.trim().parse::<usize>().map_err(|_| McpReadError {
                error: io::Error::new(ErrorKind::InvalidData, "invalid Content-Length header"),
                response_has_headers: true,
                should_continue: false,
            })?;
            if len > MAX_MCP_MESSAGE_BYTES {
                return Err(McpReadError {
                    error: io::Error::new(
                        ErrorKind::InvalidData,
                        "Content-Length exceeds maximum allowed size",
                    ),
                    response_has_headers: true,
                    should_continue: false,
                });
            }
            content_length = Some(len);
            saw_headers = true;
            continue;
        }

        // A non-empty line that is neither raw JSON nor Content-Length indicates header mode.
        saw_non_empty_non_json_line = true;
    }

    // Read the message body using the Content-Length
    let len = content_length.ok_or_else(|| McpReadError {
        error: io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"),
        response_has_headers: true,
        should_continue: false,
    })?;

    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .await
        .map_err(|err| McpReadError {
            error: err,
            response_has_headers: true,
            should_continue: false,
        })?;
    let message = String::from_utf8(buf).map_err(|_| McpReadError {
        error: io::Error::new(ErrorKind::InvalidData, "message body not valid UTF-8"),
        response_has_headers: true,
        should_continue: true,
    })?;

    // Consume optional trailing newline after message body
    let trailing = reader.fill_buf().await.map_err(|err| McpReadError {
        error: err,
        response_has_headers: true,
        should_continue: false,
    })?;
    if trailing.starts_with(b"\r\n") {
        reader.consume(2);
    } else if trailing.starts_with(b"\n") {
        reader.consume(1);
    }

    Ok(Some(McpMessage {
        body: message,
        has_headers: saw_headers,
    }))
}

async fn read_line_bytes_bounded<R>(
    reader: &mut R,
    response_has_headers: bool,
) -> std::result::Result<Vec<u8>, McpReadError>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    let mut line_bytes = Vec::new();

    loop {
        let available = reader.fill_buf().await.map_err(|err| McpReadError {
            error: err,
            response_has_headers,
            should_continue: false,
        })?;

        if available.is_empty() {
            return Ok(line_bytes);
        }

        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);

        if line_bytes.len().saturating_add(take) > MAX_MCP_MESSAGE_BYTES {
            return Err(McpReadError {
                error: io::Error::new(
                    ErrorKind::InvalidData,
                    "message line exceeds maximum allowed size",
                ),
                response_has_headers,
                should_continue: false,
            });
        }

        let reached_newline = available[take - 1] == b'\n';
        line_bytes.extend_from_slice(&available[..take]);
        reader.consume(take);

        if reached_newline {
            return Ok(line_bytes);
        }
    }
}

/// Writes an MCP response to the output stream.
///
/// Serializes the response to JSON and writes it with optional Content-Length
/// framing based on the `MCP_SKIP_HEADERS` environment variable.
///
/// # Framing Modes
///
/// ## With Headers (default)
/// ```text
/// Content-Length: 42\r\n
/// \r\n
/// {"jsonrpc":"2.0","id":1,"result":{...}}\n
/// ```
///
/// ## Without Headers (`MCP_SKIP_HEADERS=true`)
/// ```text
/// {"jsonrpc":"2.0","id":1,"result":{...}}\n
/// ```
/// Writes an MCP response to the output stream with an explicit framing mode.
///
/// This is used by the runtime when the framing mode is already known and by
/// deterministic tests that need explicit header control.
pub async fn write_mcp_payload_with_mode<W, T>(
    writer: &mut W,
    payload_value: &T,
    skip_headers: bool,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize + ?Sized,
{
    let mut payload = serde_json::to_vec(payload_value).context("serialize response")?;

    // Ensure payload ends with newline for clean line-based parsing
    if !payload.ends_with(b"\n") {
        payload.push(b'\n');
    }

    let payload_len = payload.len();

    // Write Content-Length header unless in raw JSON mode
    if !skip_headers {
        let header = format!("Content-Length: {payload_len}\r\n\r\n");
        writer
            .write_all(header.as_bytes())
            .await
            .context("write Content-Length header")?;
    }

    writer
        .write_all(&payload)
        .await
        .context("write response payload")?;

    // Flush immediately to ensure client receives the response
    writer.flush().await.context("flush stdout")?;
    Ok(())
}

pub async fn write_mcp_response_with_mode<W>(
    writer: &mut W,
    resp: &RpcResponse,
    skip_headers: bool,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_mcp_payload_with_mode(writer, resp, skip_headers).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, BufReader};

    #[tokio::test]
    async fn read_mcp_message_reads_raw_json_line() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1}\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg.body, r#"{"jsonrpc":"2.0","id":1}"#);
        assert!(!msg.has_headers);
    }

    #[tokio::test]
    async fn read_mcp_message_reads_content_length_body_and_consumes_trailing_newline() {
        let input = b"Content-Length: 5\r\n\r\nhello\r\n{\"ok\":true}\n";
        let mut reader = BufReader::new(&input[..]);

        let msg1 = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg1.body, "hello");
        assert!(msg1.has_headers);

        let msg2 = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg2.body, r#"{"ok":true}"#);
        assert!(!msg2.has_headers);
    }

    #[tokio::test]
    async fn read_mcp_message_skips_leading_blank_lines_before_headers() {
        let input = b"\n\nContent-Length: 2\n\nhi\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg.body, "hi");
        assert!(msg.has_headers);
    }

    #[tokio::test]
    async fn read_mcp_message_errors_when_headers_end_without_content_length() {
        let input = b"X-Test: 1\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1}\n";
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected missing Content-Length to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.error.to_string().contains("missing Content-Length"));
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_errors_when_non_json_line_reaches_eof() {
        let input = b"not-json\n";
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected non-json input to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.error.to_string().contains("missing Content-Length"));
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_does_not_ignore_junk_before_raw_json() {
        let input = b"X-Test: 1\n{\"jsonrpc\":\"2.0\",\"id\":1}\n";
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected header-like prelude without Content-Length to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.error.to_string().contains("missing Content-Length"));
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_errors_on_invalid_content_length() {
        let input = b"Content-Length: nope\r\n\r\n";
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected invalid Content-Length to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_errors_on_invalid_utf8_body() {
        let input = [b"Content-Length: 2\r\n\r\n".as_slice(), &[0xFFu8, 0xFFu8]].concat();
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected invalid UTF-8 body to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.response_has_headers);
        assert!(err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_rejects_oversized_line_early() {
        // Create a line that is slightly over the limit without any newline.
        let input = vec![b'a'; MAX_MCP_MESSAGE_BYTES + 1];
        // Note: no newline character.
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected oversized line error");
        assert!(
            err.error
                .to_string()
                .contains("exceeds maximum allowed size")
        );
    }

    #[tokio::test]
    async fn write_mcp_response_with_headers_includes_correct_content_length() {
        let resp = RpcResponse::ok(Some(json!(1)), json!({"ok": true}));
        let (mut r, mut w) = tokio::io::duplex(4096);

        write_mcp_response_with_mode(&mut w, &resp, false)
            .await
            .expect("write failed");
        w.shutdown().await.expect("shutdown failed");

        let mut out = Vec::new();
        r.read_to_end(&mut out).await.expect("read failed");

        let out_str = String::from_utf8(out).expect("output not utf-8");
        let (header, body) = out_str
            .split_once("\r\n\r\n")
            .expect("missing header separator");
        assert!(header.starts_with("Content-Length: "));
        let len: usize = header["Content-Length: ".len()..]
            .trim()
            .parse()
            .expect("invalid length");

        assert_eq!(body.len(), len);
        assert!(body.ends_with('\n'), "payload should end with newline");

        // Validate JSON parses (strip trailing newline).
        let json_body = body.trim_end_matches('\n');
        let v: serde_json::Value = serde_json::from_str(json_body).expect("invalid json");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["isError"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn write_mcp_response_without_headers_is_raw_json_line() {
        let resp = RpcResponse::ok(Some(json!(2)), json!({"ok": true}));
        let (mut r, mut w) = tokio::io::duplex(4096);

        write_mcp_response_with_mode(&mut w, &resp, true)
            .await
            .expect("write failed");
        w.shutdown().await.expect("shutdown failed");

        let mut out = Vec::new();
        r.read_to_end(&mut out).await.expect("read failed");
        let out_str = String::from_utf8(out).expect("output not utf-8");
        assert!(
            !out_str.starts_with("Content-Length:"),
            "raw mode should not include headers"
        );
        assert!(out_str.ends_with('\n'));

        let v: serde_json::Value =
            serde_json::from_str(out_str.trim_end_matches('\n')).expect("invalid json");
        assert_eq!(v["id"], 2);
        assert!(v.get("error").is_none() || v["error"].is_null());
    }
}
