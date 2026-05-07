//! ugrep search handler implementation.

use glob::{MatchOptions, Pattern};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{self, Instant};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::validation;

use super::search_memory::handle_memory_search;

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchRequest {
    /// Regex (or literal if `fixed_strings=true`).
    pub(super) pattern: String,
    /// Root path (file or directory). Defaults to ".".
    #[serde(default)]
    pub(super) path: Option<String>,
    /// Case handling: "smart" (default), "sensitive", or "insensitive".
    #[serde(default)]
    pub(super) case: Option<String>,
    /// Treat pattern as literal text (-F).
    #[serde(default)]
    pub(super) fixed_strings: Option<bool>,
    /// Whole-word search (-w).
    #[serde(default)]
    pub(super) word_regexp: Option<bool>,
    /// Add `--glob <pattern>` entries.
    #[serde(default)]
    pub(super) glob: Option<Vec<String>>,
    /// Include hidden files/directories (`--hidden`).
    #[serde(default)]
    pub(super) hidden: Option<bool>,
    /// Follow symlinks (`--follow`).
    #[serde(default)]
    pub(super) follow: Option<bool>,
    /// Do not respect ignore files (`--no-ignore`).
    #[serde(default)]
    pub(super) no_ignore: Option<bool>,
    /// Context lines around each match (`-C`).
    #[serde(default)]
    pub(super) context: Option<usize>,
    /// Maximum number of match/context events to return (global). Defaults to 200.
    #[serde(default)]
    pub(super) max_results: Option<usize>,
    /// Kill the search if it runs longer than this (ms). Defaults to `20_000`.
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
    /// Fuzzy match tolerance (1-4 edits). Uses the memory backend when eligible, otherwise ugrep.
    #[serde(default)]
    pub(super) fuzzy: Option<u8>,
}

impl SearchRequest {
    pub(super) fn root(&self) -> &str {
        self.path.as_deref().unwrap_or(".")
    }

    pub(super) fn max_results(&self) -> usize {
        validation::clamp_limit(self.max_results, 200, 1, 10_000)
    }

    pub(super) fn timeout_ms(&self) -> u64 {
        validation::clamp_timeout(self.timeout_ms, 20_000, 100, 300_000)
    }

    pub(super) fn fuzzy_distance(&self) -> Option<u8> {
        self.fuzzy.map(|f| f.clamp(1, 4))
    }

    pub(super) fn case_mode(&self) -> String {
        self.case.as_deref().unwrap_or("smart").to_ascii_lowercase()
    }
}

fn parse_and_validate(args: &Value) -> Result<SearchRequest, ToolCallOutcome> {
    let req = ToolCallOutcome::parse_args::<SearchRequest>(args)?;

    validation::validate_non_empty(&req.pattern, "pattern", None)?;
    validation::validate_non_empty(req.root(), "path", None)?;

    Ok(req)
}

fn add_fallback_metadata(mut outcome: ToolCallOutcome, fallback_reason: &str) -> ToolCallOutcome {
    if let Some(obj) = outcome.0.as_object_mut() {
        obj.insert("backend".to_string(), Value::String("ugrep".to_string()));
        obj.insert(
            "fallback_reason".to_string(),
            Value::String(fallback_reason.to_string()),
        );
    }
    outcome
}

#[derive(Clone, Debug)]
struct CompiledGlob {
    pattern: Pattern,
    match_basename: bool,
}

#[derive(Clone, Debug)]
struct SearchGlobFilter {
    patterns: Vec<CompiledGlob>,
    match_options: MatchOptions,
}

impl SearchGlobFilter {
    fn from_request(req: &SearchRequest, include_hidden: bool) -> Option<Self> {
        let globs = req.glob.as_ref()?;
        let mut patterns = Vec::new();

        for raw_glob in globs {
            let trimmed = raw_glob.trim();
            if trimmed.is_empty() {
                continue;
            }
            if contains_unsupported_glob_syntax(trimmed) {
                return None;
            }
            let pattern = Pattern::new(trimmed).ok()?;
            patterns.push(CompiledGlob {
                pattern,
                match_basename: !contains_path_separator(trimmed),
            });
        }

        (!patterns.is_empty()).then_some(Self {
            patterns,
            match_options: MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: !include_hidden,
            },
        })
    }

    fn is_match(&self, root: &Path, path: &Path) -> bool {
        self.patterns
            .iter()
            .any(|compiled| self.compiled_pattern_matches(compiled, root, path))
    }

    fn compiled_pattern_matches(&self, compiled: &CompiledGlob, root: &Path, path: &Path) -> bool {
        if let Some(relative) = path_relative_to_root(root, path)
            && compiled
                .pattern
                .matches_path_with(relative, self.match_options)
        {
            return true;
        }

        if compiled.match_basename
            && let Some(file_name) = path.file_name()
            && compiled
                .pattern
                .matches_path_with(Path::new(file_name), self.match_options)
        {
            return true;
        }

        compiled.pattern.matches_path_with(path, self.match_options)
    }
}

