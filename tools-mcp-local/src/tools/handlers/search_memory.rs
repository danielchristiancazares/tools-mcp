//! In-memory fast path for the `Search` tool.

use super::ripgrep::SearchRequest;
use glob::{MatchOptions, Pattern};
use ignore::WalkBuilder;
use regex::bytes::{Regex, RegexBuilder};
use regex_syntax::{
    ParserBuilder as RegexParserBuilder,
    hir::{Class, Hir, HirKind},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tools_mcp_core::ToolCallOutcome;
use tracing::{debug, info, warn};

const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_FILES: usize = 50_000;
const DEFAULT_MAX_CANDIDATES: usize = 20_000;
const DEFAULT_MAX_FUZZY_PATTERN_CHARS: usize = 512;
const DEFAULT_MAX_FUZZY_VERIFIED_LINES: usize = 200_000;
const DEFAULT_MAX_FUZZY_LINE_CHARS: usize = 16_384;
const DEFAULT_REGEX_SIZE_LIMIT_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_WARM_CACHE_TIMEOUT_MS: u64 = 300_000;

#[derive(Clone, Debug)]
pub(super) struct MemoryError {
    pub(super) error_type: &'static str,
    pub(super) fallback_reason: &'static str,
    pub(super) fallback_allowed: bool,
    message: String,
    timed_out: bool,
}

impl MemoryError {
    fn new(
        error_type: &'static str,
        fallback_reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            error_type,
            fallback_reason,
            fallback_allowed: true,
            message: message.into(),
            timed_out: false,
        }
    }

    fn timeout() -> Self {
        Self {
            error_type: "query_timeout",
            fallback_reason: "query_timeout",
            fallback_allowed: false,
            message: "memory search timed out".to_string(),
            timed_out: true,
        }
    }

    pub(super) fn into_tool_outcome(self, req: &SearchRequest) -> ToolCallOutcome {
        ToolCallOutcome::err_with(
            self.message,
            [
                ("backend", json!("memory")),
                ("error_type", json!(self.error_type)),
                ("fallback_available", json!(self.fallback_allowed)),
                (
                    "remediation",
                    json!(
                        "Use a narrower fixed-string search, reduce the search scope, or retry with a larger timeout."
                    ),
                ),
                ("pattern", json!(req.pattern)),
                ("path", json!(req.root())),
                ("exit_code", Value::Null),
                ("truncated", json!(false)),
                ("timed_out", json!(self.timed_out)),
                ("count", json!(0)),
                ("matches", json!([])),
            ],
        )
    }
}

#[derive(Clone, Debug)]
struct Limits {
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_files: usize,
    max_candidates: usize,
    max_fuzzy_pattern_chars: usize,
    max_fuzzy_verified_lines: usize,
    max_fuzzy_line_chars: usize,
    regex_size_limit_bytes: usize,
}

