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
pub async fn read_mcp_message<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    use std::io::ErrorKind;

    let mut content_length: Option<usize> = None;

    // Parse headers or detect raw JSON
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;

        // Clean EOF - no more messages
        if bytes_read == 0 {
            if content_length.is_some() {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading headers",
                ));
            }
            return Ok(None);
        }

        // DoS protection: reject oversized lines
        if line.len() > MAX_MCP_MESSAGE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "message line exceeds maximum allowed size",
            ));
        }

        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);

        // Empty line signals end of headers (if we have Content-Length)
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue; // Skip leading blank lines
        }

        // Auto-detect raw JSON mode (line starts with { or [)
        let trimmed_start = trimmed.trim_start();
        if content_length.is_none()
            && (trimmed_start.starts_with('{') || trimmed_start.starts_with('['))
        {
            return Ok(Some(trimmed.to_owned()));
        }

        // Parse Content-Length header (case-insensitive)
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let len = value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(ErrorKind::InvalidData, "invalid Content-Length header")
            })?;
            if len > MAX_MCP_MESSAGE_BYTES {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "Content-Length exceeds maximum allowed size",
                ));
            }
            content_length = Some(len);
        }
    }

    // Read the message body using the Content-Length
    let len = content_length
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "missing Content-Length header"))?;

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let message = String::from_utf8(buf)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "message body not valid UTF-8"))?;

    // Consume optional trailing newline after message body
    let trailing = reader.fill_buf().await?;
    if trailing.starts_with(b"\r\n") {
        reader.consume(2);
    } else if trailing.starts_with(b"\n") {
        reader.consume(1);
    }

    Ok(Some(message))
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
///
/// # Important
///
/// This function always flushes the writer after writing. This is critical
/// for clients that read responses synchronously.
pub async fn write_mcp_response<W>(writer: &mut W, resp: &RpcResponse<'_>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_vec(resp).context("serialize response")?;

    // Ensure payload ends with newline for clean line-based parsing
    if !payload.ends_with(b"\n") {
        payload.push(b'\n');
    }

    let payload_len = payload.len();

    // Write Content-Length header unless in raw JSON mode
    if !should_skip_headers() {
        let header = format!("Content-Length: {}\r\n\r\n", payload_len);
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
