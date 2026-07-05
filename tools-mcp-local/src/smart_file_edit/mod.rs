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

use crate::path_policy;
use serde::Deserialize;
use serde_json::Value;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::validation;

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
    region_id: Option<String>,
}

/// Replace `old_snippet` with `new_snippet` in a file. Returns a structured outcome with
/// `status: "ok" | "no_match" | "stale_file"`.
pub async fn handle_edit(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<SimpleEditRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.path, "path", None) {
        return o;
    }

    if req.old_snippet.is_empty() {
        return ToolCallOutcome::err(
            "old_snippet cannot be empty. Remediation: use Read to copy the exact snippet from the file (use LF newlines), then retry Edit.",
        );
    }

    let path = match path_policy::resolve_existing_file(&req.path, "path") {
        Ok(path) => path,
        Err(err) => return ToolCallOutcome::err(err.to_string()),
    };

    // Mandatory read-before-edit: the file must have been observed in this server session
    // (via Read, or a prior successful Edit) so the server can verify it has not changed
    // out from under the agent. The expected hash comes from the server's own snapshot,
    // never from a value copied by the caller.
    let Some(expected_hash) = crate::edit_snapshot::get(&path) else {
        return ToolCallOutcome::ok_json_content(
            &serde_json::json!({
                "action": "apply_snippet_edit",
                "status": "no_snapshot",
                "message": "no read snapshot for this file. Remediation: Read the file before editing so the server can verify it has not changed since you last saw it.",
            }),
            true,
        );
    };

    let internal_req = ApplySnippetEditRequest {
        path: path.display().to_string(),
        old_snippet: req.old_snippet,
        new_snippet: req.new_snippet,
        match_hint: req.match_hint,
        file_hash: Some(expected_hash),
        region_id: req.region_id,
    };

    match apply_snippet_edit_impl(&internal_req) {
        Ok(result) => {
            let is_error = !matches!(result.status, SnippetStatusKind::Ok);
            // A successful Edit knows exactly what it wrote, so refresh the snapshot to the
            // new content. This lets a chain of edits proceed without re-reading.
            if !is_error
                && let Some(after) = result
                    .payload
                    .get("file_hash_after")
                    .and_then(Value::as_str)
            {
                crate::edit_snapshot::record(&path, after.to_string());
            }
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
    use crate::path_policy::tempdir_in_workspace;
    use serde_json::json;

    fn edit_payload(outcome: tools_mcp_core::ToolCallOutcome) -> serde_json::Value {
        let text = outcome.0["content"][0]["text"]
            .as_str()
            .expect("json content text");
        serde_json::from_str(text).expect("edit payload json")
    }

    /// Stand in for a prior `Read`: record the current file content as the server's
    /// snapshot so an Edit is allowed to proceed.
    fn seed_snapshot(path: &std::path::Path) {
        let bytes = std::fs::read(path).expect("read for snapshot");
        crate::edit_snapshot::record_bytes(path, &bytes);
    }

    #[tokio::test]
    async fn public_edit_without_snapshot_is_rejected() {
        let dir = tempdir_in_workspace("edit-no-snapshot-");
        let path = dir.path().join("no-snapshot.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");

        let outcome = handle_edit(
            None,
            json!({
                "path": path,
                "old_snippet": "beta",
                "new_snippet": "BETA",
            }),
        )
        .await;
        let payload = edit_payload(outcome);

        assert_eq!(payload["status"], "no_snapshot");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha\nbeta\n"
        );
    }

    #[tokio::test]
    async fn public_edit_rejects_stale_file_when_changed_after_read() {
        let dir = tempdir_in_workspace("edit-stale-");
        let path = dir.path().join("stale-public.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");
        seed_snapshot(&path);

        // The file changes on disk after the snapshot was taken (e.g. another process).
        std::fs::write(&path, "alpha\nbeta\nCHANGED\n").expect("rewrite");

        let outcome = handle_edit(
            None,
            json!({
                "path": path,
                "old_snippet": "beta",
                "new_snippet": "BETA",
            }),
        )
        .await;
        let payload = edit_payload(outcome);

        assert_eq!(payload["status"], "stale_file");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha\nbeta\nCHANGED\n"
        );
    }

    #[tokio::test]
    async fn public_edit_forwards_region_id_to_success_payload() {
        let dir = tempdir_in_workspace("edit-region-");
        let path = dir.path().join("region-public.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");
        seed_snapshot(&path);

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

    #[tokio::test]
    async fn public_edit_refreshes_snapshot_for_chained_edits() {
        let dir = tempdir_in_workspace("edit-chain-");
        let path = dir.path().join("chain-public.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").expect("write");
        seed_snapshot(&path);

        let first = edit_payload(
            handle_edit(
                None,
                json!({
                    "path": path,
                    "old_snippet": "beta",
                    "new_snippet": "BETA",
                }),
            )
            .await,
        );
        assert_eq!(first["status"], "ok");

        // No re-Read: the first edit refreshed the snapshot, so the second edit is allowed.
        let second = edit_payload(
            handle_edit(
                None,
                json!({
                    "path": path,
                    "old_snippet": "gamma",
                    "new_snippet": "GAMMA",
                }),
            )
            .await,
        );
        assert_eq!(second["status"], "ok");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "alpha\nBETA\nGAMMA\n"
        );
    }

    #[tokio::test]
    async fn public_edit_rejects_parent_traversal_outside_workspace() {
        let outcome = handle_edit(
            None,
            json!({
                "path": "../outside-edit-policy.txt",
                "old_snippet": "alpha",
                "new_snippet": "beta",
            }),
        )
        .await;

        assert_eq!(outcome.0["isError"], true);
        let msg = outcome.0["content"][0]["text"].as_str().unwrap_or_default();
        assert!(msg.contains("server working directory"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn public_edit_canonicalizes_symlinked_file_before_writing() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir_in_workspace("edit-symlink-file-");
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, "alpha\nbeta\n").expect("write target");
        unix_fs::symlink(&target, &link).expect("symlink target");
        seed_snapshot(&link);

        let outcome = handle_edit(
            None,
            json!({
                "path": link,
                "old_snippet": "beta",
                "new_snippet": "BETA",
            }),
        )
        .await;
        let payload = edit_payload(outcome);

        assert_eq!(payload["status"], "ok");
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            "alpha\nBETA\n"
        );
    }
}
