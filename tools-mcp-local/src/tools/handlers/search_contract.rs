//! Shared internal request and response types for the `Search` handler.

use serde_json::{Map, Value, json};
use std::{borrow::Cow, fmt::Write as _};
use tools_mcp_core::{ToolCallOutcome, validation};

const SEARCH_SNIPPET_MAX_LINE_BYTES: usize = 200;

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
    /// Maximum number of match/context events to return (global). Defaults to 100.
    #[serde(default)]
    pub(super) max_results: Option<usize>,
    /// Kill the search if it runs longer than this (ms). Defaults to `10_000`.
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
    /// Fuzzy match tolerance (1-4 edits). Uses the memory backend when eligible, otherwise ugrep.
    #[serde(default)]
    pub(super) fuzzy: Option<u8>,
}

impl SearchRequest {
    fn root(&self) -> &str {
        self.path.as_deref().unwrap_or(".")
    }

    pub(super) fn normalize(&self) -> NormalizedSearchRequest {
        self.into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SearchCaseMode {
    Smart,
    Sensitive,
    Insensitive,
}

impl SearchCaseMode {
    fn from_raw(raw: Option<&str>) -> Self {
        match raw.unwrap_or("smart").to_ascii_lowercase().as_str() {
            "sensitive" | "case-sensitive" | "case_sensitive" => Self::Sensitive,
            "insensitive" | "ignore" | "ignore-case" | "ignore_case" => Self::Insensitive,
            _ => Self::Smart,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct NormalizedSearchRequest {
    pattern: String,
    root: String,
    case_mode: SearchCaseMode,
    fixed_strings: bool,
    word_regexp: bool,
    raw_globs: Vec<String>,
    normalized_globs: Vec<String>,
    hidden: bool,
    follow: bool,
    no_ignore: bool,
    context: usize,
    max_results: usize,
    timeout_ms: u64,
    raw_fuzzy: Option<u8>,
    fuzzy_distance: Option<u8>,
}

impl NormalizedSearchRequest {
    pub(super) fn pattern(&self) -> &str {
        &self.pattern
    }

    pub(super) fn root(&self) -> &str {
        &self.root
    }

    pub(super) fn case_mode(&self) -> SearchCaseMode {
        self.case_mode
    }

    pub(super) fn fixed_strings(&self) -> bool {
        self.fixed_strings
    }

    pub(super) fn word_regexp(&self) -> bool {
        self.word_regexp
    }

    pub(super) fn raw_globs(&self) -> &[String] {
        &self.raw_globs
    }

    pub(super) fn normalized_globs(&self) -> &[String] {
        &self.normalized_globs
    }

    pub(super) fn hidden(&self) -> bool {
        self.hidden
    }

    pub(super) fn follow(&self) -> bool {
        self.follow
    }

    pub(super) fn no_ignore(&self) -> bool {
        self.no_ignore
    }

    pub(super) fn context(&self) -> usize {
        self.context
    }

    pub(super) fn max_results(&self) -> usize {
        self.max_results
    }

    pub(super) fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    pub(super) fn raw_fuzzy(&self) -> Option<u8> {
        self.raw_fuzzy
    }

    pub(super) fn fuzzy_distance(&self) -> Option<u8> {
        self.fuzzy_distance
    }
}

impl From<&SearchRequest> for NormalizedSearchRequest {
    fn from(req: &SearchRequest) -> Self {
        let raw_globs = req.glob.clone().unwrap_or_default();
        let normalized_globs = normalize_globs(&raw_globs);

        Self {
            pattern: req.pattern.clone(),
            root: req.path.clone().unwrap_or_else(|| ".".to_string()),
            case_mode: SearchCaseMode::from_raw(req.case.as_deref()),
            fixed_strings: req.fixed_strings.unwrap_or(false),
            word_regexp: req.word_regexp.unwrap_or(false),
            raw_globs,
            normalized_globs,
            hidden: req.hidden.unwrap_or(false),
            follow: req.follow.unwrap_or(false),
            no_ignore: req.no_ignore.unwrap_or(false),
            context: req.context.unwrap_or(0),
            max_results: validation::clamp_limit(req.max_results, 100, 1, 10_000),
            timeout_ms: validation::clamp_timeout(req.timeout_ms, 10_000, 100, 300_000),
            raw_fuzzy: req.fuzzy,
            fuzzy_distance: req.fuzzy.map(|f| f.clamp(1, 4)),
        }
    }
}

fn normalize_globs(raw_globs: &[String]) -> Vec<String> {
    let mut globs: Vec<String> = raw_globs
        .iter()
        .map(|glob| glob.trim())
        .filter(|glob| !glob.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    globs.sort();
    globs.dedup();
    globs
}

pub(super) fn parse_search_request(
    args: &Value,
) -> Result<NormalizedSearchRequest, ToolCallOutcome> {
    let req = ToolCallOutcome::parse_args::<SearchRequest>(args)?;

    validation::validate_non_empty(&req.pattern, "pattern", None)?;
    validation::validate_non_empty(req.root(), "path", None)?;

    Ok(req.normalize())
}

#[derive(Clone, Debug)]
pub(super) struct SearchEvent {
    pub(super) is_match: bool,
    pub(super) path: String,
    pub(super) line_number: u64,
    pub(super) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchSnippet<'a> {
    text: Cow<'a, str>,
    line_length: usize,
    truncated: bool,
}

fn render_search_snippet(text: &str) -> SearchSnippet<'_> {
    let line_length = text.len();
    if line_length <= SEARCH_SNIPPET_MAX_LINE_BYTES {
        return SearchSnippet {
            text: Cow::Borrowed(text),
            line_length,
            truncated: false,
        };
    }

    let boundary = text
        .char_indices()
        .take_while(|(index, _)| *index <= SEARCH_SNIPPET_MAX_LINE_BYTES)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0);
    let mut snippet = String::with_capacity(boundary.saturating_add('…'.len_utf8()));
    snippet.push_str(&text[..boundary]);
    snippet.push('…');

    SearchSnippet {
        text: Cow::Owned(snippet),
        line_length,
        truncated: true,
    }
}

impl SearchEvent {
    pub(super) fn new(is_match: bool, path: String, line_number: u64, text: String) -> Self {
        Self {
            is_match,
            path,
            line_number,
            text,
        }
    }

    fn rendered_snippet(&self) -> SearchSnippet<'_> {
        render_search_snippet(&self.text)
    }
}

pub(super) struct RenderedSearchEvent<'a> {
    event: &'a SearchEvent,
    snippet: SearchSnippet<'a>,
}

impl<'a> RenderedSearchEvent<'a> {
    fn new(event: &'a SearchEvent) -> Self {
        Self {
            event,
            snippet: event.rendered_snippet(),
        }
    }

    pub(super) fn rendered_line_len(&self) -> usize {
        self.event
            .path
            .len()
            .saturating_add(decimal_digits(self.event.line_number))
            .saturating_add(self.snippet.text.len())
            .saturating_add(2)
    }

    pub(super) fn push_rendered_line(&self, output: &mut String) {
        let sep = if self.event.is_match { ":" } else { "-" };
        output.push_str(&self.event.path);
        output.push_str(sep);
        let _ = write!(output, "{}", self.event.line_number);
        output.push_str(sep);
        output.push_str(self.snippet.text.as_ref());
    }

    fn match_value(&self) -> Value {
        self.event_value(true)
    }

    fn grouped_value(&self) -> Value {
        self.event_value(false)
    }

    fn event_value(&self, include_path: bool) -> Value {
        let mut data = Map::with_capacity(match (include_path, self.snippet.truncated) {
            (true, true) => 5,
            (true, false) => 3,
            (false, true) => 4,
            (false, false) => 2,
        });
        if include_path {
            data.insert("path".to_string(), json!({"text": self.event.path.clone()}));
        }
        data.insert("line_number".to_string(), json!(self.event.line_number));
        data.insert(
            "lines".to_string(),
            json!({"text": self.snippet.text.as_ref()}),
        );
        if self.snippet.truncated {
            data.insert("snippet_truncated".to_string(), json!(true));
            data.insert("line_length".to_string(), json!(self.snippet.line_length));
        }

        json!({
            "type": if self.event.is_match { "match" } else { "context" },
            "data": data,
        })
    }
}

fn decimal_digits(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        value.ilog10() as usize + 1
    }
}

#[derive(Default)]
struct SearchPayloadParts {
    matches: Vec<Value>,
    files: Vec<Value>,
    match_count: usize,
    event_count: usize,
}

struct SearchPayloadBuilder {
    parts: SearchPayloadParts,
    current_path: Option<String>,
    current_match_count: usize,
    current_event_count: usize,
    current_events: Vec<Value>,
}

pub(super) struct SearchPayloadMeta {
    path: String,
    text_view: String,
    is_error: bool,
    exit_code: Value,
    truncated: bool,
    timed_out: bool,
}

impl SearchPayloadMeta {
    pub(super) fn new(
        path: impl Into<String>,
        text_view: String,
        is_error: bool,
        exit_code: Value,
        truncated: bool,
        timed_out: bool,
    ) -> Self {
        Self {
            path: path.into(),
            text_view,
            is_error,
            exit_code,
            truncated,
            timed_out,
        }
    }
}

impl SearchPayloadBuilder {
    fn with_event_capacity(capacity: usize) -> Self {
        Self {
            parts: SearchPayloadParts {
                matches: Vec::with_capacity(capacity),
                files: Vec::new(),
                match_count: 0,
                event_count: 0,
            },
            current_path: None,
            current_match_count: 0,
            current_event_count: 0,
            current_events: Vec::new(),
        }
    }

