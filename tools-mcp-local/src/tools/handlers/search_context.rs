use serde::Deserialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Write as _;
use std::ops::Range;
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
    read_path: String,
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

#[derive(Clone, Debug)]
struct LossyLines<'a> {
    text: Cow<'a, str>,
    line_ranges: Vec<Range<usize>>,
}

impl LossyLines<'_> {
    fn len(&self) -> usize {
        self.line_ranges.len()
    }

    fn is_empty(&self) -> bool {
        self.line_ranges.is_empty()
    }

    fn line(&self, line_number: usize) -> Option<&str> {
        let range = self.line_ranges.get(line_number.checked_sub(1)?)?;
        self.text.get(range.start..range.end)
    }
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
    // The root is constant across every match and the same file repeats for
    // each of its matching lines; canonicalize each exactly once instead of
    // twice per match event.
    let canonical_root = std::fs::canonicalize(root).ok();
    let root_is_file = canonical_root
        .as_deref()
        .is_some_and(std::path::Path::is_file);
    let mut validated_by_path: HashMap<&str, Option<String>> = HashMap::new();

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
            let read_path = validated_by_path
                .entry(path)
                .or_insert_with(|| {
                    let canonical_root = canonical_root.as_deref()?;
                    validate_match_path(path, canonical_root, root_is_file)
                })
                .clone()?;
            Some(MatchLocation {
                path: path.to_string(),
                read_path,
                line_number,
            })
        })
        .collect()
}

fn validate_match_path(
    path: &str,
    canonical_root: &std::path::Path,
    root_is_file: bool,
) -> Option<String> {
    let canonical_path = std::fs::canonicalize(path).ok()?;

    if root_is_file {
        if canonical_path == canonical_root {
            return canonical_path.to_str().map(ToOwned::to_owned);
        }
        return None;
    }

    if canonical_path.starts_with(canonical_root) {
        return canonical_path.to_str().map(ToOwned::to_owned);
    }

    None
}