fn path_relative_to_root<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    if root.is_file() {
        return path.file_name().map(Path::new);
    }

    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative);
    }

    if matches!(root.to_str(), Some(".") | Some("./")) {
        return Some(path);
    }

    None
}

fn contains_path_separator(pattern: &str) -> bool {
    pattern.contains('/') || pattern.contains('\\')
}

fn contains_unsupported_glob_syntax(pattern: &str) -> bool {
    pattern.starts_with('!') || pattern.contains('{') || pattern.contains('}')
}

/// Detects LF or CR bytes in a path's underlying OS string.
///
/// Such bytes are valid in Unix filenames but would be interpreted as
/// record terminators by ugrep's line-oriented `--from=-` file list.
/// Refusing these paths is required to keep the search root boundary
/// honest: a single in-root pathname containing `\n` could otherwise
/// inject an attacker-chosen absolute path as a separate file-list
/// entry, causing ugrep to read files outside `req.root()`.
#[cfg(unix)]
fn path_has_line_separator(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .any(|b| matches!(b, b'\n' | b'\r'))
}

/// Non-Unix fallback: Windows/NTFS forbids LF/CR in filenames at the OS
/// level, but defend in depth by scanning the lossy UTF-8 form, since
/// that is exactly what would be written to ugrep's stdin.
#[cfg(not(unix))]
fn path_has_line_separator(path: &Path) -> bool {
    path.to_string_lossy()
        .bytes()
        .any(|b| matches!(b, b'\n' | b'\r'))
}

fn resolve_globbed_files_for_ugrep(req: &SearchRequest) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let include_hidden = req.hidden.unwrap_or(false);
    let Some(glob_filter) = SearchGlobFilter::from_request(req, include_hidden) else {
        return Ok(None);
    };

    let root = Path::new(req.root());
    let no_ignore = req.no_ignore.unwrap_or(false);
    let follow = req.follow.unwrap_or(false);
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .follow_links(follow)
        .ignore(!no_ignore)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore);

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        if entry.file_type().is_some_and(|ft| ft.is_symlink()) && !follow {
            continue;
        }
        let path = entry.into_path();
        if glob_filter.is_match(root, &path) {
            // Refuse any matched path whose bytes contain LF/CR; ugrep's
            // line-oriented `--from=-` would otherwise treat the suffix
            // after the embedded newline as a separate (potentially
            // absolute and out-of-root) file to search. Use Debug-format
            // for the diagnostic so embedded control bytes render as
            // escapes rather than disrupting the error string.
            if path_has_line_separator(&path) {
                anyhow::bail!(
                    "search aborted: matched path contains LF/CR bytes that cannot \
                     be safely passed to ugrep --from=- (offending path: {:?})",
                    path
                );
            }
            files.push(path);
        }
    }
    files.sort();
    Ok(Some(files))
}

/// Defense-in-depth filter: returns true if `parsed_path` was one of the
/// pre-resolved paths we authorized for ugrep. When `allowed` is `None`
/// (no glob filter / non-`--from=-` path), every result is permitted.
fn is_path_authorized(parsed_path: &str, allowed: Option<&HashSet<String>>) -> bool {
    match allowed {
        Some(set) => set.contains(parsed_path),
        None => true,
    }
}