impl Limits {
    fn from_env() -> Self {
        Self {
            max_file_bytes: env_u64("TOOLS_SEARCH_INDEX_MAX_FILE_BYTES", DEFAULT_MAX_FILE_BYTES),
            max_total_bytes: env_u64(
                "TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES",
                DEFAULT_MAX_TOTAL_BYTES,
            ),
            max_files: env_usize("TOOLS_SEARCH_INDEX_MAX_FILES", DEFAULT_MAX_FILES),
            max_candidates: env_usize("TOOLS_SEARCH_MAX_CANDIDATES", DEFAULT_MAX_CANDIDATES),
            max_fuzzy_pattern_chars: env_usize(
                "TOOLS_SEARCH_MAX_FUZZY_PATTERN_CHARS",
                DEFAULT_MAX_FUZZY_PATTERN_CHARS,
            ),
            max_fuzzy_verified_lines: env_usize(
                "TOOLS_SEARCH_MAX_FUZZY_VERIFIED_LINES",
                DEFAULT_MAX_FUZZY_VERIFIED_LINES,
            ),
            max_fuzzy_line_chars: env_usize(
                "TOOLS_SEARCH_MAX_FUZZY_LINE_CHARS",
                DEFAULT_MAX_FUZZY_LINE_CHARS,
            ),
            regex_size_limit_bytes: env_usize(
                "TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES",
                DEFAULT_REGEX_SIZE_LIMIT_BYTES,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    change_marker: Option<MetadataChangeMarker>,
    hash: [u8; 32],
}

#[derive(Clone, Debug)]
struct Document {
    path: PathBuf,
    rendered_path: String,
    stamp: FileStamp,
    content: Vec<u8>,
    lines: Vec<LineRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiteralCase {
    Sensitive,
    AsciiInsensitive,
}

#[derive(Debug)]
struct IndexSnapshot {
    generation: u64,
    documents: Vec<Document>,
    postings: HashMap<[u8; 3], Vec<usize>>,
    ascii_folded_postings: HashMap<[u8; 3], Vec<usize>>,
    indexed_bytes: u64,
    all_content_utf8: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct IndexKey {
    root: String,
    hidden: bool,
    follow: bool,
    no_ignore: bool,
    globs: Vec<String>,
}

#[derive(Debug)]
struct IndexCache {
    next_generation: u64,
    entries: HashMap<IndexKey, Arc<IndexSnapshot>>,
}

impl Default for IndexCache {
    fn default() -> Self {
        Self {
            next_generation: 1,
            entries: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct CachedSnapshot {
    key: IndexKey,
    snapshot: Arc<IndexSnapshot>,
    cache_status: &'static str,
}

#[derive(Debug)]
struct WarmCacheSummary {
    root: String,
    generation: u64,
    indexed_files: usize,
    indexed_bytes: u64,
}

#[derive(Clone, Debug)]
struct SearchEvent {
    is_match: bool,
    path: String,
    line_number: u64,
    text: String,
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

#[derive(Clone, Debug)]
enum QueryPlan {
    Exact {
        literal: Vec<u8>,
        case: LiteralCase,
    },
    Regex {
        matcher: Regex,
        candidates: CandidateExpr,
    },
    Fuzzy {
        pattern_chars: Vec<char>,
        distance: usize,
        seeds: Vec<Vec<u8>>,
    },
}

impl QueryPlan {
    fn requires_utf8_scope(&self) -> bool {
        matches!(self, Self::Regex { .. } | Self::Fuzzy { .. })
    }

    fn fuzzy_seed_count(&self) -> usize {
        match self {
            Self::Exact { .. } | Self::Regex { .. } => 0,
            Self::Fuzzy { seeds, .. } => seeds.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CandidateExpr {
    Seed(Vec<u8>),
    And(Vec<CandidateExpr>),
    Or(Vec<CandidateExpr>),
}

impl IndexKey {
    fn from_request(req: &SearchRequest) -> Self {
        Self {
            root: req.root().to_string(),
            hidden: req.hidden.unwrap_or(false),
            follow: req.follow.unwrap_or(false),
            no_ignore: req.no_ignore.unwrap_or(false),
            globs: normalized_globs(req),
        }
    }
}

static INDEX_CACHE: OnceLock<Mutex<IndexCache>> = OnceLock::new();

#[cfg(unix)]
type MetadataChangeMarker = (i64, i64);

#[cfg(not(unix))]
type MetadataChangeMarker = ();

fn index_cache() -> &'static Mutex<IndexCache> {
    INDEX_CACHE.get_or_init(|| Mutex::new(IndexCache::default()))
}

fn lock_index_cache() -> MutexGuard<'static, IndexCache> {
    index_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn normalized_globs(req: &SearchRequest) -> Vec<String> {
    req.glob
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|glob| glob.trim())
        .filter(|glob| !glob.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn reserve_index_generation() -> u64 {
    let mut cache = lock_index_cache();
    let generation = cache.next_generation;
    cache.next_generation = cache.next_generation.saturating_add(1).max(1);
    generation
}

fn cached_snapshot(key: &IndexKey) -> Option<Arc<IndexSnapshot>> {
    lock_index_cache().entries.get(key).cloned()
}

fn publish_snapshot_if_absent(key: IndexKey, snapshot: Arc<IndexSnapshot>) -> Arc<IndexSnapshot> {
    let mut cache = lock_index_cache();
    cache.entries.entry(key).or_insert(snapshot).clone()
}

fn evict_snapshot_if_current(key: &IndexKey, snapshot: &Arc<IndexSnapshot>) {
    let mut cache = lock_index_cache();
    if cache
        .entries
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, snapshot))
    {
        cache.entries.remove(key);
    }
}

pub(super) fn start_search_cache_warmer() {
    match std::thread::Builder::new()
        .name("tools-search-index-warm".to_string())
        .spawn(|| {
            debug!("starting search index warm-cache thread");
            match warm_cache_for_current_git_repo_blocking() {
                Ok(Some(summary)) => {
                    info!(
                        root = %summary.root,
                        generation = summary.generation,
                        indexed_files = summary.indexed_files,
                        indexed_bytes = summary.indexed_bytes,
                        "warmed search index cache"
                    );
                }
                Ok(None) => {
                    debug!("search index warm-cache skipped because current directory is not inside a git worktree");
                }
                Err(err) => {
                    debug!(error = %err, "search index warm-cache did not populate");
                }
            }
        }) {
        Ok(handle) => {
            std::mem::drop(handle);
        }
        Err(err) => {
            warn!(error = %err, "failed to start search index warm-cache thread");
        }
    }
}

pub(super) async fn handle_memory_search(
    req: &SearchRequest,
) -> Result<ToolCallOutcome, MemoryError> {
    let limits = Limits::from_env();
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms());
    let plan = eligible_query_plan_with_limits(req, &limits, deadline)?;
    validate_plan_limits(&plan, &limits)?;
    let cached = get_or_build_snapshot(req, &limits, deadline, plan.requires_utf8_scope())?;
    let snapshot = cached.snapshot;

    check_deadline(deadline)?;
    let phase_one_start = Instant::now();
    let candidates = candidates_for_plan(&snapshot, &plan, limits.max_candidates)?;
    let phase_one_ms = phase_one_start.elapsed().as_millis() as u64;

    check_deadline(deadline)?;
    let phase_two_start = Instant::now();
    let (events, rendered_lines, truncated, fuzzy_verified_lines) =
        verify_and_render(&snapshot, &candidates, &plan, req, &limits, deadline)?;
    let phase_two_ms = phase_two_start.elapsed().as_millis() as u64;

    if let Err(err) = check_snapshot_fresh(req, &snapshot, deadline) {
        evict_snapshot_if_current(&cached.key, &snapshot);
        return Err(err);
    }

    let text_view = rendered_lines.join("\n");
    let exit_code = if events.iter().any(|event| event.is_match) {
        0
    } else {
        1
    };
    let matches: Vec<Value> = events
        .iter()
        .map(|event| {
            json!({
                "type": if event.is_match { "match" } else { "context" },
                "data": {
                    "path": {"text": event.path},
                    "line_number": event.line_number,
                    "lines": {"text": event.text}
                }
            })
        })
        .collect();

    Ok(ToolCallOutcome::ok(json!({
        "content": [{"type": "text", "text": text_view}],
        "isError": false,
        "pattern": req.pattern,
        "path": req.root(),
        "exit_code": exit_code,
        "truncated": truncated,
        "timed_out": false,
        "count": matches.len(),
        "matches": matches,
        "backend": "memory",
        "index_cache": cached.cache_status,
        "index_generation": snapshot.generation,
        "indexed_files": snapshot.documents.len(),
        "indexed_bytes": snapshot.indexed_bytes,
        "candidate_count": candidates.len(),
        "fuzzy_seed_count": plan.fuzzy_seed_count(),
        "fuzzy_verified_lines": fuzzy_verified_lines,
        "phase_one_ms": phase_one_ms,
        "phase_two_ms": phase_two_ms,
    })))
}

fn get_or_build_snapshot(
    req: &SearchRequest,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
) -> Result<CachedSnapshot, MemoryError> {
    let key = IndexKey::from_request(req);
    if let Some(snapshot) = cached_snapshot(&key) {
        ensure_snapshot_supports_query(&snapshot, require_utf8_scope)?;
        return Ok(CachedSnapshot {
            key,
            snapshot,
            cache_status: "hit",
        });
    }

    let generation = reserve_index_generation();
    let snapshot = Arc::new(build_index(
        req,
        limits,
        deadline,
        require_utf8_scope,
        generation,
    )?);
    let snapshot = publish_snapshot_if_absent(key.clone(), snapshot);
    ensure_snapshot_supports_query(&snapshot, require_utf8_scope)?;
    Ok(CachedSnapshot {
        key,
        snapshot,
        cache_status: "miss",
    })
}

fn ensure_snapshot_supports_query(
    snapshot: &IndexSnapshot,
    require_utf8_scope: bool,
) -> Result<(), MemoryError> {
    if require_utf8_scope && !snapshot.all_content_utf8 {
        return Err(MemoryError::new(
            "search_index_incomplete",
            "fuzzy_scope_not_utf8",
            "memory fuzzy search requires valid UTF-8 text for the selected scope",
        ));
    }
    Ok(())
}

fn warm_cache_for_current_git_repo_blocking() -> Result<Option<WarmCacheSummary>, String> {
    let cwd = std::env::current_dir().map_err(|err| format!("failed to read cwd: {err}"))?;
    let Some(repo_root) = git_worktree_root_from(&cwd) else {
        return Ok(None);
    };
    let root = warm_cache_root_argument(&cwd, &repo_root);
    warm_cache_for_root(root)
        .map(Some)
        .map_err(|err| err.message)
}

fn warm_cache_root_argument(cwd: &Path, repo_root: &Path) -> String {
    if paths_refer_to_same_location(cwd, repo_root) {
        ".".to_string()
    } else {
        repo_root.display().to_string()
    }
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn warm_cache_for_root(root: String) -> Result<WarmCacheSummary, MemoryError> {
    let req = SearchRequest {
        pattern: "__tools_mcp_search_warm_cache__".to_string(),
        path: Some(root.clone()),
        case: Some("sensitive".to_string()),
        fixed_strings: Some(true),
        word_regexp: Some(false),
        glob: None,
        hidden: Some(false),
        follow: Some(false),
        no_ignore: Some(false),
        context: None,
        max_results: None,
        timeout_ms: Some(search_index_warm_timeout_ms()),
        fuzzy: None,
    };
    let limits = Limits::from_env();
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms());
    let key = IndexKey::from_request(&req);

    if let Some(snapshot) = cached_snapshot(&key) {
        return Ok(WarmCacheSummary {
            root,
            generation: snapshot.generation,
            indexed_files: snapshot.documents.len(),
            indexed_bytes: snapshot.indexed_bytes,
        });
    }

    let generation = reserve_index_generation();
    let snapshot = Arc::new(build_index(&req, &limits, deadline, false, generation)?);
    let snapshot = publish_snapshot_if_absent(key, snapshot);

    Ok(WarmCacheSummary {
        root,
        generation: snapshot.generation,
        indexed_files: snapshot.documents.len(),
        indexed_bytes: snapshot.indexed_bytes,
    })
}

fn git_worktree_root_from(cwd: &Path) -> Option<PathBuf> {
    let start = fs::canonicalize(cwd).ok()?;
    start
        .ancestors()
        .find(|dir| has_git_worktree_marker(dir))
        .map(Path::to_path_buf)
}

fn has_git_worktree_marker(dir: &Path) -> bool {
    let marker = dir.join(".git");
    let Ok(metadata) = fs::metadata(&marker) else {
        return false;
    };

    if metadata.is_dir() {
        return true;
    }

    metadata.is_file() && fs::read(&marker).is_ok_and(|content| is_gitdir_file_marker(&content))
}

fn is_gitdir_file_marker(content: &[u8]) -> bool {
    let content = content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(content);
    let first_non_whitespace = content
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(content.len());
    content[first_non_whitespace..].starts_with(b"gitdir:")
}

#[cfg(test)]
fn git_bin() -> &'static str {
    if cfg!(target_os = "windows") {
        "git.exe"
    } else {
        "git"
    }
}

fn search_index_warm_timeout_ms() -> u64 {
    env_u64(
        "TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS",
        DEFAULT_WARM_CACHE_TIMEOUT_MS,
    )
    .clamp(100, 300_000)
}

#[cfg(test)]
fn eligible_query_plan(req: &SearchRequest) -> Result<QueryPlan, MemoryError> {
    let limits = Limits::from_env();
    eligible_query_plan_with_limits(req, &limits, Instant::now() + Duration::from_secs(60))
}

fn eligible_query_plan_with_limits(
    req: &SearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<QueryPlan, MemoryError> {
    if let Some(distance) = req.fuzzy {
        return eligible_fuzzy_plan(req, distance);
    }

    if req.word_regexp.unwrap_or(false) {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_word_regexp",
            "memory search does not support word_regexp",
        ));
    }
    if req.follow.unwrap_or(false) {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_follow",
            "memory search does not support following symlinks",
        ));
    }
    let fixed_strings = req.fixed_strings.unwrap_or(false);
    if !fixed_strings && !is_plain_regex_literal(&req.pattern) {
        return eligible_regex_plan(req, limits, deadline);
    }
    let case = literal_case_for_request(req, fixed_strings)?;

    let literal = req.pattern.as_bytes();
    if literal.len() < 3 {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "query_without_required_trigram",
            "memory search requires a literal of at least three bytes",
        ));
    }
    if literal.contains(&b'\n') || literal.contains(&b'\r') {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_multiline_literal",
            "memory search does not support multiline fixed strings",
        ));
    }
    Ok(QueryPlan::Exact {
        literal: literal.to_vec(),
        case,
    })
}

fn eligible_regex_plan(
    req: &SearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<QueryPlan, MemoryError> {
    check_deadline(deadline)?;
    ensure_regex_case_sensitive(req)?;
    ensure_no_unsupported_inline_regex_construct(&req.pattern)?;

    if req.pattern.contains('\n') || req.pattern.contains('\r') {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_multiline_regex",
            "memory regex search does not support multiline regex patterns",
        ));
    }
    if has_line_break_escape(&req.pattern) {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_multiline_regex",
            "memory regex search does not support regex patterns that match line breaks",
        ));
    }

    let hir = RegexParserBuilder::new()
        .utf8(true)
        .unicode(true)
        .build()
        .parse(&req.pattern)
        .map_err(|err| {
            MemoryError::new(
                "unsupported_regex_dialect",
                "unsupported_regex_backend",
                format!("memory regex search could not parse the pattern: {err}"),
            )
        })?;
    check_deadline(deadline)?;
    if hir_can_match_lf(&hir) {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_multiline_regex",
            "memory regex search does not support regex patterns that can match line breaks",
        ));
    }
    let mut candidate_budget = limits.max_candidates;
    let candidates = required_candidate_expr(&hir, &mut candidate_budget).ok_or_else(|| {
        MemoryError::new(
            "unsupported_regex_dialect",
            "query_without_required_trigram",
            "memory regex search requires a proven literal substring of at least three bytes",
        )
    })?;
    check_deadline(deadline)?;

    let mut builder = RegexBuilder::new(&req.pattern);
    builder
        .unicode(true)
        .case_insensitive(false)
        .multi_line(false)
        .dot_matches_new_line(false)
        .size_limit(limits.regex_size_limit_bytes);
    let matcher = builder.build().map_err(|err| {
        MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_regex_backend",
            format!("memory regex verifier could not compile the pattern: {err}"),
        )
    })?;

    Ok(QueryPlan::Regex {
        matcher,
        candidates,
    })
}

