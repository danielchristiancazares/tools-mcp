/// Smart, newline-aware file editing helper for MCP.
///
/// This module provides surgical text replacement operations that preserve original line
/// endings while allowing edits to be specified using a canonical LF-normalized view.
/// It solves the fundamental problem of cross-platform text editing: callers can work
/// with consistent LF-only text while the module transparently maintains the file's
/// original CRLF, LF, or CR line endings.
///
/// # Line Ending Preservation System
///
/// Files on different platforms use different line ending conventions:
/// - **LF** (`\n`): Unix, Linux, macOS
/// - **CRLF** (`\r\n`): Windows
/// - **CR** (`\r`): Classic Mac (rare)
///
/// When editing files, it is critical to preserve the original line ending style to avoid:
/// - Spurious diffs that show every line as changed
/// - Breaking tools that expect specific line endings
/// - Inconsistent formatting within a single file
///
const REPLACEMENT: char = '\u{FFFD}';

// This module tracks the line ending style of each file and automatically converts
// replacement text to match the dominant style.
//
// # Canonical LF Processing
//
// Internally, all file content is normalized to a **canonical LF representation**:
//
// 1. The file is read as raw bytes
// 2. Line boundaries are detected (LF, CRLF, or CR)
// 3. A canonical string is built with all newlines normalized to LF
// 4. Offset mappings are maintained between canonical positions and file byte positions
//
// This canonical view enables:
// - **Consistent string matching**: Callers can search using LF-only patterns
// - **Portable snippets**: The same `old_snippet` works regardless of the file's line endings
// - **Accurate byte offsets**: Replacements are written to the exact correct file positions
//
// # Mixed Newline Handling
//
// Real-world files sometimes contain mixed line endings (e.g., a file created on Windows
// but edited on Unix). The module handles this by:
//
// 1. **Tracking statistics**: Counting occurrences of each newline type (LF, CRLF, CR)
// 2. **Determining dominance**: The most frequently used style becomes the "dominant" style
// 3. **Applying consistently**: All new content uses the dominant style
//
// The priority order when counts are equal: CRLF > LF > CR. This prefers the more
// explicit Windows style when ambiguous, as converting CRLF to LF loses information
// while LF to CRLF is always safe.
//
// # Architecture Overview
//
// ```text
//                        +------------------+
//                        |   Raw File Bytes |
//                        +--------+---------+
//                                 |
//                                 v
//                        +------------------+
//                        |   split_lines()  |  Detect line boundaries
//                        +--------+---------+  Track newline types
//                                 |
//                 +---------------+---------------+
//                 |                               |
//                 v                               v
//        +----------------+              +----------------+
//        | CanonicalData  |              | NewlineStats   |
//        | - LF-only text |              | - LF count     |
//        | - Line views   |              | - CRLF count   |
//        | - Boundaries   |              | - CR count     |
//        +----------------+              +----------------+
//                 |                               |
//                 +---------------+---------------+
//                                 |
//                                 v
//                        +------------------+
//                        |    FileModel     |  Complete file representation
//                        +------------------+
// ```
//
// # Usage
//
// The primary entry point is [`handle_edit`], which replaces an exact substring
// (the `old_snippet`) with new content (`new_snippet`). The old snippet must match
// exactly in the canonical view. An optional `match_hint` can constrain the search
// to specific line ranges for disambiguation.
//
// ## Example
//
// ```json
// {
//   "path": "/path/to/file.rs",
//   "old_snippet": "fn old_name(",
//   "new_snippet": "fn new_name(",
//   "match_hint": { "start_line": 15, "end_line": 25 }
// }
// ```
//
// # Error Handling
//
// The module returns structured JSON responses with a `status` field:
// - `"ok"`: Operation succeeded
// - `"no_match"`: The `old_snippet` was not found (includes candidate suggestions)
// - `"stale_file"`: The file changed since the provided hash was computed
//
// All errors include descriptive messages and relevant context for debugging.
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tools_mcp_core::ToolCallOutcome;

/// Edit request - just path, `old_snippet`, `new_snippet`.
#[derive(Deserialize)]
struct SimpleEditRequest {
    path: String,
    old_snippet: String,
    new_snippet: String,
    #[serde(default)]
    match_hint: Option<MatchHint>,
}

/// Simplified edit handler - replaces `old_snippet` with `new_snippet` in a file.
///
/// This is the streamlined interface for the Edit tool. No action field needed.
pub async fn handle_edit(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<SimpleEditRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    if req.old_snippet.is_empty() {
        return ToolCallOutcome::err(
            "old_snippet cannot be empty. Remediation: use Read to copy the exact snippet from the file (use LF newlines), then retry Edit.",
        );
    }

    let internal_req = ApplySnippetEditRequest {
        path: req.path,
        old_snippet: req.old_snippet,
        new_snippet: req.new_snippet,
        match_hint: req.match_hint,
        file_hash: None,
        region_id: None,
    };

    match apply_snippet_edit_impl(&internal_req) {
        Ok(result) => {
            let is_error = !matches!(result.status, SnippetStatusKind::Ok);
            ToolCallOutcome::ok_json_content(&result.payload, is_error)
        }
        Err(err) => ToolCallOutcome::err(format!(
            "edit error: {err}. Remediation: ensure 'path' exists and 'old_snippet' matches exactly; if there are multiple matches, provide match_hint."
        )),
    }
}

