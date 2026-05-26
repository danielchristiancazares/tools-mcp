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
use memchr::memchr;
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

/// Maximum allowed size for the header/preamble section before a framed body.
///
/// This is intentionally much smaller than the message body cap because MCP only
/// requires a small `Content-Length` header. It prevents unbounded header
/// preambles from consuming transport time before a body is read.
pub const MAX_MCP_HEADER_BYTES: usize = 64 * 1024;

const CONTENT_LENGTH_PREFIX: &[u8] = b"Content-Length: ";
const CONTENT_LENGTH_SUFFIX: &[u8] = b"\r\n\r\n";

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
/// - Rejects duplicate `Content-Length` headers and oversized header sections
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
    let mut header_section_bytes = 0usize;
    let mut line_bytes = Vec::with_capacity(128);

    // Parse headers or detect raw JSON
    loop {
        let bytes_read = read_line_bytes_bounded(
            reader,
            saw_headers || saw_non_empty_non_json_line,
            &mut line_bytes,
        )
        .await?;

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

        let trimmed_len = trim_crlf_suffix(&line_bytes).len();
        line_bytes.truncate(trimmed_len);

        // Auto-detect raw JSON mode (line starts with { or [)
        if content_length.is_none() && !saw_non_empty_non_json_line {
            match raw_json_line_body(line_bytes) {
                RawJsonLineBody::Body(body) => {
                    return Ok(Some(McpMessage {
                        body,
                        has_headers: false,
                    }));
                }
                RawJsonLineBody::NotRaw(bytes) => {
                    line_bytes = bytes;
                }
            }
        }
        let trimmed = line_bytes.as_slice();

        header_section_bytes = header_section_bytes.saturating_add(bytes_read);
        if header_section_bytes > MAX_MCP_HEADER_BYTES {
            return Err(McpReadError {
                error: io::Error::new(
                    ErrorKind::InvalidData,
                    "MCP header section exceeds maximum allowed size",
                ),
                response_has_headers: saw_headers
                    || saw_non_empty_non_json_line
                    || !trimmed.is_empty(),
                should_continue: false,
            });
        }

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

        // Parse Content-Length header (case-insensitive)
        if let Some(colon_index) = memchr(b':', trimmed) {
            let name = &trimmed[..colon_index];
            if !header_name_eq_ignore_ascii_case(name, "content-length") {
                saw_non_empty_non_json_line = true;
                continue;
            }

            if content_length.is_some() {
                return Err(McpReadError {
                    error: io::Error::new(
                        ErrorKind::InvalidData,
                        "duplicate Content-Length header",
                    ),
                    response_has_headers: true,
                    should_continue: false,
                });
            }
            let value =
                std::str::from_utf8(&trimmed[colon_index + 1..]).map_err(|_| McpReadError {
                    error: io::Error::new(ErrorKind::InvalidData, "invalid Content-Length header"),
                    response_has_headers: true,
                    should_continue: false,
                })?;
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
    line_bytes: &mut Vec<u8>,
) -> std::result::Result<usize, McpReadError>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    line_bytes.clear();

    loop {
        let available = reader.fill_buf().await.map_err(|err| McpReadError {
            error: err,
            response_has_headers,
            should_continue: false,
        })?;

        if available.is_empty() {
            return Ok(line_bytes.len());
        }

        let take = memchr(b'\n', available).map_or(available.len(), |index| index + 1);

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
            return Ok(line_bytes.len());
        }
    }
}

