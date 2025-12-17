//! Smart, newline-aware file editing helper for MCP.
use crate::{RpcResponse, err_text};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Primary entry point invoked by the MCP server.
pub async fn handle_smart_file_edit(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if action.is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text("smart_file_edit requires an 'action' field")),
            error: None,
        };
    }

    match action.as_str() {
        "get_region" => match serde_json::from_value::<GetRegionRequest>(args) {
            Ok(req) => match handle_get_region(&req) {
                Ok(payload) => ok_json(id, payload),
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "smart_file_edit get_region error: {err}"
                    ))),
                    error: None,
                },
            },
            Err(err) => RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("invalid get_region arguments: {err}"))),
                error: None,
            },
        },
        "apply_snippet_edit" => match serde_json::from_value::<ApplySnippetEditRequest>(args) {
            Ok(req) => match handle_apply_snippet_edit(&req) {
                Ok(payload) => ok_json(id, payload),
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "smart_file_edit apply_snippet_edit error: {err}"
                    ))),
                    error: None,
                },
            },
            Err(err) => RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "invalid apply_snippet_edit arguments: {err}"
                ))),
                error: None,
            },
        },
        "apply_unified_diff" => match serde_json::from_value::<ApplyUnifiedDiffRequest>(args) {
            Ok(req) => match handle_apply_unified_diff(&req) {
                Ok(payload) => ok_json(id, payload),
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "smart_file_edit apply_unified_diff error: {err}"
                    ))),
                    error: None,
                },
            },
            Err(err) => RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "invalid apply_unified_diff arguments: {err}"
                ))),
                error: None,
            },
        },
        other => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text(&format!(
                "smart_file_edit does not support action '{}'",
                other
            ))),
            error: None,
        },
    }
}

#[derive(Deserialize)]
struct GetRegionRequest {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Deserialize)]
struct ApplySnippetEditRequest {
    path: String,
    old_snippet: String,
    new_snippet: String,
    #[serde(default)]
    match_hint: Option<MatchHint>,
    #[serde(default)]
    file_hash: Option<String>,
    #[serde(default)]
    region_id: Option<String>,
}