async fn expand_match_windows(
    matches: &[MatchLocation],
    context_lines: usize,
) -> Result<Vec<FileWindow>, String> {
    let mut path_order = Vec::new();
    let mut matches_by_path: HashMap<&str, (String, Vec<usize>)> = HashMap::new();
    for location in matches {
        match matches_by_path.entry(location.read_path.as_str()) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().1.push(location.line_number);
            }
            Entry::Vacant(entry) => {
                path_order.push(location.read_path.as_str());
                entry.insert((location.path.clone(), vec![location.line_number]));
            }
        }
    }

    let mut windows = Vec::with_capacity(matches.len());
    for read_path in path_order {
        let data = tokio::fs::read(read_path)
            .await
            .map_err(|err| format!("failed to read search match path {read_path}: {err}"))?;
        let lines = collect_lines_lossy(&data);
        let total_lines = lines.len();
        if lines.is_empty() {
            continue;
        }

        let mut ranges = Vec::new();
        let display_path =
            if let Some((display_path, line_numbers)) = matches_by_path.get_mut(read_path) {
                ranges.reserve(line_numbers.len());
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
                display_path.clone()
            } else {
                continue;
            };

        for range in ranges {
            let text = render_numbered_window(&lines, &range);
            windows.push(FileWindow {
                path: display_path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                match_lines: range.match_lines,
                total_lines,
                text,
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

fn collect_lines_lossy(data: &[u8]) -> LossyLines<'_> {
    let text = String::from_utf8_lossy(data);
    let line_ranges = collect_line_ranges(text.as_ref());

    LossyLines { text, line_ranges }
}

fn collect_line_ranges(text: &str) -> Vec<Range<usize>> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        ranges.push(start..end);
        start = index + 1;
    }

    if start < bytes.len() {
        ranges.push(start..bytes.len());
    }

    ranges
}

fn decimal_width(mut value: usize) -> usize {
    value = value.max(1);
    let mut width = 1;
    while value >= 10 {
        value /= 10;
        width += 1;
    }
    width
}

fn render_numbered_window(lines: &LossyLines<'_>, range: &WindowRange) -> String {
    let width = decimal_width(range.end_line);
    let line_count = range.end_line.saturating_sub(range.start_line) + 1;
    let line_bytes = (range.start_line..=range.end_line)
        .filter_map(|line_number| lines.line(line_number))
        .map(str::len)
        .sum::<usize>();
    let mut output =
        String::with_capacity(line_bytes + line_count * (width + 2) + line_count.saturating_sub(1));
    let mut match_line_index = 0;

    for line_number in range.start_line..=range.end_line {
        if line_number > range.start_line {
            output.push('\n');
        }
        while range
            .match_lines
            .get(match_line_index)
            .is_some_and(|match_line| *match_line < line_number)
        {
            match_line_index += 1;
        }
        let marker = if range.match_lines.get(match_line_index) == Some(&line_number) {
            '>'
        } else {
            ' '
        };
        let line = lines.line(line_number).unwrap_or("");
        let _ = write!(output, "{marker}{line_number:>width$}\t{line}");
    }
    output
}

fn render_context_text(windows: &[FileWindow]) -> String {
    if windows.is_empty() {
        return String::new();
    }

    let capacity = windows
        .iter()
        .fold(windows.len().saturating_sub(1) * 2, |capacity, window| {
            capacity
                + window.path.len()
                + decimal_width(window.start_line)
                + decimal_width(window.end_line)
                + 3
                + window.text.len()
        });
    let mut output = String::with_capacity(capacity);
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
    // Assemble by moving the rendered text and window bodies; `json!` would
    // deep-copy them (its leaves expand to `to_value(&expr)`) and drop the
    // originals.
    let matches = matches
        .into_iter()
        .map(|location| {
            let mut entry = serde_json::Map::with_capacity(2);
            entry.insert("path".to_string(), Value::String(location.path));
            entry.insert("line_number".to_string(), Value::from(location.line_number));
            Value::Object(entry)
        })
        .collect();
    let windows = windows
        .into_iter()
        .map(|window| {
            let mut entry = serde_json::Map::with_capacity(6);
            entry.insert("path".to_string(), Value::String(window.path));
            entry.insert("start_line".to_string(), Value::from(window.start_line));
            entry.insert("end_line".to_string(), Value::from(window.end_line));
            entry.insert(
                "match_lines".to_string(),
                Value::Array(window.match_lines.into_iter().map(Value::from).collect()),
            );
            entry.insert("total_lines".to_string(), Value::from(window.total_lines));
            entry.insert("text".to_string(), Value::String(window.text));
            Value::Object(entry)
        })
        .collect();

    let mut content_entry = serde_json::Map::with_capacity(2);
    content_entry.insert("type".to_string(), Value::String("text".to_string()));
    content_entry.insert("text".to_string(), Value::String(text));

    let mut payload = serde_json::Map::with_capacity(12);
    payload.insert(
        "content".to_string(),
        Value::Array(vec![Value::Object(content_entry)]),
    );
    payload.insert("isError".to_string(), Value::Bool(false));
    payload.insert("pattern".to_string(), Value::String(req.pattern.clone()));
    payload.insert(
        "path".to_string(),
        Value::String(req.path.as_deref().unwrap_or(".").to_string()),
    );
    payload.insert(
        "context_lines".to_string(),
        Value::from(validation::clamp_limit(
            req.context_lines,
            DEFAULT_CONTEXT_LINES,
            0,
            MAX_CONTEXT_LINES,
        )),
    );
    payload.insert(
        "match_count".to_string(),
        search_payload["match_count"].clone(),
    );
    payload.insert(
        "event_count".to_string(),
        search_payload["event_count"].clone(),
    );
    payload.insert(
        "search_truncated".to_string(),
        search_payload["truncated"].clone(),
    );
    payload.insert(
        "search_timed_out".to_string(),
        search_payload["timed_out"].clone(),
    );
    payload.insert(
        "search_backend".to_string(),
        search_payload
            .get("backend")
            .cloned()
            .unwrap_or(Value::Null),
    );
    payload.insert("matches".to_string(), Value::Array(matches));
    payload.insert("windows".to_string(), Value::Array(windows));
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::{
        FileWindow, WindowRange, collect_lines_lossy, push_merged_range, render_context_text,
        render_numbered_window, validate_match_path,
    };
    use std::fs;

    fn line_texts<'a>(lines: &'a super::LossyLines<'_>) -> Vec<&'a str> {
        (1..=lines.len())
            .map(|line_number| lines.line(line_number).expect("line exists"))
            .collect()
    }

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
    fn render_numbered_window_marks_multiple_match_lines() {
        let lines = collect_lines_lossy(b"one\ntwo\nthree\nfour\nfive\n");
        let rendered = render_numbered_window(
            &lines,
            &WindowRange {
                start_line: 1,
                end_line: 5,
                match_lines: vec![2, 4],
            },
        );

        assert_eq!(rendered, " 1\tone\n>2\ttwo\n 3\tthree\n>4\tfour\n 5\tfive");
    }

    #[test]
    fn collect_lines_lossy_matches_str_lines_edges() {
        let lines = collect_lines_lossy(b"one\r\ntwo\n\nthree\r\n");

        assert_eq!(line_texts(&lines), vec!["one", "two", "", "three"]);
    }

    #[test]
    fn collect_lines_lossy_replaces_invalid_utf8() {
        let lines = collect_lines_lossy(b"valid\ninvalid-\xFF-byte\n");

        assert_eq!(line_texts(&lines), vec!["valid", "invalid-\u{FFFD}-byte"]);
    }

    #[test]
    fn render_numbered_window_omits_crlf_terminators() {
        let lines = collect_lines_lossy(b"alpha\r\nbeta\r\ngamma\r\n");
        let rendered = render_numbered_window(
            &lines,
            &WindowRange {
                start_line: 1,
                end_line: 3,
                match_lines: vec![2],
            },
        );

        assert_eq!(rendered, " 1\talpha\n>2\tbeta\n 3\tgamma");
    }

    #[test]
    fn render_context_text_assembles_window_headers_and_spacing() {
        let rendered = render_context_text(&[
            FileWindow {
                path: "src/a.rs".to_string(),
                start_line: 2,
                end_line: 4,
                match_lines: vec![3],
                total_lines: 10,
                text: " 2\tbefore\n>3\tmatch\n 4\tafter".to_string(),
            },
            FileWindow {
                path: "src/b.rs".to_string(),
                start_line: 7,
                end_line: 7,
                match_lines: vec![7],
                total_lines: 8,
                text: ">7\tother".to_string(),
            },
        ]);

        assert_eq!(
            rendered,
            "src/a.rs:2-4\n 2\tbefore\n>3\tmatch\n 4\tafter\n\nsrc/b.rs:7-7\n>7\tother"
        );
    }

    #[test]
    fn validate_match_path_rejects_out_of_root_path() {
        let root = tempfile::tempdir().expect("temp root");
        let in_root = root.path().join("in-root.txt");
        let outside_dir = tempfile::tempdir().expect("temp outside");
        let outside = outside_dir.path().join("outside.txt");

        fs::write(&in_root, "needle").expect("write in root");
        fs::write(&outside, "secret").expect("write outside");

        let canonical_root = fs::canonicalize(root.path()).expect("canonical root");
        assert!(
            validate_match_path(in_root.to_str().expect("utf8"), &canonical_root, false).is_some()
        );
        assert!(
            validate_match_path(outside.to_str().expect("utf8"), &canonical_root, false).is_none()
        );
    }
}
