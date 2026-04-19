//! Snippet replacement: locate `old_snippet` in the canonical view and write back the
//! corresponding bytes with the file's dominant line ending applied to the replacement.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

use super::matching::{MatchHint, compute_match_range, no_match_payload};
use super::model::{FileModel, NewlineKind, compute_hash};

/// Request parameters for the `apply_snippet_edit` action.
///
/// Performs a surgical replacement of an exact substring within a file. The snippet must
/// match exactly in the canonical (LF-normalized) view. If `match_hint` is provided, it
/// constrains the search and is **strict** — no fallback search runs outside the hint.
/// If `file_hash` is provided and differs from the file's current hash, the edit is
/// rejected with `stale_file`.
#[derive(Deserialize)]
pub(super) struct ApplySnippetEditRequest {
    pub(super) path: String,
    pub(super) old_snippet: String,
    pub(super) new_snippet: String,
    #[serde(default)]
    pub(super) match_hint: Option<MatchHint>,
    #[serde(default)]
    pub(super) file_hash: Option<String>,
    #[serde(default)]
    pub(super) region_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SnippetStatusKind {
    Ok,
    NoMatch,
    StaleFile,
}

pub(super) struct SnippetResult {
    pub(super) status: SnippetStatusKind,
    pub(super) payload: Value,
}

#[cfg(test)]
fn handle_apply_snippet_edit(req: &ApplySnippetEditRequest) -> Result<Value> {
    let result = apply_snippet_edit_impl(req)?;
    Ok(result.payload)
}

pub(super) fn apply_snippet_edit_impl(req: &ApplySnippetEditRequest) -> Result<SnippetResult> {
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

/// Converts a canonical LF-based snippet to bytes with the target newline style.
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

        let original_hash = FileModel::from_path(&path).expect("model").hash.clone();

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

        // Strictness: a hint range with no match must NOT fall back to other matches.
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