#[derive(Deserialize)]
struct ApplyUnifiedDiffRequest {
    path: String,
    diff: String,
    #[serde(default)]
    file_hash: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct MatchHint {
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SnippetStatusKind {
    Ok,
    NoMatch,
    StaleFile,
}

struct SnippetResult {
    status: SnippetStatusKind,
    payload: Value,
    file_hash_before: Option<String>,
    file_hash_after: Option<String>,
}

fn handle_get_region(req: &GetRegionRequest) -> Result<Value> {
    let path = PathBuf::from(&req.path);
    let model = FileModel::from_path(&path)?;

    let total_lines = model.canonical.line_views.len().max(1);
    let start_line = req.start_line.unwrap_or(1);
    if start_line == 0 {
        return Err(anyhow!("start_line must be >= 1"));
    }
    if start_line > total_lines {
        return Err(anyhow!(
            "start_line {} exceeds total lines ({})",
            start_line,
            total_lines
        ));
    }

    let mut end_line = req.end_line.unwrap_or(total_lines);
    if end_line < start_line {
        return Err(anyhow!("end_line must be >= start_line"));
    }
    if end_line > total_lines {
        end_line = total_lines;
    }

    let canonical_range =
        canonical_range_for_lines(&model.canonical.line_views, start_line, end_line)?;
    let byte_start = model
        .canonical
        .byte_offset(canonical_range.start)
        .ok_or_else(|| anyhow!("failed to map canonical start to byte offset"))?;
    let byte_end = model
        .canonical
        .byte_offset(canonical_range.end)
        .ok_or_else(|| anyhow!("failed to map canonical end to byte offset"))?;
    let plain_text = model
        .canonical
        .text
        .get(canonical_range.clone())
        .unwrap_or_default()
        .to_string();
    let numbered = render_numbered_lines(&model.canonical.line_views, start_line, end_line);

    let newline_payload = model.newline_stats.describe();
    let region_id = Uuid::new_v4().to_string();

    Ok(json!({
        "action": "get_region",
        "path": &req.path,
        "start_line": start_line,
        "end_line": end_line,
        "total_lines": total_lines,
        "plain_text": plain_text,
        "canonical_text": numbered,
        "region_id": region_id,
        "file_hash": model.hash,
        "canonical_range": { "start": canonical_range.start, "end": canonical_range.end },
        "byte_range": { "start": byte_start, "end": byte_end },
        "newline_style": newline_payload,
        "file_size_bytes": model.bytes.len()
    }))
}

fn handle_apply_snippet_edit(req: &ApplySnippetEditRequest) -> Result<Value> {
    let result = apply_snippet_edit_impl(req)?;
    Ok(result.payload)
}

fn handle_apply_unified_diff(req: &ApplyUnifiedDiffRequest) -> Result<Value> {
    if req.diff.trim().is_empty() {
        return Err(anyhow!("diff must not be empty"));
    }
    let hunks = parse_unified_diff(&req.diff)?;
    if hunks.is_empty() {
        return Err(anyhow!("diff does not contain any hunks to apply"));
    }

    let mut expected_hash = req.file_hash.clone();
    let mut initial_hash: Option<String> = None;
    let mut final_hash: Option<String> = None;
    let mut applied = Vec::new();

    for (idx, hunk) in hunks.iter().enumerate() {
        let old_snippet = hunk.old_snippet();
        let new_snippet = hunk.new_snippet();

        if old_snippet.is_empty() {
            return Err(anyhow!(
                "hunk {} has no context or removal lines; zero-context additions are not supported yet",
                idx + 1
            ));
        }

        let start_hint = if hunk.old_start == 0 {
            Some(1)
        } else {
            Some(hunk.old_start)
        };
        let span = hunk.old_len.max(1);
        let end_hint = start_hint.map(|start| start + span - 1);

        let snippet_req = ApplySnippetEditRequest {
            path: req.path.clone(),
            old_snippet,
            new_snippet,
            match_hint: Some(MatchHint {
                start_line: start_hint,
                end_line: end_hint,
            }),
            file_hash: expected_hash.clone(),
            region_id: Some(format!("diff-hunk-{}", idx + 1)),
        };

        let result = apply_snippet_edit_impl(&snippet_req)?;
        match result.status {
            SnippetStatusKind::Ok => {
                if initial_hash.is_none() {
                    initial_hash = result.file_hash_before.clone();
                }
                expected_hash = result.file_hash_after.clone();
                final_hash = result.file_hash_after.clone();
                applied.push(json!({
                    "hunk_index": idx + 1,
                    "result": result.payload
                }));
            }
            SnippetStatusKind::NoMatch => {
                return Ok(json!({
                    "action": "apply_unified_diff",
                    "status": "no_match",
                    "failed_hunk": idx + 1,
                    "details": result.payload
                }));
            }
            SnippetStatusKind::StaleFile => {
                return Ok(json!({
                    "action": "apply_unified_diff",
                    "status": "stale_file",
                    "failed_hunk": idx + 1,
                    "details": result.payload
                }));
            }
        }
    }

    Ok(json!({
        "action": "apply_unified_diff",
        "status": "ok",
        "hunks_applied": applied.len(),
        "file_hash_before": initial_hash,
        "file_hash_after": final_hash,
        "results": applied
    }))
}

fn apply_snippet_edit_impl(req: &ApplySnippetEditRequest) -> Result<SnippetResult> {
    if req.old_snippet.is_empty() {
        return Err(anyhow!("old_snippet must not be empty"));
    }

    let path = PathBuf::from(&req.path);
    let model = FileModel::from_path(&path)?;

    if let Some(expected_hash) = req
        .file_hash
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        if expected_hash != model.hash {
            return Ok(SnippetResult {
                status: SnippetStatusKind::StaleFile,
                payload: json!({
                    "action": "apply_snippet_edit",
                    "status": "stale_file",
                    "message": "file hash mismatch, refresh region before applying changes",
                    "expected_file_hash": expected_hash,
                    "current_file_hash": model.hash,
                }),
                file_hash_before: Some(model.hash),
                file_hash_after: None,
            });
        }
    }

    let maybe_offset = compute_match_range(
        &model.canonical,
        req.match_hint.as_ref(),
        req.old_snippet.as_str(),
    )?;

    let canonical_start = match maybe_offset {
        Some(offset) => offset,
        None => {
            return Ok(SnippetResult {
                status: SnippetStatusKind::NoMatch,
                payload: no_match_payload(
                    &model,
                    req.old_snippet.as_str(),
                    req.match_hint.as_ref(),
                ),
                file_hash_before: Some(model.hash),
                file_hash_after: None,
            });
        }
    };

    let canonical_end = canonical_start + req.old_snippet.len();
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
    if old_slice != req.old_snippet {
        return Err(anyhow!(
            "internal invariant violated: canonical slice mismatch"
        ));
    }

    let default_newline = model.newline_stats.default_kind();
    let replacement = build_replacement_bytes(&req.new_snippet, default_newline);

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
        .map(|idx| idx + 1)
        .unwrap_or(1);
    let end_line = model
        .canonical
        .line_index_for_offset(canonical_end.saturating_sub(1))
        .map(|idx| idx + 1)
        .unwrap_or(start_line);

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
        file_hash_before: Some(model.hash),
        file_hash_after: Some(new_hash),
    })
}

