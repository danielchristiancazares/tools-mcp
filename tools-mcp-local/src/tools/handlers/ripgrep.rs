//! ugrep search handler implementation.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::process::Stdio;
use std::time::{Duration, Instant as StdInstant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::cancellation::current_cancellation_token;

use super::search_contract::{
    NormalizedSearchRequest, SearchCaseMode, SearchEvent, SearchPayloadMeta, build_search_payload,
    parse_search_request, render_search_text,
};
use super::search_file_selection::resolve_ugrep_path_list;
use super::search_memory::{MemoryError, handle_memory_search};

const MAX_UGREP_STDERR_BYTES: usize = 16 * 1024;
const STDERR_READ_CHUNK_BYTES: usize = 4 * 1024;

fn add_fallback_metadata(mut outcome: ToolCallOutcome, err: &MemoryError) -> ToolCallOutcome {
    if let Some(obj) = outcome.0.as_object_mut() {
        obj.insert("backend".to_string(), Value::String("ugrep".to_string()));
        obj.insert(
            "fallback_reason".to_string(),
            Value::String(err.fallback_reason.to_string()),
        );
        obj.insert("fallback_source".to_string(), json!("memory"));
        obj.insert("fallback_error_type".to_string(), json!(err.error_type));
        obj.insert(
            "fallback_available".to_string(),
            json!(err.fallback_allowed),
        );
        obj.insert("memory_eligibility".to_string(), json!("fallback"));
        obj.insert("plan_kind".to_string(), json!("ugrep"));
    }
    outcome
}

pub(super) fn ugrep_binary_name() -> &'static str {
    if cfg!(windows) { "ugrep.exe" } else { "ugrep" }
}

fn resolve_globbed_files_for_ugrep(
    req: &NormalizedSearchRequest,
    deadline: Option<StdInstant>,
) -> anyhow::Result<Option<Vec<std::path::PathBuf>>> {
    resolve_ugrep_path_list(req, deadline)
}

/// Defense-in-depth filter: returns true if `parsed_path` was one of the
/// pre-resolved paths we authorized for ugrep. When `allowed` is `None`
/// (no glob filter / non-`--from=-` path), every result is permitted.
fn is_path_authorized(parsed_path: &str, allowed: Option<&HashSet<String>>) -> bool {
    match allowed {
        Some(set) => set.contains(&path_authorization_key(parsed_path)),
        None => true,
    }
}

#[cfg(windows)]
fn path_authorization_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

#[cfg(not(windows))]
fn path_authorization_key(path: &str) -> String {
    path.to_string()
}

fn append_bounded_output(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    let remaining = limit.saturating_sub(buf.len());
    let to_keep = chunk.len().min(remaining);
    if to_keep > 0 {
        buf.extend_from_slice(&chunk[..to_keep]);
    }
    chunk.len() > remaining
}

fn render_bounded_stderr(buf: Vec<u8>, truncated: bool) -> String {
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "[stderr truncated after {MAX_UGREP_STDERR_BYTES} bytes]"
        ));
    }
    text.trim().to_string()
}

async fn read_stderr_bounded<R>(stderr: R) -> String
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0_u8; STDERR_READ_CHUNK_BYTES];
    let mut reader = BufReader::new(stderr);
    let mut truncated = false;

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                truncated |= append_bounded_output(&mut buf, &chunk[..n], MAX_UGREP_STDERR_BYTES);
            }
            Err(_) => break,
        }
    }

    render_bounded_stderr(buf, truncated)
}

