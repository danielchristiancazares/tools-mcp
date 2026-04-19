//! In-memory file representation with LF-normalized canonical view.
//!
//! Files on different platforms use different line ending conventions (LF, CRLF, CR).
//! [`FileModel`] reads a file once and exposes:
//! - the original bytes (for byte-accurate writes back to disk),
//! - a content hash (for staleness detection),
//! - a canonical LF-only view ([`CanonicalData`]) plus offset tables that map canonical
//!   positions back to file byte positions,
//! - per-newline-kind statistics ([`NewlineStats`]) used to pick the dominant style for
//!   replacement content.
//!
//! Callers can search and reason about the file using LF-only text and have edits applied
//! at the correct byte ranges in the original encoding.

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const REPLACEMENT: char = '\u{FFFD}';

/// Complete in-memory representation of a file for editing.
pub(super) struct FileModel {
    /// Raw file content as bytes (preserves original encoding and line endings).
    pub(super) bytes: Vec<u8>,
    /// SHA-256 hash of `bytes` in format `"sha256:<hex>"`.
    pub(super) hash: String,
    /// LF-normalized view with offset mappings back to `bytes`.
    pub(super) canonical: CanonicalData,
    /// Counts of each line ending type found in the file.
    pub(super) newline_stats: NewlineStats,
}

impl FileModel {
    pub(super) fn from_path(path: &Path) -> Result<Self> {
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
/// `text` is the LF-normalized view. `boundaries` is a sorted list mapping canonical
/// character offsets to file byte offsets, used to translate match positions back to
/// the original file (handles multi-byte chars and varying newline lengths).
pub(super) struct CanonicalData {
    /// The complete file content with all newlines normalized to LF.
    pub(super) text: String,
    /// Per-line metadata for quick line number lookups.
    pub(super) line_views: Vec<LineView>,
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

    pub(super) fn byte_offset(&self, canonical_offset: usize) -> Option<usize> {
        if let Ok(index) = self
            .boundaries
            .binary_search_by(|b| b.canonical_offset.cmp(&canonical_offset))
        {
            Some(self.boundaries[index].file_offset)
        } else {
            None
        }
    }

    pub(super) fn line_index_for_offset(&self, canonical_offset: usize) -> Option<usize> {
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
#[derive(Clone)]
struct LineSlice {
    content_start: usize,
    content_end: usize,
    newline_end: usize,
    newline_kind: NewlineKind,
}

/// Line ending style for a single line or an entire file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NewlineKind {
    Lf,
    CrLf,
    Cr,
    None,
}

impl NewlineKind {
    pub(super) fn as_bytes(self) -> &'static [u8] {
        match self {
            NewlineKind::CrLf => b"\r\n",
            NewlineKind::Cr => b"\r",
            NewlineKind::Lf | NewlineKind::None => b"\n",
        }
    }

    pub(super) fn label(self) -> &'static str {
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
/// On ties, [`dominant`](Self::dominant) prefers CRLF > LF > CR, since converting CRLF
/// to LF loses information while LF to CRLF is always safe.
#[derive(Clone, Copy, Default)]
pub(super) struct NewlineStats {
    lf: usize,
    crlf: usize,
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

    pub(super) fn default_kind(&self) -> NewlineKind {
        match self.dominant() {
            NewlineKind::None => NewlineKind::Lf,
            other => other,
        }
    }
}

/// Metadata about a single logical line in the canonical view.
#[derive(Clone)]
pub(super) struct LineView {
    pub(super) canonical_start: usize,
    pub(super) canonical_end: usize,
    pub(super) canonical_full_end: usize,
    pub(super) text: String,
    pub(super) has_trailing_newline: bool,
}

/// Mapping between a canonical text offset and a file byte offset.
#[derive(Clone)]
struct Boundary {
    canonical_offset: usize,
    file_offset: usize,
}

/// Splits raw file bytes into logical lines, detecting line ending types.
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
/// On invalid UTF-8, falls back to byte-by-byte decoding with U+FFFD replacements.
/// Returned boundary vector includes the final offset (equal to `bytes.len()`).
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

/// Returns `"sha256:<64-hex-chars>"` for use in staleness detection.
pub(super) fn compute_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
