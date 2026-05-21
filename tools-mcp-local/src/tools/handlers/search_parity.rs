use super::ripgrep::{handle_search_ugrep_for_test, ugrep_binary_name};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tools_mcp_core::ToolRegistry;

#[derive(Debug)]
struct ParityFixture {
    root: PathBuf,
}

impl ParityFixture {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join("test-work")
            .join(format!(
                "search-parity-{name}-{}-{unique}",
                std::process::id()
            ));

        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create parity fixture root");
        std::fs::write(
            root.join("alpha.txt"),
            "alpha\nneedle one\nbetween\nneedle two\nomega\n",
        )
        .expect("write alpha fixture");
        std::fs::write(
            root.join("beta.md"),
            "Needle capital\nother\nneedle three\n",
        )
        .expect("write beta fixture");
        std::fs::write(
            root.join("unicode.txt"),
            "cafe ascii\ncafé lowercase\nCAFÉ uppercase\n",
        )
        .expect("write unicode fixture");
        std::fs::write(
            root.join("symbols.txt"),
            "literal a.b[0] marker\nregex axb0 marker\n",
        )
        .expect("write symbols fixture");
        std::fs::write(
            root.join("regex.txt"),
            concat!(
                "needle one\n",
                "Needle one\n",
                "needle two\n",
                "prefix needle three suffix\n",
                "item-42 status READY\n",
                "item-aa status ready\n",
                "12345\n",
                "needle-haystack\n",
            ),
        )
        .expect("write regex fixture");
        std::fs::write(root.join("nomatch.txt"), "haystack only\n")
            .expect("write no-match fixture");

        Self { root }
    }

    fn file_path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }

    fn relative_root_arg(&self) -> String {
        let cwd = std::env::current_dir().expect("current dir");
        self.root
            .strip_prefix(cwd)
            .expect("fixture under current dir")
            .display()
            .to_string()
    }

    fn write_file(&self, name: &str, contents: &str) {
        std::fs::write(self.root.join(name), contents)
            .unwrap_or_else(|err| panic!("write {name} fixture: {err}"));
    }

    fn write_nested_file(&self, name: &str, contents: &str) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|err| panic!("create parent for {name} fixture: {err}"));
        }
        std::fs::write(path, contents).unwrap_or_else(|err| panic!("write {name} fixture: {err}"));
    }

    fn write_bytes(&self, name: &str, contents: &[u8]) {
        std::fs::write(self.root.join(name), contents)
            .unwrap_or_else(|err| panic!("write {name} fixture: {err}"));
    }

    fn search_args(&self, extra: Value) -> Value {
        let mut args = json!({
            "pattern": "needle",
            "path": self.root.display().to_string(),
            "case": "sensitive",
            "fixed_strings": true,
            "glob": ["*.txt", "*.md"],
            "hidden": true,
            "no_ignore": true,
            "timeout_ms": 5000
        });

        let args_obj = args.as_object_mut().expect("base args object");
        for (key, value) in extra.as_object().expect("extra args object") {
            args_obj.insert(key.clone(), value.clone());
        }
        args
    }
}

impl Drop for ParityFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedSearchPayload {
    is_error: bool,
    count: usize,
    truncated: bool,
    timed_out: bool,
    exit_code: Value,
    matches: Vec<NormalizedSearchMatch>,
    backend: Option<String>,
    fallback_reason: Option<String>,
    fallback_source: Option<String>,
    fallback_error_type: Option<String>,
    fallback_available: Option<bool>,
    memory_eligibility: Option<String>,
    plan_kind: Option<String>,
    candidate_seed_count: Option<u64>,
    candidate_estimate: Option<u64>,
    candidate_count: Option<u64>,
    verified_line_count: Option<u64>,
    fuzzy_seed_count: Option<u64>,
    fuzzy_seed_partition_count: Option<u64>,
    fuzzy_seed_selected_partition: Option<u64>,
    fuzzy_candidate_seed_count: Option<u64>,
    fuzzy_duplicate_seed_count: Option<u64>,
    fuzzy_verified_lines: Option<u64>,
    max_results_limit: Option<u64>,
    max_results_reached: Option<bool>,
    freshness_check: Option<String>,
}