/// Request parameters for the `apply_snippet_edit` action.
///
/// Performs a surgical replacement of an exact substring within a file.
/// The `old_snippet` must match exactly in the canonical (LF-normalized) view.
///
/// # Match Behavior
///
/// 1. If `match_hint` is provided, searches only within those lines first
/// 2. If no match in the hint region, falls back to searching the entire file
/// 3. Returns `no_match` status if the snippet is not found anywhere
///
/// # Staleness Check
///
/// If `file_hash` is provided, the operation fails with `stale_file` status
/// if the file's current hash differs from the expected hash.
#[derive(Deserialize)]
struct ApplySnippetEditRequest {
    /// Absolute or relative path to the file to modify.
    path: String,
    /// Exact text to find and replace (must use LF newlines).
    /// Must not be empty.
    old_snippet: String,
    /// Replacement text (must use LF newlines).
    /// LF characters are converted to the file's dominant line ending style.
    new_snippet: String,
    /// Optional line range hint to disambiguate multiple matches.
    #[serde(default)]
    match_hint: Option<MatchHint>,
    /// Expected file hash (from a previous `get_region` call).
    /// If provided and mismatched, the edit is rejected.
    #[serde(default)]
    file_hash: Option<String>,
    /// Optional identifier for tracking the region being edited.
    /// Returned unchanged in the response for correlation.
    #[serde(default)]
    region_id: Option<String>,
}

/// Line range hint for disambiguating snippet matches.
///
/// When a file contains multiple occurrences of `old_snippet`, the match hint
/// narrows the search to a specific region. The search first checks within
/// the hinted range, then falls back to the entire file if no match is found.
///
/// Both line numbers are 1-indexed and inclusive.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MatchHint {
    /// First line of the search region (1-indexed). Defaults to 1.
    #[serde(default)]
    start_line: Option<usize>,
    /// Last line of the search region (1-indexed). Defaults to last line.
    #[serde(default)]
    end_line: Option<usize>,
}

/// Result status for snippet edit operations.
///
/// Used internally to communicate the outcome of an edit attempt,
/// allowing the caller to decide how to handle partial failures.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnippetStatusKind {
    /// Edit applied successfully; file was modified.
    Ok,
    /// The `old_snippet` was not found in the file.
    NoMatch,
    /// The file's hash differs from the expected `file_hash`.
    StaleFile,
}

/// Internal result structure for snippet edit operations.
///
/// Contains both the JSON response payload and metadata.
struct SnippetResult {
    /// Outcome of the edit attempt.
    status: SnippetStatusKind,
    /// JSON response payload to return to the caller.
    payload: Value,
}

/// Test helper: wraps `apply_snippet_edit_impl` and returns the JSON payload.
#[cfg(test)]
fn handle_apply_snippet_edit(req: &ApplySnippetEditRequest) -> Result<Value> {
    let result = apply_snippet_edit_impl(req)?;
    Ok(result.payload)
}