/// Parse a grep-style output line: "path:line:text" (match) or "path-line-text" (context)
/// Returns (path, `line_number`, text, `is_match`)
fn parse_grep_line(line: &str) -> (String, u64, String, bool) {
    fn separator_candidates(line: &str, sep: u8) -> Vec<(String, u64, String, bool)> {
        let bytes = line.as_bytes();
        let mut candidates = Vec::new();
        for i in 0..bytes.len() {
            if bytes[i] != sep {
                continue;
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
                candidates.push((
                    line[..i].to_string(),
                    line_no,
                    line[j + 1..].to_string(),
                    sep == b':',
                ));
            }
        }
        candidates
    }

    fn parse_with_sep_from_end(line: &str, sep: u8) -> Option<(String, u64, String, bool)> {
        separator_candidates(line, sep).into_iter().next_back()
    }

    fn parse_match_line(line: &str) -> Option<(String, u64, String, bool)> {
        let candidates = separator_candidates(line, b':');
        if candidates.len() > 1
            && let Some(existing_path) = candidates
                .iter()
                .rev()
                .find(|(path, _, _, _)| Path::new(path).exists())
        {
            return Some(existing_path.clone());
        }
        candidates.into_iter().next()
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
    let req = match parse_and_validate(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    match handle_memory_search(&req).await {
        Ok(outcome) => outcome,
        Err(err) if err.fallback_allowed => {
            add_fallback_metadata(handle_search_ugrep(req).await, err.fallback_reason)
        }
        Err(err) => err.into_tool_outcome(&req),
    }
}

async fn handle_search_ugrep(req: SearchRequest) -> ToolCallOutcome {
    let root = req.root().to_string();
    let max_results = req.max_results();
    let timeout_ms = req.timeout_ms();
    let fuzzy_distance = req.fuzzy_distance();
    let bin = if cfg!(target_os = "windows") {
        "ugrep.exe"
    } else {
        "ugrep"
    };

    let run = async {
        let globbed_files = resolve_globbed_files_for_ugrep(&req)?;
        if matches!(globbed_files, Some(ref files) if files.is_empty()) {
            return Ok::<_, anyhow::Error>((
                Vec::new(),
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
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        });

        let mut cmd = Command::new(bin);

        // ugrep: use text output with -n -H for simpler parsing
        cmd.arg("-r").arg("-n").arg("-H");

        // Fuzzy flag
        if let Some(dist) = fuzzy_distance {
            cmd.arg(format!("-Z{dist}"));
        }

        if req.fixed_strings.unwrap_or(false) {
            cmd.arg("-F");
        }
        if req.word_regexp.unwrap_or(false) {
            cmd.arg("-w");
        }

        // Case mode: -j for smart-case, -i for insensitive
        match req
            .case
            .as_deref()
            .unwrap_or("smart")
            .to_ascii_lowercase()
            .as_str()
        {
            "sensitive" | "case-sensitive" | "case_sensitive" => {
                // default behavior (no flags)
            }
            "insensitive" | "ignore" | "ignore-case" | "ignore_case" => {
                cmd.arg("-i");
            }
            _ => {
                cmd.arg("-j"); // ugrep smart-case
            }
        }

        if req.hidden.unwrap_or(false) {
            cmd.arg("--hidden");
        }
        if req.follow.unwrap_or(false) {
            cmd.arg("--dereference");
        }
        if req.no_ignore.unwrap_or(false) {
            cmd.arg("--no-ignore-files");
        }
        if let Some(c) = req.context
            && c > 0
        {
            cmd.arg("-C").arg(c.to_string());
        }
        if globbed_files.is_some() {
            cmd.arg("--from=-");
        } else if let Some(globs) = &req.glob {
            for g in globs {
                if !g.trim().is_empty() {
                    cmd.arg("-g").arg(g);
                }
            }
        }

        // End of options marker prevents patterns like "//" or "-foo" from being
        // interpreted as flags
        cmd.arg("--").arg(&req.pattern);
        if globbed_files.is_none() {
            cmd.arg(&root);
        }

        if globbed_files.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

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
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut r = BufReader::new(stderr);
            let _ = r.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let mut matches: Vec<Value> = Vec::new();
        let mut rendered_lines: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut timed_out = false;
        let mut terminated_for_limit = false;

        let mut reader = BufReader::new(stdout).lines();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
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

                    let sep = if is_match { ":" } else { "-" };
                    rendered_lines.push(format!("{path}{sep}{line_no}{sep}{text}"));

                    let match_obj = json!({
                        "type": if is_match { "match" } else { "context" },
                        "data": {
                            "path": {"text": path},
                            "line_number": line_no,
                            "lines": {"text": text}
                        }
                    });
                    matches.push(match_obj);

                    if matches.len() >= max_results {
                        truncated = true;
                        terminated_for_limit = true;
                        let _ = child.kill().await;
                        break;
                    }
                }
                () = time::sleep_until(deadline) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    break;
                }
            }
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
            matches,
            rendered_lines,
            truncated,
            exit_code,
            stderr_text,
            success,
            timed_out,
        ))
    };

    match run.await {
        Ok((matches, rendered_lines, truncated, exit_code, stderr_text, success, timed_out)) => {
            let text_view = if !success && !stderr_text.is_empty() {
                // Show error message when search failed
                format!("Search error: {stderr_text}")
            } else if rendered_lines.is_empty() {
                String::new()
            } else {
                rendered_lines.join("\n")
            };

            let mut payload = json!({
                "content": [{"type": "text", "text": text_view}],
                "isError": !success,
                "pattern": req.pattern,
                "path": root,
                "exit_code": exit_code,
                "truncated": truncated,
                "timed_out": timed_out,
                "count": matches.len(),
                "matches": matches,
            });

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
    use super::{
        SearchRequest, classify_success, handle_search, is_path_authorized, parse_grep_line,
        path_has_line_separator, resolve_globbed_files_for_ugrep,
    };
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
    fn parse_grep_line_preserves_existing_filename_with_colon_number_colon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("foo:1:bar.txt");
        std::fs::write(&path, "needle\n").expect("write");
        let line = format!("{}:7:needle", path.display());

        let (parsed_path, line_no, text, is_match) = parse_grep_line(&line);

        assert_eq!(parsed_path, path.display().to_string());
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
        };

        let err =
            resolve_globbed_files_for_ugrep(&req).expect_err("LF-bearing matched path must abort");
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
        };

        let err =
            resolve_globbed_files_for_ugrep(&req).expect_err("CR-bearing matched path must abort");
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
        };

        let resolved = resolve_globbed_files_for_ugrep(&req)
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
        allowed.insert("/repo/inside.txt".to_string());

        assert!(is_path_authorized("/repo/inside.txt", Some(&allowed)));
        // A path that ugrep might invent or that an attacker tried to inject
        // (e.g. via a future regression) must be filtered out.
        assert!(!is_path_authorized("/etc/passwd", Some(&allowed)));
        assert!(!is_path_authorized("/repo/other.txt", Some(&allowed)));
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