fn ensure_regex_case_sensitive(req: &SearchRequest) -> Result<(), MemoryError> {
    match req.case_mode().as_str() {
        "sensitive" | "case-sensitive" | "case_sensitive" => Ok(()),
        "insensitive" | "ignore" | "ignore-case" | "ignore_case" => Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_regex_case_insensitive",
            "memory regex search does not support case-insensitive regex matching",
        )),
        _ => {
            if contains_uppercase_letter(&req.pattern) {
                Ok(())
            } else {
                Err(MemoryError::new(
                    "unsupported_search_option",
                    "unsupported_regex_smart_case_insensitive",
                    "memory regex search falls back for smart-case regexes that would be case-insensitive",
                ))
            }
        }
    }
}

fn ensure_no_unsupported_inline_regex_construct(pattern: &str) -> Result<(), MemoryError> {
    if has_inline_regex_construct(pattern) {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_regex_backend",
            "memory regex search does not support inline flags, look-around, or special group syntax",
        ));
    }
    Ok(())
}

fn has_inline_regex_construct(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut in_class = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index = index.saturating_add(2);
            continue;
        }
        if in_class {
            if byte == b']' {
                in_class = false;
            }
            index += 1;
            continue;
        }

        match byte {
            b'[' => in_class = true,
            b'(' if bytes.get(index + 1) == Some(&b'?') => return true,
            _ => {}
        }
        index += 1;
    }

    false
}

fn has_line_break_escape(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let Some(&escaped) = bytes.get(index + 1) else {
            return false;
        };
        if matches!(escaped, b'n' | b'r' | b'R' | b'X') {
            return true;
        }
        index += 2;
    }
    false
}

fn literal_case_for_request(
    req: &SearchRequest,
    fixed_strings: bool,
) -> Result<LiteralCase, MemoryError> {
    match req.case_mode().as_str() {
        "sensitive" | "case-sensitive" | "case_sensitive" => Ok(LiteralCase::Sensitive),
        "insensitive" | "ignore" | "ignore-case" | "ignore_case" => {
            if fixed_strings || req.pattern.is_ascii() {
                Ok(LiteralCase::AsciiInsensitive)
            } else {
                Err(MemoryError::new(
                    "unsupported_search_option",
                    "unsupported_unicode_regex_case_insensitive",
                    "memory regex literal search does not support Unicode case-insensitive matching",
                ))
            }
        }
        _ => {
            if contains_uppercase_letter(&req.pattern) {
                Ok(LiteralCase::Sensitive)
            } else if fixed_strings || req.pattern.is_ascii() {
                Ok(LiteralCase::AsciiInsensitive)
            } else {
                Err(MemoryError::new(
                    "unsupported_search_option",
                    "unsupported_unicode_regex_smart_case",
                    "memory regex literal search does not support Unicode smart-case matching",
                ))
            }
        }
    }
}

fn validate_plan_limits(plan: &QueryPlan, limits: &Limits) -> Result<(), MemoryError> {
    if let QueryPlan::Fuzzy { pattern_chars, .. } = plan
        && pattern_chars.len() > limits.max_fuzzy_pattern_chars
    {
        return Err(MemoryError::new(
            "resource_limit_exceeded",
            "max_fuzzy_pattern_chars_exceeded",
            "fuzzy pattern exceeds memory search verifier limit",
        ));
    }

    Ok(())
}

fn eligible_fuzzy_plan(req: &SearchRequest, distance: u8) -> Result<QueryPlan, MemoryError> {
    if !req.fixed_strings.unwrap_or(false) {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_regex_fuzzy",
            "memory search only supports fuzzy matching for fixed_strings=true",
        ));
    }
    if req.word_regexp.unwrap_or(false) {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_word_fuzzy",
            "memory search does not support fuzzy word_regexp",
        ));
    }
    if req.follow.unwrap_or(false) {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_fuzzy_follow",
            "memory search does not support following symlinks for fuzzy matching",
        ));
    }
    match req.case_mode().as_str() {
        "sensitive" | "case-sensitive" | "case_sensitive" => {}
        _ => {
            return Err(MemoryError::new(
                "unsupported_search_option",
                "unsupported_case_fuzzy",
                "memory search only supports fuzzy matching with case=sensitive",
            ));
        }
    }
    if !(1..=4).contains(&distance) {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_fuzzy_mode",
            "memory search supports fuzzy distances from 1 through 4",
        ));
    }
    if req.pattern.contains('\n') || req.pattern.contains('\r') {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_multiline_fuzzy",
            "memory search does not support multiline fuzzy fixed strings",
        ));
    }

    let seed_count = usize::from(distance) + 1;
    if req.pattern.len() < seed_count * 3 {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "fuzzy_pattern_too_short",
            "fuzzy memory search requires at least three bytes per seed",
        ));
    }
    let seeds = fuzzy_seed_segments(&req.pattern, distance).ok_or_else(|| {
        MemoryError::new(
            "unsupported_search_option",
            "fuzzy_pattern_unseedable",
            "fuzzy fixed-string pattern cannot be partitioned into required seed segments",
        )
    })?;

    Ok(QueryPlan::Fuzzy {
        pattern_chars: req.pattern.chars().collect(),
        distance: usize::from(distance),
        seeds,
    })
}

fn build_index(
    req: &SearchRequest,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
    generation: u64,
) -> Result<IndexSnapshot, MemoryError> {
    let mut documents = Vec::new();
    let mut indexed_bytes = 0_u64;
    let mut all_content_utf8 = true;

    for path in discover_files(req, Some(deadline))? {
        check_deadline(deadline)?;
        let metadata = fs::metadata(&path).map_err(|err| {
            MemoryError::new(
                "search_index_incomplete",
                "metadata_error",
                format!("failed to read metadata for {}: {err}", path.display()),
            )
        })?;

        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > limits.max_file_bytes {
            return Err(MemoryError::new(
                "resource_limit_exceeded",
                "max_file_bytes_exceeded",
                format!("file exceeds memory search size limit: {}", path.display()),
            ));
        }
        if documents.len() >= limits.max_files {
            return Err(MemoryError::new(
                "resource_limit_exceeded",
                "max_files_exceeded",
                "memory search file count limit exceeded",
            ));
        }
        indexed_bytes = indexed_bytes.checked_add(metadata.len()).ok_or_else(|| {
            MemoryError::new(
                "resource_limit_exceeded",
                "max_total_bytes_exceeded",
                "memory search byte count overflowed",
            )
        })?;
        if indexed_bytes > limits.max_total_bytes {
            return Err(MemoryError::new(
                "resource_limit_exceeded",
                "max_total_bytes_exceeded",
                "memory search total byte limit exceeded",
            ));
        }

        let content = fs::read(&path).map_err(|err| {
            MemoryError::new(
                "search_index_incomplete",
                "read_error",
                format!("failed to read {}: {err}", path.display()),
            )
        })?;
        if content_contains_nul(&content) {
            if require_utf8_scope {
                return Err(MemoryError::new(
                    "search_index_incomplete",
                    "fuzzy_scope_not_utf8",
                    format!(
                        "memory fuzzy search requires non-binary UTF-8 text in {}",
                        path.display()
                    ),
                ));
            }
            return Err(MemoryError::new(
                "search_index_incomplete",
                "binary_file_in_scope",
                format!(
                    "memory search cannot prove binary parity for {}",
                    path.display()
                ),
            ));
        }
        if std::str::from_utf8(&content).is_err() {
            all_content_utf8 = false;
            if require_utf8_scope {
                return Err(MemoryError::new(
                    "search_index_incomplete",
                    "fuzzy_scope_not_utf8",
                    format!(
                        "memory fuzzy search requires valid UTF-8 text in {}",
                        path.display()
                    ),
                ));
            }
        }

        let stamp = file_stamp_from_parts(&metadata, &content);
        let rendered_path = render_path(req.root(), &path);
        documents.push(Document {
            path,
            rendered_path,
            stamp,
            lines: line_ranges(&content),
            content,
        });
    }

    documents.sort_by(|left, right| left.rendered_path.cmp(&right.rendered_path));

    let mut postings: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let mut ascii_folded_postings: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    for (doc_id, doc) in documents.iter().enumerate() {
        let trigrams = unique_trigrams(&doc.content);
        for trigram in trigrams {
            postings.entry(trigram).or_default().push(doc_id);
        }

        let folded = ascii_folded_bytes(&doc.content);
        for trigram in unique_trigrams(&folded) {
            ascii_folded_postings
                .entry(trigram)
                .or_default()
                .push(doc_id);
        }
    }
    for docs in postings.values_mut() {
        docs.sort_unstable();
    }
    for docs in ascii_folded_postings.values_mut() {
        docs.sort_unstable();
    }

    Ok(IndexSnapshot {
        generation,
        documents,
        postings,
        ascii_folded_postings,
        indexed_bytes,
        all_content_utf8,
    })
}