/// Core implementation of snippet editing logic.
///
/// Performs the actual file modification:
/// 1. Reads and parses the file into a [`FileModel`]
/// 2. Validates the file hash if provided (staleness check)
/// 3. Searches for `old_snippet` in the canonical view
/// 4. Computes byte offsets for the matched region
/// 5. Builds replacement bytes with correct line endings
/// 6. Writes the modified content back to disk
///
/// Returns a [`SnippetResult`] with status and metadata, allowing
/// callers to handle partial success (`no_match`, `stale_file`) gracefully.
fn apply_snippet_edit_impl(req: &ApplySnippetEditRequest) -> Result<SnippetResult> {
    if req.old_snippet.is_empty() {
        return Err(anyhow!("old_snippet must not be empty"));
    }
    let old_snippet = normalize_newlines_to_lf(&req.old_snippet);
    let new_snippet = normalize_newlines_to_lf(&req.new_snippet);

    let path = PathBuf::from(&req.path);
    let model = FileModel::from_path(&path)?;

    if let Some(expected_hash) = req
        .file_hash
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        && expected_hash != model.hash
    {
        return Ok(SnippetResult {
            status: SnippetStatusKind::StaleFile,
            payload: json!({
                "action": "apply_snippet_edit",
                "status": "stale_file",
                "message": "file hash mismatch, refresh region before applying changes",
                "expected_file_hash": expected_hash,
                "current_file_hash": model.hash,
            }),
        });
    }

    let maybe_offset = compute_match_range(
        &model.canonical,
        req.match_hint.as_ref(),
        old_snippet.as_str(),
    )?;

    let Some(canonical_start) = maybe_offset else {
        return Ok(SnippetResult {
            status: SnippetStatusKind::NoMatch,
            payload: no_match_payload(&model, old_snippet.as_str(), req.match_hint.as_ref()),
        });
    };

    let canonical_end = canonical_start + old_snippet.len();
    let byte_start = model
        .canonical
        .byte_offset(canonical_start)
        .ok_or_else(|| anyhow!("could not map canonical start offset"))?;
    let byte_end = model
        .canonical
        .byte_offset(canonical_end)
        .ok_or_else(|| anyhow!("could not map canonical end offset"))?;

    let old_slice = model
        .canonical
        .text
        .get(canonical_start..canonical_end)
        .unwrap_or_default();
    if old_slice != old_snippet {
        return Err(anyhow!(
            "internal invariant violated: canonical slice mismatch"
        ));
    }

    let default_newline = model.newline_stats.default_kind();
    let replacement = build_replacement_bytes(&new_snippet, default_newline);

    let mut updated =
        Vec::with_capacity(model.bytes.len() - (byte_end - byte_start) + replacement.len());
    updated.extend_from_slice(&model.bytes[..byte_start]);
    updated.extend_from_slice(&replacement);
    updated.extend_from_slice(&model.bytes[byte_end..]);

    fs::write(&path, &updated)
        .with_context(|| format!("write patched bytes to {}", path.display()))?;

    let new_hash = compute_hash(&updated);
    let start_line = model
        .canonical
        .line_index_for_offset(canonical_start)
        .map_or(1, |idx| idx + 1);
    let end_line = model
        .canonical
        .line_index_for_offset(canonical_end.saturating_sub(1))
        .map_or(start_line, |idx| idx + 1);

    Ok(SnippetResult {
        status: SnippetStatusKind::Ok,
        payload: json!({
            "action": "apply_snippet_edit",
            "status": "ok",
            "replaced_byte_range": [byte_start, byte_end],
            "lines": { "start": start_line, "end": end_line },
            "bytes_written": replacement.len(),
            "file_hash_before": model.hash,
            "file_hash_after": new_hash,
            "newline_kind": default_newline.label(),
            "region_id": req.region_id,
        }),
    })
}