fn trim_crlf_suffix(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\r' | b'\n')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

enum RawJsonLineBody {
    Body(String),
    NotRaw(Vec<u8>),
}

fn raw_json_line_body(bytes: Vec<u8>) -> RawJsonLineBody {
    match String::from_utf8(bytes) {
        Ok(line) => {
            let trimmed_start = line.trim_start();
            if trimmed_start.starts_with('{') || trimmed_start.starts_with('[') {
                RawJsonLineBody::Body(line)
            } else {
                RawJsonLineBody::NotRaw(line.into_bytes())
            }
        }
        Err(err) => {
            let bytes = err.into_bytes();
            let trimmed_start = trim_ascii_start(&bytes);
            if matches!(trimmed_start.first(), Some(b'{' | b'[')) {
                RawJsonLineBody::Body(String::from_utf8_lossy(&bytes).into_owned())
            } else {
                RawJsonLineBody::NotRaw(bytes)
            }
        }
    }
}

fn trim_ascii_start(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(byte) if byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    bytes
}

fn header_name_eq_ignore_ascii_case(actual: &[u8], expected: &str) -> bool {
    std::str::from_utf8(actual).is_ok_and(|actual| actual.trim().eq_ignore_ascii_case(expected))
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
        let mut header = [0u8; 64];
        let header_len = format_content_length_header(payload_len, &mut header);
        writer
            .write_all(&header[..header_len])
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

fn format_content_length_header(payload_len: usize, header: &mut [u8; 64]) -> usize {
    let mut cursor = 0usize;
    header[cursor..cursor + CONTENT_LENGTH_PREFIX.len()].copy_from_slice(CONTENT_LENGTH_PREFIX);
    cursor += CONTENT_LENGTH_PREFIX.len();

    cursor += write_usize_decimal(payload_len, &mut header[cursor..]);

    header[cursor..cursor + CONTENT_LENGTH_SUFFIX.len()].copy_from_slice(CONTENT_LENGTH_SUFFIX);
    cursor + CONTENT_LENGTH_SUFFIX.len()
}

fn write_usize_decimal(mut value: usize, out: &mut [u8]) -> usize {
    let mut digits = [0u8; std::mem::size_of::<usize>() * 3];
    let mut len = 0usize;

    loop {
        let index = digits.len() - 1 - len;
        digits[index] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    let start = digits.len() - len;
    out[..len].copy_from_slice(&digits[start..]);
    len
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
    async fn read_mcp_message_reads_raw_json_line_with_crlf_and_leading_space() {
        let input = b"  {\"jsonrpc\":\"2.0\",\"id\":1}\r\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(msg.body, r#"  {"jsonrpc":"2.0","id":1}"#);
        assert!(!msg.has_headers);
    }

    #[tokio::test]
    async fn read_mcp_message_reads_raw_json_array_line() {
        let input = b"[{\"jsonrpc\":\"2.0\",\"id\":1}]\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(msg.body, r#"[{"jsonrpc":"2.0","id":1}]"#);
        assert!(!msg.has_headers);
    }

    #[tokio::test]
    async fn read_mcp_message_reads_invalid_utf8_raw_json_lossy() {
        let input = b"{\"payload\":\"\xFF\"}\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(msg.body, "{\"payload\":\"\u{FFFD}\"}");
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
    async fn read_mcp_message_accepts_content_length_case_and_ascii_whitespace() {
        let input = b"  content-length \t: \t 2 \r\n\r\nok\n";
        let mut reader = BufReader::new(&input[..]);
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(msg.body, "ok");
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
    async fn read_mcp_message_errors_on_duplicate_content_length() {
        let input = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(&input[..]);
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected duplicate Content-Length to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.error.to_string().contains("duplicate Content-Length"));
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_rejects_oversized_header_section() {
        let input = format!("X-Fill: {}\r\n\r\n", "a".repeat(MAX_MCP_HEADER_BYTES));
        let mut reader = BufReader::new(input.as_bytes());
        let err = read_mcp_message(&mut reader)
            .await
            .expect_err("expected oversized header section to error");
        assert_eq!(err.error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.error
                .to_string()
                .contains("header section exceeds maximum allowed size")
        );
        assert!(err.response_has_headers);
        assert!(!err.should_continue);
    }

    #[tokio::test]
    async fn read_mcp_message_allows_raw_json_larger_than_header_section_limit() {
        let payload = "a".repeat(MAX_MCP_HEADER_BYTES + 16);
        let input = format!("{{\"payload\":\"{payload}\"}}\n");
        let mut reader = BufReader::new(input.as_bytes());
        let msg = read_mcp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg.body, input.trim_end_matches('\n'));
        assert!(!msg.has_headers);
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
