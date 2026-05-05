//! Smart, newline-aware file editing for MCP.
//!
//! Replaces an exact substring (`old_snippet`) with new content (`new_snippet`) while
//! preserving the file's original line endings. Callers always work with LF-only text;
//! the module normalizes both the file and the snippet to a canonical LF view for
//! matching, then writes back replacement bytes using the file's dominant newline style
//! (CRLF / LF / CR).
//!
//! ### Layout
//!
//! - [`model`]: file-on-disk representation, canonical LF view, newline statistics.
//! - [`matching`]: locate the snippet in the canonical view; suggest near-misses on miss.
//! - [`edit`]: apply the replacement and write the file back.
//!
//! The public entry point is [`handle_edit`].

mod edit;
mod matching;
mod model;

use serde::Deserialize;
use serde_json::Value;
use tools_mcp_core::ToolCallOutcome;

use edit::{ApplySnippetEditRequest, SnippetStatusKind, apply_snippet_edit_impl};
use matching::MatchHint;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimpleEditRequest {
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

/// Replace `old_snippet` with `new_snippet` in a file. Returns a structured outcome with
/// `status: "ok" | "no_match" | "stale_file"`.
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
        file_hash: req.file_hash,
        region_id: req.region_id,
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

#[cfg(test)]
mod tests {
    use super::handle_edit;
    use serde_json::json;

    fn edit_payload(outcome: tools_mcp_core::ToolCallOutcome) -> serde_json::Value {
        let text = outcome.0["content"][0]["text"]
            .as_str()
            .expect("json content text");
        serde_json::from_str(text).expect("edit payload json")
    }

    #[tokio::test]
    async fn public_edit_rejects_stale_file_hash_without_modifying_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stale-public.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");

        let outcome = handle_edit(
            None,
            json!({
                "path": path,
                "old_snippet": "beta",
                "new_snippet": "BETA",
                "file_hash": "not-the-current-hash"
            }),
        )
        .await;
        let payload = edit_payload(outcome);

        assert_eq!(payload["status"], "stale_file");
        assert_eq!(payload["expected_file_hash"], "not-the-current-hash");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha\nbeta\n"
        );
    }

    #[tokio::test]
    async fn public_edit_forwards_region_id_to_success_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("region-public.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");

        let outcome = handle_edit(
            None,
            json!({
                "path": path,
                "old_snippet": "beta",
                "new_snippet": "BETA",
                "region_id": "region-123"
            }),
        )
        .await;
        let payload = edit_payload(outcome);

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["region_id"], "region-123");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha\nBETA\n"
        );
    }
}