fn discover_files(
    req: &SearchRequest,
    deadline: Option<Instant>,
) -> Result<Vec<PathBuf>, MemoryError> {
    let root = Path::new(req.root());
    let include_hidden = req.hidden.unwrap_or(false);
    let no_ignore = req.no_ignore.unwrap_or(false);
    let glob_filter = SearchGlobFilter::from_request(req, include_hidden)?;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .follow_links(false)
        .ignore(!no_ignore)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore);

    let mut files = Vec::new();
    for entry in builder.build() {
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        let entry = entry.map_err(|err| {
            MemoryError::new(
                "search_index_incomplete",
                "walk_error",
                format!("memory search walk failed: {err}"),
            )
        })?;
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
            continue;
        }
        let path = entry.into_path();
        if glob_filter
            .as_ref()
            .is_some_and(|filter| !filter.is_match(root, &path))
        {
            continue;
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

impl SearchGlobFilter {
    fn from_request(
        req: &SearchRequest,
        include_hidden: bool,
    ) -> Result<Option<Self>, MemoryError> {
        let Some(globs) = &req.glob else {
            return Ok(None);
        };

        let mut patterns = Vec::new();
        for raw_glob in globs {
            let trimmed = raw_glob.trim();
            if trimmed.is_empty() {
                continue;
            }
            if contains_unsupported_glob_syntax(trimmed) {
                return Err(MemoryError::new(
                    "unsupported_search_option",
                    "unsupported_glob_syntax",
                    format!(
                        "memory search cannot preserve Search glob semantics for pattern: {trimmed}"
                    ),
                ));
            }

            let pattern = Pattern::new(trimmed).map_err(|err| {
                MemoryError::new(
                    "unsupported_search_option",
                    "invalid_glob",
                    format!("memory search received invalid glob pattern {trimmed:?}: {err}"),
                )
            })?;
            patterns.push(CompiledGlob {
                pattern,
                match_basename: !contains_path_separator(trimmed),
            });
        }

        if patterns.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            patterns,
            match_options: MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: !include_hidden,
            },
        }))
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

fn candidates_for_plan(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    max_candidates: usize,
) -> Result<Vec<usize>, MemoryError> {
    match plan {
        QueryPlan::Exact { literal, case } => {
            candidates_for_literal(snapshot, literal, *case, max_candidates)
        }
        QueryPlan::Regex { candidates, .. } => {
            candidates_for_candidate_expr(snapshot, candidates, max_candidates)
        }
        QueryPlan::Fuzzy { seeds, .. } => {
            let mut candidate_set = BTreeSet::new();
            for seed in seeds {
                for doc_id in
                    candidates_for_literal(snapshot, seed, LiteralCase::Sensitive, max_candidates)?
                {
                    candidate_set.insert(doc_id);
                }
            }

            if candidate_set.len() > max_candidates {
                return Err(MemoryError::new(
                    "resource_limit_exceeded",
                    "max_candidates_exceeded",
                    "memory search candidate limit exceeded",
                ));
            }

            let mut candidates: Vec<usize> = candidate_set.into_iter().collect();
            candidates.sort_by(|left, right| {
                snapshot.documents[*left]
                    .rendered_path
                    .cmp(&snapshot.documents[*right].rendered_path)
            });
            Ok(candidates)
        }
    }
}

fn candidates_for_candidate_expr(
    snapshot: &IndexSnapshot,
    expr: &CandidateExpr,
    max_candidates: usize,
) -> Result<Vec<usize>, MemoryError> {
    let mut candidates = match expr {
        CandidateExpr::Seed(seed) => {
            candidates_for_literal(snapshot, seed, LiteralCase::Sensitive, max_candidates)?
        }
        CandidateExpr::And(children) => {
            let mut child_sets = Vec::with_capacity(children.len());
            for child in children {
                child_sets.push(candidates_for_candidate_expr(
                    snapshot,
                    child,
                    max_candidates,
                )?);
            }
            intersect_candidate_sets(child_sets)
        }
        CandidateExpr::Or(children) => {
            let mut candidate_set = BTreeSet::new();
            for child in children {
                candidate_set.extend(candidates_for_candidate_expr(
                    snapshot,
                    child,
                    max_candidates,
                )?);
                if candidate_set.len() > max_candidates {
                    return Err(MemoryError::new(
                        "resource_limit_exceeded",
                        "max_candidates_exceeded",
                        "memory search candidate limit exceeded",
                    ));
                }
            }
            candidate_set.into_iter().collect()
        }
    };

    if candidates.len() > max_candidates {
        return Err(MemoryError::new(
            "resource_limit_exceeded",
            "max_candidates_exceeded",
            "memory search candidate limit exceeded",
        ));
    }
    candidates.sort_by(|left, right| {
        snapshot.documents[*left]
            .rendered_path
            .cmp(&snapshot.documents[*right].rendered_path)
    });
    Ok(candidates)
}

fn intersect_candidate_sets(mut child_sets: Vec<Vec<usize>>) -> Vec<usize> {
    if child_sets.is_empty() {
        return Vec::new();
    }
    child_sets.sort_by_key(Vec::len);
    let mut result: BTreeSet<usize> = child_sets.remove(0).into_iter().collect();
    for child in child_sets {
        let child: BTreeSet<usize> = child.into_iter().collect();
        result.retain(|doc_id| child.contains(doc_id));
        if result.is_empty() {
            break;
        }
    }
    result.into_iter().collect()
}

fn candidates_for_literal(
    snapshot: &IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    max_candidates: usize,
) -> Result<Vec<usize>, MemoryError> {
    let folded_literal;
    let (indexed_literal, index_postings) = match case {
        LiteralCase::Sensitive => (literal, &snapshot.postings),
        LiteralCase::AsciiInsensitive => {
            folded_literal = ascii_folded_bytes(literal);
            (folded_literal.as_slice(), &snapshot.ascii_folded_postings)
        }
    };
    let trigrams = literal_trigrams(indexed_literal);
    let mut posting_lists: Vec<&Vec<usize>> = Vec::with_capacity(trigrams.len());
    for trigram in trigrams {
        let Some(postings) = index_postings.get(&trigram) else {
            return Ok(Vec::new());
        };
        posting_lists.push(postings);
    }
    posting_lists.sort_by_key(|postings| postings.len());
    let mut candidates = intersect_postings(&posting_lists);
    candidates.sort_by(|left, right| {
        snapshot.documents[*left]
            .rendered_path
            .cmp(&snapshot.documents[*right].rendered_path)
    });
    if candidates.len() > max_candidates {
        return Err(MemoryError::new(
            "resource_limit_exceeded",
            "max_candidates_exceeded",
            "memory search candidate limit exceeded",
        ));
    }
    Ok(candidates)
}

fn verify_and_render(
    snapshot: &IndexSnapshot,
    candidates: &[usize],
    plan: &QueryPlan,
    req: &SearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<(Vec<SearchEvent>, Vec<String>, bool, usize), MemoryError> {
    let mut events = Vec::new();
    let mut rendered_lines = Vec::new();
    let mut truncated = false;
    let mut fuzzy_verified_lines = 0;
    let max_results = req.max_results();
    let context = req.context.unwrap_or(0);

    'docs: for &doc_id in candidates {
        check_deadline(deadline)?;
        let doc = &snapshot.documents[doc_id];
        let matched_lines =
            matching_line_indexes(doc, plan, limits, deadline, &mut fuzzy_verified_lines)?;
        if matched_lines.is_empty() {
            continue;
        }

        let rendered_indexes = render_line_indexes(&matched_lines, doc.lines.len(), context);
        for line_index in rendered_indexes {
            check_deadline(deadline)?;
            let is_match = matched_lines.contains(&line_index);
            let line_number = (line_index + 1) as u64;
            let text = line_text(doc, line_index);
            let sep = if is_match { ":" } else { "-" };
            rendered_lines.push(format!(
                "{}{sep}{line_number}{sep}{text}",
                doc.rendered_path
            ));
            events.push(SearchEvent {
                is_match,
                path: doc.rendered_path.clone(),
                line_number,
                text,
            });

            if events.len() >= max_results {
                truncated = true;
                break 'docs;
            }
        }
    }

    Ok((events, rendered_lines, truncated, fuzzy_verified_lines))
}