impl NormalizedSearchPayload {
    fn from_payload(payload: Value) -> Self {
        let mut matches: Vec<_> = payload["matches"]
            .as_array()
            .expect("matches array")
            .iter()
            .map(NormalizedSearchMatch::from_value)
            .collect();
        matches.sort();

        Self {
            is_error: payload["isError"].as_bool().expect("isError bool"),
            count: payload["count"].as_u64().expect("count number") as usize,
            truncated: payload["truncated"].as_bool().expect("truncated bool"),
            timed_out: payload["timed_out"].as_bool().expect("timed_out bool"),
            exit_code: normalized_exit_code(&payload),
            matches,
            backend: payload
                .get("backend")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            fallback_reason: payload
                .get("fallback_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            fallback_source: payload
                .get("fallback_source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            fallback_error_type: payload
                .get("fallback_error_type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            fallback_available: payload.get("fallback_available").and_then(Value::as_bool),
            memory_eligibility: payload
                .get("memory_eligibility")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            plan_kind: payload
                .get("plan_kind")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            candidate_seed_count: payload.get("candidate_seed_count").and_then(Value::as_u64),
            candidate_estimate: payload.get("candidate_estimate").and_then(Value::as_u64),
            candidate_count: payload.get("candidate_count").and_then(Value::as_u64),
            verified_line_count: payload.get("verified_line_count").and_then(Value::as_u64),
            fuzzy_seed_count: payload.get("fuzzy_seed_count").and_then(Value::as_u64),
            fuzzy_seed_partition_count: payload
                .get("fuzzy_seed_partition_count")
                .and_then(Value::as_u64),
            fuzzy_seed_selected_partition: payload
                .get("fuzzy_seed_selected_partition")
                .and_then(Value::as_u64),
            fuzzy_candidate_seed_count: payload
                .get("fuzzy_candidate_seed_count")
                .and_then(Value::as_u64),
            fuzzy_duplicate_seed_count: payload
                .get("fuzzy_duplicate_seed_count")
                .and_then(Value::as_u64),
            fuzzy_verified_lines: payload.get("fuzzy_verified_lines").and_then(Value::as_u64),
            max_results_limit: payload.get("max_results_limit").and_then(Value::as_u64),
            max_results_reached: payload.get("max_results_reached").and_then(Value::as_bool),
            freshness_check: payload
                .get("freshness_check")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }
    }

    fn assert_behavior_matches(&self, expected: &Self) {
        assert_eq!(self.is_error, expected.is_error, "isError parity mismatch");
        assert_eq!(self.count, expected.count, "count parity mismatch");
        assert_eq!(
            self.truncated, expected.truncated,
            "truncated parity mismatch"
        );
        assert_eq!(
            self.timed_out, expected.timed_out,
            "timed_out parity mismatch"
        );
        assert_eq!(
            self.exit_code, expected.exit_code,
            "exit_code parity mismatch"
        );
        assert_eq!(self.matches, expected.matches, "matches parity mismatch");
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NormalizedSearchMatch {
    event_type: String,
    path: String,
    line_number: u64,
    text: String,
}

impl NormalizedSearchMatch {
    fn from_value(value: &Value) -> Self {
        Self {
            event_type: value["type"].as_str().expect("match type").to_string(),
            path: normalize_path_text(
                value["data"]["path"]["text"]
                    .as_str()
                    .expect("match path text"),
            ),
            line_number: value["data"]["line_number"].as_u64().expect("line number"),
            text: normalize_line_text(value["data"]["lines"]["text"].as_str().expect("line text")),
        }
    }
}

fn normalized_exit_code(payload: &Value) -> Value {
    let is_error = payload["isError"].as_bool().expect("isError bool");
    let truncated = payload["truncated"].as_bool().expect("truncated bool");
    let count = payload["count"].as_u64().expect("count number");
    if truncated && !is_error && count > 0 {
        return json!(0);
    }

    payload.get("exit_code").cloned().unwrap_or(Value::Null)
}

fn normalize_path_text(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_line_text(text: &str) -> String {
    text.strip_suffix('\r').unwrap_or(text).to_string()
}

async fn call_public_search(args: Value) -> NormalizedSearchPayload {
    let mut registry = ToolRegistry::new();
    crate::register_tools(&mut registry);
    let response = registry
        .call("Search", None, args)
        .await
        .expect("Search tool registered");
    assert!(
        response.error.is_none(),
        "unexpected protocol error: {:?}",
        response.error
    );
    NormalizedSearchPayload::from_payload(response.result.expect("tool result"))
}

async fn call_forced_ugrep(args: Value) -> NormalizedSearchPayload {
    NormalizedSearchPayload::from_payload(handle_search_ugrep_for_test(args).await.0)
}

async fn assert_public_matches_forced_ugrep(
    args: Value,
) -> (NormalizedSearchPayload, NormalizedSearchPayload) {
    let public = call_public_search(args.clone()).await;
    let ugrep = call_forced_ugrep(args).await;

    public.assert_behavior_matches(&ugrep);
    assert_eq!(ugrep.backend.as_deref(), Some("ugrep"));
    assert_eq!(ugrep.fallback_reason, None);

    (public, ugrep)
}

fn matched_texts(payload: &NormalizedSearchPayload) -> Vec<String> {
    let mut texts: Vec<_> = payload
        .matches
        .iter()
        .filter(|event| event.event_type == "match")
        .map(|event| event.text.clone())
        .collect();
    texts.sort();
    texts
}

fn ugrep_available() -> bool {
    std::process::Command::new(ugrep_binary_name())
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ensure_ugrep_available() -> bool {
    if ugrep_available() {
        true
    } else {
        eprintln!("Skipping Search parity test: ugrep not found on PATH");
        false
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_exact_literal_cases_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("exact-literal");
    let cases = [
        (
            "fixed-sensitive",
            fixture.search_args(json!({})),
            Some("memory"),
            None,
        ),
        (
            "fixed-smart-ascii",
            fixture.search_args(json!({"case": "smart"})),
            Some("memory"),
            None,
        ),
        (
            "fixed-insensitive-ascii",
            fixture.search_args(json!({"pattern": "NEEDLE", "case": "insensitive"})),
            Some("memory"),
            None,
        ),
        (
            "plain-literal-sensitive",
            fixture.search_args(json!({"fixed_strings": false, "case": "sensitive"})),
            Some("memory"),
            None,
        ),
        (
            "plain-literal-smart-ascii",
            fixture.search_args(json!({"fixed_strings": false, "case": "smart"})),
            Some("memory"),
            None,
        ),
        (
            "fixed-special-regex-characters",
            fixture.search_args(json!({
                "pattern": "a.b[0]",
                "glob": ["symbols.txt"]
            })),
            Some("memory"),
            None,
        ),
        (
            "fixed-unicode-sensitive",
            fixture.search_args(json!({
                "pattern": "café",
                "glob": ["unicode.txt"]
            })),
            Some("memory"),
            None,
        ),
        (
            "fixed-file-root",
            fixture.search_args(json!({
                "path": fixture.file_path("alpha.txt"),
                "glob": []
            })),
            Some("memory"),
            None,
        ),
        (
            "fixed-no-match",
            fixture.search_args(json!({"pattern": "absent"})),
            Some("memory"),
            None,
        ),
    ];

    for (name, args, expected_backend, expected_fallback_reason) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            expected_backend,
            "{name}: backend mismatch"
        );
        assert_eq!(
            public.fallback_reason.as_deref(),
            expected_fallback_reason,
            "{name}: fallback_reason mismatch"
        );
        assert_eq!(
            public.memory_eligibility.as_deref(),
            Some("eligible"),
            "{name}: memory eligibility mismatch"
        );
        assert_eq!(
            public.plan_kind.as_deref(),
            Some("exact"),
            "{name}: plan kind mismatch"
        );
        assert!(
            public.candidate_seed_count.unwrap_or_default() >= 1,
            "{name}: missing candidate seed diagnostics"
        );
        assert!(
            public.candidate_estimate.is_some(),
            "{name}: missing candidate estimate diagnostics"
        );
        assert!(
            public.candidate_count.is_some(),
            "{name}: missing candidate count diagnostics"
        );
        assert!(
            public.verified_line_count.is_some(),
            "{name}: missing verified line diagnostics"
        );
        assert_eq!(
            public.max_results_reached,
            Some(public.truncated),
            "{name}: max_results diagnostic mismatch"
        );
        assert_eq!(
            public.freshness_check.as_deref(),
            Some("verified"),
            "{name}: freshness diagnostic mismatch"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_fixed_word_regexp_ascii_boundaries_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fixed-word-regexp");
    fixture.write_file(
        "word-boundaries.txt",
        concat!(
            "foo\n",
            "foo_bar\n",
            "foo-bar\n",
            "foo1\n",
            "1foo\n",
            "foo.\n",
            ".foo\n",
            "foo foo\n",
            " foo \n",
            "barfoo\n",
            "foo\tbar\n",
            "bar foo\n",
            "(foo)\n",
            "foo at start\n",
            "end foo\n",
        ),
    );
    let args = fixture.search_args(json!({
        "pattern": "foo",
        "glob": ["word-boundaries.txt"],
        "fixed_strings": true,
        "word_regexp": true,
        "case": "sensitive"
    }));

    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(public.memory_eligibility.as_deref(), Some("eligible"));
    assert_eq!(public.plan_kind.as_deref(), Some("exact"));
    assert_eq!(
        matched_texts(&public),
        vec![
            " foo ",
            "(foo)",
            ".foo",
            "bar foo",
            "end foo",
            "foo",
            "foo\tbar",
            "foo at start",
            "foo foo",
            "foo-bar",
            "foo.",
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_memory_context_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("context");
    let args = fixture.search_args(json!({
        "glob": ["alpha.txt"],
        "context": 1
    }));

    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert!(
        public
            .matches
            .iter()
            .any(|event| event.event_type == "context")
    );
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
}

#[tokio::test(flavor = "current_thread")]
async fn public_crlf_line_rendering_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("crlf-line-rendering");
    fixture.write_bytes(
        "crlf.txt",
        b"before\r\nneedle crlf\r\nneedle second\r\nafter\r\n",
    );
    let args = fixture.search_args(json!({
        "glob": ["crlf.txt"],
        "context": 1
    }));

    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["needle crlf", "needle second"]);
    assert!(
        public
            .matches
            .iter()
            .all(|event| !event.text.ends_with('\r')),
        "memory-backed CRLF rendering must not retain carriage returns"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_memory_truncation_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("truncation");
    let args = fixture.search_args(json!({"glob": ["alpha.txt"], "max_results": 1}));

    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert!(public.truncated);
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
}

#[tokio::test(flavor = "current_thread")]
async fn public_short_fixed_literal_cases_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("short-fixed-literals");
    fixture.write_file(
        "short.txt",
        concat!(
            "lower x\n",
            "upper X\n",
            "lower id\n",
            "upper ID\n",
            "plain none\n",
        ),
    );
    fixture.write_file(
        "short-context.txt",
        concat!("before\n", "x\n", "middle\n", "x\n", "after\n"),
    );

    let cases = [
        (
            "one-byte-sensitive",
            fixture.search_args(json!({
                "pattern": "x",
                "case": "sensitive",
                "glob": ["short.txt"]
            })),
        ),
        (
            "two-byte-sensitive",
            fixture.search_args(json!({
                "pattern": "id",
                "case": "sensitive",
                "glob": ["short.txt"]
            })),
        ),
        (
            "one-byte-insensitive",
            fixture.search_args(json!({
                "pattern": "x",
                "case": "insensitive",
                "glob": ["short.txt"]
            })),
        ),
        (
            "two-byte-smart-ascii",
            fixture.search_args(json!({
                "pattern": "id",
                "case": "smart",
                "glob": ["short.txt"]
            })),
        ),
        (
            "one-byte-no-match",
            fixture.search_args(json!({
                "pattern": "q",
                "case": "sensitive",
                "glob": ["short.txt"]
            })),
        ),
    ];

    for (name, args) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("memory"),
            "{name}: backend mismatch"
        );
        assert_eq!(public.fallback_reason, None, "{name}: fallback mismatch");
        assert_eq!(
            public.memory_eligibility.as_deref(),
            Some("eligible"),
            "{name}: memory eligibility mismatch"
        );
        assert_eq!(
            public.plan_kind.as_deref(),
            Some("exact"),
            "{name}: plan kind mismatch"
        );
        assert_eq!(
            public.candidate_seed_count,
            Some(0),
            "{name}: short literals should use direct scan"
        );
        assert!(
            public.candidate_estimate.is_some(),
            "{name}: missing candidate estimate diagnostics"
        );
        assert!(
            public.candidate_count.is_some(),
            "{name}: missing candidate count diagnostics"
        );
        assert!(
            public.verified_line_count.is_some(),
            "{name}: missing verified line diagnostics"
        );
    }

    let context_args = fixture.search_args(json!({
        "pattern": "x",
        "case": "sensitive",
        "glob": ["short-context.txt"],
        "context": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(context_args).await;
    assert!(
        public
            .matches
            .iter()
            .any(|event| event.event_type == "context")
    );
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);

    let truncation_args = fixture.search_args(json!({
        "pattern": "x",
        "case": "sensitive",
        "glob": ["short-context.txt"],
        "max_results": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(truncation_args).await;
    assert!(public.truncated);
    assert_eq!(public.count, 1);
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
}

#[tokio::test(flavor = "current_thread")]
async fn public_hidden_selection_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("hidden-selection");
    fixture.write_file(".hidden-selection.txt", "hidden hiddenneedle\n");
    fixture.write_file("visible-hidden-selection.txt", "visible hiddenneedle\n");

    let hidden_excluded_args = fixture.search_args(json!({
        "pattern": "hiddenneedle",
        "glob": ["*.txt"],
        "hidden": false,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(hidden_excluded_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["visible hiddenneedle"]);

    let hidden_included_args = fixture.search_args(json!({
        "pattern": "hiddenneedle",
        "glob": ["*.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(hidden_included_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(
        matched_texts(&public),
        vec!["hidden hiddenneedle", "visible hiddenneedle"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_ignore_selection_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("ignore-selection");
    fixture.write_file(".gitignore", "ignored-selection.txt\nignored-dir/\n");
    fixture.write_file("visible-ignore-selection.txt", "visible ignoreneedle\n");
    fixture.write_file("ignored-selection.txt", "ignored ignoreneedle\n");
    fixture.write_nested_file("ignored-dir/nested.txt", "nested ignoreneedle\n");

    let ignore_enabled_args = fixture.search_args(json!({
        "pattern": "ignoreneedle",
        "glob": ["*.txt"],
        "hidden": true,
        "no_ignore": false
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(ignore_enabled_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["visible ignoreneedle"]);

    let no_ignore_args = fixture.search_args(json!({
        "pattern": "ignoreneedle",
        "glob": ["*.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(no_ignore_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(
        matched_texts(&public),
        vec![
            "ignored ignoreneedle",
            "nested ignoreneedle",
            "visible ignoreneedle"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_glob_include_selection_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("glob-include-selection");
    fixture.write_file("selected.keep", "root globneedle\n");
    fixture.write_file("unselected.skip", "skip globneedle\n");
    fixture.write_nested_file("nested/selected.keep", "nested globneedle\n");

    let args = fixture.search_args(json!({
        "pattern": "globneedle",
        "glob": ["*.keep"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(
        matched_texts(&public),
        vec!["nested globneedle", "root globneedle"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_file_root_and_directory_root_selection_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("file-root-selection");
    fixture.write_file("file-root-target.txt", "file-root fileneedle\n");

    let dir_root_args = fixture.search_args(json!({
        "pattern": "fileneedle",
        "glob": ["file-root-target.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(dir_root_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["file-root fileneedle"]);

    let file_root_args = fixture.search_args(json!({
        "pattern": "fileneedle",
        "path": fixture.file_path("file-root-target.txt"),
        "glob": ["*.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(file_root_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["file-root fileneedle"]);
}

#[tokio::test(flavor = "current_thread")]
async fn public_path_separator_globs_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("path-separator-globs");
    fixture.write_nested_file("src/path-selection.txt", "src pathneedle\n");
    fixture.write_nested_file("other/path-selection.txt", "other pathneedle\n");

    let absolute_root_relative_glob_args = fixture.search_args(json!({
        "pattern": "pathneedle",
        "glob": ["src/*.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) =
        assert_public_matches_forced_ugrep(absolute_root_relative_glob_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    // Slash globs are matched against the search-root-relative form, so
    // `src/*.txt` against an absolute root selects `<root>/src/path-selection.txt`.
    assert_eq!(matched_texts(&public), vec!["src pathneedle"]);

    let absolute_full_path_glob_args = fixture.search_args(json!({
        "pattern": "pathneedle",
        "glob": ["**/src/*.txt"],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(absolute_full_path_glob_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["src pathneedle"]);

    let relative_root = fixture.relative_root_arg();
    let relative_root_glob = format!("{}/**/src/*.txt", relative_root.replace('\\', "/"));
    let relative_root_args = fixture.search_args(json!({
        "pattern": "pathneedle",
        "path": relative_root,
        "glob": [relative_root_glob],
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(relative_root_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(matched_texts(&public), vec!["src pathneedle"]);
}

#[tokio::test(flavor = "current_thread")]
async fn public_unsupported_globs_fall_back_with_forced_ugrep_parity() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("unsupported-globs");
    let cases = [
        (
            "brace-glob",
            fixture.search_args(json!({"glob": ["*.{txt,md}"]})),
            "unsupported_glob_syntax",
        ),
        (
            "invalid-glob",
            fixture.search_args(json!({"glob": ["["]})),
            "invalid_glob",
        ),
    ];

    for (name, args, expected_fallback_reason) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("ugrep"),
            "{name}: backend mismatch"
        );
        assert_eq!(
            public.fallback_reason.as_deref(),
            Some(expected_fallback_reason),
            "{name}: fallback_reason mismatch"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_follow_true_falls_back_with_forced_ugrep_parity() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("follow-fallback");
    let args = fixture.search_args(json!({
        "glob": ["alpha.txt"],
        "follow": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert_eq!(public.backend.as_deref(), Some("ugrep"));
    assert_eq!(
        public.fallback_reason.as_deref(),
        Some("unsupported_follow")
    );
    assert_eq!(public.fallback_source.as_deref(), Some("memory"));
    assert_eq!(
        public.fallback_error_type.as_deref(),
        Some("unsupported_search_option")
    );
    assert_eq!(public.fallback_available, Some(true));
    assert_eq!(public.memory_eligibility.as_deref(), Some("fallback"));
    assert_eq!(public.plan_kind.as_deref(), Some("ugrep"));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn public_follow_true_symlink_selection_matches_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("follow-symlink");
    fixture.write_file("symlink-target.txt", "symlink linkneedle\n");
    std::os::unix::fs::symlink(
        fixture.root.join("symlink-target.txt"),
        fixture.root.join("linked-selection.txt"),
    )
    .expect("create symlink fixture");

    let args = fixture.search_args(json!({
        "pattern": "linkneedle",
        "glob": ["linked-selection.txt"],
        "follow": true,
        "hidden": true,
        "no_ignore": true
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

    assert_eq!(public.backend.as_deref(), Some("ugrep"));
    assert_eq!(
        public.fallback_reason.as_deref(),
        Some("unsupported_follow")
    );
    assert_eq!(matched_texts(&public), vec!["symlink linkneedle"]);
}

#[tokio::test(flavor = "current_thread")]
async fn public_fuzzy_edit_operations_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fuzzy-edit-ops");
    fixture.write_file(
        "fuzzy-edit-ops.txt",
        concat!(
            "exact abcdef\n",
            "insert abcXdef\n",
            "delete abdef\n",
            "substitute abcxef\n",
            "miss abXYef\n",
        ),
    );
    let args = fixture.search_args(json!({
        "pattern": "abcdef",
        "glob": ["fuzzy-edit-ops.txt"],
        "fuzzy": 1
    }));

    let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;
    let matched_texts: Vec<&str> = public
        .matches
        .iter()
        .filter(|event| event.event_type == "match")
        .map(|event| event.text.as_str())
        .collect();

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(public.fuzzy_seed_count, Some(2));
    assert_eq!(public.fuzzy_verified_lines, Some(5));
    assert_eq!(
        matched_texts,
        vec![
            "exact abcdef",
            "insert abcXdef",
            "delete abdef",
            "substitute abcxef"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_fuzzy_distances_two_through_four_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fuzzy-distances");
    let cases = [
        (
            "distance-2",
            "fuzzy-distance-2.txt",
            "abcdefghi",
            2,
            "dist2 abcXXdefghi\n",
            3,
        ),
        (
            "distance-3",
            "fuzzy-distance-3.txt",
            "abcdefghijkl",
            3,
            "dist3 abcXXXdefghijkl\n",
            4,
        ),
        (
            "distance-4",
            "fuzzy-distance-4.txt",
            "abcdefghijklmno",
            4,
            "dist4 abcXXXXdefghijklmno\n",
            5,
        ),
    ];

    for (name, file_name, pattern, distance, contents, expected_seed_count) in cases {
        fixture.write_file(file_name, contents);
        let args = fixture.search_args(json!({
            "pattern": pattern,
            "glob": [file_name],
            "fuzzy": distance
        }));

        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("memory"),
            "{name}: backend mismatch"
        );
        assert_eq!(public.fallback_reason, None, "{name}: fallback mismatch");
        assert_eq!(
            public.fuzzy_seed_count,
            Some(expected_seed_count),
            "{name}: seed count mismatch"
        );
        assert_eq!(public.count, 1, "{name}: match count mismatch");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_fuzzy_context_and_truncation_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fuzzy-context-truncation");
    fixture.write_file(
        "fuzzy-context.txt",
        concat!("before\n", "abcXdef\n", "middle\n", "abcdef\n", "after\n"),
    );

    let context_args = fixture.search_args(json!({
        "pattern": "abcdef",
        "glob": ["fuzzy-context.txt"],
        "fuzzy": 1,
        "context": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(context_args).await;

    assert!(
        public
            .matches
            .iter()
            .any(|event| event.event_type == "context")
    );
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(public.fuzzy_seed_count, Some(2));

    let truncation_args = fixture.search_args(json!({
        "pattern": "abcdef",
        "glob": ["fuzzy-context.txt"],
        "fuzzy": 1,
        "max_results": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(truncation_args).await;

    assert!(public.truncated);
    assert_eq!(public.count, 1);
    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
}

#[tokio::test(flavor = "current_thread")]
async fn public_fuzzy_repeated_and_utf8_seed_diagnostics_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fuzzy-seed-diagnostics");
    fixture.write_file(
        "fuzzy-seed-diagnostics.txt",
        concat!(
            "repeat exact aaaaaaaa\n",
            "repeat insert aaaaXaaaa\n",
            "repeat delete aaaaaaa\n",
            "unicode exact éabcé\n",
            "unicode substitute éabdé\n",
            "miss aaXXaa\n",
        ),
    );

    let repeated_args = fixture.search_args(json!({
        "pattern": "aaaaaaaa",
        "glob": ["fuzzy-seed-diagnostics.txt"],
        "fuzzy": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(repeated_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(public.fuzzy_seed_count, Some(2));
    assert_eq!(public.fuzzy_candidate_seed_count, Some(1));
    assert_eq!(public.fuzzy_duplicate_seed_count, Some(1));
    assert!(public.fuzzy_seed_partition_count.unwrap_or_default() > 1);
    assert!(public.fuzzy_seed_selected_partition.is_some());

    let unicode_args = fixture.search_args(json!({
        "pattern": "éabcé",
        "glob": ["fuzzy-seed-diagnostics.txt"],
        "fuzzy": 1
    }));
    let (public, _ugrep) = assert_public_matches_forced_ugrep(unicode_args).await;

    assert_eq!(public.backend.as_deref(), Some("memory"));
    assert_eq!(public.fallback_reason, None);
    assert_eq!(public.fuzzy_seed_count, Some(2));
    assert_eq!(public.fuzzy_candidate_seed_count, Some(2));
    assert!(public.fuzzy_seed_partition_count.unwrap_or_default() >= 1);
}

#[tokio::test(flavor = "current_thread")]
async fn public_fuzzy_fallback_boundaries_match_forced_ugrep_with_metadata() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("fuzzy-fallbacks");
    fixture.write_file(
        "fuzzy-fallbacks.txt",
        concat!("abcdef\n", "abcde\n", "ééé\n"),
    );
    fixture.write_bytes("invalid-scope.txt", &[0x66, 0x80, 0x6f, b'\n']);
    let cases = [
        (
            "regex-fuzzy",
            fixture.search_args(json!({
                "pattern": "abcdef",
                "fixed_strings": false,
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 1
            })),
            "unsupported_regex_fuzzy",
        ),
        (
            "case-insensitive-fuzzy",
            fixture.search_args(json!({
                "pattern": "abcdef",
                "case": "insensitive",
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 1
            })),
            "unsupported_case_fuzzy",
        ),
        (
            "word-fuzzy",
            fixture.search_args(json!({
                "pattern": "abcdef",
                "word_regexp": true,
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 1
            })),
            "unsupported_word_fuzzy",
        ),
        (
            "unsupported-distance",
            fixture.search_args(json!({
                "pattern": "abcdef",
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 0
            })),
            "unsupported_fuzzy_mode",
        ),
        (
            "short-pattern",
            fixture.search_args(json!({
                "pattern": "abcde",
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 1
            })),
            "fuzzy_pattern_too_short",
        ),
        (
            "unseedable-unicode-pattern",
            fixture.search_args(json!({
                "pattern": "ééé",
                "glob": ["fuzzy-fallbacks.txt"],
                "fuzzy": 1
            })),
            "fuzzy_pattern_unseedable",
        ),
        (
            "invalid-utf8-scope",
            fixture.search_args(json!({
                "pattern": "abcdef",
                "glob": ["invalid-scope.txt"],
                "fuzzy": 1
            })),
            "fuzzy_scope_not_utf8",
        ),
    ];

    for (name, args, expected_fallback_reason) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("ugrep"),
            "{name}: backend mismatch"
        );
        assert_eq!(
            public.fallback_reason.as_deref(),
            Some(expected_fallback_reason),
            "{name}: fallback_reason mismatch"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_exact_literal_fallbacks_match_forced_ugrep_with_metadata() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("exact-fallbacks");
    let cases = [
        (
            "unicode-plain-literal-insensitive",
            fixture.search_args(json!({
                "pattern": "café",
                "case": "insensitive",
                "fixed_strings": false,
                "glob": ["unicode.txt"]
            })),
            "unsupported_unicode_regex_case_insensitive",
        ),
        (
            "word-regex-literal",
            fixture.search_args(json!({"fixed_strings": false, "word_regexp": true})),
            "unsupported_word_regexp",
        ),
        (
            "word-non-ascii-fixed-literal",
            fixture.search_args(json!({
                "pattern": "café",
                "glob": ["unicode.txt"],
                "word_regexp": true
            })),
            "unsupported_word_regexp",
        ),
        (
            "short-word-fixed-literal",
            fixture.search_args(json!({"pattern": "ne", "word_regexp": true})),
            "query_without_required_trigram",
        ),
    ];

    for (name, args, expected_fallback_reason) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("ugrep"),
            "{name}: backend mismatch"
        );
        assert_eq!(
            public.fallback_reason.as_deref(),
            Some(expected_fallback_reason),
            "{name}: fallback_reason mismatch"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_seeded_regex_cases_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("seeded-regex");
    let cases = [
        (
            "case-sensitive-alternation",
            fixture.search_args(json!({
                "pattern": "needle (one|three)",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
        ),
        (
            "wildcard-with-required-literals",
            fixture.search_args(json!({
                "pattern": "needle .* suffix",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
        ),
    ];

    for (name, args) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("memory"),
            "{name}: backend mismatch"
        );
        assert_eq!(public.fallback_reason, None, "{name}: fallback mismatch");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_supported_regex_anchors_and_classes_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("regex-anchors-classes");
    let cases = [
        (
            "anchored-digit-and-upper-classes",
            fixture.search_args(json!({
                "pattern": "^item-[0-9][0-9] status [A-Z]+$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
        ),
        (
            "anchored-lowercase-character-classes",
            fixture.search_args(json!({
                "pattern": "^needle [ot][a-z]+$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
        ),
    ];

    for (name, args) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("memory"),
            "{name}: backend mismatch"
        );
        assert_eq!(public.fallback_reason, None, "{name}: fallback mismatch");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_expanded_regex_seed_cases_match_forced_ugrep() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("expanded-regex-seeds");
    fixture.write_file(
        "regex-seed-expansion.txt",
        concat!(
            "abab\n", "abcd\n", "cdab\n", "cdcd\n", "acd\n", "abxcd\n", "ab+c\n",
        ),
    );
    let cases = [
        (
            "bounded-short-repetition",
            fixture.search_args(json!({
                "pattern": "^(ab){2}$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex-seed-expansion.txt"]
            })),
        ),
        (
            "optional-short-concat",
            fixture.search_args(json!({
                "pattern": "^ab?cd$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex-seed-expansion.txt"]
            })),
        ),
        (
            "branch-local-short-alternation",
            fixture.search_args(json!({
                "pattern": "^(ab|cd){2}$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex-seed-expansion.txt"]
            })),
        ),
    ];

    for (name, args) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("memory"),
            "{name}: backend mismatch"
        );
        assert_eq!(public.fallback_reason, None, "{name}: fallback mismatch");
        assert!(
            public.candidate_seed_count.unwrap_or_default() >= 1,
            "{name}: missing regex seed diagnostics"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_regex_fallback_boundaries_match_forced_ugrep_with_metadata() {
    if !ensure_ugrep_available() {
        return;
    }

    let fixture = ParityFixture::new("regex-fallbacks");
    let cases = [
        (
            "unseeded-anchored-class",
            fixture.search_args(json!({
                "pattern": "^[0-9]+$",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "query_without_required_trigram",
        ),
        (
            "linebreak-escape",
            fixture.search_args(json!({
                "pattern": "needle\\Rhaystack",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_multiline_regex",
        ),
        (
            "lf-capable-class",
            fixture.search_args(json!({
                "pattern": "needle[^0-9]+haystack",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_multiline_regex",
        ),
        (
            "inline-flag",
            fixture.search_args(json!({
                "pattern": "(?i)needle",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_regex_backend",
        ),
        (
            "lookaround",
            fixture.search_args(json!({
                "pattern": "needle(?= one)",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_regex_backend",
        ),
        (
            "special-group",
            fixture.search_args(json!({
                "pattern": "(?:needle)",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_regex_backend",
        ),
        (
            "parser-rejected",
            fixture.search_args(json!({
                "pattern": "needle(",
                "fixed_strings": false,
                "case": "sensitive",
                "glob": ["regex.txt"]
            })),
            "unsupported_regex_backend",
        ),
    ];

    for (name, args, expected_fallback_reason) in cases {
        let (public, _ugrep) = assert_public_matches_forced_ugrep(args).await;

        assert_eq!(
            public.backend.as_deref(),
            Some("ugrep"),
            "{name}: backend mismatch"
        );
        assert_eq!(
            public.fallback_reason.as_deref(),
            Some(expected_fallback_reason),
            "{name}: fallback_reason mismatch"
        );
    }
}