/// Converts CRLF/CR newlines to LF.
fn normalize_newlines_to_lf(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

/// Searches for a needle in the canonical text, optionally constrained by a hint.
///
/// # Search Strategy
///
/// 1. If `hint` is provided, searches **only** within the specified line range
/// 2. If no hint is provided, searches the entire file
/// 3. Returns the canonical offset of the first match, or `None`
///
/// # Arguments
///
/// * `canonical` - The LF-normalized file view to search
/// * `hint` - Optional line range to prioritize
/// * `needle` - The exact string to find (must use LF newlines)
///
/// # Returns
///
/// * `Ok(Some(offset))` - Match found at canonical offset
/// * `Ok(None)` - No match found anywhere in the file
/// * `Err(_)` - Invalid hint line numbers
fn compute_match_range(
    canonical: &CanonicalData,
    hint: Option<&MatchHint>,
    needle: &str,
) -> Result<Option<usize>> {
    if needle.is_empty() {
        return Ok(None);
    }

    let haystack = &canonical.text;
    let search_slice = if let Some(h) = hint {
        let total_lines = canonical.line_views.len();
        let start_line = h.start_line.unwrap_or(1);
        let end_line = h.end_line.unwrap_or(total_lines);
        if start_line == 0
            || end_line < start_line
            || start_line > total_lines
            || end_line > total_lines
        {
            return Err(anyhow!("match_hint lines are invalid for current file"));
        }
        let span = canonical_range_for_lines(&canonical.line_views, start_line, end_line)?;
        Some((span.start, span.end))
    } else {
        None
    };

    if let Some((start, end)) = search_slice {
        return Ok(haystack[start..end].find(needle).map(|rel| start + rel));
    }

    Ok(haystack.find(needle))
}

fn no_match_payload(model: &FileModel, needle: &str, hint: Option<&MatchHint>) -> Value {
    let candidates = suggest_candidates(model, needle, 3);
    json!({
        "action": "apply_snippet_edit",
        "status": "no_match",
        "reason": "old_snippet not found in canonical view",
        "match_hint": hint,
        "candidates": candidates
    })
}

fn suggest_candidates(model: &FileModel, needle: &str, limit: usize) -> Vec<Value> {
    let lines = logical_snippet_lines(needle);
    if lines.is_empty() {
        return Vec::new();
    }

    let target = lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(&lines[0])
        .trim();
    if target.is_empty() {
        return Vec::new();
    }

    let mut start = 0usize;
    let mut seen = Vec::new();
    let mut suggestions = Vec::new();
    while let Some(pos) = model.canonical.text[start..].find(target) {
        let absolute = start + pos;
        let line_idx = model.canonical.line_index_for_offset(absolute).unwrap_or(0);
        if !seen.contains(&line_idx) {
            seen.push(line_idx);
            let similarity = compute_line_similarity(&model.canonical.line_views, line_idx, &lines);
            suggestions.push((line_idx, similarity));
            if suggestions.len() >= limit {
                break;
            }
        }
        start = absolute + target.len();
    }

    suggestions
        .into_iter()
        .map(|(idx, sim)| {
            let start_line = idx + 1;
            let end_line = (idx + lines.len()).min(model.canonical.line_views.len());
            json!({
            "start_line": start_line,
            "end_line": end_line,
                "similarity": sim
            })
        })
        .collect()
}

#[allow(clippy::cast_precision_loss)]
fn compute_line_similarity(views: &[LineView], start_idx: usize, needle_lines: &[&str]) -> f64 {
    if needle_lines.is_empty() {
        return 0.0;
    }
    let mut matches = 0usize;
    for (offset, needle) in needle_lines.iter().enumerate() {
        let idx = start_idx + offset;
        if idx >= views.len() {
            break;
        }
        if views[idx].text == *needle {
            matches += 1;
        }
    }
    matches as f64 / needle_lines.len() as f64
}

fn logical_snippet_lines(snippet: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = snippet.split('\n').collect();
    if snippet.ends_with('\n') && !parts.is_empty() {
        parts.pop();
    }
    if parts.is_empty() {
        parts.push("");
    }
    parts
}

fn canonical_range_for_lines(
    views: &[LineView],
    start_line: usize,
    end_line: usize,
) -> Result<std::ops::Range<usize>> {
    if start_line == 0 || end_line < start_line {
        return Err(anyhow!("invalid line range {start_line}-{end_line}"));
    }
    if start_line > views.len() || end_line > views.len() {
        return Err(anyhow!("line range exceeds total lines"));
    }
    let start_idx = start_line - 1;
    let end_idx = end_line - 1;
    let start = views[start_idx].canonical_start;
    let mut end = views[end_idx].canonical_end;
    if views[end_idx].has_trailing_newline {
        end += 1;
    }
    Ok(start..end)
}

/// Converts a canonical LF-based snippet to bytes with the target newline style.
///
/// This is the key function for line ending preservation: it takes replacement
/// text that uses LF newlines and converts each LF to the file's dominant
/// newline style (LF, CRLF, or CR).
///
/// # Arguments
///
/// * `new_snippet` - Replacement text with LF (`\n`) as line separators
/// * `newline` - Target newline style to use in output
///
/// # Returns
///
/// Byte vector with LF characters replaced by the target newline sequence.
///
/// # Examples
///
/// ```ignore
/// // Converting to CRLF
/// let bytes = build_replacement_bytes("line1\nline2", NewlineKind::CrLf);
/// assert_eq!(bytes, b"line1\r\nline2");
///
/// // Preserving trailing newline
/// let bytes = build_replacement_bytes("line1\n", NewlineKind::CrLf);
/// assert_eq!(bytes, b"line1\r\n");
/// ```
fn build_replacement_bytes(new_snippet: &str, newline: NewlineKind) -> Vec<u8> {
    let newline_bytes = newline.as_bytes();
    let parts: Vec<&str> = new_snippet.split('\n').collect();
    if parts.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (idx, part) in parts.iter().enumerate() {
        out.extend_from_slice(part.as_bytes());
        let is_last = idx + 1 == parts.len();
        if !is_last {
            out.extend_from_slice(newline_bytes);
        }
    }
    out
}

/// Complete in-memory representation of a file for editing.
///
/// Combines the raw file bytes with derived metadata needed for safe editing:
/// - A content-addressable hash for staleness detection
/// - A canonical LF-normalized view for consistent matching
/// - Statistics about line endings for preserving the original style
///
/// Created via [`FileModel::from_path`] which reads and parses the file.
struct FileModel {
    /// Raw file content as bytes (preserves original encoding and line endings).
    bytes: Vec<u8>,
    /// SHA-256 hash of `bytes` in format `"sha256:<hex>"`.
    hash: String,
    /// LF-normalized view with offset mappings back to `bytes`.
    canonical: CanonicalData,
    /// Counts of each line ending type found in the file.
    newline_stats: NewlineStats,
}

impl FileModel {
    fn from_path(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let hash = compute_hash(&bytes);
        let (lines, newline_stats) = split_lines(&bytes);
        let canonical = CanonicalData::from_bytes(&bytes, &lines)?;
        Ok(Self {
            bytes,
            hash,
            canonical,
            newline_stats,
        })
    }
}

/// LF-normalized representation of file content with bidirectional offset mapping.
///
/// This structure is the core of the canonical processing approach:
///
/// 1. **Normalized text**: All line endings are converted to LF (`\n`), enabling
///    consistent string matching regardless of the original file's line ending style.
///
/// 2. **Line views**: Metadata for each logical line, including its position in
///    both the canonical text and whether it has a trailing newline.
///
/// 3. **Boundaries**: A sorted list mapping canonical character offsets to file
///    byte offsets. Used for translating match positions back to the original file.
///
/// # Offset Translation
///
/// When a match is found at canonical offset N, [`byte_offset`](Self::byte_offset)
/// performs a binary search through `boundaries` to find the corresponding file
/// byte position. This handles multi-byte characters and varying newline lengths.
struct CanonicalData {
    /// The complete file content with all newlines normalized to LF.
    text: String,
    /// Per-line metadata for quick line number lookups.
    line_views: Vec<LineView>,
    /// Sorted offset mappings from canonical positions to file byte positions.
    boundaries: Vec<Boundary>,
}

impl CanonicalData {
    fn from_bytes(bytes: &[u8], lines: &[LineSlice]) -> Result<Self> {
        let mut text = String::new();
        let mut line_views = Vec::with_capacity(lines.len());
        let mut boundaries = Vec::new();
        boundaries.push(Boundary {
            canonical_offset: 0,
            file_offset: 0,
        });

        for (idx, line) in lines.iter().enumerate() {
            let content_bytes = &bytes[line.content_start..line.content_end];
            let (line_text, file_boundaries) = decode_line(content_bytes);
            let canonical_start = text.len();
            text.push_str(&line_text);
            let canonical_end = text.len();
            let canonical_boundaries = build_canonical_boundaries(&line_text);

            let has_trailing_newline = line.newline_kind != NewlineKind::None;
            line_views.push(LineView {
                canonical_start,
                canonical_end,
                canonical_full_end: if has_trailing_newline {
                    canonical_end + 1
                } else {
                    canonical_end
                },
                text: line_text,
                has_trailing_newline,
            });

            // map char boundaries to file offsets
            for boundary_idx in 1..file_boundaries.len() {
                let canonical_offset = canonical_start + canonical_boundaries[boundary_idx];
                let file_offset = line.content_start + file_boundaries[boundary_idx];
                boundaries.push(Boundary {
                    canonical_offset,
                    file_offset,
                });
            }

            if line.newline_kind != NewlineKind::None {
                text.push('\n');
                boundaries.push(Boundary {
                    canonical_offset: text.len(),
                    file_offset: line.newline_end,
                });
            }

            if line_views.len() != idx + 1 {
                return Err(anyhow!("failed to record line metadata"));
            }
        }

        if boundaries.is_empty() {
            boundaries.push(Boundary {
                canonical_offset: 0,
                file_offset: 0,
            });
        }

        Ok(Self {
            text,
            line_views,
            boundaries,
        })
    }

    fn byte_offset(&self, canonical_offset: usize) -> Option<usize> {
        if let Ok(index) = self
            .boundaries
            .binary_search_by(|b| b.canonical_offset.cmp(&canonical_offset))
        {
            Some(self.boundaries[index].file_offset)
        } else {
            None
        }
    }

    fn line_index_for_offset(&self, canonical_offset: usize) -> Option<usize> {
        for (idx, view) in self.line_views.iter().enumerate() {
            if canonical_offset < view.canonical_full_end {
                return Some(idx);
            }
        }
        if self.line_views.is_empty() {
            None
        } else {
            Some(self.line_views.len() - 1)
        }
    }
}

/// Byte range of a single line within the raw file bytes.
///
/// Separates the line content from its trailing newline sequence,
/// enabling precise byte-level manipulation during edits.
#[derive(Clone)]
struct LineSlice {
    /// Byte offset where line content begins (inclusive).
    content_start: usize,
    /// Byte offset where line content ends (exclusive, before newline).
    content_end: usize,
    /// Byte offset after the newline sequence (exclusive).
    /// Equal to `content_end` if there is no trailing newline.
    newline_end: usize,
    /// Type of newline terminating this line.
    newline_kind: NewlineKind,
}

/// Line ending style for a single line or an entire file.
///
/// Used both to record the original line ending of each line and to
/// determine the style to use when writing replacement content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NewlineKind {
    /// Unix-style: single line feed (`\n`, 0x0A).
    Lf,
    /// Windows-style: carriage return + line feed (`\r\n`, 0x0D 0x0A).
    CrLf,
    /// Classic Mac-style: single carriage return (`\r`, 0x0D). Rare.
    Cr,
    /// No newline (typically the last line of a file without trailing newline).
    None,
}

