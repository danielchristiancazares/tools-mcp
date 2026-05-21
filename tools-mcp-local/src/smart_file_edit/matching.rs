//! Snippet matching and candidate suggestion in the canonical view.
//!
//! [`compute_match_range`] locates an exact LF-normalized needle, optionally constrained
//! to a [`MatchHint`] line range. [`suggest_candidates`] returns a few near-miss locations
//! when no exact match is found, to guide the caller.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::model::{CanonicalData, FileModel, LineView};

/// Line range hint for disambiguating snippet matches.
///
/// When a file contains multiple occurrences of the snippet, the hint narrows the
/// search to a 1-indexed inclusive line range. Search is strict: if the hint is
/// provided but the snippet does not appear inside it, no fallback search is done.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MatchHint {
    #[serde(default)]
    pub(super) start_line: Option<usize>,
    #[serde(default)]
    pub(super) end_line: Option<usize>,
}

pub(super) fn compute_match_range(
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

pub(super) fn no_match_payload(model: &FileModel, needle: &str, hint: Option<&MatchHint>) -> Value {
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