/// Parse a grep-style output line: "path:line:text" (match) or "path-line-text" (context)
/// Returns (path, `line_number`, text, `is_match`)
fn parse_grep_line(line: &str) -> (String, u64, String, bool) {
    fn separator_candidates(line: &str, sep: u8) -> impl Iterator<Item = (usize, u64, usize)> + '_ {
        let bytes = line.as_bytes();
        (0..bytes.len()).filter_map(move |i| {
            if bytes[i] != sep {
                return None;
            }

            // Check if followed by digits then another matching separator.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }

            if j > i + 1
                && j < bytes.len()
                && bytes[j] == sep
                && let Ok(line_no) = line[i + 1..j].parse::<u64>()
            {
                return Some((i, line_no, j + 1));
            }

            None
        })
    }

    fn parse_with_sep_from_end(line: &str, sep: u8) -> Option<(String, u64, String, bool)> {
        separator_candidates(line, sep)
            .last()
            .map(|(path_end, line_no, text_start)| {
                (
                    line[..path_end].to_string(),
                    line_no,
                    line[text_start..].to_string(),
                    sep == b':',
                )
            })
    }

    fn parse_match_line(line: &str) -> Option<(String, u64, String, bool)> {
        fn path_continuation_fragment(fragment: &str) -> bool {
            !fragment.is_empty()
                && !fragment.chars().any(char::is_whitespace)
                && (fragment.contains('/')
                    || fragment.contains('\\')
                    || fragment.rsplit_once('.').is_some_and(|(_, ext)| {
                        !ext.is_empty()
                            && ext
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    }))
        }

        let mut candidates = separator_candidates(line, b':');
        let mut selected = candidates.next()?;
        let mut previous = selected;
        for candidate in candidates {
            let current_text_start = previous.2;
            let next_path_end = candidate.0;
            if current_text_start <= next_path_end
                && path_continuation_fragment(&line[current_text_start..next_path_end])
            {
                selected = candidate;
                previous = candidate;
            } else {
                break;
            }
        }

        let (path_end, line_no, text_start) = selected;
        Some((
            line[..path_end].to_string(),
            line_no,
            line[text_start..].to_string(),
            true,
        ))
    }

    // Parse match lines first ("path:line:text") by scanning from the start.
    // This avoids selecting a later `:<digits>:` sequence in the matched text.
    if let Some(parsed) = parse_match_line(line) {
        return parsed;
    }
    // Parse context lines ("path-line-text") by scanning from the end.
    // This avoids misparsing filenames containing "-<digits>-", e.g. "foo-1-bar.txt-10-text".
    if let Some(parsed) = parse_with_sep_from_end(line, b'-') {
        return parsed;
    }

    // Couldn't parse, return empty
    (String::new(), 0, String::new(), true)
}

fn classify_success(
    status: Option<std::process::ExitStatus>,
    exit_code: Option<i32>,
    truncated: bool,
    timed_out: bool,
) -> bool {
    (truncated
        || status
            .as_ref()
            .is_some_and(|s| s.success() || exit_code == Some(1)))
        && !timed_out
}