fn check_snapshot_fresh(
    req: &SearchRequest,
    snapshot: &IndexSnapshot,
    deadline: Instant,
) -> Result<(), MemoryError> {
    let current_paths = discover_files(req, Some(deadline))?;
    let expected_paths: BTreeSet<PathBuf> =
        snapshot.documents.iter().map(|d| d.path.clone()).collect();
    let observed_paths: BTreeSet<PathBuf> = current_paths.into_iter().collect();
    if expected_paths != observed_paths {
        return Err(MemoryError::new(
            "file_changed_during_verification",
            "file_set_changed",
            "file set changed during memory search verification",
        ));
    }

    for doc in &snapshot.documents {
        check_deadline(deadline)?;
        let metadata = fs::metadata(&doc.path).map_err(|err| {
            MemoryError::new(
                "file_changed_during_verification",
                "file_changed_during_verification",
                format!(
                    "failed to re-read metadata for {}: {err}",
                    doc.path.display()
                ),
            )
        })?;
        if file_metadata_matches_without_hash(&doc.stamp, &metadata) {
            continue;
        }

        let content = fs::read(&doc.path).map_err(|err| {
            MemoryError::new(
                "file_changed_during_verification",
                "file_changed_during_verification",
                format!("failed to re-read {}: {err}", doc.path.display()),
            )
        })?;
        if !file_stamp_content_matches(&doc.stamp, &file_stamp_from_parts(&metadata, &content)) {
            return Err(MemoryError::new(
                "file_changed_during_verification",
                "file_changed_during_verification",
                format!("file changed during memory search: {}", doc.path.display()),
            ));
        }
    }

    Ok(())
}

fn literal_trigrams(bytes: &[u8]) -> Vec<[u8; 3]> {
    let mut trigrams = Vec::new();
    let mut seen = HashSet::new();
    for window in bytes.windows(3) {
        let trigram = [window[0], window[1], window[2]];
        if seen.insert(trigram) {
            trigrams.push(trigram);
        }
    }
    trigrams
}

fn ascii_folded_bytes(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_lowercase).collect()
}

fn is_plain_regex_literal(pattern: &str) -> bool {
    !pattern.chars().any(|ch| {
        matches!(
            ch,
            '\\' | '.' | '^' | '$' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
}

fn contains_uppercase_letter(pattern: &str) -> bool {
    pattern.chars().any(char::is_uppercase)
}

fn hir_can_match_lf(hir: &Hir) -> bool {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(literal) => literal.0.contains(&b'\n'),
        HirKind::Class(class) => class_can_match_lf(class),
        HirKind::Capture(capture) => hir_can_match_lf(capture.sub.as_ref()),
        HirKind::Repetition(repetition) => hir_can_match_lf(repetition.sub.as_ref()),
        HirKind::Concat(parts) | HirKind::Alternation(parts) => parts.iter().any(hir_can_match_lf),
    }
}

fn class_can_match_lf(class: &Class) -> bool {
    match class {
        Class::Unicode(class) => class
            .ranges()
            .iter()
            .any(|range| range.start() <= '\n' && '\n' <= range.end()),
        Class::Bytes(class) => class
            .ranges()
            .iter()
            .any(|range| range.start() <= b'\n' && b'\n' <= range.end()),
    }
}

fn required_candidate_expr(hir: &Hir, budget: &mut usize) -> Option<CandidateExpr> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    match hir.kind() {
        HirKind::Empty | HirKind::Class(_) | HirKind::Look(_) => None,
        HirKind::Literal(literal) => candidate_seed(literal.0.as_ref()),
        HirKind::Capture(capture) => required_candidate_expr(capture.sub.as_ref(), budget),
        HirKind::Repetition(repetition) => {
            if repetition.min == 0 {
                None
            } else {
                required_candidate_expr(repetition.sub.as_ref(), budget)
            }
        }
        HirKind::Concat(parts) => candidate_and(
            parts
                .iter()
                .filter_map(|part| required_candidate_expr(part, budget)),
        ),
        HirKind::Alternation(parts) => {
            let mut alternatives = Vec::with_capacity(parts.len());
            for part in parts {
                alternatives.push(required_candidate_expr(part, budget)?);
            }
            candidate_or(alternatives)
        }
    }
}

fn candidate_seed(bytes: &[u8]) -> Option<CandidateExpr> {
    if bytes.len() >= 3 && !bytes.contains(&b'\n') && !bytes.contains(&b'\r') {
        Some(CandidateExpr::Seed(bytes.to_vec()))
    } else {
        None
    }
}

fn candidate_and<I>(exprs: I) -> Option<CandidateExpr>
where
    I: IntoIterator<Item = CandidateExpr>,
{
    let mut combined = Vec::new();
    for expr in exprs {
        match expr {
            CandidateExpr::And(children) => combined.extend(children),
            expr => combined.push(expr),
        }
    }
    candidate_expr_from_many(combined, CandidateExpr::And)
}

fn candidate_or<I>(exprs: I) -> Option<CandidateExpr>
where
    I: IntoIterator<Item = CandidateExpr>,
{
    let mut combined = Vec::new();
    for expr in exprs {
        match expr {
            CandidateExpr::Or(children) => combined.extend(children),
            expr => combined.push(expr),
        }
    }
    candidate_expr_from_many(combined, CandidateExpr::Or)
}

fn candidate_expr_from_many(
    mut exprs: Vec<CandidateExpr>,
    wrap: impl FnOnce(Vec<CandidateExpr>) -> CandidateExpr,
) -> Option<CandidateExpr> {
    match exprs.len() {
        0 => None,
        1 => exprs.pop(),
        _ => Some(wrap(exprs)),
    }
}

fn unique_trigrams(bytes: &[u8]) -> HashSet<[u8; 3]> {
    literal_trigrams(bytes).into_iter().collect()
}

fn intersect_postings(posting_lists: &[&Vec<usize>]) -> Vec<usize> {
    if posting_lists.is_empty() {
        return Vec::new();
    }

    let mut result = posting_lists[0].clone();
    for postings in &posting_lists[1..] {
        let mut next = Vec::new();
        let mut left = 0;
        let mut right = 0;
        while left < result.len() && right < postings.len() {
            match result[left].cmp(&postings[right]) {
                std::cmp::Ordering::Equal => {
                    next.push(result[left]);
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Greater => right += 1,
            }
        }
        result = next;
        if result.is_empty() {
            break;
        }
    }
    result
}

fn matching_line_indexes(
    doc: &Document,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
    fuzzy_verified_lines: &mut usize,
) -> Result<BTreeSet<usize>, MemoryError> {
    let mut matched = BTreeSet::new();
    for (line_index, range) in doc.lines.iter().enumerate() {
        check_deadline(deadline)?;
        let line = &doc.content[range.start..range.end];
        let is_match = match plan {
            QueryPlan::Exact { literal, case } => match case {
                LiteralCase::Sensitive => contains_subslice(line, literal),
                LiteralCase::AsciiInsensitive => {
                    contains_subslice_ascii_case_insensitive(line, literal)
                }
            },
            QueryPlan::Regex { matcher, .. } => matcher.is_match(line),
            QueryPlan::Fuzzy {
                pattern_chars,
                distance,
                ..
            } => {
                *fuzzy_verified_lines = fuzzy_verified_lines.checked_add(1).ok_or_else(|| {
                    MemoryError::new(
                        "resource_limit_exceeded",
                        "max_fuzzy_verified_lines_exceeded",
                        "fuzzy verifier line count overflowed",
                    )
                })?;
                if *fuzzy_verified_lines > limits.max_fuzzy_verified_lines {
                    return Err(MemoryError::new(
                        "resource_limit_exceeded",
                        "max_fuzzy_verified_lines_exceeded",
                        "fuzzy verifier line limit exceeded",
                    ));
                }
                let line = std::str::from_utf8(line).map_err(|_| {
                    MemoryError::new(
                        "search_index_incomplete",
                        "fuzzy_scope_not_utf8",
                        "memory fuzzy search requires valid UTF-8 lines",
                    )
                })?;
                if line.chars().count() > limits.max_fuzzy_line_chars {
                    return Err(MemoryError::new(
                        "resource_limit_exceeded",
                        "max_fuzzy_line_chars_exceeded",
                        "fuzzy verifier line length limit exceeded",
                    ));
                }
                fuzzy_line_matches(line, pattern_chars, *distance)
            }
        };
        if is_match {
            matched.insert(line_index);
        }
    }
    Ok(matched)
}

fn fuzzy_seed_segments(pattern: &str, distance: u8) -> Option<Vec<Vec<u8>>> {
    let segment_count = usize::from(distance) + 1;
    let scalar_count = pattern.chars().count();
    if scalar_count < segment_count {
        return None;
    }

    let mut byte_offsets: Vec<usize> = pattern.char_indices().map(|(offset, _)| offset).collect();
    byte_offsets.push(pattern.len());

    let mut ranges = Vec::with_capacity(segment_count);
    partition_seed_ranges(&byte_offsets, 0, segment_count, &mut ranges).then(|| {
        ranges
            .into_iter()
            .map(|(start, end)| pattern.as_bytes()[byte_offsets[start]..byte_offsets[end]].to_vec())
            .collect()
    })
}

fn partition_seed_ranges(
    byte_offsets: &[usize],
    start_scalar: usize,
    remaining_segments: usize,
    ranges: &mut Vec<(usize, usize)>,
) -> bool {
    if remaining_segments == 1 {
        let end_scalar = byte_offsets.len() - 1;
        if seed_byte_len(byte_offsets, start_scalar, end_scalar) >= 3 {
            ranges.push((start_scalar, end_scalar));
            return true;
        }
        return false;
    }

    let max_end = (byte_offsets.len() - 1).saturating_sub(remaining_segments - 1);
    for end_scalar in start_scalar + 1..=max_end {
        let segment_bytes = seed_byte_len(byte_offsets, start_scalar, end_scalar);
        let remaining_bytes = byte_offsets[byte_offsets.len() - 1] - byte_offsets[end_scalar];
        if segment_bytes < 3 || remaining_bytes < 3 * (remaining_segments - 1) {
            continue;
        }

        ranges.push((start_scalar, end_scalar));
        if partition_seed_ranges(byte_offsets, end_scalar, remaining_segments - 1, ranges) {
            return true;
        }
        ranges.pop();
    }
    false
}

fn seed_byte_len(byte_offsets: &[usize], start_scalar: usize, end_scalar: usize) -> usize {
    byte_offsets[end_scalar] - byte_offsets[start_scalar]
}

fn fuzzy_line_matches(line: &str, pattern_chars: &[char], distance: usize) -> bool {
    if pattern_chars.is_empty() {
        return false;
    }

    let line_chars: Vec<char> = line.chars().collect();
    let min_len = pattern_chars.len().saturating_sub(distance);
    let max_len = pattern_chars.len().saturating_add(distance);

    for start in 0..=line_chars.len() {
        for len in min_len..=max_len {
            let end = start.saturating_add(len);
            if end > line_chars.len() {
                break;
            }
            if bounded_edit_distance(pattern_chars, &line_chars[start..end], distance).is_some() {
                return true;
            }
        }
    }
    false
}

fn bounded_edit_distance(left: &[char], right: &[char], max_distance: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > max_distance {
        return None;
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];

        for (right_index, right_char) in right.iter().enumerate() {
            let deletion = previous[right_index + 1] + 1;
            let insertion = current[right_index] + 1;
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            let best = deletion.min(insertion).min(substitution);
            current[right_index + 1] = best;
            row_min = row_min.min(best);
        }

        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    (previous[right.len()] <= max_distance).then_some(previous[right.len()])
}

fn render_line_indexes(
    matched_lines: &BTreeSet<usize>,
    line_count: usize,
    context: usize,
) -> Vec<usize> {
    let mut lines = BTreeSet::new();
    for &match_line in matched_lines {
        let start = match_line.saturating_sub(context);
        let end = match_line
            .saturating_add(context)
            .min(line_count.saturating_sub(1));
        for line in start..=end {
            lines.insert(line);
        }
    }
    lines.into_iter().collect()
}

fn line_ranges(content: &[u8]) -> Vec<LineRange> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            let mut end = index;
            if end > start && content[end - 1] == b'\r' {
                end -= 1;
            }
            ranges.push(LineRange { start, end });
            start = index + 1;
        }
    }
    if start < content.len() {
        let mut end = content.len();
        if end > start && content[end - 1] == b'\r' {
            end -= 1;
        }
        ranges.push(LineRange { start, end });
    }
    ranges
}