fn ok_json(id: Option<Value>, payload: Value) -> RpcResponse<'static> {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(json!({
            "content": [{
                "type": "json",
                "json": payload
            }],
            "isError": false
        })),
        error: None,
    }
}

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
        if let Some(rel) = haystack[start..end].find(needle) {
            return Ok(Some(start + rel));
        }
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

fn logical_snippet_lines<'a>(snippet: &'a str) -> Vec<&'a str> {
    let mut parts: Vec<&str> = snippet.split('\n').collect();
    if snippet.ends_with('\n') && !parts.is_empty() {
        parts.pop();
    }
    if parts.is_empty() {
        parts.push("");
    }
    parts
}

fn render_numbered_lines(views: &[LineView], start_line: usize, end_line: usize) -> String {
    let mut buf = String::new();
    for line_number in start_line..=end_line {
        if let Some(view) = views.get(line_number - 1) {
            buf.push_str(&format!("{line_number}: {}\n", view.text));
        }
    }
    buf
}

fn canonical_range_for_lines(
    views: &[LineView],
    start_line: usize,
    end_line: usize,
) -> Result<std::ops::Range<usize>> {
    if start_line == 0 || end_line < start_line {
        return Err(anyhow!("invalid line range {}-{}", start_line, end_line));
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

struct FileModel {
    bytes: Vec<u8>,
    hash: String,
    canonical: CanonicalData,
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

struct CanonicalData {
    text: String,
    line_views: Vec<LineView>,
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

#[derive(Clone)]
struct LineSlice {
    content_start: usize,
    content_end: usize,
    newline_end: usize,
    newline_kind: NewlineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewlineKind {
    Lf,
    CrLf,
    Cr,
    None,
}

impl NewlineKind {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            NewlineKind::Lf => b"\n",
            NewlineKind::CrLf => b"\r\n",
            NewlineKind::Cr => b"\r",
            NewlineKind::None => b"\n",
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

#[derive(Clone, Copy, Default)]
struct NewlineStats {
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

    fn default_kind(&self) -> NewlineKind {
        match self.dominant() {
            NewlineKind::None => NewlineKind::Lf,
            other => other,
        }
    }

    fn describe(&self) -> Value {
        json!({
            "dominant": self.dominant().label(),
            "stats": {
                "LF": self.lf,
                "CRLF": self.crlf,
                "CR": self.cr
            }
        })
    }
}

#[derive(Clone)]
struct LineView {
    canonical_start: usize,
    canonical_end: usize,
    canonical_full_end: usize,
    text: String,
    has_trailing_newline: bool,
}

#[derive(Clone)]
struct Boundary {
    canonical_offset: usize,
    file_offset: usize,
}

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

fn decode_line(bytes: &[u8]) -> (String, Vec<usize>) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut boundaries = Vec::with_capacity(text.chars().count() + 1);
        for (idx, _) in text.char_indices() {
            boundaries.push(idx);
        }
        boundaries.push(bytes.len());
        return (text.to_string(), boundaries);
    }

    const REPLACEMENT: char = '\u{FFFD}';
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

fn compute_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HunkLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Clone, Debug)]
struct HunkLine {
    kind: HunkLineKind,
    text: String,
    has_newline: bool,
}

#[derive(Clone, Debug)]
struct UnifiedHunk {
    old_start: usize,
    old_len: usize,
    #[allow(dead_code)] // Parsed from diff header but not currently used
    new_start: usize,
    #[allow(dead_code)] // Parsed from diff header but not currently used
    new_len: usize,
    lines: Vec<HunkLine>,
}

impl UnifiedHunk {
    fn old_snippet(&self) -> String {
        build_snippet(&self.lines, |kind| kind != HunkLineKind::Add)
    }

    fn new_snippet(&self) -> String {
        build_snippet(&self.lines, |kind| kind != HunkLineKind::Remove)
    }
}

fn build_snippet<F>(lines: &[HunkLine], predicate: F) -> String
where
    F: Fn(HunkLineKind) -> bool,
{
    let mut out = String::new();
    for line in lines.iter().filter(|line| predicate(line.kind)) {
        out.push_str(&line.text);
        if line.has_newline {
            out.push('\n');
        }
    }
    out
}

fn parse_unified_diff(diff: &str) -> Result<Vec<UnifiedHunk>> {
    use std::iter::Peekable;

    let mut hunks = Vec::new();
    let mut iter: Peekable<std::str::Lines<'_>> = diff.lines().peekable();

    while let Some(line) = iter.next() {
        if !line.starts_with("@@") {
            continue;
        }

        let (old_start, old_len, new_start, _) = parse_hunk_header(line)?;
        let mut hunk_lines: Vec<HunkLine> = Vec::new();

        while let Some(peek) = iter.peek() {
            let next = *peek;
            if next.starts_with("@@") || next.starts_with("--- ") || next.starts_with("diff ") {
                break;
            }

            let raw = iter.next().unwrap();
            if raw == "\\ No newline at end of file" {
                if let Some(last) = hunk_lines.last_mut() {
                    last.has_newline = false;
                }
                continue;
            }

            let mut chars = raw.chars();
            let prefix = chars
                .next()
                .ok_or_else(|| anyhow!("malformed diff line: {raw:?}"))?;
            let text: String = chars.collect();
            let kind = match prefix {
                ' ' => HunkLineKind::Context,
                '+' => HunkLineKind::Add,
                '-' => HunkLineKind::Remove,
                _ => return Err(anyhow!("unexpected diff marker '{}'", prefix)),
            };
            hunk_lines.push(HunkLine {
                kind,
                text,
                has_newline: true,
            });
        }

        if hunk_lines.is_empty() {
            return Err(anyhow!(
                "diff hunk at -{},+{} is empty",
                old_start,
                new_start
            ));
        }

        hunks.push(UnifiedHunk {
            old_start,
            old_len,
            lines: hunk_lines,
        });
    }

    if hunks.is_empty() {
        Err(anyhow!(
            "diff did not contain any @@ hunk headers; ensure unified diff format"
        ))
    } else {
        Ok(hunks)
    }
}

fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize, usize)> {
    let trimmed = line.trim();
    if !trimmed.starts_with("@@") || !trimmed.ends_with("@@") {
        return Err(anyhow!("invalid hunk header: {}", line));
    }
    let inner = trimmed
        .trim_start_matches("@@")
        .trim_end_matches("@@")
        .trim();
    let mut parts = inner.split_whitespace();
    let old_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing old range in hunk header: {}", line))?;
    let new_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing new range in hunk header: {}", line))?;

    let (old_start, old_len) = parse_range_component(old_part, '-')?;
    let (new_start, new_len) = parse_range_component(new_part, '+')?;
    Ok((old_start, old_len, new_start, new_len))
}

fn parse_range_component(token: &str, prefix: char) -> Result<(usize, usize)> {
    if !token.starts_with(prefix) {
        return Err(anyhow!(
            "range component {} must start with '{}'",
            token,
            prefix
        ));
    }
    let mut parts = token[1..].split(',');
    let start = parts
        .next()
        .ok_or_else(|| anyhow!("missing start in {}", token))?
        .parse::<usize>()
        .map_err(|e| anyhow!("invalid start in {}: {}", token, e))?;
    let len = parts
        .next()
        .map(|v| {
            v.parse::<usize>()
                .map_err(|e| anyhow!("invalid length in {}: {}", token, e))
        })
        .transpose()?
        .unwrap_or(1);
    Ok((start, len))
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
    fn test_parse_unified_diff_basic() {
        let diff = "\
@@ -1,2 +1,3 @@
 line1
-line2
+line2
+line3
";
        let hunks = parse_unified_diff(diff).expect("parse diff");
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.old_len, 2);
        assert_eq!(hunk.old_snippet(), "line1\nline2\n");
        assert_eq!(hunk.new_snippet(), "line1\nline2\nline3\n");
    }

    #[test]
    fn test_apply_unified_diff_simple_flow() {
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut file, b"alpha\nbeta\n").expect("write");
        let path = file.path().to_path_buf();

        let diff = "\
@@ -1,2 +1,3 @@
 alpha
 beta
+gamma
";
        let req = ApplyUnifiedDiffRequest {
            path: path.to_string_lossy().to_string(),
            diff: diff.to_string(),
            file_hash: None,
        };

        let response = handle_apply_unified_diff(&req).expect("diff apply");
        assert_eq!(response.get("status").and_then(|v| v.as_str()), Some("ok"));
        let contents = std::fs::read_to_string(&path).expect("read file");
        assert_eq!(contents, "alpha\nbeta\ngamma\n");
    }
}