    #[cfg(test)]
    fn push(&mut self, event: &SearchEvent) {
        self.push_rendered(&RenderedSearchEvent::new(event));
    }

    fn push_rendered(&mut self, rendered: &RenderedSearchEvent<'_>) {
        let event = rendered.event;
        if self.current_path.as_deref() != Some(event.path.as_str()) {
            self.finish_current_file();
            self.current_path = Some(event.path.clone());
        }

        if event.is_match {
            self.parts.match_count = self.parts.match_count.saturating_add(1);
            self.current_match_count = self.current_match_count.saturating_add(1);
        }
        self.parts.event_count = self.parts.event_count.saturating_add(1);
        self.current_event_count = self.current_event_count.saturating_add(1);
        self.parts.matches.push(rendered.match_value());
        self.current_events.push(rendered.grouped_value());
    }

    fn finish(mut self) -> SearchPayloadParts {
        self.finish_current_file();
        self.parts
    }

    fn finish_current_file(&mut self) {
        if let Some(path) = self.current_path.take() {
            self.parts.files.push(json!({
                "path": path,
                "match_count": self.current_match_count,
                "event_count": self.current_event_count,
                "events": std::mem::take(&mut self.current_events),
            }));
            self.current_match_count = 0;
            self.current_event_count = 0;
        }
    }
}

#[cfg(test)]
fn build_search_payload_parts(events: &[SearchEvent]) -> SearchPayloadParts {
    let mut builder = SearchPayloadBuilder::with_event_capacity(events.len());
    for event in events {
        builder.push(event);
    }
    builder.finish()
}

fn build_search_payload_parts_from_rendered(
    rendered_events: &[RenderedSearchEvent<'_>],
) -> SearchPayloadParts {
    let mut builder = SearchPayloadBuilder::with_event_capacity(rendered_events.len());
    for event in rendered_events {
        builder.push_rendered(event);
    }
    builder.finish()
}

pub(super) fn render_search_events(events: &[SearchEvent]) -> Vec<RenderedSearchEvent<'_>> {
    events.iter().map(RenderedSearchEvent::new).collect()
}

pub(super) fn render_search_text_capacity_from_rendered(
    rendered_events: &[RenderedSearchEvent<'_>],
) -> usize {
    rendered_events.iter().fold(
        rendered_events.len().saturating_sub(1),
        |capacity, event| capacity.saturating_add(event.rendered_line_len()),
    )
}

#[cfg(test)]
pub(super) fn render_search_text(events: &[SearchEvent]) -> String {
    render_search_text_from_rendered(&render_search_events(events))
}

pub(super) fn render_search_text_from_rendered(
    rendered_events: &[RenderedSearchEvent<'_>],
) -> String {
    let mut output =
        String::with_capacity(render_search_text_capacity_from_rendered(rendered_events));
    for (index, event) in rendered_events.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        event.push_rendered_line(&mut output);
    }
    output
}

#[cfg(test)]
pub(super) fn build_search_payload(
    req: &NormalizedSearchRequest,
    meta: SearchPayloadMeta,
    events: &[SearchEvent],
) -> Value {
    let parts = build_search_payload_parts(events);
    build_search_payload_from_parts(req, meta, parts)
}

pub(super) fn build_search_payload_from_rendered(
    req: &NormalizedSearchRequest,
    meta: SearchPayloadMeta,
    rendered_events: &[RenderedSearchEvent<'_>],
) -> Value {
    let parts = build_search_payload_parts_from_rendered(rendered_events);
    build_search_payload_from_parts(req, meta, parts)
}

fn build_search_payload_from_parts(
    req: &NormalizedSearchRequest,
    meta: SearchPayloadMeta,
    parts: SearchPayloadParts,
) -> Value {
    // count is an alias for event_count and may be removed in a future release.
    let count = parts.event_count;

    json!({
        "content": [{"type": "text", "text": meta.text_view}],
        "isError": meta.is_error,
        "pattern": req.pattern().to_string(),
        "path": meta.path,
        "exit_code": meta.exit_code,
        "truncated": meta.truncated,
        "timed_out": meta.timed_out,
        "match_count": parts.match_count,
        "event_count": parts.event_count,
        "count": count,
        "matches": parts.matches,
        "files": parts.files,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        NormalizedSearchRequest, SEARCH_SNIPPET_MAX_LINE_BYTES, SearchCaseMode, SearchEvent,
        SearchPayloadMeta, SearchRequest, build_search_payload, build_search_payload_from_rendered,
        render_search_events, render_search_text, render_search_text_from_rendered,
    };
    use serde_json::{Value, json};

    fn normalized_request(path: &str) -> NormalizedSearchRequest {
        SearchRequest {
            pattern: "needle".to_string(),
            path: Some(path.to_string()),
            ..SearchRequest::default()
        }
        .normalize()
    }

    fn build_success_payload(
        req: &NormalizedSearchRequest,
        path: &str,
        events: &[SearchEvent],
    ) -> Value {
        build_search_payload(
            req,
            SearchPayloadMeta::new(
                path,
                render_search_text(events),
                false,
                json!(0),
                false,
                false,
            ),
            events,
        )
    }

    #[test]
    fn rendered_event_payload_matches_legacy_wrappers() {
        let req = normalized_request("src");
        let events = vec![
            SearchEvent::new(false, "src/main.rs".to_string(), 1, "before".to_string()),
            SearchEvent::new(true, "src/main.rs".to_string(), 2, "needle".to_string()),
            SearchEvent::new(true, "src/lib.rs".to_string(), 9, "needle ".repeat(80)),
        ];
        let rendered = render_search_events(&events);

        assert_eq!(
            render_search_text_from_rendered(&rendered),
            render_search_text(&events)
        );
        assert_eq!(
            build_search_payload_from_rendered(
                &req,
                SearchPayloadMeta::new(
                    "src",
                    render_search_text_from_rendered(&rendered),
                    false,
                    json!(0),
                    false,
                    false,
                ),
                &rendered,
            ),
            build_success_payload(&req, "src", &events)
        );
    }

    #[test]
    fn normalize_preserves_defaults_aliases_and_clamps() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            case: Some("ignore-case".to_string()),
            max_results: Some(0),
            timeout_ms: Some(1),
            fuzzy: Some(0),
            ..SearchRequest::default()
        }
        .normalize();

        assert_eq!(req.root(), ".");
        assert_eq!(req.case_mode(), SearchCaseMode::Insensitive);
        assert!(!req.fixed_strings());
        assert!(!req.word_regexp());
        assert!(!req.hidden());
        assert!(!req.follow());
        assert!(!req.no_ignore());
        assert_eq!(req.context(), 0);
        assert_eq!(req.max_results(), 1);
        assert_eq!(req.timeout_ms(), 100);
        assert_eq!(req.raw_fuzzy(), Some(0));
        assert_eq!(req.fuzzy_distance(), Some(1));
    }

    #[test]
    fn normalize_uses_first_pass_defaults_when_unset() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            ..SearchRequest::default()
        }
        .normalize();

        assert_eq!(req.max_results(), 100);
        assert_eq!(req.timeout_ms(), 10_000);
    }