impl NewlineKind {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            NewlineKind::CrLf => b"\r\n",
            NewlineKind::Cr => b"\r",
            NewlineKind::Lf | NewlineKind::None => b"\n",
        }
    }

    fn label(self) -> &'static str {
        match self {
            NewlineKind::Lf => "LF",
            NewlineKind::CrLf => "CRLF",
            NewlineKind::Cr => "CR",
            NewlineKind::None => "None",
        }
    }
}

/// Aggregated counts of line ending types within a file.
///
/// Used to determine the "dominant" line ending style, which is applied
/// to all newlines in replacement content. This preserves consistency
/// even when the original file has mixed line endings.
///
/// # Dominance Rules
///
/// When determining the dominant style via [`dominant`](Self::dominant):
/// 1. The style with the highest count wins
/// 2. On ties, priority order is: CRLF > LF > CR
/// 3. If no newlines exist, returns [`NewlineKind::None`]
///
/// The [`default_kind`](Self::default_kind) method returns LF as a fallback
/// when no newlines are present (e.g., single-line files).
#[derive(Clone, Copy, Default)]
struct NewlineStats {
    /// Count of LF (`\n`) line endings.
    lf: usize,
    /// Count of CRLF (`\r\n`) line endings.
    crlf: usize,
    /// Count of CR (`\r`) line endings.
    cr: usize,
}