/// Run ugrep and return both a readable summary and structured matches.
///
/// Notes:
/// - This tool executes `ugrep` directly (no shell).
/// - Uses text output with -n -H for simpler parsing.
/// - Exit code semantics: 0 = matches found, 1 = no matches, 2 = error.
pub async fn handle_search(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match parse_search_request(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    match handle_memory_search(&req).await {
        Ok(outcome) => outcome,
        Err(err) if err.fallback_allowed => {
            add_fallback_metadata(handle_search_ugrep(req).await, &err)
        }
        Err(err) => err.into_tool_outcome(&req),
    }
}

#[cfg(test)]
pub(super) async fn handle_search_ugrep_for_test(args: Value) -> ToolCallOutcome {
    let req = match parse_search_request(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let mut outcome = handle_search_ugrep(req).await;
    if let Some(obj) = outcome.0.as_object_mut() {
        obj.insert("backend".to_string(), Value::String("ugrep".to_string()));
    }
    outcome
}

async fn handle_search_ugrep(req: NormalizedSearchRequest) -> ToolCallOutcome {
    let root = req.root().to_string();
    let max_results = req.max_results();
    let timeout_ms = req.timeout_ms();
    let fuzzy_distance = req.fuzzy_distance();
    let bin = ugrep_binary_name();

    let run = async {
        let deadline = StdInstant::now() + Duration::from_millis(timeout_ms);
        let tokio_deadline = time::Instant::from_std(deadline);
        let globbed_files = resolve_globbed_files_for_ugrep(&req, Some(deadline))?;
        if matches!(globbed_files, Some(ref files) if files.is_empty()) {
            return Ok::<_, anyhow::Error>((
                Vec::new(),
                false,
                Some(1),
                String::new(),
                true,
                false,
            ));
        }

        // Defense in depth: when we hand ugrep a pre-resolved file list via
        // `--from=-`, the only paths it should report on are paths from that
        // list. Build the authorized set once (using the same lossy form we
        // serialize to ugrep stdin) so the result-parsing loop can drop any
        // path we did not explicitly authorize. This is belt-and-suspenders
        // against future selection-edge-case regressions in ugrep or in the
        // resolver; the LF/CR rejection in `resolve_globbed_files_for_ugrep`
        // is the primary control.
        let allowed_paths: Option<HashSet<String>> = globbed_files.as_ref().map(|files| {
            files
                .iter()
                .map(|p| path_authorization_key(p.to_string_lossy().as_ref()))
                .collect()
        });

        let mut cmd = Command::new(bin);

        // ugrep: use text output with -n -H for simpler parsing
        cmd.arg("-r").arg("-n").arg("-H");

        // Fuzzy flag
        if let Some(dist) = fuzzy_distance {
            cmd.arg(format!("-Z{dist}"));
        }

        if req.fixed_strings() {
            cmd.arg("-F");
        }
        if req.word_regexp() {
            cmd.arg("-w");
        }

        // Case mode: -j for smart-case, -i for insensitive
        match req.case_mode() {
            SearchCaseMode::Sensitive => {
                // default behavior (no flags)
            }
            SearchCaseMode::Insensitive => {
                cmd.arg("-i");
            }
            SearchCaseMode::Smart => {
                cmd.arg("-j"); // ugrep smart-case
            }
        }

        if req.hidden() {
            cmd.arg("--hidden");
        }
        if req.follow() {
            cmd.arg("--dereference");
        }
        if req.no_ignore() {
            cmd.arg("--no-ignore-files");
        }
        if req.context() > 0 {
            let c = req.context();
            cmd.arg("-C").arg(c.to_string());
        }
        if globbed_files.is_some() {
            cmd.arg("--from=-");
        } else {
            for g in req.raw_globs() {
                if !g.trim().is_empty() {
                    cmd.arg("-g").arg(g);
                }
            }
        }

        // End of options marker prevents patterns like "//" or "-foo" from being
        // interpreted as flags
        cmd.arg("--").arg(req.pattern());
        if globbed_files.is_none() {
            cmd.arg(&root);
        }

        if globbed_files.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn ugrep. Install: winget install Genivia.ugrep / brew install ugrep / apt install ugrep. Error: {e}")
        })?;

        let stdin_task = if let Some(files) = globbed_files {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("failed to capture stdin"))?;
            Some(tokio::spawn(async move {
                for path in files {
                    if stdin
                        .write_all(path.to_string_lossy().as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if stdin.write_all(b"\n").await.is_err() {
                        return;
                    }
                }
                let _ = stdin.shutdown().await;
            }))
        } else {
            None
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stderr"))?;

        // Read stderr concurrently to avoid deadlocks.
        let stderr_task = tokio::spawn(read_stderr_bounded(stderr));

        let mut events: Vec<SearchEvent> = Vec::new();
        let mut truncated = false;
        let mut timed_out = false;
        let mut terminated_for_limit = false;
        let mut cancelled = false;
        let cancel_token = current_cancellation_token();

        let mut reader = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
                maybe_line = reader.next_line() => {
                    let Some(line) = maybe_line? else { break; };

                    // ugrep text output: "path:line:text" or "path-line-text" for context
                    let (path, line_no, text, is_match) = parse_grep_line(&line);
                    if path.is_empty() {
                        continue;
                    }

                    // Defense in depth: when ugrep is invoked with `--from=-`,
                    // discard any reported path that is not in the authorized
                    // set we wrote to stdin. A normal run cannot reach this
                    // branch; it only fires if a future regression or ugrep
                    // edge case re-introduces a path-list injection primitive.
                    if !is_path_authorized(&path, allowed_paths.as_ref()) {
                        continue;
                    }

                    events.push(SearchEvent::new(is_match, path, line_no, text));

                    if events.len() >= max_results {
                        truncated = true;
                        terminated_for_limit = true;
                        let _ = child.kill().await;
                        break;
                    }
                }
                () = time::sleep_until(tokio_deadline) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    break;
                }
                _ = async {
                    if let Some(token) = cancel_token.as_ref() {
                        token.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    cancelled = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }

        if cancelled {
            // Best-effort process drain so we do not leak the child if cancellation fires.
            let _ = time::timeout(Duration::from_millis(500), child.wait()).await;
            anyhow::bail!("ugrep search cancelled");
        }

        // Wait for process to exit (even if killed).
        let status =
            if let Ok(res) = time::timeout(Duration::from_millis(2_000), child.wait()).await {
                Some(res?)
            } else {
                // If we intentionally terminated after collecting enough results,
                // a slow process shutdown should not be reported as a user-visible timeout.
                if !terminated_for_limit {
                    timed_out = true;
                }
                let _ = child.kill().await;
                None
            };
        let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);

        let mut stderr_task = stderr_task;
        let stderr_text = if let Ok(joined) =
            time::timeout(Duration::from_millis(2_000), &mut stderr_task).await
        {
            joined.unwrap_or_else(|_| String::new())
        } else {
            stderr_task.abort();
            String::new()
        }
        .trim()
        .to_string();

        if let Some(mut stdin_task) = stdin_task
            && time::timeout(Duration::from_millis(2_000), &mut stdin_task)
                .await
                .is_err()
        {
            stdin_task.abort();
        }

        // ugrep: 0 = matches, 1 = no matches, 2 = error
        let success = classify_success(status, exit_code, truncated, timed_out);

        Ok::<_, anyhow::Error>((
            events,
            truncated,
            exit_code,
            stderr_text,
            success,
            timed_out,
        ))
    };

    match run.await {
        Ok((events, truncated, exit_code, stderr_text, success, timed_out)) => {
            let text_view = if !success && !stderr_text.is_empty() {
                // Show error message when search failed
                format!("Search error: {stderr_text}")
            } else {
                render_search_text(&events)
            };

            let mut payload = build_search_payload(
                &req,
                SearchPayloadMeta::new(
                    root,
                    text_view,
                    !success,
                    json!(exit_code),
                    truncated,
                    timed_out,
                ),
                &events,
            );

            if !stderr_text.is_empty()
                && let Some(obj) = payload.as_object_mut()
            {
                obj.insert("stderr".to_string(), Value::String(stderr_text));
            }

            ToolCallOutcome::ok(payload)
        }
        Err(e) => ToolCallOutcome::err(format!("ugrep error: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::super::search_contract::SearchRequest;
    #[cfg(unix)]
    use super::super::search_file_selection::path_has_line_separator;
    use super::{
        MAX_UGREP_STDERR_BYTES, append_bounded_output, classify_success, is_path_authorized,
        parse_grep_line, render_bounded_stderr,
    };
    #[cfg(unix)]
    use super::{handle_search, resolve_globbed_files_for_ugrep};
    use super::{path_authorization_key, ugrep_binary_name};
    #[cfg(unix)]
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    #[cfg(unix)]
    fn truncated_without_timeout_is_success_even_after_forced_termination() {
        use std::os::unix::process::ExitStatusExt as _;
        let terminated = std::process::ExitStatus::from_raw(9);
        assert!(classify_success(Some(terminated), None, true, false));
    }

    #[test]
    fn truncated_without_status_is_success_when_not_timed_out() {
        assert!(classify_success(None, None, true, false));
    }

    #[test]
    #[cfg(unix)]
    fn truncated_with_timeout_is_error() {
        use std::os::unix::process::ExitStatusExt as _;
        let terminated = std::process::ExitStatus::from_raw(9);
        assert!(!classify_success(Some(terminated), None, true, true));
    }

    #[test]
    fn parse_grep_line_prefers_match_separator_when_filename_contains_hyphen_digits() {
        let line = "src/foo-1-bar.rs:42:let x = 1;";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/foo-1-bar.rs");
        assert_eq!(line_no, 42);
        assert_eq!(text, "let x = 1;");
        assert!(is_match);
    }

    #[test]
    fn parse_grep_line_parses_context_lines() {
        let line = "src/main.rs-7-use std::time::Duration;";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/main.rs");
        assert_eq!(line_no, 7);
        assert_eq!(text, "use std::time::Duration;");
        assert!(!is_match);
    }

    #[test]
    fn parse_grep_line_match_text_can_contain_colon_number_colon() {
        let line = "src/main.rs:12:timestamp 10:23:59";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "src/main.rs");
        assert_eq!(line_no, 12);
        assert_eq!(text, "timestamp 10:23:59");
        assert!(is_match);
    }

    #[test]
    fn bounded_stderr_caps_output_and_reports_truncation() {
        let mut buf = Vec::new();
        let oversized = vec![b'x'; MAX_UGREP_STDERR_BYTES + 64];

        let truncated = append_bounded_output(&mut buf, &oversized, MAX_UGREP_STDERR_BYTES);
        let rendered = render_bounded_stderr(buf, truncated);

        assert!(truncated);
        assert!(rendered.contains("[stderr truncated after "));
        assert!(rendered.len() < MAX_UGREP_STDERR_BYTES + 128);
    }

    #[test]
    fn parse_grep_line_parses_windows_drive_absolute_path() {
        let line = "C:\\repo\\src\\main.rs:42:needle";
        let (path, line_no, text, is_match) = parse_grep_line(line);
        assert_eq!(path, "C:\\repo\\src\\main.rs");
        assert_eq!(line_no, 42);
        assert_eq!(text, "needle");
        assert!(is_match);
    }

    #[test]
    fn parse_grep_line_parses_windows_path_with_colon_number_colon() {
        let line = "C:\\repo\\foo:1:bar.rs:42:needle";

        let (parsed_path, line_no, text, is_match) = parse_grep_line(line);

        assert_eq!(parsed_path, "C:\\repo\\foo:1:bar.rs");
        assert_eq!(line_no, 42);
        assert_eq!(text, "needle");
        assert!(is_match);
    }

    #[test]
    fn parse_grep_line_parses_unix_path_with_colon_number_colon() {
        let line = "src/foo:1:bar.rs:7:needle";

        let (parsed_path, line_no, text, is_match) = parse_grep_line(line);

        assert_eq!(parsed_path, "src/foo:1:bar.rs");
        assert_eq!(line_no, 7);
        assert_eq!(text, "needle");
        assert!(is_match);
    }

    #[test]
    #[cfg(unix)]
    fn path_has_line_separator_detects_lf_and_cr_via_raw_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        use std::path::PathBuf;

        let lf = PathBuf::from(OsString::from_vec(b"foo\nbar.txt".to_vec()));
        let cr = PathBuf::from(OsString::from_vec(b"foo\rbar.txt".to_vec()));
        let crlf = PathBuf::from(OsString::from_vec(b"foo\r\nbar.txt".to_vec()));
        let clean = PathBuf::from("foo/bar.txt");

        assert!(path_has_line_separator(&lf));
        assert!(path_has_line_separator(&cr));
        assert!(path_has_line_separator(&crlf));
        assert!(!path_has_line_separator(&clean));
    }

    #[test]
    #[cfg(unix)]
    fn resolve_globbed_files_rejects_lf_in_matched_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Filename contains a literal LF byte; this is the exact primitive
        // the path-list injection PoC relies on.
        let mut filename = OsString::from("unsafe");
        filename.push(OsString::from_vec(vec![b'\n']));
        filename.push("name.txt");
        std::fs::write(root.join(&filename), "needle\n").expect("write");

        let req = SearchRequest {
            path: Some(root.to_string_lossy().to_string()),
            pattern: "needle".to_string(),
            glob: Some(vec!["*".to_string()]),
            ..SearchRequest::default()
        }
        .normalize();

        let err = resolve_globbed_files_for_ugrep(&req, None)
            .expect_err("LF-bearing matched path must abort");
        assert!(
            err.to_string().contains("LF/CR"),
            "expected LF/CR rejection diagnostic, got: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolve_globbed_files_rejects_cr_in_matched_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let mut filename = OsString::from("unsafe");
        filename.push(OsString::from_vec(vec![b'\r']));
        filename.push("name.txt");
        std::fs::write(root.join(&filename), "needle\n").expect("write");

        let req = SearchRequest {
            path: Some(root.to_string_lossy().to_string()),
            pattern: "needle".to_string(),
            glob: Some(vec!["*".to_string()]),
            ..SearchRequest::default()
        }
        .normalize();

        let err = resolve_globbed_files_for_ugrep(&req, None)
            .expect_err("CR-bearing matched path must abort");
        assert!(
            err.to_string().contains("LF/CR"),
            "expected LF/CR rejection diagnostic, got: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolve_globbed_files_accepts_clean_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("inside.txt"), "needle\n").expect("write");

        let req = SearchRequest {
            path: Some(root.to_string_lossy().to_string()),
            pattern: "needle".to_string(),
            glob: Some(vec!["*.txt".to_string()]),
            no_ignore: Some(true),
            ..SearchRequest::default()
        }
        .normalize();

        let resolved = resolve_globbed_files_for_ugrep(&req, None)
            .expect("clean repo must resolve")
            .expect("glob filter present, expected Some(files)");
        assert_eq!(resolved.len(), 1, "exactly one matching file: {resolved:?}");
        assert_eq!(
            resolved[0].file_name().and_then(|n| n.to_str()),
            Some("inside.txt")
        );
    }

    #[test]
    fn is_path_authorized_passes_through_when_no_allowlist() {
        // When `allowed` is None (non-`--from=-` invocation) every path is
        // permitted; this preserves behavior on the unaffected code path.
        assert!(is_path_authorized("/anything", None));
        assert!(is_path_authorized("", None));
    }

    #[test]
    fn is_path_authorized_blocks_unauthorized_paths_when_allowlist_present() {
        let mut allowed: HashSet<String> = HashSet::new();
        allowed.insert(path_authorization_key("/repo/inside.txt"));

        assert!(is_path_authorized("/repo/inside.txt", Some(&allowed)));
        // A path that ugrep might invent or that an attacker tried to inject
        // (e.g. via a future regression) must be filtered out.
        assert!(!is_path_authorized("/etc/passwd", Some(&allowed)));
        assert!(!is_path_authorized("/repo/other.txt", Some(&allowed)));
    }

    #[test]
    #[cfg(windows)]
    fn is_path_authorized_accepts_windows_slash_and_case_variants() {
        let mut allowed: HashSet<String> = HashSet::new();
        allowed.insert(path_authorization_key("C:\\Repo\\Inside.txt"));

        assert!(is_path_authorized("c:/repo/inside.txt", Some(&allowed)));
        assert!(is_path_authorized("C:\\REPO\\INSIDE.TXT", Some(&allowed)));
        assert!(!is_path_authorized("C:/repo/outside.txt", Some(&allowed)));
    }

    #[test]
    fn ugrep_binary_name_matches_platform() {
        assert_eq!(
            ugrep_binary_name(),
            if cfg!(windows) { "ugrep.exe" } else { "ugrep" }
        );
    }

    /// End-to-end exploit-closure regression test. Builds the same primitive
    /// the original PoC used (an in-root path whose bytes contain `\n`
    /// followed by an absolute path to a sibling outside the search root) and
    /// invokes the real `handle_search` entry point with `glob: ["*"]`, which
    /// is forced through the ugrep fallback by `search_memory`'s
    /// regex-class-rejection rule (the `.` pattern can match LF). The
    /// LF/CR rejection in `resolve_globbed_files_for_ugrep` MUST short-circuit
    /// before ugrep is ever spawned, so we deliberately avoid faking ugrep on
    /// PATH (which would mutate process-global env state and race other
    /// tests). The asserts cover the three regression shapes that matter:
    /// (1) the search aborts (`isError == true`); (2) the abort comes from
    /// the fallback (`backend == "ugrep"`); (3) the diagnostic identifies the
    /// LF/CR cause; (4) the outside file content never appears in the
    /// payload, even if a future regression silently allows the path through.
    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn search_ugrep_glob_newline_path_injection_is_blocked() {
        use std::path::Path;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "tools_mcp_newline_fixed_{}_{}",
            std::process::id(),
            unique
        ));
        let root = base.join("repo");
        let outside = base.join("outside_secret.txt");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(&outside, "LEAKED_SECRET_MARKER\n").expect("write outside secret");

        // One in-root path whose display string contains a newline followed
        // by the absolute path of `outside_secret.txt`. With a vulnerable
        // implementation, line-oriented `--from=-` would split this into two
        // entries and leak the outside file. The fix must reject the matched
        // path before it ever reaches ugrep.
        let outside_suffix = outside
            .strip_prefix(Path::new("/"))
            .expect("outside is absolute");
        let injected_relative = format!("x\n/{}", outside_suffix.display());
        let in_scope_path = root.join(injected_relative);
        std::fs::create_dir_all(in_scope_path.parent().expect("parent"))
            .expect("create injected dirs");
        std::fs::write(&in_scope_path, "attacker-controlled in-root file\n")
            .expect("write in-root malicious pathname");

        let outcome = handle_search(
            None,
            json!({
                "pattern": ".",
                "path": root.to_string_lossy().to_string(),
                "glob": ["*"],
                "no_ignore": true,
                "max_results": 10,
                "timeout_ms": 5000
            }),
        )
        .await;

        let payload = outcome.0;
        assert_eq!(
            payload["isError"], true,
            "newline-bearing path must abort the search: {payload}"
        );
        // Confirms the abort came from the fallback path (i.e. resolution
        // happened before ugrep was spawned, not after disclosure).
        assert_eq!(
            payload["backend"], "ugrep",
            "abort must originate from the ugrep fallback: {payload}"
        );
        let text = payload["content"][0]["text"].as_str().unwrap_or("");
        // The diagnostic intentionally echoes the offending in-root pathname
        // (which an attacker controls anyway) for triage. The security
        // boundary is that outside file *contents* must never appear in the
        // payload.
        assert!(
            !text.contains("LEAKED_SECRET_MARKER"),
            "outside file content must never be disclosed: {text}"
        );
        assert!(
            text.starts_with("ugrep error: search aborted"),
            "payload should report the abort reason: {text}"
        );
        assert!(
            text.contains("LF/CR"),
            "payload should identify the LF/CR cause: {text}"
        );

        // Best-effort cleanup; failures are non-fatal because we only care
        // about the assertions above.
        let _ = std::fs::remove_dir_all(&base);
    }
}
