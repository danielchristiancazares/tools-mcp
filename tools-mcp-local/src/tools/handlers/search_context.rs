use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use tools_mcp_core::{ToolCallOutcome, validation};

use super::handle_search;

const DEFAULT_CONTEXT_LINES: usize = 3;
const MAX_CONTEXT_LINES: usize = 50;
const DEFAULT_MAX_MATCHES: usize = 20;
const MAX_MATCHES: usize = 200;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchContextRequest {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    case: Option<String>,
    #[serde(default)]
    fixed_strings: Option<bool>,
    #[serde(default)]
    word_regexp: Option<bool>,
    #[serde(default)]
    glob: Option<Vec<String>>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    follow: Option<bool>,
    #[serde(default)]
    no_ignore: Option<bool>,
    #[serde(default)]
    context_lines: Option<usize>,
    #[serde(default)]
    max_matches: Option<usize>,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    fuzzy: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchLocation {
    path: String,
    line_number: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileWindow {
    path: String,
    start_line: usize,
    end_line: usize,
    match_lines: Vec<usize>,
    total_lines: usize,
    text: String,
}

pub async fn handle_search_context(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<SearchContextRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.pattern, "pattern", None) {
        return o;
    }
    if let Some(path) = req.path.as_deref()
        && let Err(o) = validation::validate_non_empty(path, "path", None)
    {
        return o;
    }

    let context_lines = validation::clamp_limit(
        req.context_lines,
        DEFAULT_CONTEXT_LINES,
        0,
        MAX_CONTEXT_LINES,
    );
    let max_matches = validation::clamp_limit(
        req.max_matches.or(req.max_results),
        DEFAULT_MAX_MATCHES,
        1,
        MAX_MATCHES,
    );

    let search_args = build_search_args(&req, max_matches);
    let search_outcome = handle_search(None, search_args).await;
    if search_outcome.0["isError"].as_bool().unwrap_or(false) {
        return search_outcome;
    }

    let root = req.path.as_deref().unwrap_or(".");
    let matches = extract_match_locations(&search_outcome.0, root);
    let windows = match expand_match_windows(&matches, context_lines).await {
        Ok(windows) => windows,
        Err(err) => return ToolCallOutcome::err(err),
    };
    let text = render_context_text(&windows);
    let payload = build_context_payload(&req, &search_outcome.0, matches, windows, text);

    ToolCallOutcome::ok(payload)
}

fn build_search_args(req: &SearchContextRequest, max_matches: usize) -> Value {
    let mut args = serde_json::Map::new();
    args.insert("pattern".to_string(), json!(req.pattern));
    args.insert("context".to_string(), json!(0));
    args.insert("max_results".to_string(), json!(max_matches));

    insert_if_some(&mut args, "path", &req.path);
    insert_if_some(&mut args, "case", &req.case);
    insert_if_some(&mut args, "fixed_strings", &req.fixed_strings);
    insert_if_some(&mut args, "word_regexp", &req.word_regexp);
    insert_if_some(&mut args, "glob", &req.glob);
    insert_if_some(&mut args, "hidden", &req.hidden);
    insert_if_some(&mut args, "follow", &req.follow);
    insert_if_some(&mut args, "no_ignore", &req.no_ignore);
    insert_if_some(&mut args, "timeout_ms", &req.timeout_ms);
    insert_if_some(&mut args, "fuzzy", &req.fuzzy);

    Value::Object(args)
}

fn insert_if_some<T: serde::Serialize>(
    target: &mut serde_json::Map<String, Value>,
    field: &str,
    value: &Option<T>,
) {
    if let Some(value) = value {
        target.insert(field.to_string(), json!(value));
    }
}

fn extract_match_locations(search_payload: &Value, root: &str) -> Vec<MatchLocation> {
    search_payload["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["type"].as_str() == Some("match"))
        .filter_map(|event| {
            let data = event["data"].as_object()?;
            let path = data
                .get("path")?
                .get("text")?
                .as_str()
                .filter(|path| !path.trim().is_empty())?;
            let line_number = data.get("line_number")?.as_u64()?;
            let line_number = usize::try_from(line_number).ok()?;
            let path = validate_match_path(path, root)?;
            Some(MatchLocation { path, line_number })
        })
        .collect()
}

fn validate_match_path(path: &str, root: &str) -> Option<String> {
    let canonical_path = std::fs::canonicalize(path).ok()?;
    let canonical_root = std::fs::canonicalize(root).ok()?;

    if canonical_root.is_file() {
        if canonical_path == canonical_root {
            return canonical_path.to_str().map(ToOwned::to_owned);
        }
        return None;
    }

    if canonical_path.starts_with(&canonical_root) {
        return canonical_path.to_str().map(ToOwned::to_owned);
    }

    None
}

async fn expand_match_windows(
    matches: &[MatchLocation],
    context_lines: usize,
) -> Result<Vec<FileWindow>, String> {
    let mut path_order = Vec::new();
    let mut matches_by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for location in matches {
        if !matches_by_path.contains_key(location.path.as_str()) {
            path_order.push(location.path.as_str());
        }
        matches_by_path
            .entry(location.path.as_str())
            .or_default()
            .push(location.line_number);
    }

    let mut windows = Vec::new();
    for path in path_order {
        let data = tokio::fs::read(path)
            .await
            .map_err(|err| format!("failed to read search match path {path}: {err}"))?;
        let lines = collect_lines_lossy(&data);
        let total_lines = lines.len();
        if total_lines == 0 {
            continue;
        }

        let mut ranges = Vec::new();
        if let Some(line_numbers) = matches_by_path.get_mut(path) {
            line_numbers.sort_unstable();
            line_numbers.dedup();
            for line_number in line_numbers {
                if *line_number == 0 || *line_number > total_lines {
                    continue;
                }
                let start_line = line_number.saturating_sub(context_lines).max(1);
                let end_line = line_number.saturating_add(context_lines).min(total_lines);
                push_merged_range(&mut ranges, start_line, end_line, *line_number);
            }
        }

        for range in ranges {
            windows.push(FileWindow {
                path: path.to_string(),
                start_line: range.start_line,
                end_line: range.end_line,
                match_lines: range.match_lines.clone(),
                total_lines,
                text: render_numbered_window(&lines, &range),
            });
        }
    }

    Ok(windows)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowRange {
    start_line: usize,
    end_line: usize,
    match_lines: Vec<usize>,
}

fn push_merged_range(
    ranges: &mut Vec<WindowRange>,
    start_line: usize,
    end_line: usize,
    match_line: usize,
) {
    if let Some(previous) = ranges.last_mut()
        && start_line <= previous.end_line.saturating_add(1)
    {
        previous.end_line = previous.end_line.max(end_line);
        previous.match_lines.push(match_line);
        return;
    }

    ranges.push(WindowRange {
        start_line,
        end_line,
        match_lines: vec![match_line],
    });
}

fn collect_lines_lossy(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data)
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn render_numbered_window(lines: &[String], range: &WindowRange) -> String {
    let width = range.end_line.max(1).to_string().len();
    let mut output = String::new();
    for line_number in range.start_line..=range.end_line {
        if line_number > range.start_line {
            output.push('\n');
        }
        let marker = if range.match_lines.contains(&line_number) {
            '>'
        } else {
            ' '
        };
        let line = lines.get(line_number - 1).map(String::as_str).unwrap_or("");
        let _ = write!(output, "{marker}{line_number:>width$}\t{line}");
    }
    output
}

fn render_context_text(windows: &[FileWindow]) -> String {
    if windows.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for (index, window) in windows.iter().enumerate() {
        if index > 0 {
            output.push_str("\n\n");
        }
        let _ = writeln!(
            output,
            "{}:{}-{}",
            window.path, window.start_line, window.end_line
        );
        output.push_str(&window.text);
    }
    output
}

fn build_context_payload(
    req: &SearchContextRequest,
    search_payload: &Value,
    matches: Vec<MatchLocation>,
    windows: Vec<FileWindow>,
    text: String,
) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": false,
        "pattern": req.pattern,
        "path": req.path.as_deref().unwrap_or("."),
        "context_lines": validation::clamp_limit(req.context_lines, DEFAULT_CONTEXT_LINES, 0, MAX_CONTEXT_LINES),
        "match_count": search_payload["match_count"].clone(),
        "event_count": search_payload["event_count"].clone(),
        "search_truncated": search_payload["truncated"].clone(),
        "search_timed_out": search_payload["timed_out"].clone(),
        "search_backend": search_payload.get("backend").cloned().unwrap_or(Value::Null),
        "matches": matches
            .into_iter()
            .map(|m| json!({"path": m.path, "line_number": m.line_number}))
            .collect::<Vec<_>>(),
        "windows": windows
            .into_iter()
            .map(|window| json!({
                "path": window.path,
                "start_line": window.start_line,
                "end_line": window.end_line,
                "match_lines": window.match_lines,
                "total_lines": window.total_lines,
                "text": window.text,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        WindowRange, collect_lines_lossy, push_merged_range, render_numbered_window,
        validate_match_path,
    };
    use std::fs;

    #[test]
    fn push_merged_range_merges_overlapping_match_windows() {
        let mut ranges = Vec::new();

        push_merged_range(&mut ranges, 1, 5, 3);
        push_merged_range(&mut ranges, 4, 8, 6);
        push_merged_range(&mut ranges, 12, 14, 13);

        assert_eq!(
            ranges,
            vec![
                WindowRange {
                    start_line: 1,
                    end_line: 8,
                    match_lines: vec![3, 6],
                },
                WindowRange {
                    start_line: 12,
                    end_line: 14,
                    match_lines: vec![13],
                },
            ]
        );
    }

    #[test]
    fn render_numbered_window_marks_match_lines() {
        let lines = collect_lines_lossy(b"one\ntwo\nthree\nfour\n");
        let rendered = render_numbered_window(
            &lines,
            &WindowRange {
                start_line: 2,
                end_line: 4,
                match_lines: vec![3],
            },
        );

        assert_eq!(rendered, " 2\ttwo\n>3\tthree\n 4\tfour");
    }

    #[test]
    fn validate_match_path_rejects_out_of_root_path() {
        let root = tempfile::tempdir().expect("temp root");
        let in_root = root.path().join("in-root.txt");
        let outside_dir = tempfile::tempdir().expect("temp outside");
        let outside = outside_dir.path().join("outside.txt");

        fs::write(&in_root, "needle").expect("write in root");
        fs::write(&outside, "secret").expect("write outside");

        assert!(
            validate_match_path(
                in_root.to_str().expect("utf8"),
                root.path().to_str().expect("utf8")
            )
            .is_some()
        );
        assert!(
            validate_match_path(
                outside.to_str().expect("utf8"),
                root.path().to_str().expect("utf8")
            )
            .is_none()
        );
    }
}