impl NewlineStats {
    fn record(&mut self, kind: NewlineKind) {
        match kind {
            NewlineKind::Lf => self.lf += 1,
            NewlineKind::CrLf => self.crlf += 1,
            NewlineKind::Cr => self.cr += 1,
            NewlineKind::None => {}
        }
    }

    fn dominant(&self) -> NewlineKind {
        let mut best = (0usize, NewlineKind::None);
        for (count, kind) in [
            (self.crlf, NewlineKind::CrLf),
            (self.lf, NewlineKind::Lf),
            (self.cr, NewlineKind::Cr),
        ] {
            if count > best.0 {
                best = (count, kind);
            }
        }
        best.1
    }

    fn default_kind(&self) -> NewlineKind {
        match self.dominant() {
            NewlineKind::None => NewlineKind::Lf,
            other => other,
        }
    }
}

/// Metadata about a single logical line in the canonical view.
///
/// Tracks both the line's position in the canonical text and whether
/// it originally had a trailing newline. This enables accurate
/// line number lookups and proper handling of files that lack
/// a final newline.
#[derive(Clone)]
struct LineView {
    /// Offset in canonical text where line content begins.
    canonical_start: usize,
    /// Offset in canonical text where line content ends (before newline).
    canonical_end: usize,
    /// Offset after the LF in canonical text, or equal to `canonical_end`
    /// if no trailing newline exists.
    canonical_full_end: usize,
    /// The line's text content (without newline).
    text: String,
    /// Whether this line had a trailing newline in the original file.
    has_trailing_newline: bool,
}

/// Mapping between a canonical text offset and a file byte offset.
///
/// Boundaries are stored in a sorted vector and searched via binary search
/// to translate canonical positions (where matches are found) back to
/// file byte positions (where edits are applied).
///
/// Boundaries are recorded at:
/// - The start of the file (offset 0)
/// - Each character boundary within line content
/// - The end of each newline sequence
#[derive(Clone)]
struct Boundary {
    /// Position in the canonical (LF-normalized) text.
    canonical_offset: usize,
    /// Corresponding position in the raw file bytes.
    file_offset: usize,
}

/// Splits raw file bytes into logical lines, detecting line ending types.
///
/// Scans the byte array for newline sequences (LF, CRLF, CR) and records
/// the position and type of each. Handles all three newline conventions
/// and correctly identifies CRLF as a single two-byte sequence.
///
/// # Arguments
///
/// * `bytes` - Raw file content as bytes
///
/// # Returns
///
/// A tuple containing:
/// - `Vec<LineSlice>`: One entry per logical line with byte ranges
/// - `NewlineStats`: Counts of each newline type encountered
///
/// # Edge Cases
///
/// - Empty input produces a single empty line with `NewlineKind::None`
/// - A file ending without newline has `NewlineKind::None` on the last line
/// - Mixed newlines are tracked individually per line
fn split_lines(bytes: &[u8]) -> (Vec<LineSlice>, NewlineStats) {
    let mut lines = Vec::new();
    let mut stats = NewlineStats::default();
    let mut idx = 0usize;

    while idx < bytes.len() {
        let content_start = idx;
        while idx < bytes.len() && bytes[idx] != b'\n' && bytes[idx] != b'\r' {
            idx += 1;
        }
        let content_end = idx;
        let mut newline_kind = NewlineKind::None;
        if idx < bytes.len() {
            match bytes[idx] {
                b'\r' => {
                    idx += 1;
                    if idx < bytes.len() && bytes[idx] == b'\n' {
                        idx += 1;
                        newline_kind = NewlineKind::CrLf;
                    } else {
                        newline_kind = NewlineKind::Cr;
                    }
                }
                b'\n' => {
                    idx += 1;
                    newline_kind = NewlineKind::Lf;
                }
                _ => {}
            }
        }
        let newline_end = idx;
        stats.record(newline_kind);
        lines.push(LineSlice {
            content_start,
            content_end,
            newline_end,
            newline_kind,
        });
    }

    if lines.is_empty() {
        lines.push(LineSlice {
            content_start: 0,
            content_end: 0,
            newline_end: 0,
            newline_kind: NewlineKind::None,
        });
    }

    (lines, stats)
}