fn line_text(doc: &Document, line_index: usize) -> String {
    let range = doc.lines[line_index];
    String::from_utf8_lossy(&doc.content[range.start..range.end]).into_owned()
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn contains_subslice_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| bytes_eq_ignore_ascii_case(window, needle))
}

fn bytes_eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn content_contains_nul(content: &[u8]) -> bool {
    content.contains(&0)
}

fn file_stamp_from_parts(metadata: &fs::Metadata, content: &[u8]) -> FileStamp {
    let mut hasher = Sha256::new();
    hasher.update(content);
    FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        change_marker: metadata_change_marker(metadata),
        hash: hasher.finalize().into(),
    }
}

fn file_metadata_matches_without_hash(stamp: &FileStamp, metadata: &fs::Metadata) -> bool {
    stamp.len == metadata.len()
        && stamp.modified == metadata.modified().ok()
        && stamp.change_marker.is_some()
        && stamp.change_marker == metadata_change_marker(metadata)
}

fn file_stamp_content_matches(expected: &FileStamp, actual: &FileStamp) -> bool {
    expected.len == actual.len
        && expected.modified == actual.modified
        && expected.hash == actual.hash
}

#[cfg(unix)]
fn metadata_change_marker(metadata: &fs::Metadata) -> Option<MetadataChangeMarker> {
    use std::os::unix::fs::MetadataExt;

    Some((metadata.ctime(), metadata.ctime_nsec()))
}

#[cfg(not(unix))]
fn metadata_change_marker(_metadata: &fs::Metadata) -> Option<MetadataChangeMarker> {
    None
}

fn render_path(root: &str, path: &Path) -> String {
    if matches!(root, "." | "./")
        && let Ok(stripped) = path.strip_prefix(".")
    {
        return stripped.display().to_string();
    }
    path.display().to_string()
}