    #[test]
    fn normalize_preserves_raw_globs_and_dedupes_normalized_globs() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            glob: Some(vec![
                " *.rs ".to_string(),
                "".to_string(),
                "*.md".to_string(),
                "*.rs".to_string(),
            ]),
            ..SearchRequest::default()
        }
        .normalize();

        assert_eq!(
            req.raw_globs(),
            &[
                " *.rs ".to_string(),
                "".to_string(),
                "*.md".to_string(),
                "*.rs".to_string(),
            ]
        );
        assert_eq!(
            req.normalized_globs(),
            &["*.md".to_string(), "*.rs".to_string()]
        );
    }

    #[test]
    fn normalize_treats_unknown_case_as_smart() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            case: Some("unexpected".to_string()),
            ..SearchRequest::default()
        }
        .normalize();

        assert_eq!(req.case_mode(), SearchCaseMode::Smart);
    }

    #[test]
    fn render_search_text_preserves_grep_line_format() {
        let events = vec![
            SearchEvent::new(
                true,
                "src/main.rs".to_string(),
                7,
                "let needle = true;".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                8,
                "context line".to_string(),
            ),
        ];

        assert_eq!(
            render_search_text(&events),
            "src/main.rs:7:let needle = true;\nsrc/main.rs-8-context line"
        );
    }

    #[test]
    fn build_search_payload_preserves_response_shape_and_count() {
        let req = normalized_request("src");
        let events = vec![
            SearchEvent::new(
                true,
                "src/main.rs".to_string(),
                7,
                "let needle = true;".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                8,
                "context line".to_string(),
            ),
        ];

        let payload = build_success_payload(&req, "src", &events);
        let top_level = payload.as_object().expect("top-level payload object");

        assert_eq!(top_level.len(), 12);
        for key in [
            "content",
            "isError",
            "pattern",
            "path",
            "exit_code",
            "truncated",
            "timed_out",
            "match_count",
            "event_count",
            "count",
            "matches",
            "files",
        ] {
            assert!(top_level.contains_key(key), "missing top-level key {key}");
        }
        assert_eq!(
            payload["content"][0]["text"],
            "src/main.rs:7:let needle = true;\nsrc/main.rs-8-context line"
        );
        assert_eq!(payload["isError"], false);
        assert_eq!(payload["pattern"], "needle");
        assert_eq!(payload["path"], "src");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["truncated"], false);
        assert_eq!(payload["timed_out"], false);
        assert_eq!(payload["match_count"], 1);
        assert_eq!(payload["event_count"], 2);
        assert_eq!(payload["count"], 2);
        assert_eq!(payload["matches"][0]["type"], "match");
        assert_eq!(payload["matches"][0]["data"]["path"]["text"], "src/main.rs");
        assert_eq!(payload["matches"][0]["data"]["line_number"], 7);
        assert_eq!(
            payload["matches"][0]["data"]["lines"]["text"],
            "let needle = true;"
        );
        assert_eq!(payload["matches"][1]["type"], "context");
        assert_eq!(payload["matches"][1]["data"]["path"]["text"], "src/main.rs");
        assert_eq!(payload["matches"][1]["data"]["line_number"], 8);
        assert_eq!(
            payload["matches"][1]["data"]["lines"]["text"],
            "context line"
        );
        assert_eq!(payload["files"][0]["path"], "src/main.rs");
        assert_eq!(payload["files"][0]["match_count"], 1);
        assert_eq!(payload["files"][0]["event_count"], 2);
        assert!(
            payload["files"][0]["events"][0]["data"]
                .get("path")
                .is_none()
        );
        assert_eq!(payload["files"][0]["events"][0]["type"], "match");
        assert_eq!(payload["files"][0]["events"][0]["data"]["line_number"], 7);
        assert_eq!(
            payload["files"][0]["events"][0]["data"]["lines"]["text"],
            "let needle = true;"
        );
        assert_eq!(payload["files"][0]["events"][1]["type"], "context");
        assert_eq!(payload["files"][0]["events"][1]["data"]["line_number"], 8);
        assert_eq!(
            payload["files"][0]["events"][1]["data"]["lines"]["text"],
            "context line"
        );
    }

    #[test]
    fn build_search_payload_truncates_long_lines_in_text_and_structured_events() {
        let req = normalized_request("src");
        let long_line = "a".repeat(SEARCH_SNIPPET_MAX_LINE_BYTES + 25);
        let truncated_line = format!("{}…", "a".repeat(SEARCH_SNIPPET_MAX_LINE_BYTES));
        let events = vec![SearchEvent::new(
            true,
            "src/main.rs".to_string(),
            7,
            long_line.clone(),
        )];

        let payload = build_success_payload(&req, "src", &events);

        assert_eq!(
            payload["content"][0]["text"],
            format!("src/main.rs:7:{truncated_line}")
        );
        assert_eq!(
            payload["matches"][0]["data"]["lines"]["text"],
            truncated_line
        );
        assert_eq!(payload["matches"][0]["data"]["snippet_truncated"], true);
        assert_eq!(
            payload["matches"][0]["data"]["line_length"].as_u64(),
            Some(long_line.len() as u64)
        );
        assert_eq!(
            payload["files"][0]["events"][0]["data"]["line_length"].as_u64(),
            Some(long_line.len() as u64)
        );
        assert_eq!(
            payload["files"][0]["events"][0]["data"]["lines"]["text"],
            truncated_line
        );
        assert_eq!(
            payload["files"][0]["events"][0]["data"]["snippet_truncated"],
            true
        );
    }

    #[test]
    fn build_search_payload_omits_truncation_fields_for_short_lines() {
        let req = normalized_request("src");
        let short_line = "needle short line".to_string();
        let events = vec![SearchEvent::new(
            true,
            "src/main.rs".to_string(),
            7,
            short_line.clone(),
        )];

        let payload = build_success_payload(&req, "src", &events);
        let data = payload["matches"][0]["data"]
            .as_object()
            .expect("match data object");

        assert_eq!(
            payload["content"][0]["text"],
            format!("src/main.rs:7:{short_line}")
        );
        assert!(data.get("snippet_truncated").is_none());
        assert!(data.get("line_length").is_none());
    }

    #[test]
    fn render_search_text_truncates_multibyte_lines_on_char_boundary() {
        let req = normalized_request("src");
        let long_line = "🙂".repeat(51);
        let truncated_line = format!("{}…", "🙂".repeat(50));
        let events = vec![SearchEvent::new(
            true,
            "src/main.rs".to_string(),
            7,
            long_line.clone(),
        )];

        assert_eq!(
            render_search_text(&events),
            format!("src/main.rs:7:{truncated_line}")
        );

        let payload = build_success_payload(&req, "src", &events);
        assert_eq!(
            payload["matches"][0]["data"]["lines"]["text"],
            truncated_line
        );
        assert_eq!(
            payload["matches"][0]["data"]["line_length"].as_u64(),
            Some(long_line.len() as u64)
        );
    }

    #[test]
    fn build_search_payload_reports_match_count_and_event_count_separately() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some("src".to_string()),
            context: Some(2),
            ..SearchRequest::default()
        }
        .normalize();
        let events = vec![
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                1,
                "context before first".to_string(),
            ),
            SearchEvent::new(
                true,
                "src/main.rs".to_string(),
                2,
                "needle first".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                3,
                "context after first".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                10,
                "context before second".to_string(),
            ),
            SearchEvent::new(
                true,
                "src/main.rs".to_string(),
                11,
                "needle second".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/main.rs".to_string(),
                12,
                "context after second".to_string(),
            ),
            SearchEvent::new(
                true,
                "src/main.rs".to_string(),
                20,
                "needle third".to_string(),
            ),
        ];

        let payload = build_success_payload(&req, "src", &events);

        assert_eq!(payload["match_count"], 3);
        assert_eq!(payload["event_count"], 7);
        assert_eq!(payload["count"], 7);
        assert_eq!(payload["matches"].as_array().expect("matches").len(), 7);
        assert!(payload["event_count"].as_u64().expect("event_count") >= 3);
    }

    #[test]
    fn build_search_payload_groups_events_by_contiguous_file_path() {
        let req = normalized_request("src");
        let events = vec![
            SearchEvent::new(
                true,
                "src/alpha.rs".to_string(),
                1,
                "needle one".to_string(),
            ),
            SearchEvent::new(
                false,
                "src/alpha.rs".to_string(),
                2,
                "context line".to_string(),
            ),
            SearchEvent::new(
                true,
                "src/alpha.rs".to_string(),
                3,
                "needle two".to_string(),
            ),
            SearchEvent::new(
                true,
                "src/beta.rs".to_string(),
                4,
                "needle three".to_string(),
            ),
        ];

        let payload = build_success_payload(&req, "src", &events);
        let files = payload["files"].as_array().expect("files array");

        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "src/alpha.rs");
        assert_eq!(files[0]["match_count"], 2);
        assert_eq!(files[0]["event_count"], 3);
        assert!(files[0]["events"][0]["data"].get("path").is_none());
        assert_eq!(files[0]["events"][0]["type"], "match");
        assert_eq!(files[0]["events"][1]["type"], "context");
        assert_eq!(files[1]["path"], "src/beta.rs");
        assert_eq!(files[1]["match_count"], 1);
        assert_eq!(files[1]["event_count"], 1);
        assert_eq!(
            files[1]["events"][0]["data"]["lines"]["text"],
            "needle three"
        );
    }
}