/// Decodes a line's bytes to a string, tracking character boundaries.
///
/// Attempts UTF-8 decoding first. If the bytes are not valid UTF-8,
/// falls back to byte-by-byte decoding with replacement characters
/// (U+FFFD) for invalid sequences.
///
/// # Arguments
///
/// * `bytes` - Raw bytes of a single line (without newline)
///
/// # Returns
///
/// A tuple containing:
/// - `String`: Decoded text (valid UTF-8, possibly with replacement chars)
/// - `Vec<usize>`: Byte offsets of each character boundary, including
///   the final offset (equal to `bytes.len()`)
///
/// # Character Boundary Tracking
///
/// The boundary vector enables accurate offset translation when the
/// line contains multi-byte UTF-8 characters. For example, in a line
/// with an emoji (4 bytes), the boundaries would map character index 0
/// to byte 0, character index 1 to byte 4, etc.
#[allow(clippy::manual_let_else)]
fn decode_line(bytes: &[u8]) -> (String, Vec<usize>) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut boundaries = Vec::with_capacity(text.chars().count() + 1);
        for (idx, _) in text.char_indices() {
            boundaries.push(idx);
        }
        boundaries.push(bytes.len());
        return (text.to_string(), boundaries);
    }

    let mut output = String::with_capacity(bytes.len());
    let mut boundaries = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        boundaries.push(i);
        let byte = bytes[i];
        let width = utf8_char_width(byte);
        if width == 1 {
            output.push(byte as char);
            i += 1;
            continue;
        }
        if width == 0 || i + width > bytes.len() {
            output.push(REPLACEMENT);
            i += 1;
            continue;
        }
        let slice = &bytes[i..i + width];
        if let Ok(valid) = std::str::from_utf8(slice) {
            output.push_str(valid);
            i += width;
        } else {
            output.push(REPLACEMENT);
            i += 1;
        }
    }
    boundaries.push(bytes.len());
    (output, boundaries)
}

fn build_canonical_boundaries(line_text: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(line_text.chars().count() + 1);
    for (idx, _) in line_text.char_indices() {
        offsets.push(idx);
    }
    offsets.push(line_text.len());
    offsets
}

fn utf8_char_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