fn check_deadline(deadline: Instant) -> Result<(), MemoryError> {
    if Instant::now() >= deadline {
        Err(MemoryError::timeout())
    } else {
        Ok(())
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fixed_string_trigram_extraction_deduplicates_in_order() {
        assert_eq!(
            literal_trigrams(b"ababa"),
            vec![[b'a', b'b', b'a'], [b'b', b'a', b'b']]
        );
    }

    #[test]
    fn candidate_intersection_uses_all_postings() {
        let first = vec![1, 2, 4, 7];
        let second = vec![2, 3, 4, 8];
        let third = vec![0, 2, 4, 9];
        assert_eq!(intersect_postings(&[&first, &second, &third]), vec![2, 4]);
    }

    #[test]
    fn line_rendering_deduplicates_context() {
        let doc = Document {
            path: PathBuf::from("sample.txt"),
            rendered_path: "sample.txt".to_string(),
            stamp: FileStamp {
                len: 0,
                modified: None,
                change_marker: None,
                hash: [0; 32],
            },
            content: b"alpha\nneedle one\nmiddle\nneedle two\nomega\n".to_vec(),
            lines: line_ranges(b"alpha\nneedle one\nmiddle\nneedle two\nomega\n"),
        };
        let mut fuzzy_verified_lines = 0;
        let matched = matching_line_indexes(
            &doc,
            &QueryPlan::Exact {
                literal: b"needle".to_vec(),
                case: LiteralCase::Sensitive,
            },
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            &mut fuzzy_verified_lines,
        )
        .expect("match lines");
        assert_eq!(
            render_line_indexes(&matched, doc.lines.len(), 1),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn memory_error_payload_is_structured() {
        let req = SearchRequest {
            pattern: "ab".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("short literal should be ineligible");
        let outcome = error.into_tool_outcome(&req);
        assert_eq!(outcome.0["isError"], true);
        assert_eq!(outcome.0["backend"], "memory");
        assert_eq!(outcome.0["error_type"], "unsupported_search_option");
        assert_eq!(outcome.0["exit_code"], Value::Null);
        assert_eq!(outcome.0["timed_out"], false);
    }

    #[test]
    fn seeded_regex_queries_are_memory_eligible() {
        let req = SearchRequest {
            pattern: "nee.*dle".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan = eligible_query_plan(&req).expect("seeded regex should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::And(vec![
                        CandidateExpr::Seed(b"nee".to_vec()),
                        CandidateExpr::Seed(b"dle".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. } | QueryPlan::Fuzzy { .. } => {
                panic!("expected regex plan")
            }
        }
    }

    #[test]
    fn seeded_regex_alternation_unions_fully_seeded_branches() {
        let req = SearchRequest {
            pattern: "needle|haystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan = eligible_query_plan(&req).expect("seeded alternation should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::Or(vec![
                        CandidateExpr::Seed(b"needle".to_vec()),
                        CandidateExpr::Seed(b"haystack".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. } | QueryPlan::Fuzzy { .. } => {
                panic!("expected regex plan")
            }
        }
    }

    #[test]
    fn unseeded_regex_queries_fall_back() {
        let req = SearchRequest {
            pattern: "^[0-9]+$".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("unseeded regex should fall back");
        assert_eq!(error.error_type, "unsupported_regex_dialect");
        assert_eq!(error.fallback_reason, "query_without_required_trigram");
    }

    #[test]
    fn regex_common_escapes_with_required_literals_are_memory_eligible() {
        let req = SearchRequest {
            pattern: "needle\\d+haystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan = eligible_query_plan(&req).expect("common escaped class should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::And(vec![
                        CandidateExpr::Seed(b"needle".to_vec()),
                        CandidateExpr::Seed(b"haystack".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. } | QueryPlan::Fuzzy { .. } => {
                panic!("expected regex plan")
            }
        }
    }

    #[test]
    fn regex_lazy_quantifier_syntax_is_memory_eligible() {
        let req = SearchRequest {
            pattern: "needle.*?haystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan = eligible_query_plan(&req).expect("lazy quantifier should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::And(vec![
                        CandidateExpr::Seed(b"needle".to_vec()),
                        CandidateExpr::Seed(b"haystack".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. } | QueryPlan::Fuzzy { .. } => {
                panic!("expected regex plan")
            }
        }
    }

    #[test]
    fn unsupported_inline_regex_constructs_fall_back() {
        let req = SearchRequest {
            pattern: "needle(?=haystack)".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("inline construct should fall back");
        assert_eq!(error.error_type, "unsupported_regex_dialect");
        assert_eq!(error.fallback_reason, "unsupported_regex_backend");
    }

    #[test]
    fn multiline_regex_constructs_fall_back() {
        let req = SearchRequest {
            pattern: "needle\\nhaystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("line break escape should fall back");
        assert_eq!(error.error_type, "unsupported_regex_dialect");
        assert_eq!(error.fallback_reason, "unsupported_multiline_regex");
    }

    #[test]
    fn regex_classes_that_can_match_lf_fall_back() {
        let req = SearchRequest {
            pattern: "needle\\D+haystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("LF-capable class should fall back");
        assert_eq!(error.error_type, "unsupported_regex_dialect");
        assert_eq!(error.fallback_reason, "unsupported_multiline_regex");
    }

    #[test]
    fn case_insensitive_unicode_regex_still_falls_back() {
        let req = SearchRequest {
            pattern: "straße.*needle".to_string(),
            path: Some(".".to_string()),
            case: Some("insensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error =
            eligible_query_plan(&req).expect_err("Unicode case-insensitive regex should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(error.fallback_reason, "unsupported_regex_case_insensitive");
    }

    #[test]
    fn word_regex_and_follow_symlink_requests_fall_back() {
        let mut req = SearchRequest {
            pattern: "needle.*haystack".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(false),
            word_regexp: Some(true),
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("word regex should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(error.fallback_reason, "unsupported_word_regexp");

        req.word_regexp = None;
        req.follow = Some(true);
        let error = eligible_query_plan(&req).expect_err("follow symlink search should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(error.fallback_reason, "unsupported_follow");
    }

    #[test]
    fn smart_case_lowercase_regex_falls_back() {
        let req = SearchRequest {
            pattern: "needle.*haystack".to_string(),
            path: Some(".".to_string()),
            case: None,
            fixed_strings: Some(false),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let error = eligible_query_plan(&req).expect_err("smart lowercase regex should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(
            error.fallback_reason,
            "unsupported_regex_smart_case_insensitive"
        );
    }

    #[test]
    fn plain_regex_literal_with_default_smart_case_is_memory_eligible() {
        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some(".".to_string()),
            case: None,
            fixed_strings: None,
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan = eligible_query_plan(&req).expect("plain literal regex should be eligible");
        match plan {
            QueryPlan::Exact { literal, case } => {
                assert_eq!(literal, b"needle");
                assert_eq!(case, LiteralCase::AsciiInsensitive);
            }
            QueryPlan::Regex { .. } | QueryPlan::Fuzzy { .. } => panic!("expected exact plan"),
        }
    }

    #[test]
    fn smart_case_with_uppercase_literal_stays_case_sensitive() {
        let req = SearchRequest {
            pattern: "Needle".to_string(),
            path: Some(".".to_string()),
            case: None,
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let plan =
            eligible_query_plan(&req).expect("uppercase smart fixed string should be eligible");
        match plan {
            QueryPlan::Exact { literal, case } => {
                assert_eq!(literal, b"Needle");
                assert_eq!(case, LiteralCase::Sensitive);
            }
            QueryPlan::Regex { .. } | QueryPlan::Fuzzy { .. } => panic!("expected exact plan"),
        }
    }

    #[test]
    fn ascii_case_insensitive_candidates_and_verification_find_uppercase_content() {
        let root =
            workspace_test_dir("ascii_case_insensitive_candidates_and_verification_find_uppercase");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("upper.txt"), "prefix NEEDLE suffix\n").expect("write uppercase");

        let mut req = memory_req(&root);
        req.pattern = "needle".to_string();
        req.case = Some("insensitive".to_string());

        let plan = eligible_query_plan(&req).expect("case-insensitive fixed string plan");
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, false, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, limits.max_candidates).expect("candidates");
        assert_eq!(candidates.len(), 1);

        let (events, _, _, _) = verify_and_render(
            &snapshot,
            &candidates,
            &plan,
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("verify");
        assert_eq!(events.iter().filter(|event| event.is_match).count(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn seeded_regex_candidates_and_verification_eliminate_false_positives() {
        let root = workspace_test_dir("seeded_regex_candidates_and_verification");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("match.txt"), "prefix nee123dle suffix\n").expect("write match");
        fs::write(root.join("false-positive.txt"), "nee here\nseparate dle\n")
            .expect("write false positive");
        fs::write(root.join("miss.txt"), "nee only\n").expect("write miss");

        let mut req = memory_req(&root);
        req.pattern = "nee.*dle".to_string();
        req.fixed_strings = Some(false);

        let plan = eligible_query_plan(&req).expect("seeded regex plan");
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, true, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, limits.max_candidates).expect("candidates");
        let candidate_names: BTreeSet<String> = candidates
            .iter()
            .map(|doc_id| {
                snapshot.documents[*doc_id]
                    .path
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(candidate_names.contains("match.txt"));
        assert!(candidate_names.contains("false-positive.txt"));
        assert!(!candidate_names.contains("miss.txt"));

        let (events, _, _, _) = verify_and_render(
            &snapshot,
            &candidates,
            &plan,
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("verify");
        let matched_names: BTreeSet<String> = events
            .iter()
            .filter(|event| event.is_match)
            .map(|event| {
                Path::new(&event.path)
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(matched_names, BTreeSet::from(["match.txt".to_string()]));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_seed_partitioning_requires_searchable_unicode_segments() {
        assert_eq!(
            fuzzy_seed_segments("abcdef", 1).expect("seedable ascii"),
            vec![b"abc".to_vec(), b"def".to_vec()]
        );
        assert_eq!(
            fuzzy_seed_segments("abcdefghi", 2).expect("seedable ascii"),
            vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
        );
        assert!(fuzzy_seed_segments("ééé", 1).is_none());
    }

    #[test]
    fn fuzzy_verifier_accepts_insertion_deletion_and_substitution() {
        let pattern: Vec<char> = "abcdef".chars().collect();

        assert!(fuzzy_line_matches("prefix abcXdef suffix", &pattern, 1));
        assert!(fuzzy_line_matches("prefix abdef suffix", &pattern, 1));
        assert!(fuzzy_line_matches("prefix abcxef suffix", &pattern, 1));
        assert!(!fuzzy_line_matches("prefix abXYef suffix", &pattern, 1));
    }

    #[test]
    fn fuzzy_candidates_keep_all_one_edit_matches() {
        let root = workspace_test_dir("fuzzy_candidates_keep_all_one_edit_matches");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("exact.txt"), "abcdef\n").expect("write exact");
        fs::write(root.join("insertion.txt"), "abcXdef\n").expect("write insertion");
        fs::write(root.join("deletion.txt"), "abdef\n").expect("write deletion");
        fs::write(root.join("substitution.txt"), "abcxef\n").expect("write substitution");
        fs::write(root.join("miss.txt"), "abXYef\n").expect("write miss");

        let mut req = memory_req(&root);
        req.pattern = "abcdef".to_string();
        req.fuzzy = Some(1);

        let plan = eligible_query_plan(&req).expect("eligible fuzzy plan");
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, true, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, limits.max_candidates).expect("candidates");
        let candidate_names: BTreeSet<String> = candidates
            .iter()
            .map(|doc_id| {
                snapshot.documents[*doc_id]
                    .path
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(candidate_names.contains("exact.txt"));
        assert!(candidate_names.contains("insertion.txt"));
        assert!(candidate_names.contains("deletion.txt"));
        assert!(candidate_names.contains("substitution.txt"));
        assert!(!candidate_names.contains("miss.txt"));

        let (events, _, _, fuzzy_verified_lines) = verify_and_render(
            &snapshot,
            &candidates,
            &plan,
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("verify");
        let matched_names: BTreeSet<String> = events
            .iter()
            .filter(|event| event.is_match)
            .map(|event| {
                Path::new(&event.path)
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(fuzzy_verified_lines, 4);
        assert_eq!(matched_names, candidate_names);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_ineligible_queries_use_specific_fallback_reasons() {
        let mut req = fuzzy_req("abcdef", 1);
        req.fixed_strings = Some(false);
        let err = eligible_query_plan(&req).expect_err("regex fuzzy should fall back");
        assert_eq!(err.fallback_reason, "unsupported_regex_fuzzy");

        let mut req = fuzzy_req("abcdef", 1);
        req.case = Some("insensitive".to_string());
        let err = eligible_query_plan(&req).expect_err("case fuzzy should fall back");
        assert_eq!(err.fallback_reason, "unsupported_case_fuzzy");

        let mut req = fuzzy_req("abcdef", 1);
        req.word_regexp = Some(true);
        let err = eligible_query_plan(&req).expect_err("word fuzzy should fall back");
        assert_eq!(err.fallback_reason, "unsupported_word_fuzzy");

        let req = fuzzy_req("abc\ndef", 1);
        let err = eligible_query_plan(&req).expect_err("multiline fuzzy should fall back");
        assert_eq!(err.fallback_reason, "unsupported_multiline_fuzzy");

        let req = fuzzy_req("abcdef", 0);
        let err = eligible_query_plan(&req).expect_err("unsupported distance should fall back");
        assert_eq!(err.fallback_reason, "unsupported_fuzzy_mode");
    }

    #[test]
    fn fuzzy_too_short_unseedable_and_invalid_scope_fall_back() {
        let err = eligible_query_plan(&fuzzy_req("abcde", 1)).expect_err("too short");
        assert_eq!(err.fallback_reason, "fuzzy_pattern_too_short");

        let err = eligible_query_plan(&fuzzy_req("ééé", 1)).expect_err("unseedable");
        assert_eq!(err.fallback_reason, "fuzzy_pattern_unseedable");

        let root = workspace_test_dir("fuzzy_invalid_utf8_scope_falls_back");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("invalid.txt"), [0x66, 0x80, 0x6f]).expect("write invalid utf8");

        let mut req = memory_req(&root);
        req.pattern = "abcdef".to_string();
        req.fuzzy = Some(1);
        let err = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            true,
            1,
        )
        .expect_err("invalid UTF-8 scope should fall back");
        assert_eq!(err.fallback_reason, "fuzzy_scope_not_utf8");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn glob_filter_includes_matching_file() {
        let root = workspace_test_dir("glob_filter_includes_matching_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src").join("lib.rs"), "needle in rust\n").expect("write rust");
        fs::write(root.join("notes.md"), "needle in markdown\n").expect("write markdown");

        let mut req = memory_req(&root);
        req.glob = Some(vec!["*.rs".to_string()]);

        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        assert_eq!(snapshot.documents.len(), 1);
        assert_eq!(snapshot.documents[0].path, root.join("src").join("lib.rs"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn glob_filter_excludes_non_matching_file() {
        let root = workspace_test_dir("glob_filter_excludes_non_matching_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src").join("lib.rs"), "needle in rust\n").expect("write rust");
        fs::write(root.join("notes.md"), "needle in markdown\n").expect("write markdown");

        let mut req = memory_req(&root);
        req.glob = Some(vec!["*.rs".to_string()]);

        let files = discover_files(&req, None).expect("discover files");

        assert!(files.contains(&root.join("src").join("lib.rs")));
        assert!(!files.contains(&root.join("notes.md")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_and_blank_globs_are_ignored() {
        let root = workspace_test_dir("empty_and_blank_globs_are_ignored");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("lib.rs"), "needle in rust\n").expect("write rust");
        fs::write(root.join("notes.md"), "needle in markdown\n").expect("write markdown");

        let mut req = memory_req(&root);
        req.glob = Some(vec!["".to_string(), "  ".to_string(), "\t".to_string()]);

        let files = discover_files(&req, None).expect("discover files");

        assert_eq!(files.len(), 2);
        assert!(files.contains(&root.join("lib.rs")));
        assert!(files.contains(&root.join("notes.md")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_glob_returns_fallback_allowed_error() {
        let root = workspace_test_dir("invalid_glob_returns_fallback_allowed_error");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");

        let mut req = memory_req(&root);
        req.glob = Some(vec!["[".to_string()]);

        let err = discover_files(&req, None).expect_err("invalid glob should fall back");

        assert_eq!(err.error_type, "unsupported_search_option");
        assert_eq!(err.fallback_reason, "invalid_glob");
        assert!(err.fallback_allowed);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn freshness_check_detects_modified_file() {
        let root = workspace_test_dir("freshness_check_detects_modified_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some(root.display().to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: Some(true),
            follow: None,
            no_ignore: Some(true),
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let limits = test_limits();
        let snapshot = build_index(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file_path)
            .expect("open for rewrite");
        file.write_all(b"changed\n").expect("rewrite");
        file.sync_all().expect("sync");

        let err = check_snapshot_fresh(&req, &snapshot, Instant::now() + Duration::from_secs(30))
            .expect_err("freshness should fail");
        assert_eq!(err.error_type, "file_changed_during_verification");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_metadata_fast_path_skips_hash_verification() {
        let root = workspace_test_dir("unchanged_metadata_fast_path_skips_hash_verification");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let metadata = fs::metadata(&file_path).expect("metadata");
        let content = fs::read(&file_path).expect("read");
        let stamp = file_stamp_from_parts(&metadata, &content);
        let metadata = fs::metadata(&file_path).expect("metadata again");

        assert!(file_metadata_matches_without_hash(&stamp, &metadata));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_index_cache_reuses_snapshot_for_same_file_selection() {
        let root = workspace_test_dir("search_index_cache_reuses_snapshot_for_same_file_selection");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = memory_req(&root);
        let limits = test_limits();
        let first = get_or_build_snapshot(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
            false,
        )
        .expect("first snapshot");
        let second = get_or_build_snapshot(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
            false,
        )
        .expect("second snapshot");

        assert_eq!(first.cache_status, "miss");
        assert_eq!(second.cache_status, "hit");
        assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn git_worktree_root_from_detects_parent_git_directory() {
        let root = workspace_test_dir("git_worktree_root_from_detects_parent_git_directory");
        let nested = root.join("nested").join("child");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).expect("create git marker directory");
        fs::create_dir_all(&nested).expect("create nested worktree directory");

        let repo_root = git_worktree_root_from(&nested).expect("git repo root");

        assert_eq!(repo_root, fs::canonicalize(&root).expect("canonical root"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn git_worktree_root_from_detects_gitdir_file_marker() {
        let root = workspace_test_dir("git_worktree_root_from_detects_gitdir_file_marker");
        let nested = root.join("nested");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).expect("create nested worktree directory");
        fs::write(root.join(".git"), "gitdir: ../actual-git-dir\n").expect("write gitdir marker");

        let repo_root = git_worktree_root_from(&nested).expect("git repo root");

        assert_eq!(repo_root, fs::canonicalize(&root).expect("canonical root"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn has_git_worktree_marker_ignores_non_git_file_marker() {
        let root = workspace_test_dir("has_git_worktree_marker_ignores_non_git_file_marker");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create directory");
        fs::write(root.join(".git"), "not a gitdir marker\n").expect("write non-git marker");

        assert!(!has_git_worktree_marker(&root));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_gitdir_file_marker_accepts_bom_whitespace_and_non_utf8_path() {
        assert!(is_gitdir_file_marker(
            b"\xef\xbb\xbf  gitdir: ../actual-git-dir\n"
        ));
        assert!(is_gitdir_file_marker(b"gitdir: \xff\n"));
        assert!(!is_gitdir_file_marker(b"not gitdir: ../actual-git-dir\n"));
    }

    #[test]
    fn git_repo_warm_cache_populates_search_index_cache() {
        if !command_available(git_bin()) {
            eprintln!("Skipping warm cache git test: git not found on PATH");
            return;
        }

        let root = workspace_test_dir("git_repo_warm_cache_populates_search_index_cache");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("cached.txt"), "cachedneedle\n").expect("write fixture");

        let init_status = std::process::Command::new(git_bin())
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .expect("git init should start");
        assert!(init_status.success(), "git init failed");

        let repo_root = git_worktree_root_from(&root).expect("git repo root");
        let summary =
            warm_cache_for_root(repo_root.display().to_string()).expect("warm cache should build");

        let req = SearchRequest {
            pattern: "cachedneedle".to_string(),
            path: Some(repo_root.display().to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: Some(false),
            follow: None,
            no_ignore: Some(false),
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        };
        let cached = get_or_build_snapshot(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
        )
        .expect("query should use warmed cache");

        assert_eq!(cached.cache_status, "hit");
        assert_eq!(cached.snapshot.generation, summary.generation);
        assert!(cached.snapshot.documents.iter().any(|doc| {
            doc.path
                .file_name()
                .is_some_and(|name| name == "cached.txt")
        }));

        let _ = fs::remove_dir_all(&root);
    }

    fn command_available(bin: &str) -> bool {
        std::process::Command::new(bin)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn memory_req(root: &Path) -> SearchRequest {
        SearchRequest {
            pattern: "needle".to_string(),
            path: Some(root.display().to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: Some(true),
            follow: None,
            no_ignore: Some(true),
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        }
    }

    fn fuzzy_req(pattern: &str, distance: u8) -> SearchRequest {
        SearchRequest {
            pattern: pattern.to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: Some(true),
            follow: None,
            no_ignore: Some(true),
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: Some(distance),
        }
    }

    fn test_limits() -> Limits {
        Limits {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_files: DEFAULT_MAX_FILES,
            max_candidates: DEFAULT_MAX_CANDIDATES,
            max_fuzzy_pattern_chars: DEFAULT_MAX_FUZZY_PATTERN_CHARS,
            max_fuzzy_verified_lines: DEFAULT_MAX_FUZZY_VERIFIED_LINES,
            max_fuzzy_line_chars: DEFAULT_MAX_FUZZY_LINE_CHARS,
            regex_size_limit_bytes: DEFAULT_REGEX_SIZE_LIMIT_BYTES,
        }
    }

    fn workspace_test_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join("test-work")
            .join(format!("{name}-{unique}"))
    }
}