/// Computes a SHA-256 hash of the given bytes.
///
/// Returns the hash in the format `"sha256:<64-hex-chars>"` for use
/// in staleness detection. This format is chosen to be:
/// - Self-describing (includes algorithm name)
/// - URL-safe (no special characters)
/// - Consistent across platforms
fn compute_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_lone_lf(bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                assert!(
                    i > 0 && bytes[i - 1] == b'\r',
                    "found LF not preceded by CR at byte offset {i}"
                );
            }
        }
    }

    fn assert_contains_subslice(haystack: &[u8], needle: &[u8]) {
        assert!(
            haystack.windows(needle.len().max(1)).any(|w| w == needle),
            "expected output to contain {:?}",
            String::from_utf8_lossy(needle)
        );
    }

    #[test]
    fn test_split_lines_handles_mixed_newlines() {
        let data = b"foo\r\nbar\nbaz\rqux";
        let (lines, stats) = split_lines(data);
        assert_eq!(lines.len(), 4);
        assert_eq!(stats.crlf, 1);
        assert_eq!(stats.lf, 1);
        assert_eq!(stats.cr, 1);
    }

    #[test]
    fn test_build_replacement_bytes_tracks_trailing_newline() {
        let bytes = build_replacement_bytes("a\n", NewlineKind::CrLf);
        assert_eq!(bytes, b"a\r\n");
    }

    #[test]
    fn test_build_replacement_bytes_handles_no_trailing_newline() {
        let bytes = build_replacement_bytes("a\nb", NewlineKind::CrLf);
        assert_eq!(bytes, b"a\r\nb");
    }

    #[test]
    fn normalize_newlines_to_lf_handles_crlf_and_cr() {
        assert_eq!(normalize_newlines_to_lf("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn test_canonical_byte_offsets_cover_line_boundaries() {
        let data = b"line1\r\nline2\n";
        let (lines, _) = split_lines(data);
        let canonical = CanonicalData::from_bytes(data, &lines).expect("canonical data");
        let second_start = canonical.line_views[1].canonical_start;
        assert_eq!(canonical.byte_offset(second_start), Some(7));
        let newline_start = canonical.line_views[0].canonical_end;
        assert_eq!(canonical.byte_offset(newline_start), Some(5));
        let newline_end = canonical.line_views[0].canonical_full_end;
        assert_eq!(canonical.byte_offset(newline_end), Some(7));
    }

    #[test]
    fn apply_snippet_edit_preserves_crlf_newlines_in_replacement_bytes() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("crlf.txt");
        std::fs::write(&path, b"one\r\ntwo\r\nthree\r\n").expect("write");

        let req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "two\nthree".to_string(),
            new_snippet: "TWO\nTHREE".to_string(),
            match_hint: None,
            file_hash: None,
            region_id: None,
        };

        let payload = handle_apply_snippet_edit(&req).expect("apply");
        assert_eq!(payload["status"].as_str(), Some("ok"));
        assert_eq!(payload["newline_kind"].as_str(), Some("CRLF"));

        let out = std::fs::read(&path).expect("read");
        assert_eq!(out, b"one\r\nTWO\r\nTHREE\r\n");
        assert_no_lone_lf(&out);
    }

    #[test]
    fn apply_snippet_edit_accepts_crlf_snippets_from_clients() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("crlf-input.txt");
        std::fs::write(&path, b"one\r\ntwo\r\nthree\r\n").expect("write");

        let req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "two\r\nthree".to_string(),
            new_snippet: "TWO\r\nTHREE".to_string(),
            match_hint: None,
            file_hash: None,
            region_id: None,
        };

        let payload = handle_apply_snippet_edit(&req).expect("apply");
        assert_eq!(payload["status"].as_str(), Some("ok"));

        let out = std::fs::read(&path).expect("read");
        assert_eq!(out, b"one\r\nTWO\r\nTHREE\r\n");
    }

    #[test]
    fn apply_snippet_edit_preserves_cr_newlines_in_replacement_bytes() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("cr.txt");
        std::fs::write(&path, b"one\rtwo\rthree\r").expect("write");

        let req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "two\nthree".to_string(),
            new_snippet: "TWO\nTHREE".to_string(),
            match_hint: None,
            file_hash: None,
            region_id: None,
        };

        let payload = handle_apply_snippet_edit(&req).expect("apply");
        assert_eq!(payload["status"].as_str(), Some("ok"));
        assert_eq!(payload["newline_kind"].as_str(), Some("CR"));

        let out = std::fs::read(&path).expect("read");
        assert_eq!(out, b"one\rTWO\rTHREE\r");
        assert!(
            !out.contains(&b'\n'),
            "CR-only file should not contain LF bytes"
        );
    }

    #[test]
    fn apply_snippet_edit_uses_dominant_newline_for_mixed_file() {
        use tempfile::tempdir;

        // 1 LF + 1 CRLF => tie; dominance prefers CRLF.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("mixed.txt");
        std::fs::write(&path, b"one\ntwo\r\nthree").expect("write");

        let req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "two\nthree".to_string(),
            new_snippet: "TWO\nTHREE".to_string(),
            match_hint: None,
            file_hash: None,
            region_id: None,
        };

        let payload = handle_apply_snippet_edit(&req).expect("apply");
        assert_eq!(payload["status"].as_str(), Some("ok"));
        assert_eq!(payload["newline_kind"].as_str(), Some("CRLF"));

        let out = std::fs::read(&path).expect("read");
        assert_contains_subslice(&out, b"TWO\r\nTHREE");
    }

    #[test]
    fn apply_snippet_edit_rejects_stale_file_hash_and_does_not_modify_file() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("stale.txt");
        std::fs::write(&path, b"alpha\nbeta\n").expect("write");

        let original_hash = FileModel::from_path(&path).expect("model").hash;

        // External modification after the hash was computed.
        std::fs::write(&path, b"alpha\nbeta\nCHANGED\n").expect("rewrite");

        let req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "beta".to_string(),
            new_snippet: "BETA".to_string(),
            match_hint: None,
            file_hash: Some(original_hash.clone()),
            region_id: None,
        };

        let payload = handle_apply_snippet_edit(&req).expect("apply");
        assert_eq!(payload["status"].as_str(), Some("stale_file"));
        assert_eq!(
            payload["expected_file_hash"].as_str(),
            Some(original_hash.as_str())
        );

        let out = std::fs::read(&path).expect("read");
        assert_eq!(out, b"alpha\nbeta\nCHANGED\n");
    }

    #[test]
    fn match_hint_selects_correct_occurrence_and_is_strict() {
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("hint.txt");
        std::fs::write(&path, b"line1\ntarget\nline3\ntarget\nline5\n").expect("write");

        // Replace the second "target" (line 4).
        let ok_req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "target".to_string(),
            new_snippet: "TARGET2".to_string(),
            match_hint: Some(MatchHint {
                start_line: Some(4),
                end_line: Some(4),
            }),
            file_hash: None,
            region_id: None,
        };
        let ok_payload = handle_apply_snippet_edit(&ok_req).expect("apply");
        assert_eq!(ok_payload["status"].as_str(), Some("ok"));

        let out = std::fs::read_to_string(&path).expect("read");
        assert_eq!(out, "line1\ntarget\nline3\nTARGET2\nline5\n");

        // Now prove strictness: a hint range that does NOT include any match should not fall back
        // to editing elsewhere.
        std::fs::write(&path, b"line1\ntarget\nline3\ntarget\nline5\n").expect("reset");
        let no_req = ApplySnippetEditRequest {
            path: path.to_string_lossy().to_string(),
            old_snippet: "target".to_string(),
            new_snippet: "SHOULD_NOT_APPLY".to_string(),
            match_hint: Some(MatchHint {
                start_line: Some(1),
                end_line: Some(1),
            }),
            file_hash: None,
            region_id: None,
        };
        let no_payload = handle_apply_snippet_edit(&no_req).expect("apply");
        assert_eq!(no_payload["status"].as_str(), Some("no_match"));
        let out2 = std::fs::read_to_string(&path).expect("read");
        assert_eq!(out2, "line1\ntarget\nline3\ntarget\nline5\n");
    }
}
